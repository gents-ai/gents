//! Numbering a commit's appends in the ordered step.
//!
//! A transaction's `append` calls queue as [`PendingAppend`]s and are not
//! read or validated for. Deciding the commit's slot, in ring order and once
//! the commit's reads are validated, gives each one the log's next position
//! and turns it into plain puts of the same atomic commit, so the WAL
//! carries final bytes. These rules follow `CommitOrderedAppend.tla`
//! and `Append.lean`:
//!
//! - Positions are assigned after validation, never before: a commit that
//!   fails validation reaches [`AppendOrder::number`] with nothing, so it
//!   takes no position.
//! - The head and the once keys come from the view plus the appends already
//!   numbered by this [`AppendOrder`], never from the view alone. The view
//!   holds every commit decided before this one: a slot is applied to the
//!   memtable before the next is decided (`pipeline.rs`, P2), so a later
//!   commit counts from the head an earlier one left and sees the once keys
//!   it set. One [`AppendOrder`] spans one commit's own appends.
//! - Nothing outlives the commit: no cached head, no remembered once key. A
//!   commit a failure discards is taken out of the memtable with what it
//!   numbered, and the next reads the view again, so a failure leaves no
//!   hole. A commit that fails while it is numbered gives back what it
//!   took, so the commits after it leave no hole either.
//! - Each commit writes the head of every log it moved in its own record.
//!
//! A commit that carries no append never reaches this module.

use std::collections::HashSet;
use std::io;
use std::sync::Arc;

use super::super::callback;
use super::super::memtable::MemTable;
use super::super::wal::{RecordLen, check_write_len};
use super::super::{ReadView, RegolithEngine};
use crate::column_family::{DEFAULT_CF_ID, prefix_key};
use crate::{Error, LogLayout, WriteBatchOp};

/// Length of the column-family prefix on the keys the engine stores.
const CF_PREFIX_LEN: usize = 4;

/// One `append` call a transaction queued: the log it names, the entry's
/// bytes, and the once key if the call gave one. Keys are as the caller wrote
/// them, without the column-family prefix.
#[derive(Clone)]
pub(crate) struct PendingAppend {
    pub(crate) log: Arc<dyn LogLayout>,
    pub(crate) entry: Vec<u8>,
    pub(crate) once_key: Option<Vec<u8>>,
}

/// Runs `f`, a call into the caller's [`LogLayout`]. Inside a commit a panic
/// in it is caught and returned as [`Error::CallbackPanicked`], which the
/// commit's boundary turns into the read-only latch.
fn layout<T>(f: impl FnOnce() -> T) -> io::Result<T> {
    callback::contain("LogLayout", f).map_err(Error::into_io_error)
}

/// The head one log has reached in the group being ordered.
struct LogHead {
    /// The log's head key, column-family prefixed.
    key: Vec<u8>,
    /// The newest position the view or an earlier member left: where the
    /// member being numbered starts.
    settled: u64,
    /// The newest position the member being numbered took; `settled` until it
    /// takes one.
    position: u64,
}

/// The running state of numbering one commit's appends.
///
/// Create one per commit, number the commit against it when its slot is
/// decided, and drop it with the decision.
#[derive(Default)]
pub(crate) struct AppendOrder {
    logs: Vec<LogHead>,
    /// Once keys, prefixed, that hold a position: set by an earlier append
    /// of this group, or found holding one in the view.
    taken: HashSet<Vec<u8>>,
    /// The once keys the member being numbered set, given back if it fails.
    member_taken: Vec<Vec<u8>>,
    /// Scratch for the caller's `entry_key`.
    key: Vec<u8>,
}

impl AppendOrder {
    /// Numbers one member's `appends`, in order, against `view` and the
    /// appends this order already numbered, and pushes the puts that write
    /// them onto `ops`: for each numbered append its entry and its once key,
    /// then, for each log the member moved, its head key once, holding the
    /// last position the member took. Each member writes the heads it moved
    /// in its own record, so a crash that keeps an earlier member of the
    /// group and loses a later one leaves the head where the kept member set
    /// it (`CommitOrderedAppend.tla`: each assignment writes `head_key = p`
    /// in the member's own atomic commit).
    ///
    /// An append whose once key already holds a position, in the view or
    /// through an earlier append, writes nothing. On error the member takes
    /// nothing: every position and once key it took is given back, so a later
    /// member numbers as if it had never been there. `ops` may then hold puts
    /// of appends numbered before the error, and the caller drops the member.
    pub(crate) fn number<A>(
        &mut self,
        engine: &RegolithEngine,
        view: &ReadView,
        appends: A,
        ops: &mut Vec<WriteBatchOp>,
    ) -> io::Result<()>
    where
        A: AsRef<[PendingAppend]>,
    {
        let numbered = self.number_appends(engine, view, appends.as_ref(), ops);
        if numbered.is_ok() {
            for head in &mut self.logs {
                if head.position != head.settled {
                    ops.push(WriteBatchOp::Put {
                        key: head.key.clone(),
                        value: head.position.to_be_bytes().to_vec(),
                    });
                    head.settled = head.position;
                }
            }
            self.member_taken.clear();
        } else {
            for head in &mut self.logs {
                head.position = head.settled;
            }
            for once in self.member_taken.drain(..) {
                self.taken.remove(&once);
            }
        }
        numbered
    }

    fn number_appends(
        &mut self,
        engine: &RegolithEngine,
        view: &ReadView,
        appends: &[PendingAppend],
        ops: &mut Vec<WriteBatchOp>,
    ) -> io::Result<()> {
        for PendingAppend {
            log,
            entry,
            once_key,
        } in appends
        {
            let once_key = once_key
                .as_deref()
                .map(|key| prefix_key(DEFAULT_CF_ID, key));
            if let Some(once) = &once_key
                && self.holds_position(engine, view, once)?
            {
                continue;
            }
            let at = self.head_of(engine, view, &**log)?;
            let position = self.logs[at].position.checked_add(1).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "the log has used every position a 64-bit number can name",
                )
            })?;
            self.key.clear();
            let declared = layout(|| {
                log.entry_key(position, &mut self.key);
                log.max_entry_key_len()
            })?;
            if self.key.len() > declared {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "a log layout built an entry key of {} bytes, longer than the {declared} it declared",
                        self.key.len(),
                    ),
                ));
            }
            ops.push(WriteBatchOp::Put {
                key: prefix_key(DEFAULT_CF_ID, &self.key),
                value: entry.clone(),
            });
            if let Some(once) = once_key {
                self.taken.insert(once.clone());
                self.member_taken.push(once.clone());
                ops.push(WriteBatchOp::Put {
                    key: once,
                    value: position.to_be_bytes().to_vec(),
                });
            }
            self.logs[at].position = position;
        }
        Ok(())
    }

    /// Whether `once` (prefixed) already holds a position: through an earlier
    /// append of this order, or in the view. A key found in the view is
    /// remembered, so the view is read once per key.
    fn holds_position(
        &mut self,
        engine: &RegolithEngine,
        view: &ReadView,
        once: &[u8],
    ) -> io::Result<bool> {
        if self.taken.contains(once) {
            return Ok(true);
        }
        let held = engine.read_u64_in_view(once, view)?.is_some();
        if held {
            self.taken.insert(once.to_vec());
        }
        Ok(held)
    }

    /// The index in `self.logs` of `log`'s head, read from the view the first
    /// time this order meets the log.
    fn head_of(
        &mut self,
        engine: &RegolithEngine,
        view: &ReadView,
        log: &dyn LogLayout,
    ) -> io::Result<usize> {
        let key = layout(|| prefix_key(DEFAULT_CF_ID, log.head_key()))?;
        // vertexia: linear scan over the logs of one group, which is a
        // handful; a map if a group ever spans thousands of logs.
        if let Some(at) = self.logs.iter().position(|head| head.key == key) {
            return Ok(at);
        }
        let position = engine.read_u64_in_view(&key, view)?.unwrap_or(0);
        self.logs.push(LogHead {
            key,
            settled: position,
            position,
        });
        Ok(self.logs.len() - 1)
    }
}

impl RegolithEngine {
    /// Refuses a commit whose appends cannot fit, before it waits for a place
    /// in the pipeline: a key or entry past the configured limits, or a
    /// record that would pass the WAL limit once the appends are numbered.
    ///
    /// Positions do not exist yet, so each entry key counts at its layout's
    /// declared maximum and each log counts its head put once. The ordered
    /// step re-checks the keys it builds; the bound only makes this answer
    /// early.
    ///
    /// Returns the bounds the commit pipeline admits the commit by: the
    /// framed bytes of its record with the appends numbered, and the memtable
    /// bytes the appends' puts can add.
    pub(super) fn validate_append_sizes(
        &self,
        ops: &[WriteBatchOp],
        appends: &[PendingAppend],
    ) -> io::Result<(usize, usize)> {
        let mut record = RecordLen::default();
        ops.iter().for_each(|op| record.op(op));
        let mut cost = 0usize;
        let mut put = |record: &mut RecordLen, key_len: usize, value_len: usize| {
            record.put_sized(key_len, value_len);
            cost = cost.saturating_add(MemTable::max_entry_size(key_len, value_len));
        };
        let mut heads: Vec<&[u8]> = Vec::new();
        for append in appends {
            let log = &*append.log;
            let (head, max_entry_key) = layout(|| (log.head_key(), log.max_entry_key_len()))?;
            self.validate_user_key_len(head.len())?;
            self.validate_user_key_len(max_entry_key)?;
            self.validate_value_size(&append.entry)?;
            put(
                &mut record,
                CF_PREFIX_LEN + max_entry_key,
                append.entry.len(),
            );
            if let Some(once) = &append.once_key {
                self.validate_user_key_len(once.len())?;
                put(&mut record, CF_PREFIX_LEN + once.len(), size_of::<u64>());
            }
            // vertexia: linear scan over the distinct logs of one commit;
            // a set if a commit ever spans thousands of logs.
            if !heads.contains(&head) {
                heads.push(head);
                put(&mut record, CF_PREFIX_LEN + head.len(), size_of::<u64>());
            }
        }
        check_write_len(record.framed())?;
        Ok((record.framed(), cost))
    }
}
