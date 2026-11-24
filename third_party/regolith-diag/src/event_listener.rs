//! Lifecycle event callbacks for flush, compaction, ingest, and transaction
//! conflicts.
//!
//! Callers register one or more [`EventListener`] implementations via
//! [`crate::Options::listeners`] to react to engine lifecycle events.
//! Typical uses: metrics pipelines (emit a span per flush), debugging
//! (log which files compaction picked), test harnesses (wait on a
//! callback instead of sleeping), and operational triggers (upload
//! the freshly-flushed SSTable to object storage).
//!
//! # Dispatch
//!
//! Events are dispatched **synchronously** on the thread that
//! triggered them - flush events on the thread that ran the flush (a
//! writer, or an ingest writing out the memtables written before it),
//! compaction events on the compaction thread, ingest events on
//! the ingest caller's thread, conflict events on the thread whose commit
//! lost the race, once the commit has released its locks. Listeners
//! **MUST NOT block** or re-enter the database. The contract is "do a
//! cheap thing or spawn a task."
//! Blocking inside a listener stalls the engine and will starve
//! background compaction / flush pipelines.
//!
//! # Non-guarantees
//!
//! - Listener order is unspecified. Multiple listeners registered
//!   in the same `Options::listeners` vector see events in vector
//!   order, but a caller that cares about sequencing should own
//!   its own fan-out.
//! - Per-column-family filtering is out of scope. A listener sees
//!   every event from every CF; callers that only care about a
//!   subset should filter in the callback.

use std::path::PathBuf;
use std::time::Duration;

use crate::{Conflict, Error};

/// Why the flush / compaction path chose to produce a file, used
/// by [`TableFileCreationInfo::reason`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableFileCreationReason {
    /// A memtable was flushed to L0.
    Flush,
    /// A compaction merged files at one level into another.
    Compaction,
    /// A file was ingested via [`crate::Db::ingest_external_files`].
    Recovery,
}

/// Information about a flush that just completed.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FlushJobInfo {
    /// Numeric id of the SSTable that now holds the flushed
    /// memtable's contents.
    pub file_id: u64,
    /// Path of the produced SSTable.
    pub file_path: PathBuf,
    /// File size on disk, in bytes.
    pub file_size: u64,
    /// Number of point entries in the flushed SSTable.
    pub num_entries: u64,
    /// Lower bound of the file in storage order. A table holds entries of
    /// every column family, so this is not a key as the caller wrote it:
    /// it is the 4-byte big-endian id of a column family, then the
    /// caller's key in that family. Not a key to read back with.
    pub smallest_key: Vec<u8>,
    /// Upper bound of the file in storage order, in the form of
    /// [`FlushJobInfo::smallest_key`].
    pub largest_key: Vec<u8>,
    /// Wall-clock duration of the flush, from memtable rotation
    /// through manifest apply.
    /// Zero on a platform whose [`crate::env::Env`] has no
    /// monotonic clock, where nothing was measured.
    pub duration: Duration,
}

/// Information about a compaction job, passed to
/// [`EventListener::on_compaction_begin`] and
/// [`EventListener::on_compaction_completed`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CompactionJobInfo {
    /// Source level (the level compaction is reading from).
    pub input_level: usize,
    /// Destination level (one deeper than `input_level`).
    pub output_level: usize,
    /// File ids of the inputs at `input_level` (the "L" files).
    pub input_files_input_level: Vec<u64>,
    /// File ids of the inputs at `output_level` that overlap the
    /// input range (the "L+1" files that get merged in).
    pub input_files_output_level: Vec<u64>,
    /// File ids of the newly-produced output files at
    /// `output_level`. Populated on
    /// [`EventListener::on_compaction_completed`]; empty on
    /// [`EventListener::on_compaction_begin`].
    pub output_files: Vec<u64>,
    /// Wall-clock duration of the compaction. Zero on begin, and
    /// zero on a platform whose [`crate::env::Env`] has no monotonic
    /// clock, where nothing was measured.
    pub duration: Duration,
}

/// Information about a freshly-created SSTable file. Fires for
/// both flush output and compaction output, distinguished by
/// [`TableFileCreationReason`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TableFileCreationInfo {
    /// Numeric id of the SSTable.
    pub file_id: u64,
    /// Path of the created file.
    pub file_path: PathBuf,
    /// Level the file was placed at.
    pub level: usize,
    /// Reason the file was produced.
    pub reason: TableFileCreationReason,
    /// File size on disk.
    pub file_size: u64,
    /// Number of point entries in the file.
    pub num_entries: u64,
}

/// Information about an SSTable file a compaction just removed. Fires
/// after the compaction has committed the version edit that took the file
/// out of the live set and has removed the file's name. The removal is
/// best effort: one that fails is logged and left for the next open to
/// sweep, and the listener is told all the same.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TableFileDeletionInfo {
    /// Numeric id of the unlinked file.
    pub file_id: u64,
    /// Path of the unlinked file at the time of deletion.
    pub file_path: PathBuf,
}

/// Information about a file ingested via
/// [`crate::Db::ingest_external_files`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ExternalFileIngestionInfo {
    /// Path the caller supplied to `ingest_external_files`.
    pub external_file_path: PathBuf,
    /// Internal file id assigned by the engine.
    pub internal_file_id: u64,
    /// Level the ingested file was placed at.
    pub level: usize,
    /// Total entries in the ingested file (point + range deletes).
    pub num_entries: u64,
    /// Size of the re-emitted file on disk.
    pub file_size: u64,
}

/// A discarded end of the newest write-ahead log, passed to
/// [`EventListener::on_wal_tail_discarded`].
///
/// Opening a database replays its write-ahead logs. When the newest one
/// holds a record that is torn, zeroed or garbled, and no surviving record
/// proves that record was synced, everything from it to the end of the
/// file is dropped: under [`crate::DurabilityMode::Immediate`] no
/// acknowledged write is among it, and under
/// [`crate::DurabilityMode::Eventual`] the database keeps a gap-free
/// prefix of the writes, up to `last_sequence`. Damage to bytes a sync
/// had made durable, bit rot or a torn write over a sector already
/// written, can read the same way; reporting every discard is what keeps
/// that case from passing silently.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WalTailDiscardedInfo {
    /// The log the bytes were dropped from.
    pub file_path: PathBuf,
    /// Offset of the first byte dropped: where the first unusable record
    /// starts.
    pub offset: u64,
    /// How many bytes were dropped, through the end of the file.
    pub discarded_bytes: u64,
    /// The newest sequence number the open kept, across every log and
    /// table. Every write at or below it that was in the logs survived.
    pub last_sequence: u64,
}

/// Reason passed to [`EventListener::on_background_error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BackgroundErrorReason {
    /// A background flush (memtable → L0) failed.
    Flush,
    /// A background compaction failed.
    Compaction,
    /// A manifest write or manifest compaction failed.
    Manifest,
    /// A WAL append or WAL sync failed.
    WriteAheadLog,
}

/// Trait implemented by callers that want to react to engine
/// lifecycle events. Registered via [`crate::Options::listeners`].
///
/// Every method has a default empty body, so implementations only
/// override the callbacks they care about.
///
/// **Listeners must not block or re-enter the database.** See the
/// module-level docs for the dispatch contract.
///
/// A panic in this trait's code while a commit's memtable rotation runs it
/// fails that commit with [`crate::Error::CallbackPanicked`] and latches the
/// database read-only until it is reopened.
pub trait EventListener: Send + Sync + 'static {
    /// Called after a memtable has been flushed to a new L0
    /// SSTable and the manifest edit has been applied.
    fn on_flush_completed(&self, info: &FlushJobInfo) {
        let _ = info;
    }

    /// Called right before a compaction job starts reading its
    /// input files. The `output_files` field is empty at this
    /// point; it's populated on `on_compaction_completed`.
    fn on_compaction_begin(&self, info: &CompactionJobInfo) {
        let _ = info;
    }

    /// Called after a compaction job has written its output files
    /// and applied the manifest edit.
    fn on_compaction_completed(&self, info: &CompactionJobInfo) {
        let _ = info;
    }

    /// Called when a new SSTable file is created. Fires once per
    /// file for both flush and compaction paths, with
    /// `info.reason` distinguishing the caller.
    fn on_table_file_created(&self, info: &TableFileCreationInfo) {
        let _ = info;
    }

    /// Called after a compaction took an SSTable file out of the live
    /// set (the manifest edit is applied) and removed the file's name;
    /// see [`TableFileDeletionInfo`] for a removal that fails.
    fn on_table_file_deleted(&self, info: &TableFileDeletionInfo) {
        let _ = info;
    }

    /// Called per file inside
    /// [`crate::Db::ingest_external_files`], once the ingested
    /// file has been placed at a level and committed to the
    /// manifest.
    fn on_external_file_ingested(&self, info: &ExternalFileIngestionInfo) {
        let _ = info;
    }

    /// Called when a background flush / compaction / manifest /
    /// WAL operation returns an error. The engine keeps running -
    /// the listener is for observability, not error handling.
    fn on_background_error(&self, reason: BackgroundErrorReason, err: &Error) {
        let _ = (reason, err);
    }

    /// Called once for each transaction commit that lost a race, on the
    /// thread that committed, after the commit released its locks. The
    /// [`Conflict`] says what the transaction did with the key and what the
    /// newer write was, so a caller can count the reasons in its own
    /// telemetry. A [`crate::OptimisticTransactionDb::transact`] retry that
    /// conflicts again calls it again.
    ///
    /// # Example
    ///
    /// A listener that counts lost commits, apart from those that lost to a
    /// delete: a caller usually settles those as "already gone".
    ///
    /// ```
    /// use std::sync::Arc;
    /// use std::sync::atomic::{AtomicU64, Ordering};
    ///
    /// use regolith::{
    ///     Conflict, EventListener, OptimisticTransactionDb, Options, TransactionError, TxnOptions,
    ///     WriteKind,
    /// };
    ///
    /// #[derive(Default)]
    /// struct Lost {
    ///     to_a_delete: AtomicU64,
    ///     to_a_write: AtomicU64,
    /// }
    ///
    /// impl EventListener for Lost {
    ///     // Counts and returns: a listener must not block or call the database.
    ///     fn on_conflict(&self, conflict: &Conflict) {
    ///         let counter = match conflict.theirs() {
    ///             WriteKind::Delete | WriteKind::RangeDelete => &self.to_a_delete,
    ///             _ => &self.to_a_write,
    ///         };
    ///         counter.fetch_add(1, Ordering::Relaxed);
    ///     }
    /// }
    ///
    /// let lost = Arc::new(Lost::default());
    /// let options = Options::memory().listeners(vec![lost.clone()]);
    /// let db = OptimisticTransactionDb::open("rows", options)?;
    /// db.db().put(b"row", b"1")?;
    ///
    /// // The row is deleted between this transaction's read and its commit.
    /// let txn = db.begin(&TxnOptions::new());
    /// txn.get_for_update(b"row")?;
    /// db.db().delete(b"row")?;
    /// txn.put(b"row", b"2")?;
    ///
    /// // The commit returns the conflict, and the listener was told once.
    /// assert!(matches!(txn.commit(), Err(TransactionError::Conflict(_))));
    /// assert_eq!(lost.to_a_delete.load(Ordering::Relaxed), 1);
    /// assert_eq!(lost.to_a_write.load(Ordering::Relaxed), 0);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    fn on_conflict(&self, conflict: &Conflict) {
        let _ = conflict;
    }

    /// Called when a transaction callback panics after its transaction's
    /// outcome was decided: `callback` is `"on_commit"`, `"on_abort"`,
    /// `"TransactionHooks::on_commit"` or `"TransactionHooks::on_abort"`.
    /// The panic is caught, the outcome stands, and the transaction's other
    /// callbacks still run. Called on the thread that completed the outcome.
    fn on_callback_panic(&self, callback: &'static str) {
        let _ = callback;
    }

    /// Called while a database opens, once, when replay dropped the end of
    /// the newest write-ahead log (see [`WalTailDiscardedInfo`]). By then a
    /// read-write open has already truncated the bytes away durably; a
    /// read-only open leaves them, and reports them again on the next open.
    /// Called on the opening thread, before the open returns; a panic here
    /// fails the open.
    fn on_wal_tail_discarded(&self, info: &WalTailDiscardedInfo) {
        let _ = info;
    }
}

/// Dispatch a closure over every listener in a slice. Silences
/// the trivial fan-out boilerplate at every call site. `Arc` is
/// cheap enough to clone through the iterator.
pub(crate) fn dispatch<F>(listeners: &[std::sync::Arc<dyn EventListener>], f: F)
where
    F: Fn(&dyn EventListener),
{
    for l in listeners {
        f(l.as_ref());
    }
}

/// [`dispatch`] for a call that can run inside a commit's ordered step: there
/// a listener's panic is caught and returned as
/// [`Error::CallbackPanicked`](crate::Error::CallbackPanicked), and the
/// listeners after it are not told.
pub(crate) fn dispatch_contained<F>(
    listeners: &[std::sync::Arc<dyn EventListener>],
    f: F,
) -> Result<(), crate::Error>
where
    F: Fn(&dyn EventListener),
{
    listeners
        .iter()
        .try_for_each(|l| crate::engine::callback::contain("EventListener", || f(l.as_ref())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Access, WriteKind};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Listener that counts every callback it receives. Used to
    /// verify that `dispatch` reaches every registered listener
    /// and that the default no-op implementations don't panic.
    #[derive(Default)]
    struct CountingListener {
        flushes: AtomicUsize,
        compactions: AtomicUsize,
        files_created: AtomicUsize,
        files_deleted: AtomicUsize,
        ingests: AtomicUsize,
        bg_errors: AtomicUsize,
    }

    impl EventListener for CountingListener {
        fn on_flush_completed(&self, _: &FlushJobInfo) {
            self.flushes.fetch_add(1, Ordering::Relaxed);
        }
        fn on_compaction_completed(&self, _: &CompactionJobInfo) {
            self.compactions.fetch_add(1, Ordering::Relaxed);
        }
        fn on_table_file_created(&self, _: &TableFileCreationInfo) {
            self.files_created.fetch_add(1, Ordering::Relaxed);
        }
        fn on_table_file_deleted(&self, _: &TableFileDeletionInfo) {
            self.files_deleted.fetch_add(1, Ordering::Relaxed);
        }
        fn on_external_file_ingested(&self, _: &ExternalFileIngestionInfo) {
            self.ingests.fetch_add(1, Ordering::Relaxed);
        }
        fn on_background_error(&self, _: BackgroundErrorReason, _: &Error) {
            self.bg_errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn sample_flush() -> FlushJobInfo {
        FlushJobInfo {
            file_id: 1,
            file_path: PathBuf::from("/tmp/x.sst"),
            file_size: 1024,
            num_entries: 10,
            smallest_key: b"a".to_vec(),
            largest_key: b"z".to_vec(),
            duration: Duration::from_millis(5),
        }
    }

    fn sample_compaction() -> CompactionJobInfo {
        CompactionJobInfo {
            input_level: 0,
            output_level: 1,
            input_files_input_level: vec![1, 2],
            input_files_output_level: vec![3],
            output_files: vec![4],
            duration: Duration::from_millis(20),
        }
    }

    #[test]
    fn default_trait_impls_are_noop() {
        struct NoOp;
        impl EventListener for NoOp {}
        let n = NoOp;
        n.on_flush_completed(&sample_flush());
        n.on_compaction_begin(&sample_compaction());
        n.on_compaction_completed(&sample_compaction());
        n.on_conflict(&Conflict::new(
            b"k".to_vec(),
            Access::Read,
            WriteKind::Put,
            1,
            2,
        ));
        // No panic reaching here is the assertion.
    }

    #[test]
    fn dispatch_visits_every_listener() {
        let a = Arc::new(CountingListener::default());
        let b = Arc::new(CountingListener::default());
        let listeners: Vec<Arc<dyn EventListener>> = vec![
            a.clone() as Arc<dyn EventListener>,
            b.clone() as Arc<dyn EventListener>,
        ];

        let info = sample_flush();
        dispatch(&listeners, |l| l.on_flush_completed(&info));
        assert_eq!(a.flushes.load(Ordering::Relaxed), 1);
        assert_eq!(b.flushes.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn dispatch_on_empty_list_is_noop() {
        let empty: Vec<Arc<dyn EventListener>> = Vec::new();
        dispatch(&empty, |_| panic!("should not be called"));
    }

    #[test]
    fn table_file_creation_reason_variants_are_equal_only_to_themselves() {
        assert_eq!(
            TableFileCreationReason::Flush,
            TableFileCreationReason::Flush
        );
        assert_ne!(
            TableFileCreationReason::Flush,
            TableFileCreationReason::Compaction
        );
        assert_ne!(
            TableFileCreationReason::Compaction,
            TableFileCreationReason::Recovery
        );
    }

    #[test]
    fn background_error_reason_copy_and_eq() {
        let r = BackgroundErrorReason::Flush;
        let copy = r; // Copy
        assert_eq!(r, copy);
        assert_ne!(r, BackgroundErrorReason::Compaction);
    }

    #[test]
    fn info_structs_are_cloneable_and_debug_formattable() {
        // Sanity check that every public info struct carries
        // Clone + Debug so telemetry callers can snapshot them.
        let f = sample_flush();
        let _ = format!("{f:?}");
        let _ = f.clone();
        let c = sample_compaction();
        let _ = format!("{c:?}");
        let _ = c.clone();
        let ext = ExternalFileIngestionInfo {
            external_file_path: PathBuf::from("/x"),
            internal_file_id: 1,
            level: 0,
            num_entries: 1,
            file_size: 2,
        };
        let _ = format!("{ext:?}");
        let _ = ext.clone();
    }
}
