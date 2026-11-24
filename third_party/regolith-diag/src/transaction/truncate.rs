//! `Transaction::truncate_log`: dropping a range of a commit-ordered log's
//! entries.
//!
//! The transaction buffers one plain delete for the entry key of each
//! position. Nothing about the log's head or once keys is written, and the
//! buffered deletes bypass `refuse_log_key`, which only `put`, `delete` and
//! `merge` consult.

use std::ops::RangeInclusive;
use std::sync::Arc;

use super::append::{check_layout, refused};
use super::policy::classifier_for;
use super::write_buffer::Write;
use super::{Transaction, TxResult, read_error};
use crate::LogLayout;
use crate::column_family::{DEFAULT_CF_ID, prefix_key};

impl Transaction {
    /// Drops the entries of the commit-ordered log `log` describes at
    /// `positions`, in the same atomic commit as the transaction's other
    /// writes.
    ///
    /// Nothing is written until commit. The call reads the log's head key at
    /// the snapshot this transaction reads at, without recording the read,
    /// and buffers a delete of the entry key of each position in
    /// `positions`. Position 0 never holds an entry, so it is skipped; a
    /// range that is empty (its start is past its end, or it is only
    /// position 0) is a no-op.
    ///
    /// # Guarantees
    ///
    /// - **Drops exactly those entries.** At commit the entry key of every
    ///   position in `positions` is deleted, and nothing else of the log is
    ///   written. The head key and the once keys are untouched, so the next
    ///   `append` takes the position it would have taken, and a once key
    ///   still names the position its entry had. A position whose entry is
    ///   already gone is deleted again, which changes nothing a reader can
    ///   see.
    /// - **Never a conflict.** A truncate is not validated: it adds no read
    ///   to the transaction, and the delete of an entry another commit
    ///   already deleted is not a conflict. Two transactions that truncate
    ///   overlapping ranges both commit, and so do a truncate and a
    ///   concurrent `append` to the same log: an append writes only
    ///   positions past the head, and a truncate only positions at or below
    ///   the head it read. This holds at every isolation level, for an
    ///   optimistic and a pessimistic transaction alike. A conflict can only
    ///   come from a read the transaction itself made of an entry it
    ///   truncates, at a level that validates reads and for a key no
    ///   classifier declares [`KeyClass::Log`](crate::KeyClass::Log).
    /// - **No lock, by design.** `truncate_log` is the one pessimistic write
    ///   that takes no lock, and that is deliberate, not a precedent for
    ///   other writes: a delete is a version like any other, and the deletes
    ///   of overlapping truncates elide into one, so a lock would order
    ///   nothing a reader can see and would only give truncate a way to
    ///   block. So a lock another pessimistic transaction holds on an entry
    ///   key (`get_for_update`) does not hold against it: the entry can be
    ///   dropped while the lock is held.
    /// - **Snapshots keep their entries.** A delete is a version like any
    ///   other: a [`Snapshot`](crate::Snapshot) taken, or a transaction
    ///   begun, before the truncating commit still reads every entry it
    ///   dropped. Only once that snapshot or transaction is dropped may a
    ///   compaction remove those versions. A reader that reads the head in a
    ///   snapshot and then the entries up to it finds every entry that no
    ///   commit at or before its snapshot truncated.
    /// - **Rollback.** A savepoint saves the truncates made before it, and
    ///   `rollback_to_savepoint` drops those made after, as it does any
    ///   buffered write.
    ///
    /// # Cost, and truncating a long range
    ///
    /// The call costs one buffered delete per position in `positions`, and
    /// the commit then writes them all in one WAL record and one memtable
    /// insert each: O(range) memory in the transaction and O(range) work at
    /// commit, bounded by nothing but the range. regolith does not assume the
    /// entry keys of a log sort by position, so it cannot use one range
    /// delete. Pass only the positions not yet truncated: truncating `1..=n`
    /// after `1..=m` with `m < n` writes `n` deletes, not `n - m`.
    ///
    /// So truncate a long range, such as the first pruning of a log that has
    /// grown for months, in slices: a slice of at most `K` positions per
    /// transaction, each committed before the next begins, with `K` sized to
    /// the write batch the caller is willing to hold in memory (thousands, not
    /// millions). That is safe to stop at any point. Truncates commute and
    /// are idempotent, so a crash between slices leaves the log valid, with a
    /// prefix truncated, and the caller resumes with the first slice it has
    /// no record of committing; a slice committed twice changes nothing.
    /// Splitting gives up only the atomicity of the whole range.
    ///
    /// ```
    /// use std::sync::Arc;
    ///
    /// use regolith::{LogLayout, OptimisticTransactionDb, Options, TxnOptions};
    /// # struct Journal;
    /// # impl LogLayout for Journal {
    /// #     fn head_key(&self) -> &[u8] {
    /// #         b"journal-head"
    /// #     }
    /// #     fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
    /// #         out.extend_from_slice(b"journal/");
    /// #         out.extend_from_slice(&position.to_be_bytes());
    /// #     }
    /// #     fn max_entry_key_len(&self) -> usize {
    /// #         b"journal/".len() + 8
    /// #     }
    /// # }
    /// let db = OptimisticTransactionDb::open("journal", Options::memory())?;
    /// let journal: Arc<dyn LogLayout> = Arc::new(Journal);
    /// let writer = db.begin(&TxnOptions::new());
    /// for n in 0..10u8 {
    ///     writer.append(&journal, &[n], None)?;
    /// }
    /// writer.commit()?;
    ///
    /// // Drop positions 1 to 9, at most K = 4 per commit: 1..=4, 5..=8, 9..=9.
    /// const K: u64 = 4;
    /// let (first, last) = (1, 9);
    /// let mut start = first;
    /// while start <= last {
    ///     let end = (start + K - 1).min(last);
    ///     let slice = db.begin(&TxnOptions::new());
    ///     slice.truncate_log(&journal, start..=end)?;
    ///     slice.commit()?;
    ///     start = end + 1;
    /// }
    ///
    /// let mut key = Vec::new();
    /// Journal.entry_key(10, &mut key);
    /// assert!(db.db().get(&key)?.is_some());
    /// key.clear();
    /// Journal.entry_key(9, &mut key);
    /// assert!(db.db().get(&key)?.is_none());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Refused here, with the transaction unchanged, as
    /// [`Error::InvalidArgument`](crate::Error::InvalidArgument):
    ///
    /// - the checks `append` makes of a layout: an entry key for the largest
    ///   position longer than [`LogLayout::max_entry_key_len`], and, when a
    ///   [`KeyClassifier`](crate::KeyClassifier) is installed and applies to
    ///   this transaction (an optimistic transaction at
    ///   [`IsolationLevel::DefraLevel`](crate::IsolationLevel::DefraLevel)), a
    ///   layout whose head or entry key the classifier does not declare
    ///   [`KeyClass::Log`](crate::KeyClass::Log);
    /// - a range that reaches past the head this transaction can read: its
    ///   end is above the head key's value, or the log is empty. A truncate
    ///   cannot drop an entry that does not exist yet, and positions past
    ///   the head are the ones the next appends take;
    /// - a head key that holds anything but eight bytes.
    ///
    /// # Preconditions
    ///
    /// The layout must follow the contract of [`LogLayout`], and a log must
    /// be appended to and truncated through one layout. A write to the keys
    /// of the log by anything but `append` and `truncate_log`, a plain
    /// [`Db::put`](crate::Db::put) or a pessimistic transaction (which
    /// consults no classifier) included, is outside the guarantees above.
    /// Under [`DurabilityMode::Eventual`](crate::DurabilityMode::Eventual) a
    /// crash can lose a committed truncate; the entries are then back, and
    /// the log is as valid as before it.
    ///
    /// # Example
    ///
    /// `Journal` is the layout of the [`LogLayout`] example: the head at
    /// `journal-head`, and entries at `journal/` followed by the position.
    ///
    /// ```
    /// use std::sync::Arc;
    ///
    /// use regolith::{LogLayout, OptimisticTransactionDb, Options, TxnOptions};
    /// # struct Journal;
    /// # impl LogLayout for Journal {
    /// #     fn head_key(&self) -> &[u8] {
    /// #         b"journal-head"
    /// #     }
    /// #     fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
    /// #         out.extend_from_slice(b"journal/");
    /// #         out.extend_from_slice(&position.to_be_bytes());
    /// #     }
    /// #     fn max_entry_key_len(&self) -> usize {
    /// #         b"journal/".len() + 8
    /// #     }
    /// # }
    ///
    /// let db = OptimisticTransactionDb::open("journal", Options::memory())?;
    /// let journal: Arc<dyn LogLayout> = Arc::new(Journal);
    /// let entry = |position: u64| {
    ///     let mut key = Vec::new();
    ///     Journal.entry_key(position, &mut key);
    ///     db.db().get(&key)
    /// };
    ///
    /// let writer = db.begin(&TxnOptions::new());
    /// for text in [&b"one"[..], b"two", b"three"] {
    ///     writer.append(&journal, text, None)?;
    /// }
    /// writer.commit()?;
    ///
    /// // A snapshot taken now keeps reading all three entries.
    /// let reader = db.db().snapshot();
    ///
    /// // Drop the first two. The head stays where it was.
    /// let truncate = db.begin(&TxnOptions::new());
    /// truncate.truncate_log(&journal, 1..=2)?;
    /// truncate.commit()?;
    /// assert_eq!((entry(1)?, entry(2)?), (None, None));
    /// assert_eq!(entry(3)?.as_deref(), Some(&b"three"[..]));
    /// assert!(db.db().get(Journal.head_key())?.is_some());
    ///
    /// // The snapshot still sees what was dropped.
    /// let mut key = Vec::new();
    /// Journal.entry_key(1, &mut key);
    /// assert_eq!(reader.get(&key)?.as_deref(), Some(&b"one"[..]));
    ///
    /// // Positions keep counting from the head: the next entry is the fourth.
    /// let later = db.begin(&TxnOptions::new());
    /// later.append(&journal, b"four", None)?;
    /// later.commit()?;
    /// assert_eq!(entry(4)?.as_deref(), Some(&b"four"[..]));
    ///
    /// // A range past the head is refused.
    /// let past = db.begin(&TxnOptions::new());
    /// assert!(past.truncate_log(&journal, 1..=5).is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn truncate_log(
        &self,
        log: &Arc<dyn LogLayout>,
        positions: RangeInclusive<u64>,
    ) -> TxResult<()> {
        check_layout(&**log, None, classifier_for(&self.policy, self.isolation))?;
        let (start, end) = ((*positions.start()).max(1), *positions.end());
        if start > end {
            return Ok(());
        }
        let head = self.read_log_head(&**log)?;
        if end > head {
            return Err(refused(format!(
                "a truncate reaches position {end} but the log's head, as this transaction reads \
                 it, is {head}"
            )));
        }
        let mut entry_key = Vec::new();
        for position in start..=end {
            entry_key.clear();
            log.entry_key(position, &mut entry_key);
            self.writes
                .insert(prefix_key(DEFAULT_CF_ID, &entry_key), Write::Delete);
        }
        Ok(())
    }

    /// The head of `log` as this transaction reads it: 0 while the log is
    /// empty. The read is not recorded, so it is never validated, whatever
    /// the isolation level.
    fn read_log_head(&self, log: &dyn LogLayout) -> TxResult<u64> {
        let prefixed = prefix_key(DEFAULT_CF_ID, log.head_key());
        let _scope = self.read_scope();
        let horizon = self.read_horizon(&prefixed, false);
        let held = self
            .engine
            .get_slice_at(&prefixed, horizon)
            .map_err(|e| read_error(e, &prefixed))?;
        match held {
            None => Ok(0),
            Some(bytes) => <[u8; 8]>::try_from(&*bytes)
                .map(u64::from_be_bytes)
                .map_err(|_| {
                    refused(format!(
                        "a log head holds {} bytes where an 8-byte big-endian number is \
                         required; something else wrote it",
                        bytes.len()
                    ))
                }),
        }
    }
}
