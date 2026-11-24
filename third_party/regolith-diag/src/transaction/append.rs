//! `Transaction::append`: queueing entries for a commit-ordered log.
//!
//! The transaction only queues. A position is the log's next number at the
//! moment the commit is ordered, so it cannot exist before commit and the
//! transaction cannot read its own appends. The numbering is in
//! `engine::commit::append`.

use std::ops::ControlFlow;
use std::sync::Arc;

use super::policy::classifier_for;
use super::{KeyClass, KeyClassifier, Transaction, TransactionError, TxResult};
use crate::engine::PendingAppend;
use crate::txn_buffer::TxnBuffer;
use crate::{Error, LogLayout};

impl Transaction {
    /// Adds `entry` to the commit-ordered log `log` describes, at the
    /// position the commit gives it.
    ///
    /// Nothing is written until commit. In the ordered step of the commit,
    /// after the transaction's reads are validated and before the write-ahead
    /// log is written, regolith:
    ///
    /// 1. skips the append, writing nothing, when `once_key` is given and
    ///    already holds a position, whether from an earlier commit or from an
    ///    earlier `append` of this transaction or of a commit in the same
    ///    group;
    /// 2. otherwise gives the entry the log's next position `p`, one past the
    ///    newest position any earlier commit, or earlier `append` of this
    ///    commit, took: dense from 1; and
    /// 3. writes `entry_key(p) = entry`, `once_key = p` (when given, as an
    ///    8-byte big-endian number) and `head_key = p`, in the same atomic
    ///    commit as the transaction's other writes.
    ///
    /// # Guarantees
    ///
    /// - **Commit order, no holes.** Positions follow commit order. Several
    ///   appends to one log in one transaction take consecutive positions in
    ///   the order they were made. A transaction that aborts, fails
    ///   validation or fails to reach the log takes no position, and the next
    ///   commit reuses it. A reader that reads the head key in a snapshot, and
    ///   then the entries up to it, finds every one of them that no commit at
    ///   or before its snapshot dropped with [`Transaction::truncate_log`].
    /// - **Never a conflict.** An append is never validated, so it cannot
    ///   fail a commit and no other commit's write can fail it. The head key
    ///   is not part of the transaction's read or write set, so two
    ///   transactions that append to one log and touch nothing else in
    ///   common both commit. This holds at every isolation level, for an
    ///   optimistic and a pessimistic transaction alike; `append` takes no
    ///   lock.
    /// - **At most once.** Of any number of appends sharing a `once_key`, in
    ///   one transaction or in many, only the first to commit writes an
    ///   entry. The key then holds the entry's position, so a caller can
    ///   look an entry up by it.
    /// - **Own appends are invisible at a position.** The positions do not
    ///   exist until commit, so nothing this transaction reads, a scan of the
    ///   log or its head included, shows its own appends. To see the entries
    ///   themselves, without positions, use [`Transaction::appended`].
    /// - **Rollback.** A savepoint saves the appends made before it, and
    ///   `rollback_to_savepoint` drops those made after.
    ///
    /// Durability is the database's. Under
    /// [`DurabilityMode::Eventual`](crate::DurabilityMode::Eventual) a crash
    /// can lose committed appends, and the next commit then gives their
    /// positions to other entries; the database stays consistent, but a
    /// consumer outside it that recorded a position should use
    /// [`DurabilityMode::Immediate`](crate::DurabilityMode::Immediate).
    ///
    /// # Errors
    ///
    /// Refused here, with the transaction unchanged, as
    /// [`Error::InvalidArgument`]: a layout whose entry key for the largest
    /// position is longer than [`LogLayout::max_entry_key_len`], and, when a
    /// [`KeyClassifier`] is installed and applies to this transaction (an
    /// optimistic transaction at [`IsolationLevel::DefraLevel`]), a layout
    /// whose head or entry key, or a `once_key`, the classifier does not
    /// declare [`KeyClass::Log`]. At commit, a key or entry beyond the
    /// database's size limits, a commit too large to log, a head or once key
    /// that holds anything but eight bytes, or a layout that builds an entry
    /// key longer than it declared fails the commit with
    /// [`Error::InvalidArgument`], and nothing is applied.
    ///
    /// # Preconditions
    ///
    /// Only appends and [`Transaction::truncate_log`] write the keys of a
    /// log, and a truncate writes only deletes of entry keys. A write to them
    /// by anything else, a plain [`Db::put`](crate::Db::put) or a pessimistic
    /// transaction (which consults no classifier) included, can number two
    /// entries alike or leave the head behind its entries, and regolith
    /// cannot tell. The layout must follow the contract of [`LogLayout`], and
    /// a log must be appended to through one layout, and no two logs may
    /// share a key.
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
    /// let position = |key: &[u8]| -> Result<Option<u64>, Box<dyn std::error::Error>> {
    ///     let held = db.db().get(key)?;
    ///     Ok(held.map(|bytes| u64::from_be_bytes(bytes.try_into().expect("eight bytes"))))
    /// };
    ///
    /// // Two transactions append at once and touch nothing else in common:
    /// // neither conflicts, and each entry gets its own position.
    /// let (one, two) = (db.begin(&TxnOptions::new()), db.begin(&TxnOptions::new()));
    /// one.append(&journal, b"order 7 placed", Some(b"placed/7"))?;
    /// two.append(&journal, b"order 8 placed", Some(b"placed/8"))?;
    /// // The second to append commits first, and so takes position 1.
    /// two.commit()?;
    /// one.commit()?;
    /// assert_eq!(position(b"placed/8")?, Some(1));
    /// assert_eq!(position(b"placed/7")?, Some(2));
    ///
    /// // A retry that appends under a once key already used adds nothing.
    /// let retry = db.begin(&TxnOptions::new());
    /// retry.append(&journal, b"order 7 placed", Some(b"placed/7"))?;
    /// retry.commit()?;
    /// assert_eq!(position(b"placed/7")?, Some(2));
    /// assert_eq!(position(Journal.head_key())?, Some(2));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// [`IsolationLevel::DefraLevel`]: crate::IsolationLevel::DefraLevel
    pub fn append(
        &self,
        log: &Arc<dyn LogLayout>,
        entry: &[u8],
        once_key: Option<&[u8]>,
    ) -> TxResult<()> {
        check_layout(
            &**log,
            once_key,
            classifier_for(&self.policy, self.isolation),
        )?;
        self.appends_buffer().insert(
            (),
            PendingAppend {
                log: Arc::clone(log),
                entry: entry.to_vec(),
                once_key: once_key.map(<[u8]>::to_vec),
            },
        );
        Ok(())
    }

    /// The entries this transaction has appended to the log `log` describes,
    /// oldest first: the order the commit gives them positions.
    ///
    /// This is the one way to read a transaction's own appends back. Their
    /// positions do not exist until commit, so [`Transaction::append`] keeps
    /// them out of every read at a position; this returns the entry bytes
    /// alone, never a position.
    ///
    /// # Guarantees
    ///
    /// - **Order.** The entries come in the order the `append` calls were
    ///   made, which is the order the commit numbers them: the last one
    ///   returned takes the highest of this transaction's positions.
    /// - **One log.** Only appends to the log with the same
    ///   [`LogLayout::head_key`] as `log` are returned. A log is identified
    ///   by that key, so a fresh layout value for the same log (as callers
    ///   build per call) names the same log. Appends to other logs are left
    ///   out.
    /// - **Rollback.** After [`Transaction::rollback_to_savepoint`] only the
    ///   appends kept are returned.
    /// - **Every append.** An append whose `once_key` the commit will skip,
    ///   because the key already holds a position, is returned all the same:
    ///   the transaction cannot know that before the commit orders it. The
    ///   entry then gets no position of its own.
    /// - **Free of the engine.** It does no I/O, records no read, is never
    ///   validated (so it cannot make a commit conflict), never returns
    ///   [`Error::WouldBlock`], and whatever the log or the database holds,
    ///   each call walks every append this transaction has made and copies
    ///   the matching entries. A caller on a hot path, or one that only needs
    ///   to know whether there are any, uses [`Transaction::with_appended`],
    ///   which copies nothing.
    ///
    /// # What it does not give
    ///
    /// No positions, and nothing of what other transactions or earlier
    /// commits appended: it is this transaction's queue and no more. To read
    /// the committed log, scan it at its positions as before. An entry a
    /// reader finds there once this transaction commits is the same bytes
    /// returned here, but a caller must not assume a skipped append's entry
    /// is in the log.
    ///
    /// # Misuse
    ///
    /// Treating the index of an entry in the returned list as its position:
    /// positions are the log's next numbers at commit and depend on other
    /// commits. Two layouts that share a head key but differ in their entry
    /// keys are one log here, which the [`LogLayout`] contract already
    /// forbids.
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
    /// let tx = db.begin(&TxnOptions::new());
    /// assert!(tx.appended(&journal).is_empty());
    /// tx.append(&journal, b"first", None)?;
    /// tx.append(&journal, b"second", None)?;
    /// assert_eq!(tx.appended(&journal), vec![b"first".to_vec(), b"second".to_vec()]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// [`Error::WouldBlock`]: crate::Error::WouldBlock
    pub fn appended(&self, log: &Arc<dyn LogLayout>) -> Vec<Vec<u8>> {
        let mut entries = Vec::new();
        self.with_appended(log, |entry| {
            entries.push(entry.to_vec());
            ControlFlow::Continue(())
        });
        entries.reverse();
        entries
    }

    /// Hands each entry this transaction has appended to the log `log`
    /// describes to `visit`, **newest first**, borrowed: no copy and no
    /// allocation. `visit` returns [`ControlFlow::Break`] to stop early.
    ///
    /// The same entries as [`Transaction::appended`], with the same
    /// guarantees except order: this is the order the buffer holds them in,
    /// the reverse of the commit's. A reader that does not care about order,
    /// such as a search over pending entries, reads them here for free.
    /// Whether there are any is a visit that breaks on the first.
    ///
    /// An append made from inside `visit` is not visited: the walk starts at
    /// the newest entry there was when it began.
    ///
    /// ```
    /// use std::ops::ControlFlow;
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
    /// let tx = db.begin(&TxnOptions::new());
    /// tx.append(&journal, b"first", None)?;
    /// tx.append(&journal, b"second", None)?;
    ///
    /// let mut bytes = 0;
    /// tx.with_appended(&journal, |entry| {
    ///     bytes += entry.len();
    ///     ControlFlow::Continue(())
    /// });
    /// assert_eq!(bytes, 11);
    ///
    /// let mut any = false;
    /// tx.with_appended(&journal, |_| {
    ///     any = true;
    ///     ControlFlow::Break(())
    /// });
    /// assert!(any);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn with_appended<F>(&self, log: &Arc<dyn LogLayout>, mut visit: F)
    where
        F: FnMut(&[u8]) -> ControlFlow<()>,
    {
        let Some(appends) = self.appends.get() else {
            return;
        };
        let head = log.head_key();
        appends.walk_chain(&(), |pending| {
            if pending.log.head_key() == head {
                visit(&pending.entry)?;
            }
            ControlFlow::Continue(())
        });
    }

    /// Refuses a put, delete or merge of `key` when the installed classifier
    /// declares it [`KeyClass::Log`]. A transaction with no classifier to
    /// consult, which is every pessimistic one, accepts it.
    pub(super) fn refuse_log_key(&self, key: &[u8]) -> TxResult<()> {
        match classifier_for(&self.policy, self.isolation) {
            Some(classifier) if classifier.classify(key) == KeyClass::Log => {
                Err(TransactionError::Engine(Error::LogKeyWrite))
            }
            _ => Ok(()),
        }
    }

    /// The buffer the appends are queued in, made by the first one.
    pub(super) fn appends_buffer(&self) -> &TxnBuffer<(), PendingAppend> {
        self.appends.get_or_init(|| Box::new(TxnBuffer::new(0)))
    }

    /// How many appends the transaction has made: the mark a savepoint keeps.
    pub(super) fn appends_len(&self) -> usize {
        self.appends.get().map_or(0, |appends| appends.len())
    }

    /// Takes back the appends made after a savepoint that marked `len` of them.
    pub(super) fn truncate_appends(&mut self, len: usize) {
        if let Some(appends) = self.appends.get_mut() {
            appends.truncate(len);
        }
    }
}

/// Checks what `append` can know without a position: the longest entry key
/// the layout builds fits what it declared, and, when `classifier` applies,
/// every key of the log is a [`KeyClass::Log`] key.
///
/// The longest position stands for every other: a layout that grows its keys
/// with the position, as a decimal or hexadecimal rendering does, is longest
/// there. The commit checks the keys it actually builds.
pub(super) fn check_layout(
    log: &dyn LogLayout,
    once_key: Option<&[u8]>,
    classifier: Option<&dyn KeyClassifier>,
) -> TxResult<()> {
    let mut entry_key = Vec::new();
    log.entry_key(u64::MAX, &mut entry_key);
    let declared = log.max_entry_key_len();
    if entry_key.len() > declared {
        return Err(refused(format!(
            "a log layout builds an entry key of {} bytes but declares a maximum of {declared}",
            entry_key.len()
        )));
    }
    if let Some(classifier) = classifier {
        let log_keys = [log.head_key(), entry_key.as_slice()]
            .into_iter()
            .chain(once_key)
            .all(|key| classifier.classify(key) == KeyClass::Log);
        if !log_keys {
            return Err(refused(
                "the installed key classifier does not declare every key of this log \
                 (head, entry and once keys) as a log key"
                    .to_owned(),
            ));
        }
    }
    Ok(())
}

pub(super) fn refused(message: String) -> TransactionError {
    TransactionError::Engine(Error::InvalidArgument(message))
}
