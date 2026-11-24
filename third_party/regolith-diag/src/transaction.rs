//! ACID transactions on top of [`crate::Db`].
//!
//! Two flavors:
//!
//! - [`OptimisticTransactionDb`]: transactions take no locks,
//!   buffer writes in memory, and detect write-write conflicts at
//!   commit time by re-checking the visible seq of each touched key
//!   against the snapshot seq the transaction was anchored at.
//!   Best for low-contention workloads where most transactions
//!   commit on the first try.
//!
//! - [`TransactionDb`]: transactions acquire exclusive key locks
//!   as they write (or as [`Transaction::get_for_update`] is called)
//!   and hold them until commit / rollback. Contention is resolved
//!   when the lock is taken rather than at commit, and a
//!   timeout-based deadlock defense returns [`TransactionError::Busy`]
//!   when a lock cannot be acquired in time.
//!   [`Transaction::try_get_for_update`] never waits: it returns a
//!   [`LockWait`](crate::LockWait) instead. Best for workloads
//!   where contention is high and retry cost dominates.
//!
//! Both flavors share a single [`Transaction`] type. The type
//! carries the mode internally so user code can write helpers that
//! work against either db.
//!
//! # Isolation level
//!
//! A transaction runs at the [`IsolationLevel`] it is begun with, which
//! defaults to snapshot isolation. At every level it reads from a snapshot,
//! and at snapshot isolation and above both flavors prevent lost updates.
//! The two anchor their reads at different points.
//!
//! An optimistic transaction reads everything as of the engine seq
//! captured at begin. A pessimistic transaction reads a key it has
//! not locked at that same begin seq, and reads a key it locks
//! through [`Transaction::get_for_update`] as of the moment the
//! lock was acquired. The lock handoff orders it after every
//! transaction that committed while it waited, so a
//! read-modify-write through `get_for_update` sees the value the
//! previous lock holder committed instead of a stale one. Reads
//! that hit the transaction's own buffered writes always see the
//! written value, and a key the transaction merged into reads as the
//! merge operator folds its operands, in the order it made them, onto
//! the value it last put or deleted the key to, or else onto the
//! value it reads for the key. A read anchor only ever moves forward,
//! so two reads of the same key inside one transaction never travel
//! backwards in time.
//!
//! At commit each flavor validates a set of keys, every key against
//! the *earliest* sequence this transaction observed it at:
//!
//! - Optimistic: every key the transaction wrote or read through
//!   `get_for_update`, against the begin snapshot. At
//!   [`IsolationLevel::DefraLevel`], a key the transaction only merges into
//!   conflicts with a newer put, delete or range delete of the key, and not
//!   with a newer merge operand.
//! - Pessimistic: every key the transaction read and then wrote
//!   (the read-modify-write set) plus every key it read through
//!   `get_for_update`. A key written blind, without ever being
//!   read, is not validated: there is no read to invalidate, and the
//!   key lock already orders it against every other transaction.
//!
//! For a pessimistic transaction the check cannot fire while every
//! writer goes through the lock manager. It fires when a key the
//! transaction read was written around the lock manager (for example
//! through [`TransactionDb::db`]) or before the lock was taken,
//! which surfaces as [`TransactionError::Conflict`] rather than as
//! a lost update.
//!
//! Rolling back to a savepoint discards buffered writes; it does not
//! discard what the transaction has already read, so a read anchor
//! survives the rollback and still guards the commit. A savepoint is a
//! mark on the write buffer's entry count, so setting one is O(1) and
//! rolling back costs the writes it discards.
//!
//! Below [`IsolationLevel::RepeatableRead`] a plain read ([`Transaction::get`],
//! [`Transaction::get_slice`] or a transactional scan) of a key the
//! transaction never writes is not validated; at
//! [`IsolationLevel::SnapshotIsolation`] a [`Transaction::get_for_update`]
//! key is validated even when it is not written. At `RepeatableRead` every key a
//! point read returned is validated and a scanned key still is not; at
//! `Serializable` every key read by any of them is. At every level, a key a
//! transactional scan walked that the transaction then writes is validated
//! as a read from the begin snapshot, so a scan-then-write is never taken
//! for a blind write. One exception is a stretch that stays inside one
//! [`KeyClass::CommutativePrefix`] at [`IsolationLevel::DefraLevel`] with a
//! [`KeyClassifier`] installed: a write there is validated as a blind write.
//! The other is a key that classifier declares [`KeyClass::ContentAddressed`]:
//! at that level a put or merge of it is not validated, a read of it that
//! found a value is validated for its presence only, and a delete of it, or a
//! read that found nothing, is validated as for any key.
//!
//! # Callbacks
//!
//! [`Transaction::before_commit`], [`Transaction::on_commit`] and
//! [`Transaction::on_abort`] attach code to one transaction, and
//! [`TransactionHooks`] (through [`crate::Options::transaction_hooks`]) to
//! every transaction of a database, at every isolation level. Exactly one of
//! `on_commit` and `on_abort` runs for each transaction, once per callback,
//! whichever way it ends. The rules are on [`TransactionHooks`] and on each
//! method.
//!
//! # A constant write is not a claim
//!
//! A write of exactly the bytes the key holds at commit is not a conflict
//! (identical-write elision), because the schedule has a serial order that
//! reaches the same state. So two optimistic transactions that each put the
//! same constant under a key neither read both commit: writing a sentinel
//! does not claim the key.
//!
//! To claim a key, read it with [`Transaction::get_for_update`] first. At
//! commit that read is validated, so the later committer conflicts; under a
//! pessimistic transaction the read is taken under the key's lock and sees
//! what the earlier holder committed. An optimistic transaction can also write
//! a value unique to the writer, such as its own id: the later committer's
//! bytes then differ from the committed ones and it conflicts.
//!
//! # Limits
//!
//! - A plain scan does not detect phantoms. It records the stretches of keys
//!   it walked only to anchor keys the commit validates anyway, so a key
//!   another transaction inserts into the range after the snapshot is
//!   detected only when this transaction also writes it or validates it as a
//!   point read at its level. A scan whose result decides something is a
//!   [`Transaction::cursor`] with [`ScanCheck::Range`]: any write inside the
//!   range it covered since the snapshot is a conflict, at every level.
//! - [`Transaction::delete_range`] refuses a non-empty range: a range delete
//!   takes no part in conflict detection or in key locking.
//! - A deadlock between pessimistic transactions is found by the lock
//!   timeout ([`TransactionError::Busy`]) and not sooner: no wait-for graph
//!   is kept.
//! - A transaction reads and writes the default column family only. The
//!   other families are reached through [`Db`]'s `*_cf` methods, outside any
//!   transaction.

use crate::portability::{AtomicBool, AtomicU64, Ordering};
use kovan_queue::seg_queue::SegQueue;

use crate::txn_buffer::TxnBuffer;
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::column_family::{DEFAULT_CF_ID, prefix_key};
use crate::conflict::RedactedKey;
use crate::engine::commit::EarlyWrite;
use crate::engine::io::scope::Scope;
use crate::engine::{
    CommitOutcome, ConflictKey, PendingAppend, ReadRule, RegolithEngine, TxnCommit, ValidationSet,
    callback,
};
use crate::{Access, Conflict, Db, DbSlice, Error, Options, QueueId, ReadMode, Result};

mod append;
mod callbacks;
mod claim;
mod cursor;
mod early;
mod exclusive;
mod first_four;
mod handoff;
mod in_place;
#[cfg(loom)]
pub mod lock_loom;
mod lock_table;
mod locking;
mod nowait;
mod policy;
mod projection;
mod queue;
mod receipt;
mod retry;
mod scan_range;
mod truncate;
mod txn_options;
mod validated_range;
mod write_buffer;
pub use callbacks::{AbortReason, CommitInfo, TransactionHooks};
use callbacks::{Callbacks, Prepare};
pub(crate) use claim::Claim;
pub use cursor::{Page, ScanCheck, TxnCursor, TxnScanStream};
use lock_table::{KeyLock, LockWaits};
pub(crate) use lock_table::{LockOwner, LockTable};
pub use policy::{KeyClass, KeyClassifier};
use projection::Projection;
pub use receipt::CommitReceipt;
pub use retry::{RetryPolicy, TransactError, ready};
use scan_range::ScanRun;
pub use txn_options::TxnOptions;
use validated_range::RangeRecord;
use write_buffer::{Write, fold_merges, read_buffered, settle};

/// Default lock-acquisition timeout for [`TransactionDb`] when the
/// caller doesn't specify one on [`TransactionDb::with_lock_timeout`].
/// Tuned to "long enough that a fast transaction finishes, short
/// enough that a deadlock surfaces quickly".
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(1);

/// Reasons a transaction can fail to commit. Not a variant of
/// [`crate::Error`]: a conflict is a retry-able business outcome,
/// distinct from an engine failure.
#[derive(thiserror::Error)]
#[non_exhaustive]
pub enum TransactionError {
    /// The underlying engine failed, with the same typed [`Error`] a plain
    /// read or write reports: [`Error::Closed`] for a closed database,
    /// [`Error::Busy`] for a write stall the engine cannot relieve, and so on.
    /// Never [`Error::WouldBlock`]: that is [`TransactionError::WouldBlock`].
    #[error(transparent)]
    Engine(Error),
    /// The call returned instead of waiting. A read through a transaction
    /// begun with [`ReadMode::CacheOnly`](crate::ReadMode::CacheOnly)
    /// needed a block the cache does not hold: poll the queue the wait
    /// names, then run the call again (see [`TxnOptions::read_mode`]); a
    /// cursor or stream carries on where it stopped, and the read is not a
    /// reason to roll back. A [`Transaction::commit_nowait`] would have read
    /// the device to run its `before_commit` callbacks: the transaction
    /// applied nothing and ended with its `on_abort` callbacks; run it again
    /// once the wait is ready. A commit is never ended by a miss of its own
    /// validation, nor by a write stall ([`crate::WouldBlock::Stall`]): it
    /// waits on its queue for the read or the stall (see
    /// [`Transaction::commit_nowait`] and [`Transaction::commit`]).
    #[error("{0}")]
    WouldBlock(crate::WouldBlock),
    /// A key in the transaction's validation set was written by
    /// someone else after this transaction first observed it: after
    /// the begin snapshot for an optimistic transaction, or after
    /// the read that the pessimistic transaction is about to
    /// overwrite. The [`Conflict`] says what this transaction did with the
    /// key and what the newer write was; its message names the reason and
    /// the sequences but never the key's bytes. The caller should roll back
    /// and retry, or let [`OptimisticTransactionDb::transact`] do it.
    #[error("{0}")]
    Conflict(Conflict),
    /// A pessimistic transaction could not acquire a key lock in
    /// time. Indicates either high contention or a deadlock; the
    /// caller should roll back and retry (possibly with a
    /// different operation order). The payload is the key, without the
    /// column-family prefix; neither the message nor the `Debug` output
    /// prints its bytes, only its length and a short hash, since a key can
    /// hold user data.
    #[error("transaction busy acquiring the lock on a {}; retry the transaction", RedactedKey(.0))]
    Busy(Vec<u8>),
    /// The caller tried to roll back to or release a savepoint when none is
    /// set.
    #[error("no savepoint is set")]
    NoSavepoint,
    /// Transactional range deletes are disabled until they can
    /// participate in conflict detection or range locking.
    #[error("transactional range deletes are not supported")]
    UnsupportedRangeDelete,
}

// A hand-written `Debug`, because the derived one would print a `Busy` key's
// bytes into a log or a panic message.
impl std::fmt::Debug for TransactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Engine(e) => f.debug_tuple("Engine").field(e).finish(),
            Self::WouldBlock(w) => f.debug_tuple("WouldBlock").field(w).finish(),
            Self::Conflict(c) => f.debug_tuple("Conflict").field(c).finish(),
            Self::Busy(key) => f
                .debug_tuple("Busy")
                .field(&format_args!("{}", RedactedKey(key)))
                .finish(),
            Self::NoSavepoint => f.write_str("NoSavepoint"),
            Self::UnsupportedRangeDelete => f.write_str("UnsupportedRangeDelete"),
        }
    }
}

impl TransactionError {
    /// A copy of this error, for an outcome several readers are handed (see
    /// [`Error::duplicate`]).
    pub(crate) fn duplicate(&self) -> Self {
        match self {
            Self::Engine(err) => Self::Engine(err.duplicate()),
            Self::WouldBlock(would_block) => Self::WouldBlock(would_block.clone()),
            Self::Conflict(conflict) => Self::Conflict(conflict.clone()),
            Self::Busy(key) => Self::Busy(key.clone()),
            Self::NoSavepoint => Self::NoSavepoint,
            Self::UnsupportedRangeDelete => Self::UnsupportedRangeDelete,
        }
    }
}

/// Convenience alias for results returned by transaction methods.
pub type TxResult<T> = std::result::Result<T, TransactionError>;

/// A failed point read of `prefixed` as a transaction reports it: the typed
/// error a plain `get` gives, so a merge the operator declined is
/// [`Error::MergeFailed`] carrying the key.
fn read_error(err: std::io::Error, prefixed: &[u8]) -> TransactionError {
    crate::map_point_read_error(err, prefixed).into()
}

impl From<Error> for TransactionError {
    /// The engine's error as a transaction reports it: a wait is
    /// [`TransactionError::WouldBlock`], everything else
    /// [`TransactionError::Engine`].
    fn from(e: Error) -> Self {
        match e {
            Error::WouldBlock(would_block) => Self::WouldBlock(would_block),
            other => Self::Engine(other),
        }
    }
}

impl From<std::io::Error> for TransactionError {
    fn from(e: std::io::Error) -> Self {
        Error::from(e).into()
    }
}

/// Optimistic-concurrency-control wrapper over a [`Db`].
///
/// `begin` returns a fresh [`Transaction`] whose
/// `commit` performs write-write conflict detection against the
/// seq captured at begin time. No locks are taken; other writers
/// proceed in parallel. Conflicts surface as
/// [`TransactionError::Conflict`].
pub struct OptimisticTransactionDb {
    isolation: IsolationLevel,
    policy: Option<Arc<dyn KeyClassifier>>,
    inner: Db,
}

impl std::fmt::Debug for OptimisticTransactionDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OptimisticTransactionDb")
            .finish_non_exhaustive()
    }
}

impl OptimisticTransactionDb {
    /// Open or create an optimistic-transaction database at `path`.
    pub fn open<P: AsRef<Path>>(path: P, opts: Options) -> Result<Self> {
        Ok(Self {
            inner: Db::open(path, opts)?,
            isolation: IsolationLevel::default(),
            policy: None,
        })
    }

    /// Install the [`KeyClassifier`] that [`IsolationLevel::DefraLevel`]
    /// consults. Transactions at any other level ignore it.
    ///
    /// Without a classifier every scan is recorded as at
    /// [`IsolationLevel::RepeatableRead`], and every read and write is
    /// validated as there. With one, the commit disregards a scan stretch
    /// that stays inside one [`KeyClass::CommutativePrefix`], so a write or
    /// merge the transaction makes inside that stretch is validated as a
    /// blind write instead of as a read. It does not validate a put or merge
    /// of a [`KeyClass::ContentAddressed`] key, nor a read of a key the
    /// transaction puts or merges, and validates a read of one that found it
    /// for presence only, so two transactions that create the same such key
    /// both commit while a delete of it is still checked.
    ///
    /// This is safe only when nothing the transaction writes outside the
    /// prefix depends on which keys of the prefix the scan returned, its
    /// writes inside the prefix are unique keys or identical rewrites, and a
    /// content-addressed key never holds different bytes. regolith catches a
    /// put of different bytes beside a newer commit of the key with
    /// [`Error::ContentMismatch`], and cannot check the rest, so it rests on
    /// the caller's word.
    ///
    /// The classifier does not change how blind merges are validated: they
    /// commute at this level for every key.
    pub fn with_policy(mut self, classifier: Arc<dyn KeyClassifier>) -> Self {
        self.policy = Some(classifier);
        self
    }

    /// Borrow the underlying [`Db`] for APIs that
    /// [`OptimisticTransactionDb`] doesn't wrap: snapshots, the
    /// streaming iterator, `compact_range`, `close`, etc.
    pub fn db(&self) -> &Db {
        &self.inner
    }

    /// Start a new optimistic transaction anchored at the engine's
    /// current sequence number. Reads see a consistent view as of
    /// that seq; writes buffer in memory until [`Transaction::commit`].
    ///
    /// The transaction runs at `opts`' [`IsolationLevel`], or at this
    /// database's default when `opts` sets none. It borrows nothing from the
    /// database, so it can be stored, moved to another thread, or held past
    /// the database handle that began it.
    pub fn begin(&self, opts: &TxnOptions) -> Transaction {
        let engine = self.inner.engine_arc();
        let (snapshot_seq, snapshot_pin) = engine.register_snapshot_at_horizon();
        Transaction::new(
            engine,
            snapshot_seq,
            snapshot_pin,
            self.inner.durability(),
            TxMode::Optimistic,
            None,
            DEFAULT_LOCK_TIMEOUT,
            opts.resolve_isolation(self.isolation),
            self.inner.transaction_keys_inline(),
            self.policy.clone(),
            opts.early(),
            opts.reading(),
        )
    }

    /// Set the default [`IsolationLevel`] for transactions this database
    /// begins. Defaults to [`IsolationLevel::SnapshotIsolation`].
    pub fn with_isolation(mut self, isolation: IsolationLevel) -> Self {
        self.isolation = isolation;
        self
    }

    /// The default level [`Self::begin`] uses when its [`TxnOptions`] set none.
    pub fn isolation(&self) -> IsolationLevel {
        self.isolation
    }

    /// [`Db::allocate`] on the underlying database: reserves `n` values of
    /// the counter at `key`, outside any transaction and never a conflict.
    pub fn allocate(&self, key: &[u8], n: u64) -> Result<Range<u64>> {
        self.inner.allocate(key, n)
    }

    /// [`Db::allocate_nowait`] on the underlying database: the reservation
    /// that never waits, handing back a wait on `queue` instead.
    pub fn allocate_nowait(&self, queue: QueueId, key: &[u8], n: u64) -> Result<Range<u64>> {
        self.inner.allocate_nowait(queue, key, n)
    }
}

/// Pessimistic-concurrency-control wrapper over a [`Db`].
///
/// Transactions acquire exclusive locks on every key they touch
/// for update (via `put`, `delete`, `merge`, `get_for_update` or
/// `try_get_for_update`) and hold the locks until commit or rollback.
/// Lock contention is resolved immediately, not at commit. A
/// timeout-based deadlock defense surfaces as [`TransactionError::Busy`]
/// when a lock cannot be acquired in time.
///
/// The locks live in a lock-free table: taking a free lock is one
/// compare-and-swap, and no call takes a mutex or waits on a condition
/// variable. A free lock goes to whoever asks first, even while others
/// wait, and a waiter passed over [`MAX_BYPASS`](crate::sync::MAX_BYPASS)
/// times is handed it, so no waiter starves.
pub struct TransactionDb {
    isolation: IsolationLevel,
    inner: Db,
    locks: Arc<LockTable>,
    lock_timeout: Duration,
}

impl std::fmt::Debug for TransactionDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransactionDb")
            .field("lock_timeout", &self.lock_timeout)
            .finish_non_exhaustive()
    }
}

impl TransactionDb {
    /// Open or create a pessimistic-transaction database at `path`.
    /// The lock-acquisition timeout defaults to `DEFAULT_LOCK_TIMEOUT`;
    /// customize it via [`TransactionDb::with_lock_timeout`].
    pub fn open<P: AsRef<Path>>(path: P, opts: Options) -> Result<Self> {
        let inner = Db::open(path, opts)?;
        let locks = Arc::new(LockTable::new());
        // `close` releases the locks of the transactions it aborts and wakes
        // every waiter.
        inner
            .engine_arc()
            .open_transactions()
            .track_locks(Arc::clone(&locks));
        Ok(Self {
            inner,
            isolation: IsolationLevel::default(),
            locks,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
        })
    }

    /// Borrow the underlying [`Db`].
    pub fn db(&self) -> &Db {
        &self.inner
    }

    /// Override the default lock-acquisition timeout for every
    /// future transaction created by this db: how long a blocking lock call
    /// ([`Transaction::get_for_update`], `put`, `delete`, `merge`) waits for
    /// a lock another transaction holds before it fails with
    /// [`TransactionError::Busy`]. [`Transaction::try_get_for_update`] never
    /// waits, so the timeout does not apply to it.
    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }

    /// Start a new pessimistic transaction. Reads see the engine
    /// state as of the current seq; writes acquire key locks that
    /// the caller retains until commit or rollback.
    ///
    /// The transaction runs at `opts`' [`IsolationLevel`], or at this
    /// database's default when `opts` sets none. It borrows nothing from the
    /// database, so it can be stored, moved to another thread, or held past
    /// the database handle that began it.
    pub fn begin(&self, opts: &TxnOptions) -> Transaction {
        let engine = self.inner.engine_arc();
        let (snapshot_seq, snapshot_pin) = engine.register_snapshot_at_horizon();
        Transaction::new(
            engine,
            snapshot_seq,
            snapshot_pin,
            self.inner.durability(),
            TxMode::Pessimistic,
            Some(Arc::clone(&self.locks)),
            self.lock_timeout,
            opts.resolve_isolation(self.isolation),
            self.inner.transaction_keys_inline(),
            None,
            false,
            opts.reading(),
        )
    }

    /// Set the default [`IsolationLevel`] for transactions this database
    /// begins. Defaults to [`IsolationLevel::SnapshotIsolation`].
    pub fn with_isolation(mut self, isolation: IsolationLevel) -> Self {
        self.isolation = isolation;
        self
    }

    /// The default level [`Self::begin`] uses when its [`TxnOptions`] set none.
    pub fn isolation(&self) -> IsolationLevel {
        self.isolation
    }

    /// [`Db::allocate`] on the underlying database: reserves `n` values of
    /// the counter at `key`, outside any transaction and never a conflict.
    pub fn allocate(&self, key: &[u8], n: u64) -> Result<Range<u64>> {
        self.inner.allocate(key, n)
    }

    /// [`Db::allocate_nowait`] on the underlying database: the reservation
    /// that never waits, handing back a wait on `queue` instead.
    pub fn allocate_nowait(&self, queue: QueueId, key: &[u8], n: u64) -> Result<Range<u64>> {
        self.inner.allocate_nowait(queue, key, n)
    }
}

/// Which way a scan walks its range.
///
/// The range is the same either way: `[start, end)`, with `start` inclusive
/// and `end` exclusive. Only the order entries arrive in changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScanDirection {
    /// Ascending, from `start` towards `end`.
    #[default]
    Forward,
    /// Descending, from the highest key below `end` down to `start`.
    Reverse,
}

/// How much a transaction is protected against concurrent commits.
///
/// The level decides what the commit-time validation covers, so it
/// decides which anomalies are reachable.
///
/// # What each level admits
///
/// | Level | Dirty read | Non-repeatable read | Lost update | Write skew |
/// |---|---|---|---|---|
/// | [`IsolationLevel::ReadCommitted`] | no | possible | possible | possible |
/// | [`IsolationLevel::SnapshotIsolation`] | no | no | no | **possible** |
/// | [`IsolationLevel::RepeatableRead`] | no | no | no | **through a scan** |
/// | [`IsolationLevel::Serializable`] | no | no | no | no |
/// | [`IsolationLevel::DefraLevel`] | no | no | no | **through a scan** |
///
/// The [`IsolationLevel::DefraLevel`] row is `RepeatableRead`'s, with the
/// relaxations described on that variant. A key that is only merged into is
/// not snapshot isolated there: concurrent transactions can all commit a
/// merge to it, and every operand applies. When every transaction uses only
/// point reads, puts, deletes and merges, the committed transactions are
/// serializable in commit order. The keys a scan returns are not validated.
/// A key the installed classifier declares [`KeyClass::ContentAddressed`] is
/// outside that statement: transactions that create it all commit, and a read
/// that found it holds as long as the key is not deleted.
///
/// Every level reads from a snapshot captured when the transaction
/// began, so none of them can observe a dirty read. What changes is the
/// size of the read set validated at commit.
///
/// # Where RepeatableRead sits
///
/// [`IsolationLevel::RepeatableRead`] validates every key a point read
/// returned, as `Serializable` does, and records a scan as
/// `SnapshotIsolation` does: one entry per stretch it walked, none per key
/// it yielded. A point read therefore refuses write skew and a scan does
/// not: two transactions that each scan what the other writes both commit.
/// In Adya's terms it is PL-2.99, the formal repeatable read, on top of
/// PL-SI: G1, G-SI and G2-item are forbidden, and an anti-dependency
/// through a predicate read (a key a scan yielded, or a phantom) is
/// allowed. It is the level for a scan whose result is allowed to change
/// underneath the transaction by construction, a set of markers that
/// concurrent writers only ever add to or reclaim, where an entry per key
/// would cost commit time and abort transactions no serial order needed
/// to abort.
///
/// This is the contract both [`OptimisticTransactionDb`] and
/// [`TransactionDb`] run.
///
/// # Why write skew survives snapshot isolation
///
/// Under [`IsolationLevel::SnapshotIsolation`] a key is validated only
/// if the transaction wrote it, or read it through
/// [`Transaction::get_for_update`]. Two transactions can therefore each
/// read what the other is about to overwrite, write disjoint keys, and
/// both commit - a schedule no serial order produces. Elle reports it as
/// a G2 anti-dependency cycle.
///
/// [`IsolationLevel::Serializable`] validates *every* key the
/// transaction read. A concurrent commit to any of them aborts it, so
/// the anti-dependency edge that would close the cycle cannot form.
/// This is backward validation: the cost is paid by the transaction
/// committing second, and it is proportional to its read set.
///
/// # The one thing to know before relying on it
///
/// Serializability here is validation of a set of keys, not of a
/// predicate. Point reads and every key a transactional scan yields are
/// in that set. The stretches a scan walked are recorded too, but only to
/// anchor keys the transaction validates anyway: a phantom - a key that
/// did not exist to be yielded, inserted into the range by a concurrent
/// transaction - aborts the transaction only if it also writes that key or
/// point-reads it. A transaction whose correctness depends on the absence
/// of keys in a range must not rely on this level for it: it scans the
/// range through [`Transaction::cursor`] with [`ScanCheck::Range`], which
/// validates the range itself, at this level and every other.
/// [`Transaction::delete_range`] stays rejected.
///
/// # What a scan costs at Serializable
///
/// A transactional scan at [`IsolationLevel::Serializable`] adds one
/// read-set entry per key it yields, held until the transaction resolves,
/// and the commit checks them in two passes. The first runs on the
/// committing thread before the commit enters the pipeline, against
/// everything committed by then, and holds up nobody. The second runs when
/// the commit's turn in the pipeline comes, for what was committed in
/// between: one lookup of each key's newest version, in the memtables alone
/// unless a flush or an ingest landed since the first pass. The commits and
/// plain writes behind it in the pipeline, [`crate::Db::put`] included, are
/// decided only after that second pass, so a large scan delays every writer
/// by one lookup per scanned key. A transaction that writes nothing does not
/// enter the pipeline: its commit checks every entry once, on the committing
/// thread, and holds up no writer. Below this level a scan records one entry
/// per stretch of keys it walked, not one per key.
///
/// # A write-free reader needs writers that validate their reads
///
/// At [`IsolationLevel::DefraLevel`] an optimistic transaction that writes
/// nothing validates none of its plain reads: it read one snapshot, so what
/// it saw is a state the database was in. That state lies on a serial order
/// of the committed transactions only when every transaction that writes
/// to the database validates the reads its writes depend on: its point
/// reads at `RepeatableRead`, `Serializable` and `DefraLevel`, and a scan
/// it decides on through [`ScanCheck::Range`]. A writer at
/// [`IsolationLevel::SnapshotIsolation`], the default, or at
/// `ReadCommitted`, commits on reads nobody checked, and a reader can then
/// see a state no serial order passes through, whatever level the reader
/// runs at. Give the writers a validating level when a write-free reader's
/// result must be one a serial order could have produced.
///
/// A reader at `RepeatableRead` or `Serializable` checks every read at its
/// commit, against every commit decided by then, published or not, so one
/// that commits after the writer whose input it missed is refused: the
/// report below, made at `Serializable` and committed after the withdrawal,
/// conflicts on `checking`. One that commits first cannot be refused, since
/// the write it missed is not there yet; only the writers' level prevents
/// that.
///
/// The example, the same one the TLA+ model (`RepeatableRead.tla`), the Lean
/// proof (`read_only_anomaly`) and the tests use. A checking account holds
/// 30 and a savings account 0. A withdrawal reads both and takes 25 from
/// checking, plus a fee of 1 when the total it saw is under 35. A deposit
/// puts 20 into savings. The withdrawal reads first (30 and 0, so it will
/// charge the fee), the deposit commits, a report reads (30, 20), and the
/// withdrawal commits checking = 4. The fee says the withdrawal ran before
/// the deposit; the report says the deposit ran first and the withdrawal
/// not yet. No order of the three gives both.
///
/// ```
/// use regolith::{
///     IsolationLevel, OptimisticTransactionDb, Options, TransactionError, TxnOptions,
/// };
///
/// fn balance(bytes: Option<Vec<u8>>) -> u64 {
///     bytes.map_or(0, |bytes| u64::from_be_bytes(bytes.try_into().expect("eight bytes")))
/// }
///
/// // What a report that writes nothing saw, and the checking balance the
/// // database ends with, when the two writers run at `writers`.
/// let run = |writers: IsolationLevel| -> Result<((u64, u64), u64), Box<dyn std::error::Error>> {
///     let db = OptimisticTransactionDb::open("bank", Options::memory())?;
///     db.db().put(b"checking", &30u64.to_be_bytes())?;
///     let writer = TxnOptions::new().isolation(writers);
///
///     // The withdrawal reads both balances: a total of 30.
///     let withdrawal = db.begin(&writer);
///     let checking = balance(withdrawal.get(b"checking")?);
///     let savings = balance(withdrawal.get(b"savings")?);
///
///     // A deposit of 20 into savings commits.
///     let deposit = db.begin(&writer);
///     let before = balance(deposit.get(b"savings")?);
///     deposit.put(b"savings", &(before + 20).to_be_bytes())?;
///     deposit.commit()?;
///
///     // The report reads both balances and writes nothing.
///     let report = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
///     let seen = (balance(report.get(b"checking")?), balance(report.get(b"savings")?));
///     report.commit()?;
///
///     // The withdrawal takes 25, and a fee of 1 for the total under 35 it read.
///     let fee = u64::from(checking + savings < 35);
///     withdrawal.put(b"checking", &(checking - 25 - fee).to_be_bytes())?;
///     // It may lose its commit; either way, look at what is left.
///     let _ = withdrawal.commit();
///     Ok((seen, balance(db.db().get(b"checking")?)))
/// };
///
/// // Snapshot isolation lets the withdrawal commit on the total it read.
/// // The report saw the deposit and no withdrawal, so the withdrawal comes
/// // after both, yet it was charged the fee as if no deposit had been made:
/// // no order of the three produces what the report read.
/// assert_eq!(run(IsolationLevel::SnapshotIsolation)?, ((30, 20), 4));
///
/// // A writer that validates its reads loses instead, because the savings it
/// // read changed. The report read the state after the deposit alone.
/// assert_eq!(run(IsolationLevel::RepeatableRead)?, ((30, 20), 30));
///
/// // A report at `Serializable` that commits after the withdrawal checks
/// // what it read, finds the withdrawal's checking, and is refused.
/// let db = OptimisticTransactionDb::open("bank", Options::memory())?;
/// db.db().put(b"checking", &30u64.to_be_bytes())?;
/// let writer = TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation);
/// let withdrawal = db.begin(&writer);
/// let checking = balance(withdrawal.get(b"checking")?);
/// let savings = balance(withdrawal.get(b"savings")?);
/// let deposit = db.begin(&writer);
/// let before = balance(deposit.get(b"savings")?);
/// deposit.put(b"savings", &(before + 20).to_be_bytes())?;
/// deposit.commit()?;
/// let report = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
/// let seen = (balance(report.get(b"checking")?), balance(report.get(b"savings")?));
/// assert_eq!(seen, (30, 20));
/// let fee = u64::from(checking + savings < 35);
/// withdrawal.put(b"checking", &(checking - 25 - fee).to_be_bytes())?;
/// withdrawal.commit()?;
/// let Err(TransactionError::Conflict(lost)) = report.commit() else {
///     panic!("the report read a checking balance the withdrawal replaced");
/// };
/// assert_eq!(lost.key(), b"checking");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IsolationLevel {
    /// Validate nothing beyond what the transaction wrote.
    ReadCommitted,
    /// Validate writes and keys read through
    /// [`Transaction::get_for_update`]. regolith's default.
    ///
    /// A transaction that writes at this level commits on plain reads nobody
    /// validated. That weakens every reader beside it, a write-free reader
    /// at [`IsolationLevel::DefraLevel`] included: see "A write-free reader
    /// needs writers that validate their reads" on this type.
    #[default]
    SnapshotIsolation,
    /// Validate every key a point read returned; record a scan per
    /// stretch, not per key. Adya's PL-2.99 over snapshot isolation.
    RepeatableRead,
    /// Validate the entire read set, including every key a transactional
    /// scan yields.
    Serializable,
    /// [`IsolationLevel::RepeatableRead`] with relaxations, for
    /// optimistic transactions only. A pessimistic transaction at this
    /// level validates exactly as at `RepeatableRead`.
    ///
    /// What the commit validates, by kind of key (the first is
    /// `RepeatableRead` itself, the rest are the relaxations):
    ///
    /// - An ordinary key. Every point read of it is validated, whether it
    ///   found a value or not, and a put or delete of it conflicts with any
    ///   commit to the key since the transaction began, unless the key
    ///   already holds what the write would leave. A read is validated by
    ///   value: it is current while the key holds the bytes it returned, so
    ///   a rewrite of the same bytes, or a change that came back, is not a
    ///   change. A merge operand on top always is, whatever it adds.
    /// - Blind merges commute. A key the transaction only merges into (it
    ///   did not read the key, did not walk it in a scan, and did not put or
    ///   delete it) conflicts only with a newer put, delete or covering
    ///   range delete of that key, never with a newer merge operand.
    ///   Concurrent blind merges to one key all commit, and their operands
    ///   apply in commit order. This needs no classifier.
    /// - Commutative prefixes. With a [`KeyClassifier`] installed through
    ///   [`OptimisticTransactionDb::with_policy`], a scan stretch that stays
    ///   inside one [`KeyClass::CommutativePrefix`] is not recorded, and a
    ///   put or merge the transaction makes inside it, before or after the
    ///   scan, is validated as a blind write instead of as a read.
    ///   [`KeyClass::CommutativePrefix`] states when that is safe.
    /// - Content-addressed keys. With the same classifier, a put or merge of
    ///   a key it declares [`KeyClass::ContentAddressed`] is never
    ///   validated, nor is a read of a key the transaction puts or merges,
    ///   so two transactions that create the same key both commit when they
    ///   put the same bytes, and a put of different bytes beside a newer
    ///   commit of the key fails with [`Error::ContentMismatch`]. A delete
    ///   of it is validated like a delete of an ordinary key. A point read
    ///   that found it is validated for presence only: it conflicts at
    ///   commit when the key is gone, and not when a newer put or merge left
    ///   it there. A read that found nothing is validated in full, so a
    ///   newer put conflicts. [`KeyClass::ContentAddressed`] states the
    ///   caller's contract.
    /// - Log keys. With the same classifier, a read of a key it declares
    ///   [`KeyClass::Log`] is never validated, and a put, delete or merge of
    ///   one is refused with [`Error::LogKeyWrite`]. Only
    ///   [`Transaction::append`] writes them.
    /// - Projected reads. [`Transaction::get_parts`] names the parts of a
    ///   value its decision used; it is refused only by a newer write that
    ///   changes one of them (see [`crate::MergeOperator::touches`]).
    /// - Write-free transactions. A transaction that writes nothing
    ///   validates no plain read: it read one snapshot, and nothing it did
    ///   can have moved. It checks only the reads it made through
    ///   [`Transaction::get_for_update`], each by value, against what is
    ///   published and what is decided and not yet published (see
    ///   [`Transaction::commit`]), and, like a commit that writes nothing at
    ///   any level, takes
    ///   no part in the commit pipeline, so its commit waits for no writer.
    ///   What it read lies on a serial order of the commits only while every
    ///   writer on the database validates its own reads: a writer at
    ///   `SnapshotIsolation` or `ReadCommitted` beside it breaks that (see
    ///   "A write-free reader needs writers that validate their reads" on
    ///   this type). A pessimistic transaction at this level that writes
    ///   nothing gets no relaxation: it checks every read, as at
    ///   `RepeatableRead`, since its `get_for_update` reads can see two
    ///   points in time.
    ///
    /// A scan checked with [`ScanCheck::Range`] is stricter than any of
    /// these: any write inside the range it covered since the snapshot
    /// conflicts, at this level and every other.
    ///
    /// A transaction reads its own puts, deletes and merges in the order it
    /// made them, and the commit applies them in that order: an operand
    /// applies to the put or delete before it, and a put or delete replaces
    /// every operand before it. That changes what a commit stores for a merge
    /// made before a put of the same key. Earlier versions applied every
    /// merge on top of the puts, whatever order they were made in; the
    /// commit now stores the put alone.
    ///
    /// A key that is only merged into does not get snapshot isolation:
    /// first committer wins does not apply to merge operands. When every
    /// transaction uses only point reads, puts, deletes and merges, the
    /// committed transactions are serializable in commit order, because
    /// every point read is validated: a writing transaction as of its commit,
    /// a write-free one as of its snapshot. A content-addressed key, a log
    /// key and a read by parts are outside that statement as far as they are
    /// relaxed above. The keys a scan returns are not validated, as at
    /// `RepeatableRead`, so phantoms are possible: a key another transaction
    /// inserts into a scanned range is detected only when this transaction
    /// also reads or writes it.
    ///
    /// A [`crate::CompactionFilter`] runs in every snapshot stripe, so a
    /// filter that changes or removes a value changes what a live snapshot,
    /// a transaction's included, reads from then on. No commit check sees
    /// it.
    DefraLevel,
}

impl IsolationLevel {
    /// Whether every point read is validated at commit, written or not.
    pub(crate) fn validates_every_read(self) -> bool {
        matches!(
            self,
            Self::RepeatableRead | Self::Serializable | Self::DefraLevel
        )
    }

    /// Whether a transactional scan records each key it yields as a read.
    pub(crate) fn validates_scanned_keys(self) -> bool {
        self == Self::Serializable
    }
}

#[derive(Clone, Copy)]
enum TxMode {
    Optimistic,
    Pessimistic,
}

/// An in-flight transaction. Created by
/// [`OptimisticTransactionDb::begin`] or
/// [`TransactionDb::begin`] and resolved by
/// [`Transaction::commit`] or [`Transaction::rollback`].
///
/// Reads within the transaction see a consistent snapshot captured
/// at begin time, except for keys that the transaction itself has
/// written; those always read back the buffered write. A key it merged
/// into reads as the database's merge operator folds the transaction's
/// operands onto the value it replaced, or onto the value it reads for the
/// key when it replaced nothing.
///
/// Dropping a `Transaction` without committing is equivalent to
/// calling [`Transaction::rollback`]: buffered writes are
/// discarded and any held locks are released. Its `on_abort`
/// callbacks see [`AbortReason::Dropped`] rather than
/// [`AbortReason::Rollback`].
///
/// # Example
///
/// ```
/// use regolith::{OptimisticTransactionDb, Options, TxnOptions};
///
/// let db = OptimisticTransactionDb::open("accounts", Options::memory())?;
/// db.db().put(b"balance", b"100")?;
///
/// // A transaction reads its snapshot, with its own writes on top.
/// let txn = db.begin(&TxnOptions::new());
/// assert_eq!(txn.get(b"balance")?.as_deref(), Some(&b"100"[..]));
/// txn.put(b"balance", b"90")?;
/// assert_eq!(txn.get(b"balance")?.as_deref(), Some(&b"90"[..]));
///
/// // Nobody else sees a write before the commit, and everybody after.
/// assert_eq!(db.db().get(b"balance")?.as_deref(), Some(&b"100"[..]));
/// txn.commit()?;
/// assert_eq!(db.db().get(b"balance")?.as_deref(), Some(&b"90"[..]));
///
/// // Dropping a transaction rolls it back.
/// let abandoned = db.begin(&TxnOptions::new());
/// abandoned.put(b"balance", b"0")?;
/// drop(abandoned);
/// assert_eq!(db.db().get(b"balance")?.as_deref(), Some(&b"90"[..]));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
/// Laid out in declaration order (`repr(C)`), hot fields first: a read or a
/// write of a transaction meets the fields it needs in its first three cache
/// lines, rather than spread across the seventeen the struct covers, and the
/// callbacks' in-place queues come last.
#[repr(C)]
pub struct Transaction {
    engine: Arc<RegolithEngine>,
    /// The key classes [`IsolationLevel::DefraLevel`] applies.
    policy: Option<Arc<dyn KeyClassifier>>,
    snapshot_seq: u64,
    /// Buffer of the transaction's writes: puts, deletes and merge
    /// operands in the order they were made, which is what lets a read and
    /// a commit agree on what a put after a merge, or a merge after a put,
    /// leaves. Concurrent so that buffering a write takes `&self`; drained
    /// at commit, which is where the order the engine applies them in is
    /// restored.
    writes: TxnBuffer<Vec<u8>, Write>,
    /// What this transaction has observed about each key it read,
    /// through a point read, or through a scan at Serializable. Sorted
    /// at commit so a multi-key conflict always reports the same key.
    /// Never rewound: a savepoint rollback undoes buffered writes, not
    /// reads that already happened.
    tracked: TxnBuffer<Vec<u8>, Arc<KeyState>>,
    /// Where this transaction's block-cache misses go, and the queue its
    /// completions belong to ([`TxnOptions::read_mode`],
    /// [`TxnOptions::io_queue`]).
    reading: txn_options::Reading,
    /// Shared with the engine so `close` can abort the transaction; made when
    /// the first `on_abort` callback is registered, or at once under a
    /// database with [`TransactionHooks`].
    claim: Option<Arc<Claim>>,
    /// The database's lock table; `None` for an optimistic transaction.
    lock_table: Option<Arc<LockTable>>,
    /// What the commit-time validation covers. See [`IsolationLevel`].
    isolation: IsolationLevel,
    mode: TxMode,
    durability: crate::engine::DurabilityMode,
    /// Each write is checked against newer versions as it is made
    /// ([`TxnOptions::early_validation`]). Optimistic transactions only.
    early_validation: bool,
    /// The owner won the commit claim, so a claim made from here on, by an
    /// `on_abort` registered inside a `before_commit` callback, starts as
    /// committing too: `close` must not abort a commit that began.
    committing: bool,
    /// The commit is a [`Transaction::commit_nowait`]: the `before_commit`
    /// callbacks' reads keep the transaction's read mode instead of reading
    /// the device, so one that would block ends the commit with that wait.
    nowait: bool,
    resolved: bool,
    resources_released: bool,
    /// A read was made through [`Transaction::get_for_update`]: a commit that
    /// writes nothing at `SnapshotIsolation`, or under DefraLevel's
    /// write-free relaxation, has a read to check. Only latches on.
    for_update_reads: AtomicBool,
    /// Highest sequence [`Transaction::get_for_update`] has ever promoted a
    /// key to; `snapshot_seq` while nothing has been promoted past it.
    /// Lets `scan_read_seq` skip the `tracked` lookup outright for a
    /// pessimistic scan that could not possibly find a promoted key,
    /// instead of walking `tracked` once per yielded key.
    promoted_seq: AtomicU64,
    /// Where `snapshot_seq` is pinned in the engine's snapshot registry.
    /// Released exactly there when the transaction ends, on whichever
    /// thread that happens.
    snapshot_pin: crate::engine::SnapshotPin,
    /// How far [`Transaction::prepare`] got.
    prepare_state: Prepare,
    /// Savepoint stack: how much of each buffer the transaction held when each
    /// was set.
    savepoints: Vec<Savepoint>,
    /// The `append` calls, in the order they were made. Not part of `writes`:
    /// their positions do not exist until the commit orders them, so nothing
    /// in the transaction can read them. Allocated by the first `append`, and
    /// boxed for the reason `scan_runs` is: a transaction that never appends
    /// pays nothing for it. A buffer rather than a queue so a savepoint can
    /// take the appends made after it back.
    appends: OnceLock<Box<TxnBuffer<(), PendingAppend>>>,
    /// The stretches of snapshot keys this transaction's scans yielded, one
    /// record per stretch rather than one per key. Allocated by the first
    /// scan that yields a snapshot key. Never rewound, like `tracked`.
    ///
    /// Boxed so the queue (and its 32-slot first segment once it exists)
    /// stays off the `Transaction` struct itself: every commit moves this
    /// value, and a transaction that never scans must not pay for it.
    scan_runs: OnceLock<Box<SegQueue<Arc<ScanRun>>>>,
    /// The ranges the validated scans ([`ScanCheck::Range`]) cover, one record
    /// per cursor. Boxed for the reason `scan_runs` is.
    scan_ranges: OnceLock<Box<SegQueue<Arc<RangeRecord>>>>,
    /// Keys for which this transaction holds a pessimistic lock, each with
    /// its lock and the use of it the hold took. A set rather than a list:
    /// membership is checked on every locking operation, and release order
    /// does not matter. Released by `Drop` if not already released by
    /// `commit` or `rollback`, or by the landing of a `commit_nowait`.
    held_locks: TxnBuffer<Vec<u8>, Arc<KeyLock>>,
    /// The name this transaction's locks carry: taken at begin for a
    /// pessimistic transaction, from the ones this thread's earlier
    /// transactions gave back (`LockOwner::take`), and given back when it
    /// ends. `None` for an optimistic transaction.
    lock_owner: Option<Arc<LockOwner>>,
    /// The waits for locks other transactions held, one per key, kept across
    /// calls (`locking`). Allocated by the first wait.
    lock_waits: OnceLock<Box<LockWaits>>,
    lock_timeout: Duration,
    /// What the caller registered to run when the transaction ends, the
    /// first four of each kind in place: no allocation for them.
    callbacks: Callbacks,
}

/// The mark a savepoint keeps: the entry counts of the buffers it rolls back.
#[derive(Clone, Copy)]
struct Savepoint {
    writes: usize,
    appends: usize,
}

/// What one transaction knows about one key it has read.
///
/// Shared, never copied: `tracked` holds an `Arc` of this cell and every
/// observation of the key folds into that one instance. The mutable
/// fields are atomic and every update is monotonic, so two threads
/// reading the same key through the same transaction cannot lose an
/// observation between them: `read_seq` only rises and `for_update` only
/// latches on. Copying the cell, or a non-atomic read-modify-write on
/// it, would let one thread's observation overwrite another's and
/// silently shrink the commit-time validation set.
struct KeyState {
    /// Sequence this transaction first observed the key at.
    /// Validation uses this one, because it is the read a later
    /// write of the same key would otherwise silently overwrite.
    /// Written once, when the cell is created, then read-only.
    first_read_seq: u64,
    /// Sequence later reads of the key are served at. Only ever
    /// moves forward, so `get_for_update` can promote a key that was
    /// already read at the begin snapshot to the lock horizon
    /// without any read of this transaction going backwards.
    read_seq: AtomicU64,
    /// The key was read through [`Transaction::get_for_update`], so
    /// it is validated at commit whether or not it is written.
    for_update: AtomicBool,
    /// A read of the key found a value in the database. Recorded only while
    /// the transaction consults a [`KeyClassifier`], which uses it to tell a
    /// read that must see the key stay present from one that found nothing.
    /// Latches on, like `for_update`.
    found: AtomicBool,
    /// The parts the key was read by, when its first read was
    /// [`Transaction::get_parts`]. `None` for a key read in whole. A cell with
    /// a projection is widened to the whole value by any other kind of read,
    /// which `Projection::widen_to_full` records.
    projection: Option<Box<Projection>>,
}

impl KeyState {
    fn new(horizon: u64, for_update: bool) -> Self {
        Self {
            first_read_seq: horizon,
            read_seq: AtomicU64::new(horizon),
            for_update: AtomicBool::new(for_update),
            found: AtomicBool::new(false),
            projection: None,
        }
    }
}

impl Transaction {
    #[allow(clippy::too_many_arguments)]
    fn new(
        engine: Arc<RegolithEngine>,
        snapshot_seq: u64,
        snapshot_pin: crate::engine::SnapshotPin,
        durability: crate::engine::DurabilityMode,
        mode: TxMode,
        lock_table: Option<Arc<LockTable>>,
        lock_timeout: Duration,
        isolation: IsolationLevel,
        keys_inline: usize,
        policy: Option<Arc<dyn KeyClassifier>>,
        early_validation: bool,
        reading: txn_options::Reading,
    ) -> Self {
        let claim = engine
            .transaction_hooks()
            .is_some()
            .then(|| Claim::track(&engine, false));
        let lock_owner = lock_table.as_ref().map(|_| LockOwner::take());
        // `close` releases the locks of a transaction it aborts by this name.
        if let (Some(claim), Some(owner)) = (&claim, &lock_owner) {
            claim.hold_locks_of(Arc::clone(owner));
        }
        Self {
            engine,
            snapshot_seq,
            snapshot_pin,
            durability,
            mode,
            isolation,
            policy,
            writes: TxnBuffer::new(keys_inline),
            appends: OnceLock::new(),
            tracked: TxnBuffer::new(keys_inline),
            scan_runs: OnceLock::new(),
            scan_ranges: OnceLock::new(),
            early_validation,
            reading,
            promoted_seq: AtomicU64::new(snapshot_seq),
            for_update_reads: AtomicBool::new(false),
            savepoints: Vec::new(),
            callbacks: Callbacks::default(),
            claim,
            committing: false,
            nowait: false,
            prepare_state: Prepare::Pending,
            held_locks: TxnBuffer::new(keys_inline),
            lock_table,
            lock_owner,
            lock_waits: OnceLock::new(),
            lock_timeout,
            resolved: false,
            resources_released: false,
        }
    }

    /// The mode this transaction reads in ([`TxnOptions::read_mode`]).
    pub fn read_mode(&self) -> ReadMode {
        self.reading.mode
    }

    /// The queue this transaction's completions belong to: the one
    /// [`TxnOptions::io_queue`] named, or else the one its
    /// [`ReadMode::CacheOnly`] names. `None` for a transaction that blocks.
    pub fn io_queue(&self) -> Option<QueueId> {
        self.reading.queue.or(self.reading.mode.queue())
    }

    /// Route this thread's device misses as the transaction's mode says, for
    /// one read call. Inside `commit` every read blocks: the commit does its
    /// own I/O, the `before_commit` callbacks' reads included. Inside
    /// `commit_nowait` they keep the mode, and one that misses ends the
    /// commit with its wait.
    fn read_scope(&self) -> Option<Scope> {
        if self.committing && !self.nowait {
            return None;
        }
        self.reading.mode.scope()
    }

    /// Read `key` from the default column family. Returns the
    /// buffered write if the transaction has already written to
    /// `key`, otherwise the value visible at the sequence this
    /// transaction observes `key` at: its begin snapshot, or, for a
    /// key a pessimistic transaction already holds a lock on, the
    /// horizon sampled when that lock was acquired.
    ///
    /// A key the transaction merged into reads as the configured
    /// [`crate::MergeOperator`] folds the transaction's operands, in the order
    /// it made them, onto the value of its newest put or delete of the key,
    /// or, when it made neither, onto the value read as above. A merge that
    /// the operator declines is an error, as it is for a read of the database.
    ///
    /// Takes no key lock. The read is remembered, so writing the same
    /// key later turns it into a read-modify-write that is validated
    /// at commit and aborts with [`TransactionError::Conflict`]
    /// rather than losing the update. Below
    /// [`IsolationLevel::RepeatableRead`], a key that is read and
    /// never written is not validated: use
    /// [`Transaction::get_for_update`] when a read must participate
    /// in conflict detection on its own.
    ///
    /// # Example
    ///
    /// ```
    /// use regolith::{Access, OptimisticTransactionDb, Options, TransactionError, TxnOptions};
    ///
    /// let db = OptimisticTransactionDb::open("counter", Options::memory())?;
    /// db.db().put(b"n", b"5")?;
    ///
    /// let txn = db.begin(&TxnOptions::new());
    /// assert_eq!(txn.get(b"n")?.as_deref(), Some(&b"5"[..]));
    ///
    /// // Another writer commits. The snapshot does not move.
    /// db.db().put(b"n", b"6")?;
    /// assert_eq!(txn.get(b"n")?.as_deref(), Some(&b"5"[..]));
    ///
    /// // Writing a key after reading it is a read-modify-write: the commit is
    /// // refused, not applied over the update the read did not see.
    /// txn.put(b"n", b"6")?;
    /// let Err(TransactionError::Conflict(lost)) = txn.commit() else {
    ///     panic!("the key changed after it was read");
    /// };
    /// assert_eq!(lost.mine(), Access::Read);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn get(&self, key: &[u8]) -> TxResult<Option<Vec<u8>>> {
        let _scope = self.read_scope();
        let prefixed = prefix_key(DEFAULT_CF_ID, key);
        let committed = || self.read_committed(&prefixed);
        match self.read_own(&prefixed, committed)? {
            Some(own) => Ok(own),
            None => Ok(committed()
                .map_err(|e| read_error(e, &prefixed))?
                .map(DbSlice::into_vec)),
        }
    }

    /// [`Transaction::get`] without copying the value.
    ///
    /// The returned [`DbSlice`] borrows the bytes the database already
    /// holds. A value the transaction wrote itself, or that its merge
    /// operands build, is copied out of the write buffer first, since the
    /// buffer cannot lend it.
    ///
    /// # Example
    ///
    /// ```
    /// use regolith::{OptimisticTransactionDb, Options, TxnOptions};
    ///
    /// let db = OptimisticTransactionDb::open("rows", Options::memory())?;
    /// db.db().put(b"row", b"a long value")?;
    ///
    /// let txn = db.begin(&TxnOptions::new());
    /// let value = txn.get_slice(b"row")?.expect("the row exists");
    /// txn.rollback();
    ///
    /// // The slice keeps the bytes alive on its own, past the transaction
    /// // and past a later write of the key.
    /// db.db().put(b"row", b"replaced")?;
    /// assert_eq!(&value[..6], b"a long");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn get_slice(&self, key: &[u8]) -> TxResult<Option<DbSlice>> {
        let _scope = self.read_scope();
        let prefixed = prefix_key(DEFAULT_CF_ID, key);
        let committed = || self.read_committed(&prefixed);
        match self.read_own(&prefixed, committed)? {
            Some(own) => Ok(own.map(DbSlice::from)),
            None => committed().map_err(|e| read_error(e, &prefixed)),
        }
    }

    /// What the database holds for `prefixed` as this transaction reads it,
    /// at the sequence it observes the key at. The read is recorded.
    fn read_committed(&self, prefixed: &[u8]) -> std::io::Result<Option<DbSlice>> {
        let (state, read_seq) = self.observe(prefixed, self.snapshot_seq, false);
        self.read_noting(&state, prefixed, read_seq)
    }

    /// What the database holds for `prefixed` at `read_seq`, noted on `state`
    /// when it holds a value and a classifier is there to make use of that.
    fn read_noting(
        &self,
        state: &KeyState,
        prefixed: &[u8],
        read_seq: u64,
    ) -> std::io::Result<Option<DbSlice>> {
        let found = self.engine.get_slice_at(prefixed, read_seq)?;
        if found.is_some() && policy::classifier_for(&self.policy, self.isolation).is_some() {
            state.found.store(true, Ordering::Release);
        }
        Ok(found)
    }

    /// What a read of `prefixed` finds in this transaction's own writes, or
    /// `None` when they do not decide it and the caller reads the database.
    /// `Some(None)` is a key the transaction deleted.
    ///
    /// `committed` reads what the database holds for the key. It runs only
    /// for a key the transaction merged into and did not put or delete
    /// since, since a put or a delete leaves nothing of it to show.
    fn read_own(
        &self,
        prefixed: &[u8],
        committed: impl FnOnce() -> std::io::Result<Option<DbSlice>>,
    ) -> TxResult<Option<Option<Vec<u8>>>> {
        read_buffered(
            &self.writes,
            || self.engine.merge_operator(),
            prefixed,
            committed,
        )
        .map_err(|e| read_error(e, prefixed))
    }

    /// Scan a key range without materializing it, merging this
    /// transaction's buffered writes over the snapshot underneath.
    ///
    /// The database side is streamed, and a caller that stops early pays
    /// only for what it read. The transaction's own writes are sorted up
    /// front, which is bounded by what this transaction has written rather
    /// than by what the database holds.
    ///
    /// A key the transaction merged into is yielded as [`Transaction::get`]
    /// reads it, the operands applied by the configured
    /// [`crate::MergeOperator`], and a key the snapshot does not hold appears
    /// with them applied to nothing. The value the operands apply to is read
    /// when the scan reaches the key, as the snapshot key it is: it joins the
    /// stretch and is recorded as a read only where a scan records every key.
    /// A merge the operator declines, or a read that fails, reaches the caller
    /// as an `Err` item and ends the stream: the items are `TxResult`s so an
    /// error is never mistaken for the end of the range. Writes this
    /// transaction makes while the stream is open are seen when they lie
    /// ahead of its position.
    ///
    /// What the scan leaves in the transaction depends on the level. Below
    /// [`IsolationLevel::Serializable`] it records each unbroken stretch of
    /// snapshot keys it yields once, as its first and last key, so what it
    /// holds until the transaction resolves grows with the number of scans
    /// (and the transaction's own writes inside the range), not with the size
    /// of the range. At `Serializable` it also records every yielded key as a
    /// read, exactly as [`Transaction::get`] does: that grows by one entry per
    /// distinct key yielded, and the commit checks each one, the second time
    /// while the writes behind it in the pipeline wait to be decided (see
    /// "What a scan costs at Serializable" on [`IsolationLevel`]).
    ///
    /// At every level, a key inside a stretch the scan walked that the
    /// transaction then writes is validated as a read from the begin
    /// snapshot, as a `get` followed by the write would be, so it is never
    /// elided as a blind write. The one exception is a stretch that stays
    /// inside one [`KeyClass::CommutativePrefix`] at
    /// [`IsolationLevel::DefraLevel`] with a [`KeyClassifier`] installed: the
    /// commit disregards that stretch, and a write or merge inside it is
    /// validated as a blind write. At `Serializable` a concurrent commit to
    /// any yielded key also aborts the transaction. Keys yielded from the
    /// transaction's own buffered writes are not recorded and end a stretch,
    /// as `get` does not record them either. The exception is a key the
    /// transaction merged into and did not put or delete: its base is a
    /// snapshot key, so it joins the stretch and records no read below
    /// Serializable, where `get` of such a key records one. A key a
    /// pessimistic transaction
    /// already locked through [`Transaction::get_for_update`] is served where
    /// `get` serves it, at the lock horizon. A key a concurrent transaction
    /// inserts into the range is not detected unless this transaction
    /// validates it anyway; see [`IsolationLevel`].
    ///
    /// A stretch is recorded when its first key is yielded and closed when
    /// the stream is exhausted or dropped. A stream that is never dropped
    /// (leaked) counts as having walked to the end of the keyspace in its
    /// direction.
    ///
    /// This is [`Transaction::cursor`] with [`ScanCheck::Stretch`], one entry
    /// at a time. A scan whose result decides something takes a cursor with
    /// [`ScanCheck::Range`], which validates the range.
    pub fn scan_stream(&self, start: Option<&[u8]>, end: Option<&[u8]>) -> TxnScanStream<'_> {
        self.scan_stream_in(start, end, ScanDirection::Forward)
    }

    /// [`Transaction::scan_stream`], walking `direction`.
    ///
    /// The range is `[start, end)` either way; only the order the entries
    /// arrive in changes. Reverse starts at the highest key below `end` and
    /// walks down to `start` inclusive.
    ///
    /// Both sides of the merge reverse together: the snapshot cursor steps
    /// with `prev`, and the transaction's own writes are sorted descending, so
    /// a buffered put still replaces the snapshot entry it shadows and a
    /// buffered delete still hides it.
    pub fn scan_stream_in(
        &self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        direction: ScanDirection,
    ) -> TxnScanStream<'_> {
        TxnScanStream::new(self, start, end, direction)
    }

    /// Read `key`, flag it for conflict detection at commit, and,
    /// for pessimistic transactions, take an exclusive lock on it
    /// for the rest of the transaction.
    ///
    /// A pessimistic transaction reads the value as of the moment
    /// it acquired the lock, not as of its begin snapshot, so a
    /// read-modify-write under `get_for_update` observes every
    /// transaction that committed before the lock was released to
    /// it. An optimistic transaction reads at its begin snapshot
    /// and detects the conflict at commit instead.
    ///
    /// The transaction's own writes lie over the value read, as for
    /// [`Transaction::get`], merge operands included.
    ///
    /// This is how a transaction claims a key: a bare put of a constant is not
    /// a claim, since a write of the bytes the key already holds does not
    /// conflict (see the module documentation).
    ///
    /// A pessimistic transaction waits for a lock another transaction holds,
    /// up to the database's lock timeout
    /// ([`TransactionDb::with_lock_timeout`]), and then fails with
    /// [`TransactionError::Busy`]: the defense against a deadlock, since no
    /// wait-for graph is kept. The wait parks this thread, holding no mutex
    /// and no condition variable, and is woken when the lock may be free. It
    /// keeps the transaction's place among the key's waiters, so after
    /// [`MAX_BYPASS`](crate::sync::MAX_BYPASS) losses to callers that took
    /// the lock first it is handed the lock. On a platform with no clock
    /// (wasm32 in a browser) nothing can bound a wait, so a lock another
    /// transaction holds fails with `Busy` at once. A database closed while
    /// the call waits ends it with [`Error::Closed`].
    /// [`Transaction::try_get_for_update`] is the form that never waits.
    ///
    /// # Example
    ///
    /// Two optimistic transactions try to take one seat:
    ///
    /// ```
    /// use regolith::{Access, OptimisticTransactionDb, Options, TransactionError, TxnOptions};
    ///
    /// let db = OptimisticTransactionDb::open("seats", Options::memory())?;
    ///
    /// // Each finds the seat free and marks it. The read is the claim.
    /// let (one, two) = (db.begin(&TxnOptions::new()), db.begin(&TxnOptions::new()));
    /// for txn in [&one, &two] {
    ///     assert_eq!(txn.get_for_update(b"seat/12")?, None);
    ///     txn.put(b"seat/12", b"taken")?;
    /// }
    /// one.commit()?;
    /// let Err(TransactionError::Conflict(lost)) = two.commit() else {
    ///     panic!("the seat has one taker");
    /// };
    /// assert_eq!(lost.mine(), Access::ReadForUpdate);
    ///
    /// // Without the read, the same puts both commit: writing the bytes a
    /// // key already holds claims nothing.
    /// let (one, two) = (db.begin(&TxnOptions::new()), db.begin(&TxnOptions::new()));
    /// one.put(b"seat/13", b"taken")?;
    /// two.put(b"seat/13", b"taken")?;
    /// one.commit()?;
    /// two.commit()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn get_for_update(&self, key: &[u8]) -> TxResult<Option<Vec<u8>>> {
        let _scope = self.read_scope();
        let prefixed = prefix_key(DEFAULT_CF_ID, key);
        let already_held = self.lock_key(&prefixed)?;
        self.read_for_update(&prefixed, already_held)
    }

    /// [`Transaction::get_for_update`] that never waits for a lock.
    ///
    /// When another transaction holds `key`'s lock, it takes nothing, reads
    /// nothing, and returns [`TransactionError::WouldBlock`] with a
    /// [`WouldBlock::Locked`](crate::WouldBlock::Locked) wait: wait for it,
    /// then call again (`ready(|| txn.try_get_for_update(key))` does both
    /// inside an async attempt). The wait is completed on this transaction's
    /// [`IoQueue`](crate::IoQueue) at that queue's poll, or directly, on the
    /// releasing thread, for a transaction with no queue (see
    /// [`LockWait`](crate::LockWait)); it says only that the lock may be free
    /// now. The call run again takes the lock, or returns a new wait when
    /// another caller took it first. The transaction keeps its place among
    /// the key's waiters across those calls, so after
    /// [`MAX_BYPASS`](crate::sync::MAX_BYPASS) losses it is handed the lock
    /// and the call run again finds it held. A transaction that stops calling
    /// keeps its place, and any lock handed to it, until it ends: the waiters
    /// behind it wait on it meanwhile, as they would on a holder.
    ///
    /// Once the lock is held, this is `get_for_update`: the read is served as
    /// of the moment the lock was taken, and a `CacheOnly` read that misses
    /// returns [`WouldBlock::Io`](crate::WouldBlock::Io) with the lock kept.
    /// A database closed while the transaction waits makes the wait ready,
    /// and the call run again fails with [`Error::Closed`]. A transaction
    /// that names a queue not open on this database fails with
    /// [`Error::InvalidArgument`] when it would wait.
    ///
    /// An optimistic transaction takes no locks, so for it this is
    /// `get_for_update`.
    pub fn try_get_for_update(&self, key: &[u8]) -> TxResult<Option<Vec<u8>>> {
        let _scope = self.read_scope();
        let prefixed = prefix_key(DEFAULT_CF_ID, key);
        let already_held = self.try_lock_key(&prefixed)?;
        self.read_for_update(&prefixed, already_held)
    }

    /// What `get_for_update` reads once the lock, if any, is held:
    /// `already_held` says the transaction held it before this call.
    fn read_for_update(&self, prefixed: &[u8], already_held: bool) -> TxResult<Option<Vec<u8>>> {
        let horizon = self.read_horizon(prefixed, already_held);
        let (state, read_seq) = self.observe(prefixed, horizon, true);
        // Tells `scan_read_seq` a promoted key might exist, so it is worth
        // looking `tracked` up for a pessimistic scan.
        self.promoted_seq.fetch_max(read_seq, Ordering::AcqRel);
        // Read only by the commit, which holds the transaction alone: the
        // end of every borrow that could store it orders the store first.
        self.for_update_reads.store(true, Ordering::Relaxed);
        let committed = || self.read_noting(&state, prefixed, read_seq);
        match self.read_own(prefixed, committed)? {
            Some(own) => Ok(own),
            None => Ok(committed()
                .map_err(|e| read_error(e, prefixed))?
                .map(DbSlice::into_vec)),
        }
    }

    /// Buffer a put. For pessimistic transactions, acquires an
    /// exclusive lock on the key if not already held, waiting for a lock
    /// another transaction holds as [`Transaction::get_for_update`] does: up
    /// to the lock timeout, then [`TransactionError::Busy`].
    ///
    /// A put of a constant claims nothing: if the key holds exactly these
    /// bytes at commit, the write does not conflict. Read the key with
    /// [`Transaction::get_for_update`] first, or write a value unique to
    /// the writer, to make a claim (see the module documentation).
    ///
    /// # Example
    ///
    /// Two optimistic transactions put different values under one key: the
    /// first to commit wins.
    ///
    /// ```
    /// use regolith::{
    ///     Access, OptimisticTransactionDb, Options, TransactionError, TxnOptions, WriteKind,
    /// };
    ///
    /// let db = OptimisticTransactionDb::open("owners", Options::memory())?;
    /// let (one, two) = (db.begin(&TxnOptions::new()), db.begin(&TxnOptions::new()));
    /// one.put(b"owner", b"one")?;
    /// two.put(b"owner", b"two")?;
    /// two.put(b"elsewhere", b"two")?;
    ///
    /// one.commit()?;
    /// let Err(TransactionError::Conflict(lost)) = two.commit() else {
    ///     panic!("one key, two different values");
    /// };
    /// assert_eq!(lost.key(), b"owner");
    /// assert_eq!((lost.mine(), lost.theirs()), (Access::Put, WriteKind::Put));
    ///
    /// // The loser applied none of its writes.
    /// assert_eq!(db.db().get(b"owner")?.as_deref(), Some(&b"one"[..]));
    /// assert_eq!(db.db().get(b"elsewhere")?, None);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn put(&self, key: &[u8], value: &[u8]) -> TxResult<()> {
        self.refuse_log_key(key)?;
        let prefixed = prefix_key(DEFAULT_CF_ID, key);
        self.lock_key(&prefixed)?;
        self.validate_early(&prefixed, EarlyWrite::Put(value))?;
        self.writes.insert(prefixed, Write::Put(value.to_vec()));
        Ok(())
    }

    /// Buffer a delete. For pessimistic transactions, acquires an
    /// exclusive lock on the key if not already held, waiting for a lock
    /// another transaction holds as [`Transaction::get_for_update`] does: up
    /// to the lock timeout, then [`TransactionError::Busy`].
    ///
    /// In an optimistic transaction, a delete of a key it did not read
    /// conflicts with a newer commit to the key unless that commit left the
    /// key absent: deleting what is already gone is not a conflict.
    ///
    /// # Example
    ///
    /// ```
    /// use regolith::{OptimisticTransactionDb, Options, TxnOptions};
    ///
    /// let db = OptimisticTransactionDb::open("rows", Options::memory())?;
    /// db.db().put(b"row", b"1")?;
    ///
    /// // Two transactions delete the row without reading it.
    /// let (one, two) = (db.begin(&TxnOptions::new()), db.begin(&TxnOptions::new()));
    /// one.delete(b"row")?;
    /// two.delete(b"row")?;
    ///
    /// // A transaction reads its own delete; nobody else does yet.
    /// assert_eq!(one.get(b"row")?, None);
    /// assert!(db.db().get(b"row")?.is_some());
    ///
    /// // Both commit: the second leaves the key as the first left it.
    /// one.commit()?;
    /// two.commit()?;
    /// assert_eq!(db.db().get(b"row")?, None);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn delete(&self, key: &[u8]) -> TxResult<()> {
        self.refuse_log_key(key)?;
        let prefixed = prefix_key(DEFAULT_CF_ID, key);
        self.lock_key(&prefixed)?;
        self.validate_early(&prefixed, EarlyWrite::Delete)?;
        self.writes.insert(prefixed, Write::Delete);
        Ok(())
    }

    /// Attempt to delete every key in `[start, end)`.
    ///
    /// Non-empty transactional range deletes are rejected until
    /// they can participate in optimistic conflict detection or
    /// pessimistic range locking. For correctness-critical
    /// workloads, delete known keys individually with
    /// [`Transaction::delete`] and use [`Transaction::get_for_update`]
    /// when a read must also participate in conflict detection.
    ///
    /// Calls with `start >= end` are treated as no-ops and return
    /// `Ok(())`. That is deliberate and it is the one place where a
    /// transactional write's acceptance depends on the arguments being
    /// empty: `Db::write` rejects an empty batch on a read-only handle
    /// because handle state is not an argument, while here the
    /// rejection is a missing feature and an empty range asks for no
    /// work from it. Callers that want the unsupported-feature error
    /// unconditionally must check the range themselves.
    pub fn delete_range(&self, start: &[u8], end: &[u8]) -> TxResult<()> {
        if start >= end {
            return Ok(());
        }
        Err(TransactionError::UnsupportedRangeDelete)
    }

    /// Buffer a merge operand. Merges are conflict-checked at the key
    /// level, so two optimistic transactions that run concurrently cannot
    /// both commit a merge to the same key. For pessimistic transactions,
    /// acquires an exclusive lock on the key if not already held, waiting
    /// for a lock another transaction holds as
    /// [`Transaction::get_for_update`] does: up to the lock timeout, then
    /// [`TransactionError::Busy`].
    ///
    /// Refused with [`TransactionError::Engine`] carrying
    /// [`Error::NoMergeOperator`], buffering nothing, when the database has
    /// no [`crate::MergeOperator`] configured.
    ///
    /// [`IsolationLevel::DefraLevel`] is the exception, for a key the
    /// transaction only merges into: it did not read the key, did not walk
    /// it in a scan, and did not put or delete it. That merge conflicts only
    /// with a newer put, delete or range delete of the key, never with a
    /// newer merge operand. Concurrent blind merges to one key all commit,
    /// and their operands apply in commit order.
    ///
    /// A read of the key through this transaction sees the operand applied
    /// (see [`Transaction::get`]), and a commit stores what that read found:
    /// the operand applies to the transaction's own put or delete of the key
    /// when it made one earlier, and a put or a delete made after the operand
    /// replaces the key outright, so the operand has no effect.
    ///
    /// # Example
    ///
    /// A counter two transactions add to at once. `SumCounter` is the
    /// operator of the [`crate::MergeOperator`] example: it adds big-endian
    /// `u64` operands.
    ///
    /// ```
    /// use std::sync::Arc;
    ///
    /// use regolith::{
    ///     Error, IsolationLevel, OptimisticTransactionDb, Options, TransactionError, TxnOptions,
    /// };
    /// # use regolith::MergeOperator;
    /// # struct SumCounter;
    /// # fn number(bytes: &[u8]) -> Option<u64> {
    /// #     Some(u64::from_be_bytes(bytes.try_into().ok()?))
    /// # }
    /// # impl MergeOperator for SumCounter {
    /// #     fn name(&self) -> &'static str {
    /// #         "sum-counter"
    /// #     }
    /// #     fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
    /// #         let mut total = base.map_or(Some(0), number)?;
    /// #         for operand in operands {
    /// #             total = total.wrapping_add(number(operand)?);
    /// #         }
    /// #         Some(total.to_be_bytes().to_vec())
    /// #     }
    /// # }
    ///
    /// let options = Options::memory().merge_operator(Some(Arc::new(SumCounter)));
    /// let db = OptimisticTransactionDb::open("counters", options)?;
    /// let one = 1u64.to_be_bytes();
    ///
    /// // At DefraLevel a merge into a key the transaction did not read is
    /// // blind: both commit, and both operands apply.
    /// let blind = TxnOptions::new().isolation(IsolationLevel::DefraLevel);
    /// let (a, b) = (db.begin(&blind), db.begin(&blind));
    /// a.merge(b"hits", &one)?;
    /// b.merge(b"hits", &one)?;
    /// a.commit()?;
    /// b.commit()?;
    /// assert_eq!(db.db().get(b"hits")?, Some(2u64.to_be_bytes().to_vec()));
    ///
    /// // At the default level the same race has one winner.
    /// let (a, b) = (db.begin(&TxnOptions::new()), db.begin(&TxnOptions::new()));
    /// a.merge(b"hits", &one)?;
    /// b.merge(b"hits", &one)?;
    /// a.commit()?;
    /// assert!(matches!(b.commit(), Err(TransactionError::Conflict(_))));
    ///
    /// // A database with no operator refuses the merge when it is made.
    /// let plain = OptimisticTransactionDb::open("plain", Options::memory())?;
    /// let refused = plain.begin(&TxnOptions::new()).merge(b"hits", &one);
    /// assert!(matches!(refused, Err(TransactionError::Engine(Error::NoMergeOperator))));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn merge(&self, key: &[u8], operand: &[u8]) -> TxResult<()> {
        self.refuse_log_key(key)?;
        self.engine.require_merge_operator()?;
        let prefixed = prefix_key(DEFAULT_CF_ID, key);
        self.lock_key(&prefixed)?;
        self.validate_early(&prefixed, EarlyWrite::Merge)?;
        self.writes.insert(prefixed, Write::Merge(operand.to_vec()));
        Ok(())
    }

    /// Set a savepoint at the current state of the buffered writes. A later
    /// call to [`Transaction::rollback_to_savepoint`] reverts every buffered
    /// write made after this call, and [`Transaction::release_savepoint`]
    /// forgets the savepoint and keeps the writes.
    ///
    /// O(1): the savepoint is the number of entries the write buffer holds,
    /// and nothing is copied. Savepoints nest; each rollback or release takes
    /// the most recently set one that is still open.
    ///
    /// # Example
    ///
    /// ```
    /// use regolith::{OptimisticTransactionDb, Options, TransactionError, TxnOptions};
    ///
    /// let db = OptimisticTransactionDb::open("rows", Options::memory())?;
    /// let mut txn = db.begin(&TxnOptions::new());
    /// txn.put(b"a", b"kept")?;
    ///
    /// // Rolling back takes the writes made since the savepoint, no others.
    /// txn.set_savepoint();
    /// txn.put(b"a", b"undone")?;
    /// txn.put(b"b", b"undone")?;
    /// txn.rollback_to_savepoint()?;
    /// assert_eq!(txn.get(b"a")?.as_deref(), Some(&b"kept"[..]));
    /// assert_eq!(txn.get(b"b")?, None);
    ///
    /// // Releasing forgets the savepoint and keeps the writes.
    /// txn.set_savepoint();
    /// txn.put(b"c", b"kept")?;
    /// txn.release_savepoint()?;
    /// assert!(matches!(txn.rollback_to_savepoint(), Err(TransactionError::NoSavepoint)));
    ///
    /// txn.commit()?;
    /// assert_eq!(db.db().get(b"a")?.as_deref(), Some(&b"kept"[..]));
    /// assert_eq!(db.db().get(b"b")?, None);
    /// assert_eq!(db.db().get(b"c")?.as_deref(), Some(&b"kept"[..]));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn set_savepoint(&mut self) {
        // `&mut self` is what makes this coherent: a savepoint over a
        // buffer another thread is still writing would capture a torn
        // state, so taking one is an exclusive operation even though
        // buffering is not.
        self.savepoints.push(Savepoint {
            writes: self.writes.len(),
            appends: self.appends_len(),
        });
    }

    /// Roll back to the most recent savepoint and forget it. Discards every
    /// buffered write made after the savepoint, in time proportional to the
    /// writes discarded. Locks acquired after the savepoint stay held:
    /// regolith's pessimistic lock manager does not release mid-transaction
    /// locks.
    ///
    /// Reads are not rolled back. A key this transaction has already
    /// read keeps the sequence it was read at, so a rollback can
    /// neither rewind a later read of that key nor launder a write
    /// that landed around the lock manager in the meantime.
    ///
    /// Inside a [`Transaction::before_commit`] callback the rollback is one
    /// of the callback's effects: if the callback fails, the savepoint is set
    /// again and the writes and appends the rollback took are put back, in
    /// their order.
    ///
    /// Returns [`TransactionError::NoSavepoint`] when no savepoint is set.
    pub fn rollback_to_savepoint(&mut self) -> TxResult<()> {
        let mark = self.savepoints.pop().ok_or(TransactionError::NoSavepoint)?;
        self.roll_back_to(mark);
        Ok(())
    }

    /// Forget the most recent savepoint without rolling back: the writes made
    /// since it stay, and an older savepoint, if one is set, becomes the
    /// current one. O(1).
    ///
    /// Inside a [`Transaction::before_commit`] callback the release is one of
    /// the callback's effects: if the callback fails, the savepoint is set
    /// again where it was.
    ///
    /// Returns [`TransactionError::NoSavepoint`] when no savepoint is set.
    pub fn release_savepoint(&mut self) -> TxResult<()> {
        let mark = self.savepoints.pop().ok_or(TransactionError::NoSavepoint)?;
        self.released(mark);
        Ok(())
    }

    /// Commit the transaction. Every key in the validation set is
    /// re-checked against the earliest sequence this transaction
    /// observed it at, and any conflict surfaces as
    /// [`TransactionError::Conflict`]; otherwise the buffered
    /// writes are applied atomically.
    ///
    /// An optimistic transaction validates every key it wrote or
    /// read through [`Transaction::get_for_update`] against its
    /// begin snapshot, so the check catches any concurrent writer. At
    /// [`IsolationLevel::DefraLevel`] a key it only merges into is the
    /// exception: a newer put, delete or range delete of the key is a
    /// conflict, and a newer merge operand is not. A
    /// pessimistic transaction validates every key it read and then
    /// wrote, plus every key it read through `get_for_update`,
    /// against the sequence that read observed. The check passes
    /// whenever every writer went through the lock manager and fires
    /// for a write that bypassed it or that landed before the lock
    /// was taken. A pessimistic blind write is not validated: the
    /// key lock orders it, and there is no read to lose.
    ///
    /// A commit that carries writes passes the same admission as a plain
    /// write with [`crate::WriteOptions::default`], in the same order: key
    /// and value sizes are validated first, along with the size of the
    /// record the commit will log (at most 1 GiB), so an oversized commit
    /// is rejected first, then the write-stall admission runs, before the
    /// commit takes its place in the pipeline.
    ///
    /// A commit is never lost to a write stall. A plain write a stop
    /// trigger holds returns the stall's wait, because its caller still has
    /// the data; a transaction cannot be committed a second time, so its
    /// commit waits the stop out instead, on this thread, and then commits.
    /// With a compaction worker it sleeps until the worker's flushes and
    /// compactions relieve the stop. With none it runs the steps that
    /// relieve the stop itself, as many as the stop needs and no more,
    /// whether or not this thread has a [`crate::IoQueue`] and whatever
    /// [`crate::Options::inline_compaction`] says, since nobody else is
    /// certain to; commit with [`Transaction::commit_nowait`] to keep the
    /// thread free. A step another thread's work is in the way of waits for
    /// that work. A job this thread's own [`crate::IoQueue`] holds (a
    /// [`crate::JobTicket`]'s, a step a write left owing) is stepped here
    /// meanwhile, as that queue's poll would step it, so the commit never
    /// waits for work only its own poll runs; a block read pending on the
    /// queue is not run here, and holds nothing the step waits for. The
    /// time it is held either way is counted in
    /// [`crate::Ticker::WriteStallMicros`]. A slowdown trigger delays
    /// nothing. A commit whose early check already found its conflict
    /// returns it without waiting. A commit that waits is told what a write
    /// arriving then is told when the stop will not clear: the database
    /// closed, the engine latched, the work that relieves the stop keeps
    /// failing, or no step relieves it ([`TransactionError::Engine`] with
    /// the same [`crate::Error`] a plain write reports, [`crate::Error::Busy`]
    /// for the last). A commit too large to log fails with
    /// [`TransactionError::Engine`] of [`crate::Error::InvalidArgument`] and
    /// applies nothing; split it into smaller transactions.
    ///
    /// A commit with no put, delete, merge or append writes nothing, at any
    /// level and in either flavour: it takes no part in the write pipeline,
    /// no capacity and no sequence, and no stall or other commit holds it up.
    /// On the calling thread it checks what its level checks at two points:
    /// everything visible at that moment, where it is placed (every commit
    /// visible by then comes before it), and everything decided by then and
    /// not yet visible, so it is refused whenever a commit through the
    /// pipeline, decided after those, would be. A read checked by value
    /// passes a rewrite of the bytes it read. At
    /// [`IsolationLevel::ReadCommitted`] that is nothing; at
    /// [`IsolationLevel::SnapshotIsolation`] only the reads made through
    /// [`Transaction::get_for_update`]; at [`IsolationLevel::RepeatableRead`]
    /// and [`IsolationLevel::Serializable`] every read the level validates;
    /// and a scan checked with [`ScanCheck::Range`] at each of them. So a
    /// read-only transaction at the first two never conflicts unless it asked
    /// to. An optimistic transaction at [`IsolationLevel::DefraLevel`] checks
    /// only its `get_for_update` reads: its plain reads are one snapshot,
    /// which lies on a serial order of the commits only while every writer on
    /// the database validates its own reads, and a writer at
    /// `SnapshotIsolation`, the default, breaks that (see [`IsolationLevel`]).
    /// A pessimistic one checks every read, as at `RepeatableRead`. Since it
    /// writes nothing, it commits on a database latched read-only too.
    ///
    /// Concurrent commits go through one pipeline with each other and with
    /// plain writes, with no lock: each is decided in the order it entered,
    /// by whichever thread gets there, written to the log, and, at
    /// [`crate::DurabilityMode::Immediate`], made durable by an fsync it
    /// shares with every commit written by then; it becomes visible only
    /// once that fsync returned. Each commit is validated as if the commits
    /// ahead of it had committed one at a time before it, so it conflicts
    /// with them exactly as it would with any earlier commit, and its outcome
    /// is its own. A log write or fsync that fails fails every commit not yet
    /// visible with that error. Before it enters the pipeline, an optimistic
    /// commit checks itself on the calling thread against everything
    /// committed up to that moment, so the pipeline only checks what landed
    /// after, and a conflict that is already certain returns at once.
    ///
    /// A conflict is reported to every [`crate::EventListener`] once, after
    /// the commit has left the pipeline and this transaction released its key
    /// locks, and then returned as [`TransactionError::Conflict`].
    ///
    /// The [`CommitReceipt`] carries the sequence the writes became visible
    /// at. A commit with no writes returns the sequence of its snapshot,
    /// even for a pessimistic transaction whose `get_for_update` reads were
    /// served later than that.
    ///
    /// The commit begins with [`Transaction::prepare`]: the transaction's
    /// `before_commit` callbacks, then the database's
    /// [`TransactionHooks::before_commit`], whose writes are validated with the
    /// rest. An error from either fails the commit. Once the outcome is known
    /// and the locks and snapshot are released, the transaction's `on_commit`
    /// callbacks run on this thread if it committed, and its `on_abort`
    /// callbacks if it did not, and then the database's hook for the same
    /// outcome.
    ///
    /// # Example
    ///
    /// ```
    /// use regolith::{OptimisticTransactionDb, Options, TransactionError, TxnOptions};
    ///
    /// let db = OptimisticTransactionDb::open("rows", Options::memory())?;
    /// let write = |key: &[u8]| {
    ///     let txn = db.begin(&TxnOptions::new());
    ///     txn.put(key, b"1")?;
    ///     txn.commit()
    /// };
    ///
    /// // Receipts order commits: a later commit has a higher sequence.
    /// let (first, second) = (write(b"a")?, write(b"b")?);
    /// assert!(first.seq() < second.seq());
    ///
    /// // A commit with no writes reports the sequence of its snapshot.
    /// let reader = db.begin(&TxnOptions::new());
    /// reader.get(b"a")?;
    /// assert_eq!(reader.commit()?.seq(), second.seq());
    ///
    /// // A lost race is an error value. It names the key and the reason,
    /// // and its message never prints the key's bytes.
    /// let late = db.begin(&TxnOptions::new());
    /// late.put(b"user/zoe", b"2")?;
    /// db.db().put(b"user/zoe", b"3")?;
    /// let Err(TransactionError::Conflict(lost)) = late.commit() else {
    ///     panic!("another write of the key committed first");
    /// };
    /// assert_eq!(lost.key(), b"user/zoe");
    /// assert!(lost.observed_seq() < lost.latest_seq());
    /// assert!(!lost.to_string().contains("zoe"));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn commit(self) -> TxResult<CommitReceipt> {
        self.conclude(Self::commit_inner)
    }

    /// End the transaction as a commit that failed with `error` before it
    /// reached the engine, as [`Transaction::commit`] ends one whose
    /// `prepare` returned it: the abort callbacks run once, with that error.
    /// For a caller that ran `prepare` itself (`transact_async`).
    fn fail_commit(self, error: TransactionError) -> TxResult<CommitReceipt> {
        self.conclude(|_| Err(error))
    }

    /// Win the commit claim, run `attempt`, and end the transaction by its
    /// result: release its resources, then run the callbacks of the outcome.
    fn conclude(
        mut self,
        attempt: impl FnOnce(&mut Self) -> TxResult<CommitReceipt>,
    ) -> TxResult<CommitReceipt> {
        // `close` may have aborted the transaction since it began: the commit
        // is then refused, and `close` ran the abort callbacks.
        let claimed = self.claim.as_ref().is_none_or(|claim| claim.begin_commit());
        self.committing = claimed;
        let result = if claimed {
            attempt(&mut self)
        } else {
            Err(Error::Closed.into())
        };
        self.resolved = true;
        // Locks and snapshot go before a listener runs, so a callback holds
        // nothing of this transaction. Drop finds them released.
        self.release_resources();
        if let Err(TransactionError::Conflict(conflict)) = &result {
            self.engine.notify_conflict(conflict);
        }
        self.finish(claimed, &result);
        result
    }

    /// Move this transaction out, leaving in its place an empty one that
    /// holds nothing: it pins no snapshot, has no claim, no lock and no
    /// write, and its end runs no callback and no hook. It only holds the
    /// place, for an owner that has the transaction by reference for a
    /// while, such as an [`OptimisticTransactionDb::transact_async`] attempt,
    /// and hands it to another owner until it puts it back
    /// (`*txn = taken`). It reads at this transaction's snapshot without
    /// pinning it, so compaction may drop what it would read.
    ///
    /// Beginning a transaction to hold the place instead registers a
    /// snapshot, and under [`TransactionHooks`] a claim, only to drop them.
    ///
    /// ```
    /// use regolith::{OptimisticTransactionDb, Options, TxnOptions};
    ///
    /// let db = OptimisticTransactionDb::open("taken", Options::memory())?;
    /// let mut held = db.begin(&TxnOptions::new());
    /// let taken = held.take();
    /// taken.put(b"key", b"value")?;
    /// held = taken;
    /// held.commit()?;
    /// assert_eq!(db.db().get(b"key")?.as_deref(), Some(&b"value"[..]));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn take(&mut self) -> Transaction {
        let shell = Self {
            engine: Arc::clone(&self.engine),
            policy: None,
            snapshot_seq: self.snapshot_seq,
            writes: TxnBuffer::new(0),
            tracked: TxnBuffer::new(0),
            reading: self.reading,
            claim: None,
            lock_table: None,
            isolation: self.isolation,
            mode: self.mode,
            durability: self.durability,
            early_validation: false,
            committing: false,
            nowait: false,
            resolved: true,
            resources_released: true,
            for_update_reads: AtomicBool::new(false),
            promoted_seq: AtomicU64::new(self.snapshot_seq),
            snapshot_pin: crate::engine::SnapshotPin::none(),
            prepare_state: Prepare::Pending,
            savepoints: Vec::new(),
            appends: OnceLock::new(),
            scan_runs: OnceLock::new(),
            scan_ranges: OnceLock::new(),
            held_locks: TxnBuffer::new(0),
            lock_owner: None,
            lock_waits: OnceLock::new(),
            lock_timeout: self.lock_timeout,
            callbacks: Callbacks::default(),
        };
        std::mem::replace(self, shell)
    }

    /// Discard the transaction's buffered writes and release any
    /// pessimistic locks. Equivalent to dropping the transaction,
    /// but surfaces as an explicit call in user code. Its `on_abort`
    /// callbacks run with [`AbortReason::Rollback`], and its `on_commit`
    /// callbacks are dropped.
    pub fn rollback(mut self) {
        self.resolved = true;
        self.release_resources();
        self.finish_rolled_back();
    }

    fn commit_inner(&mut self) -> TxResult<CommitReceipt> {
        self.prepare()?;
        let drained = self.drain_for_commit()?;
        // The write-stall admission (same order as a plain write with
        // `WriteOptions::default()`: closed/read-only, then size
        // validation, then the stall wait) runs inside
        // `commit_optimistic`, after its own `ensure_writable` and
        // `validate_ops_sizes` and before the commit takes a slot, gated on
        // the commit carrying an op. See the comment there for why.
        let outcome = if drained.write_free {
            // A blocking commit does its own I/O: no cache-only scope.
            self.engine.commit_write_free(&drained.commit.checks, None)
        } else {
            let commit = drained.commit;
            self.engine.commit_with_conflict_check(
                commit.checks,
                commit.point_ops,
                commit.range_deletes,
                commit.merges,
                commit.appends,
                commit.durability,
            )
        };
        receipt_of(
            self.engine.statistics(),
            outcome,
            self.snapshot_seq,
            drained.scan_runs_dropped,
        )
    }

    /// Everything the commit hands the engine, drained out of the
    /// transaction once its `before_commit` callbacks ran.
    fn drain_for_commit(&mut self) -> TxResult<Drained> {
        // `&mut self` here means buffering is over, so draining the
        // concurrent buffers cannot race. Every buffer is moved out
        // rather than copied: the transaction is being consumed, so
        // nothing needs the buffers' own copies afterwards, and draining
        // hands over the stored keys and values instead of cloning each
        // one. `settle` says what the drained writes commit as.
        let repeated =
            self.engine.merge_operator().is_some() && self.writes.may_have_repeated_keys();
        let (writes, mut merges) = settle(self.writes.drain());
        if repeated
            && merges.len() > 1
            && let Some(op) = self.engine.merge_operator()
        {
            fold_merges(&mut merges, op, self.engine.max_value_size());
        }
        let appends = self.appends.take().map_or_else(Vec::new, |mut buffer| {
            // Newest first out of the buffer; the commit wants the order made.
            let mut made = buffer.drain();
            made.reverse();
            made.into_iter().map(|(_, append)| append).collect()
        });
        // A commit with no put, delete, merge or append writes nothing, at
        // any level and in either flavour: it takes no part in the commit
        // pipeline and checks what its level checks at the published horizon
        // (`commit_write_free`, plan 3.8). A transaction never carries a
        // range delete (`delete_range` refuses a non-empty range).
        let write_free = writes.is_empty() && merges.is_empty() && appends.is_empty();
        // F2: when the level checks none of this transaction's reads, the set
        // holds none: it is built from no read, and no scan stretch, which
        // only widens reads and adds written keys, is folded into it.
        let checks_no_read = write_free && self.checks_no_read_written_free();
        let tracked = if checks_no_read {
            Vec::new()
        } else {
            self.tracked.drain()
        };
        let mut checks = self.validation_set(tracked, &writes, &merges);
        // DefraLevel's relaxation of a write-free transaction: an optimistic
        // one read one consistent snapshot and changes nothing, so no read of
        // it can be stale in a way that matters, except a read it asked to
        // have checked. A pessimistic one keeps every check of
        // RepeatableRead, since its `get_for_update` reads past the snapshot
        // can see two points in time (`MC_DefraLevel_Red_WriteFreePessimistic`).
        let relaxed = write_free && self.projects();
        if relaxed {
            checks
                .reads
                .retain(|read| read.access == Access::ReadForUpdate);
        }
        // Only an optimistic transaction at `DefraLevel` has a classifier to
        // consult: the policy is installed on the optimistic database alone,
        // and every other level ignores it.
        let classifier = policy::classifier_for(&self.policy, self.isolation);
        // The classifier is the caller's code, and a panic in it is caught,
        // here and in the engine's validation the commit then runs.
        let contain = classifier.map(|_| callback::InCommit::enter());
        // Counted here, recorded only once the commit succeeded: a commit
        // that aborts or fails dropped nothing.
        let mut scan_runs_dropped = 0u64;
        // Behind the `take`, not threaded through `validation_set`, so a
        // transaction that never scans (the common case) pays no `Vec`
        // round trip for an empty run list on its commit path.
        if let Some(runs) = self
            .scan_runs
            .take()
            .filter(|_| !relaxed && !checks_no_read)
        {
            let mut runs = drain(&runs);
            if let Some(classifier) = classifier {
                let before = runs.len();
                self.contain_classifier(|| {
                    runs.retain(|run| !policy::run_is_commutative(classifier, run))
                })?;
                scan_runs_dropped = (before - runs.len()) as u64;
            }
            scan_range::cover(
                &mut checks.reads,
                &runs,
                &writes,
                &merges,
                self.snapshot_seq,
                &self.full_read_rule(),
            );
        }
        if let Some(records) = self.scan_ranges.take().filter(|_| !relaxed) {
            checks.ranges = validated_range::checks(&drain(&records), self.snapshot_seq);
        }
        // After the scans are folded in, so the reads they added for put or
        // merged keys are dropped with the rest.
        if let Some(classifier) = classifier {
            self.contain_classifier(|| {
                policy::exempt_content_addressed(classifier, &mut checks, &writes, &merges)
            })?;
        }
        Ok(Drained {
            commit: TxnCommit {
                checks,
                point_ops: writes,
                // `delete_range` refuses every non-empty range, so a
                // transaction never carries one.
                range_deletes: Vec::new(),
                merges,
                appends,
                durability: self.durability,
                read_queue: None,
            },
            write_free,
            scan_runs_dropped,
            _contain: contain,
        })
    }

    /// Whether a commit that writes nothing checks none of its reads at this
    /// transaction's level, so its reads need not be drained, sorted or
    /// filtered, and no scan stretch folded over them (F2):
    /// `ReadCommitted` checks no read of a key the commit does not write,
    /// and `SnapshotIsolation` and DefraLevel's write-free relaxation check
    /// only `get_for_update` reads. The other levels check every read. A
    /// validated scan's range is checked apart from the reads, either way.
    /// `validation_set` keeps exactly these reads, so the two agree
    /// (`a_scan_validates_like_gets_of_what_it_walked`).
    fn checks_no_read_written_free(&mut self) -> bool {
        let for_update = *self.for_update_reads.get_mut();
        match self.isolation {
            IsolationLevel::ReadCommitted => true,
            IsolationLevel::SnapshotIsolation => !for_update,
            IsolationLevel::DefraLevel if self.projects() => !for_update,
            IsolationLevel::RepeatableRead
            | IsolationLevel::Serializable
            | IsolationLevel::DefraLevel => false,
        }
    }

    /// Run `f`, which calls the caller's [`KeyClassifier`]. A panic in it
    /// fails this commit with [`Error::CallbackPanicked`] and latches the
    /// database read-only, as a panic anywhere in the commit's ordered step
    /// does.
    fn contain_classifier<T>(&self, f: impl FnOnce() -> T) -> TxResult<T> {
        callback::contain("KeyClassifier", f).map_err(|err| {
            if let Error::CallbackPanicked { callback, .. } = err {
                self.engine.latch_callback_panic(callback);
            }
            TransactionError::Engine(err)
        })
    }

    /// What this commit validates beyond the keys it writes: every tracked
    /// read the isolation level cares about, at the earliest sequence the
    /// transaction observed it, plus the anchor the written keys are checked
    /// against.
    ///
    /// The size of this set *is* the isolation level:
    ///
    /// * [`IsolationLevel::ReadCommitted`] validates only what the
    ///   transaction wrote, so a read it did not write is never checked.
    /// * [`IsolationLevel::SnapshotIsolation`] adds keys read through
    ///   `get_for_update`, which is what stops a lost update. A plain
    ///   read is still unvalidated, which is what leaves write skew
    ///   reachable.
    /// * [`IsolationLevel::RepeatableRead`] adds every point read, so an
    ///   anti-dependency edge can only form through a scan.
    /// * [`IsolationLevel::Serializable`] adds every remaining read, so
    ///   no anti-dependency edge can form unseen.
    ///
    /// Optimistic: the written and merged keys are validated against the
    /// begin snapshot; a key that was also read is validated once, as the
    /// read. At `DefraLevel`, `blind_merges_commute` relaxes that for a
    /// merged key the transaction did not read: only a newer put, delete or
    /// range delete conflicts, and a newer merge operand does not.
    /// Pessimistic: written keys are not validated (`writes_at` is
    /// `None`), the key lock already orders them and there is no read for a
    /// concurrent writer to invalidate.
    ///
    /// This set does not yet account for what a transactional scan walked;
    /// `commit_inner` folds that in afterward with `scan_range::cover`,
    /// skipped entirely when the transaction ran no scan. It then applies the
    /// content-addressed rule, with `policy::exempt_content_addressed`,
    /// skipped entirely when the transaction has no classifier to consult.
    /// That is why each read carries whether it found a value: the rule
    /// needs it and only the read could say.
    fn validation_set(
        &self,
        mut tracked: Vec<(Vec<u8>, Arc<KeyState>)>,
        writes: &BTreeMap<Vec<u8>, Option<Vec<u8>>>,
        merges: &[(Vec<u8>, Vec<u8>)],
    ) -> ValidationSet {
        let optimistic = matches!(self.mode, TxMode::Optimistic);
        let every_read = self.isolation.validates_every_read();
        let read_committed = self.isolation == IsolationLevel::ReadCommitted;
        // The drain yields the newest node first and a stable sort keeps that
        // within a key, so dedup keeps the cell every reader of the key used
        // (`get_or_insert` settles a race by re-reading, newest first).
        tracked.sort_by(|a, b| a.0.cmp(&b.0));
        tracked.dedup_by(|later, first| later.0 == first.0);
        let value_rule = self.projects();
        // Part sets in use, so reads by one set share one allocation.
        let mut pool: Vec<Arc<[u32]>> = Vec::new();
        let reads = tracked
            .into_iter()
            .filter(|(key, state)| {
                let written =
                    || writes.contains_key(key) || merges.iter().any(|(merged, _)| merged == key);
                if every_read {
                    // Every read, whether or not the transaction wrote it.
                    true
                } else if read_committed {
                    written()
                } else {
                    state.for_update.load(Ordering::Acquire) || written()
                }
            })
            // Any read at all, whatever the level. This flag exists to
            // stop an idempotent-write elision at commit, and the
            // equivalence that elision rests on holds only for a write
            // nobody derived from a read.
            //
            // A read-modify-write is the counterexample: two transactions
            // read a counter at 5 and both write 6. The writes are
            // byte-identical, so eliding the second lets both commit and
            // the counter advances once, losing an increment. No serial
            // order produces that: run second, the transaction reads 6 and
            // writes 7. `first_read_seq` anchors the check at the read for
            // exactly this reason, and honouring the anchor is what makes
            // the abort correct.
            .filter_map(|(key, state)| {
                let for_update = state.for_update.load(Ordering::Acquire);
                let full = if value_rule {
                    ReadRule::Value
                } else {
                    ReadRule::Seq
                };
                // `None`: a key read in whole.
                let parts = state.projection.as_deref().and_then(|projection| {
                    projection.with_parts(|parts| parts.map(|parts| intern(&mut pool, parts)))
                });
                let (rule, access) = match parts {
                    // A read by no parts decided on nothing in the value.
                    Some(parts) if parts.is_empty() => return None,
                    // A put or delete replaces every part, and what it
                    // replaces was read by some: validated in whole.
                    Some(_) if writes.contains_key(&key) => (full, Access::ReadParts),
                    Some(parts) => (ReadRule::Parts(parts), Access::ReadParts),
                    None => (
                        full,
                        if for_update {
                            Access::ReadForUpdate
                        } else {
                            Access::Read
                        },
                    ),
                };
                Some(ConflictKey {
                    key,
                    observed_seq: state.first_read_seq,
                    found: state.found.load(Ordering::Acquire),
                    access,
                    rule,
                })
            })
            .collect();
        ValidationSet {
            reads,
            writes_at: optimistic.then_some(self.snapshot_seq),
            blind_merges_commute: self.isolation.blind_merges_commute(),
            exempt: Vec::new(),
            ranges: Vec::new(),
        }
    }

    /// The rule a read of a key's whole value is validated by. DefraLevel's
    /// relaxations apply to an optimistic transaction only; a pessimistic one
    /// validates as RepeatableRead, by sequence.
    fn full_read_rule(&self) -> ReadRule {
        if self.projects() {
            ReadRule::Value
        } else {
            ReadRule::Seq
        }
    }

    /// Sequence a fresh read of `key` is anchored at, given whether
    /// this transaction already held the key's lock.
    ///
    /// The pessimistic horizon is sampled with the lock held: a
    /// committing writer's commit is visible before its lock is released
    /// (at once for `commit`, when the commit lands for `commit_nowait`), and
    /// the lock's state word is the release/acquire edge between them, so
    /// nothing newer can hide behind it.
    /// Under a lock this transaction already holds nothing can have
    /// advanced, so the anchor recorded then still stands.
    fn read_horizon(&self, key: &[u8], already_held: bool) -> u64 {
        match self.mode {
            TxMode::Optimistic => self.snapshot_seq,
            TxMode::Pessimistic => {
                if already_held && let Some(state) = self.tracked.get(key) {
                    return state.read_seq.load(Ordering::Acquire);
                }
                self.engine.snapshot_seq()
            }
        }
    }

    /// Record that this transaction observed `key` at `horizon` and
    /// return the key's cell with the sequence the read should be served at.
    ///
    /// `first_read_seq` keeps the earliest observation, because that
    /// is the read a later write would overwrite. `read_seq` only
    /// moves forward, so promoting a key from a plain `get` to
    /// `get_for_update` never makes a later read of the same key
    /// return an older value than an earlier one.
    fn observe(&self, key: &[u8], horizon: u64, for_update: bool) -> (Arc<KeyState>, u64) {
        let state = self
            .tracked
            .get_or_insert(key.to_vec(), Arc::new(KeyState::new(horizon, for_update)));
        // `get_or_insert` is linearizable, so exactly one caller's cell
        // wins and every other caller folds its observation into that
        // one. Each fold is monotonic, so the result does not depend on
        // the order they land in.
        if let Some(projection) = &state.projection {
            // A read of the whole value: no set of parts narrows it again.
            projection.widen_to_full();
        }
        // `first_read_seq` is deliberately not touched here. It is set
        // once, by whichever call created the cell, and every later read
        // of the key is served at `read_seq`, which only rises. So every
        // read this transaction made happened at or after
        // `first_read_seq`, and validating against it is the strictest
        // check that is still true. Lowering it to a later call's
        // requested horizon would invent conflicts: a pessimistic
        // `get_for_update` anchors at the lock horizon on purpose, and a
        // plain `get` afterwards is served there too, not at the older
        // begin snapshot it asked for.
        if for_update {
            state.for_update.store(true, Ordering::Release);
        }
        let read_seq = state
            .read_seq
            .fetch_max(horizon, Ordering::AcqRel)
            .max(horizon);
        (state, read_seq)
    }

    /// The sequence a scan serves `key` (CF-prefixed) at, recording nothing:
    /// the begin snapshot, or the sequence a pessimistic transaction already
    /// reads the key at because `get_for_update` promoted it, which is what
    /// [`Transaction::get`] would serve.
    ///
    /// `tracked` is walked, not indexed, below
    /// [`crate::Options::transaction_keys_inline`] entries, so a `tracked.get`
    /// here would cost O(tracked keys) per yielded key. `promoted_seq` makes
    /// the common case (no `get_for_update` promoted anything past the begin
    /// snapshot) one Acquire load instead: no cell can have `read_seq` above
    /// `snapshot_seq` then, so a lookup could only ever confirm what the
    /// caller already knows.
    ///
    /// When a promotion might exist, this indexes `tracked` right here,
    /// before the lookup, rather than only once when the stream was built:
    /// `scan_stream_in` and `get_for_update` both take `&self`, so a
    /// promotion can land after the stream already exists, and indexing at
    /// construction alone would leave every later key walking the list.
    /// Doing it per key instead of once still costs O(1) per call past the
    /// first: `ensure_indexed` is a single `OnceLock::get` once the index is
    /// built, and it is built at most once (the `OnceLock` inside it makes a
    /// second, concurrent build a no-op). Sound with concurrent promotions:
    /// `get_for_update` inserts into `tracked` before it raises
    /// `promoted_seq` (`Release`-ordered by the `fetch_max` below, paired
    /// with this `Acquire` load), so once this load observes a promotion,
    /// that promotion's entry is already linked into `tracked`'s list and
    /// visible to the index build this call triggers.
    fn scan_read_seq(&self, key: &[u8]) -> u64 {
        match self.mode {
            // An optimistic transaction reads every key at its begin
            // snapshot: `read_horizon` never returns anything else.
            TxMode::Optimistic => self.snapshot_seq,
            TxMode::Pessimistic => {
                if self.promoted_seq.load(Ordering::Acquire) <= self.snapshot_seq {
                    return self.snapshot_seq;
                }
                self.tracked.ensure_indexed();
                self.tracked.get(key).map_or(self.snapshot_seq, |state| {
                    state
                        .read_seq
                        .load(Ordering::Acquire)
                        .max(self.snapshot_seq)
                })
            }
        }
    }

    /// Register the range a validated scan covers, so commit sees it however
    /// far the cursor got.
    fn record_scan_range(&self, range: Arc<RangeRecord>) {
        self.scan_ranges
            .get_or_init(|| Box::new(SegQueue::new()))
            .push(range);
    }

    /// Register a stretch a scan began, so commit sees it even if the stream
    /// is never closed.
    fn record_scan_run(&self, run: Arc<ScanRun>) {
        self.scan_runs
            .get_or_init(|| Box::new(SegQueue::new()))
            .push(run);
    }

    fn release_resources(&mut self) {
        if self.resources_released {
            return;
        }
        self.resources_released = true;
        if let Some(slot) = self.claim.as_ref().and_then(|claim| claim.slot) {
            self.engine.open_transactions().remove(slot);
        }
        self.release_locks();
        self.engine.release_snapshot(self.snapshot_pin.take());
    }
}

/// What a transaction's commit hands the engine, drained out of it.
struct Drained {
    commit: TxnCommit,
    /// A commit with no put, delete, merge or append, at any level: it takes
    /// no part in the write pipeline (`RegolithEngine::commit_write_free`).
    write_free: bool,
    /// Scanned stretches the classifier dropped, recorded once the commit
    /// succeeds.
    scan_runs_dropped: u64,
    /// Contains a panic in the caller's classifier until the commit is
    /// decided.
    _contain: Option<callback::InCommit>,
}

/// The commit's receipt or the reason it failed, from what the engine
/// decided, with the commit's counts recorded. The one mapping a
/// synchronous commit and a ticket's delivery share.
fn receipt_of(
    stats: Option<&crate::Statistics>,
    outcome: std::io::Result<CommitOutcome>,
    snapshot_seq: u64,
    scan_runs_dropped: u64,
) -> TxResult<CommitReceipt> {
    match outcome? {
        CommitOutcome::Ok { seq } => {
            if let Some(s) = stats {
                s.add(crate::Ticker::CommitCount, 1);
                if scan_runs_dropped > 0 {
                    s.add(crate::Ticker::PolicyScanRunsDropped, scan_runs_dropped);
                }
            }
            Ok(CommitReceipt::new(seq.unwrap_or(snapshot_seq)))
        }
        CommitOutcome::Conflict(mut conflict) => {
            conflict.strip_cf_prefix();
            if let Some(s) = stats {
                s.add(crate::Ticker::CommitConflicts, 1);
                if conflict.theirs().is_deletion() {
                    s.add(crate::Ticker::CommitConflictsAfterDelete, 1);
                }
            }
            Err(TransactionError::Conflict(conflict))
        }
    }
}

/// The shared copy of `parts` in `pool`, adding it if it is new.
fn intern(pool: &mut Vec<Arc<[u32]>>, parts: &[u32]) -> Arc<[u32]> {
    if let Some(shared) = pool.iter().find(|shared| shared.as_ref() == parts) {
        return Arc::clone(shared);
    }
    let shared: Arc<[u32]> = Arc::from(parts);
    pool.push(Arc::clone(&shared));
    shared
}

/// Empty a queue into a `Vec`, preserving push order.
///
/// Only ever called with exclusive access to the transaction, so no
/// producer can be pushing concurrently and the result is the complete
/// buffer rather than a snapshot of one.
fn drain<T: 'static>(queue: &SegQueue<T>) -> Vec<T> {
    let mut drained = Vec::with_capacity(queue.len());
    while let Some(entry) = queue.pop() {
        drained.push(entry);
    }
    drained
}

/// Drop the 4-byte column-family prefix that `prefix_key` adds so
/// errors surface the key the caller passed in.
fn strip_cf_prefix(key: Vec<u8>) -> Vec<u8> {
    if key.len() >= 4 {
        key[4..].to_vec()
    } else {
        key
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        let abandoned = !self.resolved;
        self.resolved = true;
        self.release_resources();
        if abandoned {
            self.finish_dropped();
        }
        // The claim holds a clone of the lock owner; letting it go first lets
        // the owner be lent to this thread's next transaction when nothing
        // else holds either.
        self.claim = None;
        if let Some(owner) = self.lock_owner.take() {
            LockOwner::give_back(owner);
        }
    }
}

#[cfg(test)]
mod lock_tests;

#[cfg(test)]
mod validation_set_tests;

#[cfg(test)]
mod record_limit_tests;

#[cfg(test)]
mod commit_claim_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WriteKind;
    use tempfile::TempDir;

    fn opt_db() -> (OptimisticTransactionDb, TempDir) {
        let dir = TempDir::new().unwrap();
        let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
        (db, dir)
    }

    fn pes_db() -> (TransactionDb, TempDir) {
        let dir = TempDir::new().unwrap();
        let db = TransactionDb::open(dir.path(), Options::default()).unwrap();
        (db, dir)
    }

    // ── Optimistic flavor ───────────────────────────────────────────────

    #[test]
    fn optimistic_basic_put_commit_read() {
        let (db, _dir) = opt_db();
        let tx = db.begin(&TxnOptions::new());
        tx.put(b"k", b"v").unwrap();
        tx.commit().unwrap();
        assert_eq!(db.db().get(b"k").unwrap(), Some(b"v".to_vec()));
    }

    #[test]
    fn optimistic_read_your_own_writes() {
        let (db, _dir) = opt_db();
        db.db().put(b"k", b"initial").unwrap();
        let tx = db.begin(&TxnOptions::new());
        assert_eq!(tx.get(b"k").unwrap(), Some(b"initial".to_vec()));
        tx.put(b"k", b"staged").unwrap();
        // Tx sees its own write.
        assert_eq!(tx.get(b"k").unwrap(), Some(b"staged".to_vec()));
        // Outside the tx the old value is still visible until commit.
        assert_eq!(db.db().get(b"k").unwrap(), Some(b"initial".to_vec()));
        tx.commit().unwrap();
        assert_eq!(db.db().get(b"k").unwrap(), Some(b"staged".to_vec()));
    }

    #[test]
    fn optimistic_rollback_discards_writes() {
        let (db, _dir) = opt_db();
        let tx = db.begin(&TxnOptions::new());
        tx.put(b"k", b"never").unwrap();
        tx.rollback();
        assert_eq!(db.db().get(b"k").unwrap(), None);
    }

    #[test]
    fn optimistic_rollback_releases_shared_snapshot_pin_once() {
        let (db, _dir) = opt_db();
        let tx1 = db.begin(&TxnOptions::new());
        let tx2 = db.begin(&TxnOptions::new());
        assert_eq!(db.db().get_int_property("regolith.num-snapshots"), Some(2));

        tx1.rollback();
        assert_eq!(db.db().get_int_property("regolith.num-snapshots"), Some(1));

        drop(tx2);
        assert_eq!(db.db().get_int_property("regolith.num-snapshots"), Some(0));
    }

    #[test]
    fn optimistic_conflict_detected() {
        let (db, _dir) = opt_db();
        db.db().put(b"k", b"v0").unwrap();
        let tx1 = db.begin(&TxnOptions::new());
        assert_eq!(tx1.get(b"k").unwrap(), Some(b"v0".to_vec()));
        // Concurrent writer bumps the key.
        db.db().put(b"k", b"v1").unwrap();
        tx1.put(b"k", b"v2").unwrap();
        match tx1.commit() {
            Err(TransactionError::Conflict(conflict)) => {
                assert_eq!(conflict.key(), b"k");
                assert_eq!(
                    (conflict.mine(), conflict.theirs()),
                    (Access::Read, WriteKind::Put)
                );
            }
            other => panic!("expected conflict, got {other:?}"),
        }
        // db still reflects the concurrent writer's value.
        assert_eq!(db.db().get(b"k").unwrap(), Some(b"v1".to_vec()));
    }

    #[test]
    fn optimistic_get_for_update_tracks_conflicts() {
        let (db, _dir) = opt_db();
        db.db().put(b"k", b"v0").unwrap();
        let tx = db.begin(&TxnOptions::new());
        assert_eq!(tx.get_for_update(b"k").unwrap(), Some(b"v0".to_vec()));
        // Concurrent writer invalidates the read.
        db.db().put(b"k", b"v1").unwrap();
        // The tx didn't buffer a write on k, but it flagged k for
        // conflict detection, so commit must still detect.
        tx.put(b"other", b"stuff").unwrap();
        match tx.commit() {
            Err(TransactionError::Conflict(conflict)) => {
                assert_eq!(conflict.key(), b"k");
                assert_eq!(conflict.mine(), Access::ReadForUpdate);
            }
            other => panic!("expected conflict, got {other:?}"),
        }
    }

    #[test]
    fn optimistic_conflict_reports_the_lowest_conflicting_key() {
        let (db, _dir) = opt_db();
        db.db().put(b"a", b"v0").unwrap();
        db.db().put(b"z", b"v0").unwrap();
        let tx = db.begin(&TxnOptions::new());
        // Tracked in the reverse of their sort order.
        tx.get_for_update(b"z").unwrap();
        tx.get_for_update(b"a").unwrap();
        db.db().put(b"z", b"v1").unwrap();
        db.db().put(b"a", b"v1").unwrap();
        match tx.commit() {
            Err(TransactionError::Conflict(conflict)) => assert_eq!(conflict.key(), b"a"),
            other => panic!("expected conflict, got {other:?}"),
        }
    }

    #[test]
    fn optimistic_no_conflict_passes() {
        let (db, _dir) = opt_db();
        db.db().put(b"k", b"v0").unwrap();
        let tx = db.begin(&TxnOptions::new());
        tx.put(b"other", b"stuff").unwrap();
        tx.commit().unwrap();
        assert_eq!(db.db().get(b"other").unwrap(), Some(b"stuff".to_vec()));
    }

    #[test]
    fn optimistic_snapshot_isolation_reads() {
        let (db, _dir) = opt_db();
        db.db().put(b"k", b"v0").unwrap();
        let tx = db.begin(&TxnOptions::new());
        db.db().put(b"k", b"v1").unwrap();
        // Tx is anchored at the seq before the second put.
        assert_eq!(tx.get(b"k").unwrap(), Some(b"v0".to_vec()));
    }

    #[test]
    fn optimistic_savepoint_rollback() {
        let (db, _dir) = opt_db();
        let mut tx = db.begin(&TxnOptions::new());
        tx.put(b"a", b"1").unwrap();
        tx.set_savepoint();
        tx.put(b"b", b"2").unwrap();
        tx.put(b"c", b"3").unwrap();
        tx.rollback_to_savepoint().unwrap();
        // a survives, b and c are rolled back.
        assert_eq!(tx.get(b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(tx.get(b"b").unwrap(), None);
        assert_eq!(tx.get(b"c").unwrap(), None);
        tx.commit().unwrap();
        assert_eq!(db.db().get(b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(db.db().get(b"b").unwrap(), None);
    }

    #[test]
    fn optimistic_rollback_to_savepoint_without_savepoint_errors() {
        let (db, _dir) = opt_db();
        let mut tx = db.begin(&TxnOptions::new());
        assert!(matches!(
            tx.rollback_to_savepoint(),
            Err(TransactionError::NoSavepoint)
        ));
    }

    #[test]
    fn optimistic_delete_commit() {
        let (db, _dir) = opt_db();
        db.db().put(b"k", b"v").unwrap();
        let tx = db.begin(&TxnOptions::new());
        tx.delete(b"k").unwrap();
        tx.commit().unwrap();
        assert_eq!(db.db().get(b"k").unwrap(), None);
    }

    #[test]
    fn optimistic_range_delete_is_rejected() {
        let (db, _dir) = opt_db();
        let tx = db.begin(&TxnOptions::new());

        assert!(matches!(
            tx.delete_range(b"a", b"z"),
            Err(TransactionError::UnsupportedRangeDelete)
        ));
        assert!(tx.delete_range(b"z", b"a").is_ok());
    }

    // ── Pessimistic flavor ──────────────────────────────────────────────

    #[test]
    fn pessimistic_basic_put_commit_read() {
        let (db, _dir) = pes_db();
        let tx = db.begin(&TxnOptions::new());
        tx.put(b"k", b"v").unwrap();
        tx.commit().unwrap();
        assert_eq!(db.db().get(b"k").unwrap(), Some(b"v".to_vec()));
    }

    #[test]
    fn pessimistic_range_delete_is_rejected() {
        let (db, _dir) = pes_db();
        let tx = db.begin(&TxnOptions::new());

        assert!(matches!(
            tx.delete_range(b"a", b"z"),
            Err(TransactionError::UnsupportedRangeDelete)
        ));
        assert!(tx.delete_range(b"z", b"a").is_ok());
    }

    #[test]
    fn pessimistic_lock_blocks_second_writer() {
        let (db, _dir) = pes_db();
        let db = Arc::new(db);
        let tx1 = db.begin(&TxnOptions::new());
        tx1.put(b"k", b"v1").unwrap();
        // Second tx with a short timeout must fail to lock `k`.
        let db2 = Arc::clone(&db);
        let join = std::thread::spawn(move || {
            // The default lock timeout is 1s, which is too long for
            // the test, so recreate the transaction with a shorter
            // manual lock acquisition via `get_for_update`. We
            // rely on acquire_lock using the DB's
            // configured timeout; so we just do a normal put and
            // expect `Busy`.
            let tx2 = db2.begin(&TxnOptions::new());
            tx2.put(b"k", b"v2")
        });
        // Give tx2 some time to actually start waiting.
        std::thread::sleep(std::time::Duration::from_millis(50));
        // Commit tx1: lock releases, tx2 should now succeed.
        tx1.commit().unwrap();
        let result = join.join().unwrap();
        assert!(result.is_ok(), "tx2 put should succeed once tx1 commits");
    }

    #[test]
    fn pessimistic_lock_timeout_returns_busy() {
        let dir = TempDir::new().unwrap();
        let db = TransactionDb::open(dir.path(), Options::default())
            .unwrap()
            .with_lock_timeout(Duration::from_millis(50));
        let db = Arc::new(db);
        // tx1 grabs the lock and holds it.
        let tx1 = db.begin(&TxnOptions::new());
        tx1.put(b"k", b"v1").unwrap();
        let db2 = Arc::clone(&db);
        let join = std::thread::spawn(move || {
            let tx2 = db2.begin(&TxnOptions::new());
            tx2.put(b"k", b"v2")
        });
        // tx2 should time out within ~50ms.
        let result = join.join().unwrap();
        assert!(matches!(result, Err(TransactionError::Busy(_))));
        // Cleanup.
        tx1.rollback();
    }

    #[test]
    fn pessimistic_reentrant_lock() {
        let (db, _dir) = pes_db();
        let tx = db.begin(&TxnOptions::new());
        tx.put(b"k", b"v1").unwrap();
        // Re-lock (via a second put) must not deadlock against
        // itself.
        tx.put(b"k", b"v2").unwrap();
        tx.get_for_update(b"k").unwrap();
        tx.commit().unwrap();
        assert_eq!(db.db().get(b"k").unwrap(), Some(b"v2".to_vec()));
    }

    #[test]
    fn pessimistic_rollback_releases_locks() {
        let (db, _dir) = pes_db();
        let db = Arc::new(db);
        let tx1 = db.begin(&TxnOptions::new());
        tx1.put(b"k", b"v1").unwrap();
        tx1.rollback();
        // New tx must now be able to grab the lock without blocking.
        let tx2 = db.begin(&TxnOptions::new());
        tx2.put(b"k", b"v2").unwrap();
        tx2.commit().unwrap();
        assert_eq!(db.db().get(b"k").unwrap(), Some(b"v2".to_vec()));
    }

    #[test]
    fn pessimistic_rollback_releases_shared_snapshot_pin_once() {
        let (db, _dir) = pes_db();
        let tx1 = db.begin(&TxnOptions::new());
        let tx2 = db.begin(&TxnOptions::new());
        assert_eq!(db.db().get_int_property("regolith.num-snapshots"), Some(2));

        tx1.rollback();
        assert_eq!(db.db().get_int_property("regolith.num-snapshots"), Some(1));

        drop(tx2);
        assert_eq!(db.db().get_int_property("regolith.num-snapshots"), Some(0));
    }

    #[test]
    fn pessimistic_drop_releases_locks() {
        let (db, _dir) = pes_db();
        let db = Arc::new(db);
        {
            let tx1 = db.begin(&TxnOptions::new());
            tx1.put(b"k", b"v1").unwrap();
            // tx1 dropped here without explicit rollback.
        }
        let tx2 = db.begin(&TxnOptions::new());
        tx2.put(b"k", b"v2").unwrap();
        tx2.commit().unwrap();
        assert_eq!(db.db().get(b"k").unwrap(), Some(b"v2".to_vec()));
    }

    #[test]
    fn pessimistic_read_your_own_writes() {
        let (db, _dir) = pes_db();
        db.db().put(b"k", b"initial").unwrap();
        let tx = db.begin(&TxnOptions::new());
        tx.put(b"k", b"staged").unwrap();
        assert_eq!(tx.get(b"k").unwrap(), Some(b"staged".to_vec()));
        tx.commit().unwrap();
    }

    #[test]
    fn pessimistic_get_for_update_locks() {
        let dir = TempDir::new().unwrap();
        let db = Arc::new(
            TransactionDb::open(dir.path(), Options::default())
                .unwrap()
                .with_lock_timeout(Duration::from_millis(50)),
        );
        db.db().put(b"k", b"v0").unwrap();
        let tx1 = db.begin(&TxnOptions::new());
        assert_eq!(tx1.get_for_update(b"k").unwrap(), Some(b"v0".to_vec()));
        // Concurrent tx2 can't touch k.
        let db2 = Arc::clone(&db);
        let join = std::thread::spawn(move || {
            let tx2 = db2.begin(&TxnOptions::new());
            tx2.put(b"k", b"v1")
        });
        let result = join.join().unwrap();
        assert!(matches!(result, Err(TransactionError::Busy(_))));
        tx1.rollback();
    }

    #[test]
    fn pessimistic_savepoint_keeps_locks_but_rolls_back_writes() {
        let (db, _dir) = pes_db();
        let mut tx = db.begin(&TxnOptions::new());
        tx.put(b"a", b"1").unwrap();
        tx.set_savepoint();
        tx.put(b"b", b"2").unwrap();
        tx.rollback_to_savepoint().unwrap();
        // a's put survives, b's put is rolled back.
        assert_eq!(tx.get(b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(tx.get(b"b").unwrap(), None);
        tx.commit().unwrap();
        assert_eq!(db.db().get(b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(db.db().get(b"b").unwrap(), None);
    }

    #[test]
    fn pessimistic_get_for_update_sees_writes_committed_after_begin() {
        let (db, _dir) = pes_db();
        db.db().put(b"k", b"v0").unwrap();
        let tx = db.begin(&TxnOptions::new());
        // Lands after the transaction began, before it locks `k`.
        db.db().put(b"k", b"v1").unwrap();
        assert_eq!(tx.get_for_update(b"k").unwrap(), Some(b"v1".to_vec()));
        // A plain `get` on the locked key reads at the same horizon.
        assert_eq!(tx.get(b"k").unwrap(), Some(b"v1".to_vec()));
        tx.put(b"k", b"v2").unwrap();
        tx.commit().unwrap();
        assert_eq!(db.db().get(b"k").unwrap(), Some(b"v2".to_vec()));
    }

    #[test]
    fn pessimistic_second_locker_does_not_observe_precommit_value() {
        let (db, _dir) = pes_db();
        db.db().put(b"k", b"v0").unwrap();
        // Both transactions begin before either one commits, so both
        // are anchored at the seq where `k` is still `v0`.
        let tx1 = db.begin(&TxnOptions::new());
        let tx2 = db.begin(&TxnOptions::new());
        assert_eq!(tx1.get_for_update(b"k").unwrap(), Some(b"v0".to_vec()));
        tx1.put(b"k", b"v1").unwrap();
        tx1.commit().unwrap();
        // tx2 only now gets the lock, so it must not see `v0`.
        assert_eq!(tx2.get_for_update(b"k").unwrap(), Some(b"v1".to_vec()));
        tx2.put(b"k", b"v2").unwrap();
        tx2.commit().unwrap();
        assert_eq!(db.db().get(b"k").unwrap(), Some(b"v2".to_vec()));
    }

    #[test]
    fn pessimistic_commit_detects_write_from_outside_the_lock_manager() {
        let (db, _dir) = pes_db();
        let tx = db.begin(&TxnOptions::new());
        tx.get_for_update(b"k").unwrap();
        // A raw `Db` write never touches the lock manager.
        db.db().put(b"k", b"racer").unwrap();
        tx.put(b"k", b"mine").unwrap();
        match tx.commit() {
            Err(TransactionError::Conflict(conflict)) => assert_eq!(conflict.key(), b"k"),
            other => panic!("expected conflict, got {other:?}"),
        }
        assert_eq!(db.db().get(b"k").unwrap(), Some(b"racer".to_vec()));
    }

    #[test]
    fn pessimistic_sequential_transactions_do_not_conflict() {
        let (db, _dir) = pes_db();
        let tx1 = db.begin(&TxnOptions::new());
        tx1.put(b"k", b"v1").unwrap();
        tx1.commit().unwrap();
        let tx2 = db.begin(&TxnOptions::new());
        assert_eq!(tx2.get_for_update(b"k").unwrap(), Some(b"v1".to_vec()));
        tx2.put(b"k", b"v2").unwrap();
        tx2.commit().unwrap();
        assert_eq!(db.db().get(b"k").unwrap(), Some(b"v2".to_vec()));
    }

    #[test]
    fn pessimistic_blind_put_after_external_write_is_not_a_conflict() {
        let (db, _dir) = pes_db();
        db.db().put(b"k", b"v0").unwrap();
        let tx = db.begin(&TxnOptions::new());
        db.db().put(b"k", b"v1").unwrap();
        // Nothing was read, so there is no read to lose: a blind write
        // is last-writer-wins against a non-transactional writer.
        tx.put(b"k", b"v2").unwrap();
        tx.commit().unwrap();
        assert_eq!(db.db().get(b"k").unwrap(), Some(b"v2".to_vec()));
    }

    #[test]
    fn pessimistic_blind_put_before_external_write_is_not_a_conflict() {
        // The mirror ordering: the external write lands after the
        // transaction has already buffered its blind write.
        let (db, _dir) = pes_db();
        db.db().put(b"k", b"v0").unwrap();
        let tx = db.begin(&TxnOptions::new());
        tx.put(b"k", b"v2").unwrap();
        db.db().put(b"k", b"v1").unwrap();
        tx.commit().unwrap();
        assert_eq!(db.db().get(b"k").unwrap(), Some(b"v2".to_vec()));
    }

    #[test]
    fn pessimistic_savepoint_rollback_keeps_untouched_keys_unvalidated() {
        let (db, _dir) = pes_db();
        let mut tx = db.begin(&TxnOptions::new());
        tx.set_savepoint();
        tx.put(b"b", b"rolled-back").unwrap();
        tx.rollback_to_savepoint().unwrap();
        // `b` was written blind and the write was rolled back, so it
        // was never read and is not validated. The lock stays held.
        db.db().put(b"b", b"external").unwrap();
        tx.put(b"a", b"1").unwrap();
        tx.commit().unwrap();
        assert_eq!(db.db().get(b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(db.db().get(b"b").unwrap(), Some(b"external".to_vec()));
    }

    #[test]
    fn pessimistic_savepoint_rollback_keeps_the_read_anchor() {
        // Rolling back to a savepoint used to restore the conflict map
        // and let the next write re-anchor at a newer horizon, which
        // laundered a write that had bypassed the lock manager.
        let (db, _dir) = pes_db();
        db.db().put(b"k", b"v0").unwrap();
        let mut tx = db.begin(&TxnOptions::new());
        assert_eq!(tx.get_for_update(b"k").unwrap(), Some(b"v0".to_vec()));
        tx.set_savepoint();
        tx.put(b"k", b"rolled-back").unwrap();
        tx.rollback_to_savepoint().unwrap();
        db.db().put(b"k", b"racer").unwrap();
        tx.put(b"k", b"mine").unwrap();
        let err = tx.commit().expect_err("the bypassing write must be caught");
        assert!(matches!(err, TransactionError::Conflict { .. }), "{err:?}");
        assert_eq!(db.db().get(b"k").unwrap(), Some(b"racer".to_vec()));
    }

    #[test]
    fn pessimistic_reads_do_not_travel_backwards_after_a_savepoint_rollback() {
        let (db, _dir) = pes_db();
        db.db().put(b"k", b"v0").unwrap();
        let mut tx = db.begin(&TxnOptions::new());
        db.db().put(b"k", b"v1").unwrap();
        let first = tx.get_for_update(b"k").unwrap();
        assert_eq!(first, Some(b"v1".to_vec()));
        tx.set_savepoint();
        tx.put(b"k", b"staged").unwrap();
        tx.rollback_to_savepoint().unwrap();
        // The lock is still held and the read anchor with it, so the
        // second read cannot return an older value than the first.
        assert_eq!(tx.get(b"k").unwrap(), first);
    }

    #[test]
    fn pessimistic_read_then_write_detects_a_concurrent_write() {
        // A read-modify-write through plain `get` takes no lock, so the
        // commit check is the only thing standing between it and a lost
        // update.
        let (db, _dir) = pes_db();
        db.db().put(b"k", b"v0").unwrap();
        let tx = db.begin(&TxnOptions::new());
        assert_eq!(tx.get(b"k").unwrap(), Some(b"v0".to_vec()));
        db.db().put(b"k", b"v1").unwrap();
        tx.put(b"k", b"derived-from-v0").unwrap();
        let err = tx.commit().expect_err("the stale read must be caught");
        assert!(matches!(err, TransactionError::Conflict { .. }), "{err:?}");
        assert_eq!(db.db().get(b"k").unwrap(), Some(b"v1".to_vec()));
    }

    #[test]
    fn pessimistic_read_without_a_write_is_not_validated() {
        let (db, _dir) = pes_db();
        db.db().put(b"read-only", b"v0").unwrap();
        let tx = db.begin(&TxnOptions::new());
        assert_eq!(tx.get(b"read-only").unwrap(), Some(b"v0".to_vec()));
        db.db().put(b"read-only", b"v1").unwrap();
        tx.put(b"other", b"1").unwrap();
        tx.commit().unwrap();
        assert_eq!(db.db().get(b"other").unwrap(), Some(b"1".to_vec()));
    }

    #[test]
    fn pessimistic_range_delete_over_a_tracked_key_is_a_conflict() {
        let (db, _dir) = pes_db();
        db.db().put(b"k", b"v0").unwrap();
        let tx = db.begin(&TxnOptions::new());
        assert_eq!(tx.get_for_update(b"k").unwrap(), Some(b"v0".to_vec()));
        db.db().delete_range(b"a", b"z").unwrap();
        tx.put(b"k", b"resurrected").unwrap();
        let err = tx
            .commit()
            .expect_err("a range delete over a tracked key is a conflict");
        assert!(matches!(err, TransactionError::Conflict { .. }), "{err:?}");
        assert_eq!(db.db().get(b"k").unwrap(), None);
    }

    #[test]
    fn optimistic_range_delete_over_a_tracked_key_is_a_conflict() {
        let (db, _dir) = opt_db();
        db.db().put(b"k", b"v0").unwrap();
        let tx = db.begin(&TxnOptions::new());
        assert_eq!(tx.get_for_update(b"k").unwrap(), Some(b"v0".to_vec()));
        db.db().delete_range(b"a", b"z").unwrap();
        tx.put(b"k", b"resurrected").unwrap();
        let err = tx
            .commit()
            .expect_err("a range delete over a tracked key is a conflict");
        assert!(matches!(err, TransactionError::Conflict { .. }), "{err:?}");
        assert_eq!(db.db().get(b"k").unwrap(), None);
    }

    // ── Shared behavior ─────────────────────────────────────────────────

    #[test]
    fn commit_is_atomic_with_respect_to_other_writers() {
        let (db, _dir) = opt_db();
        let tx = db.begin(&TxnOptions::new());
        tx.put(b"a", b"1").unwrap();
        tx.put(b"b", b"2").unwrap();
        tx.put(b"c", b"3").unwrap();
        tx.commit().unwrap();
        assert_eq!(db.db().get(b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(db.db().get(b"b").unwrap(), Some(b"2".to_vec()));
        assert_eq!(db.db().get(b"c").unwrap(), Some(b"3".to_vec()));
    }

    #[test]
    fn multi_get_within_transaction() {
        let (db, _dir) = opt_db();
        db.db().put(b"a", b"1").unwrap();
        db.db().put(b"b", b"2").unwrap();
        let tx = db.begin(&TxnOptions::new());
        tx.put(b"a", b"staged").unwrap();
        assert_eq!(tx.get(b"a").unwrap(), Some(b"staged".to_vec()));
        assert_eq!(tx.get(b"b").unwrap(), Some(b"2".to_vec()));
        assert_eq!(tx.get(b"missing").unwrap(), None);
    }

    // -- Write-stall admission --------------------------------------------

    fn stop_opts() -> Options {
        Options::default()
            // Tiny memtable so every handful of puts rolls an L0 file.
            .write_buffer_size(4 * 1024)
            // Disable automatic compaction so L0 can't drain on us.
            .l0_compaction_trigger(1000)
            // Stop writes once L0 has 2 files.
            .level0_slowdown_writes_trigger(0)
            .level0_stop_writes_trigger(2)
            // Disable the memtable-count trigger so this isolates the L0
            // stop.
            .max_write_buffer_number(0)
    }

    /// Write `plain` until L0 reaches the stop trigger. The worker writes
    /// the sealed memtables out off the commit path.
    fn cross_stop(plain: &Db) {
        let payload = vec![0xCDu8; 600];
        let l0 = || {
            plain
                .get_int_property("regolith.num-files-at-level0")
                .unwrap_or(0)
        };
        for i in 0..64 {
            if l0() + plain.engine.frozen_memtables() as u64 >= 2 {
                break;
            }
            let k = format!("k{i:04}");
            crate::through_stalls(|| plain.put(k.as_bytes(), &payload)).unwrap();
        }
        crate::tests::wait_for_l0_files(plain, 2);
    }

    /// Drive `plain` to the stop, then check that a commit through `begin`
    /// waits the stop out when, and only when, it carries a write: while the
    /// stop holds it is on the stall's unit with nothing of it stored, and
    /// once another thread's compaction relieved the stop it returns with
    /// the commit. A plain write the same stop holds is handed the wait.
    fn probe_stop(plain: &Db, begin: impl Fn() -> Transaction + Sync) {
        cross_stop(plain);

        let tx = begin();
        tx.get(b"k0000").unwrap();
        tx.commit()
            .expect("a read-only commit takes no capacity, so a stall does not stop it");
        assert!(
            !plain.engine.stall_signal().has_unit(),
            "nothing waits on the stall yet"
        );

        std::thread::scope(|scope| {
            let committer = scope.spawn(|| {
                let tx = begin();
                tx.put(b"txn", b"v").unwrap();
                tx.commit()
            });
            // The commit waits once the stall has its unit: only a writer the
            // stop holds installs one.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while !plain.engine.stall_signal().has_unit() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the commit never met the stop"
                );
                std::thread::yield_now();
            }
            assert!(!committer.is_finished(), "the commit waits the stop out");
            assert_eq!(plain.get(b"txn").unwrap(), None, "it applied nothing yet");
            assert!(
                matches!(
                    plain.put(b"plain", b"v"),
                    Err(Error::WouldBlock(crate::WouldBlock::Stall(_)))
                ),
                "a plain write is still handed the stall's wait"
            );
            plain.compact_range(None, None).wait().unwrap();
            committer
                .join()
                .unwrap()
                .expect("the commit goes through once the stop is relieved");
        });
        assert_eq!(plain.get(b"txn").unwrap(), Some(b"v".to_vec()));
    }

    #[test]
    fn optimistic_commit_waits_a_stop_out() {
        let dir = TempDir::new().unwrap();
        let db = OptimisticTransactionDb::open(dir.path(), stop_opts()).unwrap();
        probe_stop(db.db(), || db.begin(&TxnOptions::new()));
    }

    #[test]
    fn pessimistic_commit_waits_a_stop_out() {
        let dir = TempDir::new().unwrap();
        let db = TransactionDb::open(dir.path(), stop_opts()).unwrap();
        probe_stop(db.db(), || db.begin(&TxnOptions::new()));
    }

    #[test]
    fn commit_on_a_closed_handle_reports_closed_before_it_waits() {
        let dir = TempDir::new().unwrap();
        let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
        let tx = db.begin(&TxnOptions::new());
        tx.put(b"k", b"v").unwrap();
        db.db().close().unwrap();
        match tx.commit() {
            Err(TransactionError::Engine(Error::Closed)) => {}
            other => panic!("expected Engine(Closed), got {other:?}"),
        }
    }

    /// The closed-handle test above cannot tell whether `commit_optimistic`'s
    /// `ensure_writable` call runs before the stall admission, because a
    /// closed handle produces the identical error either way (`admit_write`
    /// also refuses a closed engine). A latched write-ahead-log failure under
    /// a stop is the one case that can: without the check before the
    /// admission, the commit would report the stall instead of the WAL
    /// error, and a caller would wait for a stall to clear on a database that
    /// can take no write at all. Regression for deleting that call.
    #[test]
    fn commit_on_a_wal_failed_handle_is_refused_before_the_stall() {
        let dir = TempDir::new().unwrap();
        let db = OptimisticTransactionDb::open(dir.path(), stop_opts()).unwrap();

        cross_stop(db.db());
        assert!(
            matches!(db.db().engine.stall_signal().classify(), Some((_, true))),
            "precondition: writes are stopped"
        );

        db.db()
            .engine
            .latch_wal_failure(&std::io::Error::other("injected"));

        let tx = db.begin(&TxnOptions::new());
        tx.put(b"txn", b"v").unwrap();
        match tx.commit() {
            Err(TransactionError::Engine(Error::Io(e))) => {
                assert!(
                    e.to_string()
                        .contains("write-ahead log left in an unknown state"),
                    "expected the WAL failure reason, got: {e}"
                );
            }
            other => panic!("expected an Engine(Io) WAL failure, got {other:?}"),
        }
    }

    // A scan's read set grows by yielded key, not by call: a key it
    // already tracked folds into the same cell, a buffered key it
    // yields is never tracked, and a bound key the walk stops on is
    // never handed out so it never reaches `tracked` either.
    #[test]
    fn a_scan_records_exactly_the_snapshot_keys_it_yields() {
        let (db, _dir) = opt_db();
        db.db().put(b"a", b"0").unwrap();
        db.db().put(b"b", b"0").unwrap();
        db.db().put(b"c", b"0").unwrap();
        let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));

        assert_eq!(tx.scan_stream(Some(b"x"), Some(b"y")).count(), 0);
        assert_eq!(tx.tracked.len(), 0, "an empty scan tracks nothing");

        assert_eq!(tx.scan_stream(Some(b"a"), Some(b"b")).count(), 1);
        assert_eq!(tx.tracked.len(), 1, "the bound key b is not recorded");

        tx.put(b"c", b"pending").unwrap();
        assert_eq!(tx.scan_stream(None, None).count(), 3);
        assert_eq!(
            tx.tracked.len(),
            2,
            "a folds into its existing cell, b is newly tracked, c came from \
             the write buffer and is not tracked"
        );

        let fresh = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
        assert_eq!(fresh.scan_stream(None, None).take(0).count(), 0);
        assert_eq!(
            fresh.tracked.len(),
            0,
            "a stream that yields nothing records nothing"
        );
        assert!(fresh.scan_runs.get().is_none());
    }

    /// Appends every operand to the base.
    struct Append;

    impl crate::MergeOperator for Append {
        fn name(&self) -> &'static str {
            "append"
        }

        fn full_merge(
            &self,
            _key: &[u8],
            base: Option<&[u8]>,
            operands: &[&[u8]],
        ) -> Option<Vec<u8>> {
            let mut out = base.unwrap_or_default().to_vec();
            out.extend(operands.concat());
            Some(out)
        }
    }

    // The base of a key the transaction merged into is a snapshot key like
    // any the scan yields: it joins the stretch and is recorded as a read only
    // where a scan records every key. `get` records it at every level.
    #[test]
    fn a_scan_reaching_a_merged_key_records_a_read_of_it_only_at_serializable() {
        for (level, recorded) in [
            (IsolationLevel::SnapshotIsolation, 0),
            (IsolationLevel::RepeatableRead, 0),
            (IsolationLevel::DefraLevel, 0),
            (IsolationLevel::Serializable, 1),
        ] {
            let dir = TempDir::new().unwrap();
            let opts = Options::default().merge_operator(Some(Arc::new(Append)));
            let db = OptimisticTransactionDb::open(dir.path(), opts).unwrap();
            db.db().put(b"k", b"base").unwrap();
            let tx = db.begin(&TxnOptions::new().isolation(level));
            tx.merge(b"k", b"+op").unwrap();

            let walked: Vec<(Vec<u8>, Vec<u8>)> = tx
                .scan_stream(None, None)
                .map(|item| {
                    let (key, value) = item.unwrap();
                    (key, value.to_vec())
                })
                .collect();
            assert_eq!(walked, [(b"k".to_vec(), b"base+op".to_vec())]);
            assert_eq!(tx.tracked.len(), recorded, "{level:?}");
            assert_eq!(
                tx.scan_runs.get().map(|runs| runs.len()),
                Some(1),
                "{level:?}: the merged key's base is a snapshot key and starts a stretch"
            );

            assert_eq!(tx.get(b"k").unwrap(), Some(b"base+op".to_vec()));
            assert_eq!(tx.tracked.len(), 1, "{level:?}: a get records the read");
        }
    }

    /// Regression: `scan_stream_in` used to index `tracked` only at the
    /// moment the stream was built, so a `get_for_update` promotion that
    /// landed after a stream was already open left every later yielded key
    /// walking the unindexed list instead of hitting the hash index.
    /// `scan_read_seq` now builds the index itself, lazily, the first time
    /// it finds a promotion, so the order `get_for_update` and
    /// `scan_stream` run in cannot matter.
    #[test]
    fn a_promotion_mid_scan_indexes_tracked_lazily_not_only_at_construction() {
        let dir = TempDir::new().unwrap();
        // `0` never indexes from ordinary inserts (`TxnBuffer::new`), so the
        // only thing that can index `tracked` in this test is the lazy
        // build `scan_read_seq` triggers, isolated from the unrelated
        // "past N keys" auto-index a bigger default would also trigger.
        let opts = Options::default().transaction_keys_inline(0);
        let db = TransactionDb::open(dir.path(), opts).unwrap();
        for i in 0..64u32 {
            db.db().put(format!("k{i:04}").as_bytes(), b"0").unwrap();
        }
        let tx = db.begin(&TxnOptions::new());

        let mut stream = tx.scan_stream(None, None);
        assert_eq!(stream.next().unwrap().unwrap().0, b"k0000".to_vec());
        assert!(
            !tx.tracked.is_indexed(),
            "nothing is promoted yet: scan_stream_in must not index speculatively"
        );

        // Lands after the begin snapshot; the promotion below must read
        // this, not the snapshot value, proving it is served at the lock
        // horizon like `get` and not accidentally read from the cursor.
        db.db().put(b"k0010", b"promoted").unwrap();
        assert_eq!(
            tx.get_for_update(b"k0010").unwrap(),
            Some(b"promoted".to_vec()),
            "get_for_update while the stream is open reads past the begin snapshot"
        );
        assert!(
            !tx.tracked.is_indexed(),
            "a promotion by itself must not index; the next scan lookup does"
        );

        // Draining the rest is exactly where the bug bit: every one of
        // these calls used to fall back to walking the list, O(1) here but
        // O(tracked keys) had more been promoted, because construction-time
        // indexing had already run and missed this promotion.
        let rest: Vec<(Vec<u8>, Vec<u8>)> = stream
            .map(|item| {
                let (k, v) = item.unwrap();
                (k, v.to_vec())
            })
            .collect();
        assert!(
            tx.tracked.is_indexed(),
            "scan_read_seq must index tracked lazily on first need, mid-stream, \
             not only when scan_stream_in constructed the stream"
        );
        assert_eq!(rest.len(), 63, "every remaining key is still yielded");
        let promoted = rest
            .iter()
            .find(|(k, _)| k == b"k0010")
            .expect("the promoted key is still yielded");
        assert_eq!(
            promoted.1,
            b"promoted".to_vec(),
            "yielded at the promoted read sequence, not the stale begin snapshot"
        );
    }
}
