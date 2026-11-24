//! Reserving a range of a `u64` counter in the ordered step (plan 3.7).
//!
//! The counter lives at a key as the last value reserved, eight big-endian
//! bytes, absent meaning zero. `allocate` takes a slot of the commit ring
//! like a write: when its turn comes, whichever thread decides the slot reads
//! the counter from the state the slot before it left, reserves the next `n`
//! values, and puts the new counter in the slot's record. This follows
//! `Allocate.tla` and `Allocate.lean`:
//!
//! - The caller does no I/O for its own record. The call returns once its
//!   slot is decided, and the record is written by whichever thread brings
//!   the pipeline past the slot next: a later commit, a flush, close. It is
//!   in the log before the record of any commit made after the call
//!   returned, since that commit's slot comes later, and a sync covers a
//!   prefix of the log. So a crash that keeps a commit using a value keeps
//!   the reservation, and the counter recovers at or above every value in
//!   use. Nothing is handed out ahead of the log a commit depends on (RED
//!   LogAfterUse), and the counter is never part of a transaction's write
//!   set (RED InTxn), so it never conflicts.
//! - The record asks for no sync of its own. It becomes durable with the
//!   next sync, at the latest with the first commit that uses one of its
//!   values. Until its record is written and its slot published, a read of
//!   the key shows the counter before the reservation: the range handed
//!   back is the authority, not the key.
//! - A log failure discards every slot decided and not yet published, a
//!   reservation's included, after its range was handed out. The database
//!   goes on, and the caller still holds the range, so the rollback writes
//!   the discarded reservations' counters back before any later slot is
//!   decided ([`RegolithEngine::write_back_reservations`]), and latches the
//!   database if it cannot (RED ForgetOnRollback): no value is handed out
//!   twice without a crash either.
//! - The counter is read once: the pipeline's state keeps it from then on
//!   (`counters.rs`), so a later reservation of the key reads no device.
//! - It does not wait for write capacity. A stall delays commits, not the
//!   counter, and the record is a few bytes.

use std::io;
use std::ops::Range;
use std::sync::Arc;

use super::super::io::job::{Delivery, Job};
use super::super::io::scope;
use super::super::io::shared::WaitSlot;
use super::super::wal::{check_write_len, encode_ops_record, ops_record_len};
use super::super::wal_frame::KIND_GROUP;
use super::super::{RegolithEngine, apply_batch_op_to_memtable, batch_op_wal_bytes};
use super::descriptor::{ABANDONED, After, Decision, Descriptor, FAILED, Verdict as Fate, Work};
use super::park::Help;
use super::pipeline::Turn;
use super::request::batch_op_memtable_cost;
use super::spare;
use crate::WriteBatchOp;
use crate::io_queue::{IoWait, QueueId};
use crate::statistics::Ticker;
use crate::sync::internal::Arc as RingArc;

impl RegolithEngine {
    /// Reserves `n` values of the counter at `key` (column-family prefixed)
    /// and returns them as a range. `n == 0` reserves nothing and writes
    /// nothing: the range is empty and starts at the next value.
    ///
    /// The counter holds the last value reserved, so the range starts one
    /// past it. A counter that would pass `u64::MAX - 1` is refused with
    /// `InvalidInput`, since a `Range<u64>` cannot end past `u64::MAX`; so is
    /// a counter that holds anything but eight bytes. Neither changes the
    /// counter.
    ///
    /// Returns once the slot is decided. It waits where a blocking commit
    /// entering the pipeline waits: for room in a full ring, and for an
    /// ordered unit ahead of its slot. The first read of a counter the
    /// pipeline does not keep yet may read the device.
    pub(crate) fn allocate(&self, key: Vec<u8>, n: u64) -> io::Result<Range<u64>> {
        self.ensure_writable()?;
        let pipe = self.pipe()?;
        let desc = spare::descriptor(
            Work::Allocate {
                key,
                n,
                read_queue: None,
            },
            true,
        );
        let index = self.enter(pipe, &desc);
        // Every slot up to this one is decided, this one included: the
        // reservation is made then. Its record is encoded here, so the
        // next run of the write unit takes it (`write_unit.rs`), and left
        // to be written by whoever passes the slot next.
        while let Turn::Blocked(job) = self.help_decide(pipe, index, Help::Wait, Some(&desc)) {
            self.run_or_wait(&job);
        }
        self.fill(&desc);
        let reserved = Self::reserved(&desc);
        spare::keep_descriptor(desc);
        reserved
    }

    /// [`Self::allocate`] for a caller that never waits and never reads the
    /// device: what would wait is handed back as a wait on `queue`, an
    /// `io::Error` carrying [`crate::WouldBlock::Io`], completed at that
    /// queue's poll, after which the call run again goes on.
    ///
    /// - A counter the pipeline does not keep yet, whose block the cache
    ///   does not hold: the slot is refused, taking nothing, and the read is
    ///   recorded on `queue`.
    /// - A full ring, a rollback under way, or a unit ahead of the slot that
    ///   another thread has or that may read the device: the wait is for
    ///   that unit. A slot already taken is given up, and is refused by
    ///   whoever decides it; if a helper decided it first, its values are
    ///   skipped.
    pub(crate) fn allocate_nowait(
        &self,
        queue: QueueId,
        key: Vec<u8>,
        n: u64,
    ) -> io::Result<Range<u64>> {
        self.ensure_writable()?;
        let Some(shared) = self.io().queue(queue) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the I/O queue this allocation names is not open on this database",
            ));
        };
        let pipe = self.pipe()?;
        // Neither this slot nor one this call helps decide reads the device.
        let _scope = scope::cache_only(queue);
        let desc = spare::descriptor(
            Work::Allocate {
                key,
                n,
                read_queue: Some(queue),
            },
            false,
        );
        let in_the_way = match self.try_enter(pipe, &desc, Help::Leave) {
            // The reservation was given up: nothing is taken.
            Err(job) => Some(job),
            Ok(index) => match self.help_decide(pipe, index, Help::Leave, Some(&desc)) {
                Turn::Done => {
                    // As a blocking reservation: encoded for the next run.
                    self.fill(&desc);
                    None
                }
                Turn::Blocked(job) => {
                    desc.mark(ABANDONED);
                    Some(job)
                }
            },
        };
        let outcome = match in_the_way {
            Some(job) => Err(self.wait_on_queue(job, &shared).into_io_error()),
            None => Self::reserved(&desc),
        };
        spare::keep_descriptor(desc);
        outcome
    }

    /// A wait on `queue` for `job` to land, completed at the queue's poll.
    fn wait_on_queue(
        &self,
        job: Arc<Job>,
        queue: &Arc<super::super::io::shared::QueueShared>,
    ) -> IoWait {
        let slot = Arc::new(WaitSlot::new());
        let waiter = Arc::clone(&slot) as Arc<dyn Delivery>;
        // A queue dropped meanwhile cannot be polled: the wait is over.
        if !self.io().wait_on(queue, job, Some(waiter)) {
            slot.complete();
        }
        IoWait::new(queue, None, slot)
    }

    /// What the reservation at slot `desc`, decided, tells its caller.
    fn reserved(desc: &Descriptor) -> io::Result<Range<u64>> {
        if desc.is(FAILED) {
            return Err(desc.failure.take().unwrap_or_else(|| {
                io::Error::other("the write-ahead log failed and this reservation was discarded")
            }));
        }
        match desc.decision().map(|decision| &decision.verdict) {
            Some(Fate::Allocated(range)) => Ok(range.clone()),
            Some(Fate::Refused(err)) => Err(crate::Error::clone_io(err)),
            _ => Err(io::Error::other(
                "a counter reservation settled as something else",
            )),
        }
    }

    /// Decide a reservation of `n` values of the counter at `key` from
    /// `base`: read the counter, which the pipeline's state keeps or the
    /// view holds as of every slot decided before this one, and set the
    /// verdict, the put that logs the new counter, and the counter the next
    /// slot reads.
    pub(super) fn decide_allocation(
        &self,
        key: &[u8],
        n: u64,
        base: &After,
        decision: &mut Decision,
    ) -> io::Result<()> {
        let last = match base.counters.get(key) {
            Some(last) => last,
            None => {
                let view = self.view.load();
                self.read_u64_in_view(key, &view)?.unwrap_or(0)
            }
        };
        let first = last.saturating_add(1);
        if n == 0 {
            decision.verdict = Fate::Allocated(first..first);
            decision.after.counters = base.counters.set(key, last);
            return Ok(());
        }
        let end = first
            .checked_add(n)
            .filter(|_| last < u64::MAX)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "the counter cannot reserve {n} more values below the largest 64-bit number"
                    ),
                )
            })?;
        decision.verdict = Fate::Allocated(first..end);
        decision.batch = Some(vec![WriteBatchOp::Put {
            key: key.to_vec(),
            value: (end - 1).to_be_bytes().to_vec(),
        }]);
        decision.after.counters = base.counters.set(key, end - 1);
        Ok(())
    }

    /// The refusal of a reservation its caller gave up before its slot was
    /// decided (`allocate_nowait`): it takes nothing. Nobody reads it.
    pub(super) fn abandoned(desc: &Descriptor) -> Option<io::Error> {
        desc.is(ABANDONED)
            .then(|| io::Error::other("a counter reservation its caller gave up"))
    }

    /// After a log failure discarded `failed`, the slots decided and not
    /// published, rebuild `after`, the state the next slot is decided from:
    /// the counter of every reservation among them is written back, at the
    /// last value any of them reserved, and kept. Their ranges may have
    /// been handed out already, and the database goes on.
    ///
    /// Called by the rollback, which holds the round, before any later slot
    /// is decided. A write that fails latches the database: it can no
    /// longer vouch that no value is handed out twice.
    pub(super) fn write_back_reservations(
        &self,
        after: &mut After,
        failed: &[RingArc<Descriptor>],
    ) {
        // Per key, the last value a discarded reservation reserved. As many
        // entries as discarded slots at most, which a ring holds.
        let mut last: Vec<(&[u8], u64)> = Vec::new();
        for desc in failed {
            let Work::Allocate { key, .. } = &desc.work else {
                continue;
            };
            let Some(Fate::Allocated(range)) = desc.decision().map(|decision| &decision.verdict)
            else {
                continue;
            };
            if range.is_empty() {
                continue;
            }
            match last.iter_mut().find(|(held, _)| *held == key.as_slice()) {
                Some(entry) => entry.1 = entry.1.max(range.end - 1),
                None => last.push((key, range.end - 1)),
            }
        }
        if last.is_empty() || self.is_latched() {
            return;
        }
        let ops: Vec<WriteBatchOp> = last
            .iter()
            .map(|(key, value)| WriteBatchOp::Put {
                key: key.to_vec(),
                value: value.to_be_bytes().to_vec(),
            })
            .collect();
        match self.log_alone(after, &ops) {
            Ok(()) => {
                for (key, value) in &last {
                    after.counters = after.counters.set(key, *value);
                }
            }
            Err(err) => {
                tracing::error!(error = %err, "writing back the counters of discarded reservations failed");
                self.latch_wal_failure(&err);
            }
        }
    }

    /// Log `ops` at the end of `after`'s log, apply them to its memtable and
    /// publish them, holding the round: what the rollback's write-back does.
    fn log_alone(&self, after: &mut After, ops: &[WriteBatchOp]) -> io::Result<()> {
        #[cfg(test)]
        if write_back_fails() {
            return Err(io::Error::other("a test failed the write-back"));
        }
        let payload = ops_record_len(ops);
        check_write_len(payload)?;
        let log = Arc::clone(&after.log);
        let place = log.place(after.next_off, payload, after.epoch)?;
        let base = after.next_seq;
        let mut record = vec![0u8; log.prefix_len()];
        encode_ops_record(&mut record, ops, base);
        log.finish_record(KIND_GROUP, &place, &mut record)?;
        log.write_record(&place, &record)?;
        if let Some(s) = self.statistics() {
            s.add(
                Ticker::WalBytesWritten,
                ops.iter().map(batch_op_wal_bytes).sum(),
            );
        }
        let memtable = &after.memtable;
        let mut hint = memtable.insert_hint();
        for (op, seq) in ops.iter().zip(base..) {
            apply_batch_op_to_memtable(memtable, &mut hint, op, seq);
        }
        let last = base + ops.len() as u64 - 1;
        after.next_seq = last + 1;
        after.next_off = place.end();
        after.mem_used = after.mem_used.saturating_add(
            ops.iter()
                .map(batch_op_memtable_cost)
                .fold(0usize, usize::saturating_add),
        );
        self.visible_seq.publish(last);
        Ok(())
    }
}

#[cfg(test)]
std::thread_local! {
    static WRITE_BACK_FAILS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test-only: whether the rollback's write-back on this thread fails.
#[cfg(test)]
fn write_back_fails() -> bool {
    WRITE_BACK_FAILS.with(std::cell::Cell::get)
}

/// Test-only: make the rollback's write-back on this thread fail, until
/// the guard is dropped.
#[cfg(test)]
pub(crate) fn fail_write_back() -> impl Drop {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            WRITE_BACK_FAILS.with(|fails| fails.set(false));
        }
    }
    WRITE_BACK_FAILS.with(|fails| fails.set(true));
    Restore
}
