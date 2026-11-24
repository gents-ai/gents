//! Per-transaction settings passed to `begin`.
//!
//! A [`TxnOptions`] is plain data: copying it is free and it holds no
//! reference to a database. Anything it leaves unset falls back to what the
//! database it is begun on is configured with.

use super::IsolationLevel;
use crate::{QueueId, ReadMode};

/// Settings for one transaction, passed to
/// [`OptimisticTransactionDb::begin`](super::OptimisticTransactionDb::begin) or
/// [`TransactionDb::begin`](super::TransactionDb::begin).
///
/// ```
/// use regolith::{IsolationLevel, TxnOptions};
///
/// let default = TxnOptions::new();
/// let serializable = TxnOptions::new().isolation(IsolationLevel::Serializable);
/// ```
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct TxnOptions {
    isolation: Option<IsolationLevel>,
    early_validation: bool,
    reading: Reading,
}

/// How a transaction reads the device, and the queue it belongs to.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Reading {
    /// Where its block-cache misses go.
    pub(super) mode: ReadMode,
    /// The queue named by [`TxnOptions::io_queue`].
    pub(super) queue: Option<QueueId>,
}

impl TxnOptions {
    /// Options that leave every setting at the database's configured value.
    pub const fn new() -> Self {
        Self {
            isolation: None,
            early_validation: false,
            reading: Reading {
                mode: ReadMode::Blocking,
                queue: None,
            },
        }
    }

    /// Begin the transaction at `isolation`, whatever the database default is.
    ///
    /// The level is a property of what a unit of work needs rather than of the
    /// database: a read-mostly query and a read-modify-write against the same
    /// database want different answers, and paying serializable validation for
    /// the former is waste. Unset, the transaction uses the level the database
    /// was configured with through `with_isolation`.
    pub const fn isolation(mut self, isolation: IsolationLevel) -> Self {
        self.isolation = Some(isolation);
        self
    }

    /// Check each write against newer versions as it is made, and fail the
    /// write at once with [`crate::TransactionError::Conflict`] when the commit
    /// could only lose, instead of at commit after the rest of the work is
    /// done. Off by default: it costs one probe of the key per write (a read
    /// of its value too, for a put or delete that is about to conflict).
    ///
    /// Only an optimistic transaction validates early; a pessimistic one
    /// ignores this, its locks already order its writes. The check is the
    /// commit's own check of one written key, so it never fails a write the
    /// commit would pass at that moment, with two limits that keep it from
    /// failing one that a later read would clear: a key this transaction has
    /// read, and every key once it has scanned, are left to the commit. A
    /// write that passes may still conflict at commit, because the key can
    /// change after the check, and one that fails may have passed had the
    /// transaction waited, because the newer version can be rewritten back.
    /// With a [`crate::KeyClassifier`] installed at
    /// [`IsolationLevel::DefraLevel`] the classifier is asked about each
    /// written key as well, and a put or merge of a content-addressed key is
    /// not checked.
    pub const fn early_validation(mut self, on: bool) -> Self {
        self.early_validation = on;
        self
    }

    /// Read in `mode`: [`ReadMode::Blocking`] by default.
    ///
    /// Under [`ReadMode::CacheOnly`] no read the transaction makes touches
    /// the device: [`Transaction::get`](super::Transaction::get),
    /// `get_slice`, `get_for_update`, `get_parts`, a cursor's
    /// [`next_page`](super::TxnCursor::next_page), a scan stream's next
    /// item, and the reads a merge needs underneath them each return
    /// [`crate::TransactionError::WouldBlock`] when they need a block the
    /// cache does not hold. Poll the queue it names and run the call again: a
    /// read run again returns what the first attempt would have, and a cursor
    /// or stream continues where it stopped, skipping and repeating nothing.
    ///
    /// The reads [`Transaction::commit`](super::Transaction::commit) makes,
    /// the `before_commit` callbacks' included, read the device whatever the
    /// mode: `commit` does its own I/O.
    ///
    /// A read that returns `WouldBlock` may already have recorded the key as
    /// read, the same record the read run again makes. A caller that gives
    /// the read up instead leaves that record behind, which can only add a
    /// conflict at commit, never hide one.
    pub const fn read_mode(mut self, mode: ReadMode) -> Self {
        self.reading.mode = mode;
        self
    }

    /// Name the queue this transaction's completions belong to.
    ///
    /// Recorded on the transaction and reported by
    /// [`Transaction::io_queue`](super::Transaction::io_queue).
    /// [`Transaction::commit_nowait`](super::Transaction::commit_nowait)
    /// delivers its [`CommitTicket`](crate::CommitTicket) through this
    /// queue: the ticket completes only at this queue's poll, on the thread
    /// that owns it. Without one the commit syncs inline and hands back a
    /// ticket that is already complete. Reads that miss the cache go to the
    /// queue their [`read_mode`](Self::read_mode) names.
    pub const fn io_queue(mut self, queue: QueueId) -> Self {
        self.reading.queue = Some(queue);
        self
    }

    /// Whether writes are checked as they are made.
    pub(super) fn early(&self) -> bool {
        self.early_validation
    }

    /// The read mode and queue the transaction takes.
    pub(super) fn reading(&self) -> Reading {
        self.reading
    }

    /// The level this transaction runs at, given the database's configured
    /// `default`. The one place both database flavors resolve it.
    pub(super) fn resolve_isolation(&self, default: IsolationLevel) -> IsolationLevel {
        self.isolation.unwrap_or(default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_isolation_takes_the_database_default() {
        let opts = TxnOptions::new();
        for default in [
            IsolationLevel::ReadCommitted,
            IsolationLevel::SnapshotIsolation,
            IsolationLevel::Serializable,
            IsolationLevel::DefraLevel,
        ] {
            assert_eq!(opts.resolve_isolation(default), default);
        }
    }

    #[test]
    fn explicit_isolation_overrides_the_database_default() {
        let opts = TxnOptions::new().isolation(IsolationLevel::Serializable);
        assert_eq!(
            opts.resolve_isolation(IsolationLevel::ReadCommitted),
            IsolationLevel::Serializable
        );
    }

    #[test]
    fn early_validation_is_off_until_asked_for() {
        assert!(!TxnOptions::new().early());
        assert!(TxnOptions::new().early_validation(true).early());
        assert!(
            !TxnOptions::new()
                .early_validation(true)
                .early_validation(false)
                .early()
        );
        assert!(!TxnOptions::default().early());
    }

    #[test]
    fn default_is_new() {
        assert_eq!(
            TxnOptions::default().resolve_isolation(IsolationLevel::RepeatableRead),
            TxnOptions::new().resolve_isolation(IsolationLevel::RepeatableRead)
        );
        assert_eq!(TxnOptions::default().reading().mode, ReadMode::Blocking);
        assert_eq!(TxnOptions::new().reading().mode, ReadMode::Blocking);
        assert_eq!(TxnOptions::new().reading().queue, None);
    }

    #[test]
    fn reads_block_until_told_otherwise() {
        let queue = crate::engine::io::shared::test_id();
        let opts = TxnOptions::new().read_mode(ReadMode::CacheOnly(queue));
        assert_eq!(opts.reading().mode, ReadMode::CacheOnly(queue));
        assert_eq!(opts.reading().queue, None);
        let opts = TxnOptions::new().io_queue(queue);
        assert_eq!(opts.reading().mode, ReadMode::Blocking);
        assert_eq!(opts.reading().queue, Some(queue));
    }
}
