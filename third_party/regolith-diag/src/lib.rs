#![doc(
    html_logo_url = "https://raw.githubusercontent.com/sourcenetwork/regolith/main/art/regolith_banner_2048x512.png"
)]
//! Regolith: ACID, performance oriented, embedded key-value database engine for edge systems.
//!
//! Regolith provides a fast, embedded key-value store with:
//! - **Read committed, snapshot isolation, repeatable read, serializable, or
//!   DefraLevel** per transaction, via MVCC sequence numbers
//! - **Shareable transactions** whose reads and writes take `&self`, so one
//!   transaction can be used from several threads. An optimistic transaction
//!   takes no key locks; every commit passes through one write pipeline
//! - **Crash recovery** via write-ahead logging (WAL)
//! - **LZ4 compression** for data blocks
//! - **Bloom filters** for fast negative lookups
//! - **Level-based compaction** on background worker threads, or inline on a
//!   target that has none
//! - **Reads that take no lock**: the published view of memtables and tables
//!   is loaded wait-free, the arena-backed skip list memtable and its range
//!   tombstones are walked without a lock, and the CLOCK block cache admits,
//!   evicts and serves blocks by compare-and-swap, never evicting a block a
//!   reader holds
//! - **Zero-copy reads** via [`DbSlice`], which borrows the bytes the
//!   database already holds
//!
//! # Quick Start
//!
//! ```
//! use regolith::{Db, Options};
//!
//! // In memory, so the example runs anywhere. With `Options::default()`
//! // the database lives in the directory named here.
//! let db = Db::open("my_db", Options::memory()).unwrap();
//!
//! // Write
//! db.put(b"hello", b"world").unwrap();
//!
//! // Read
//! let value = db.get(b"hello").unwrap();
//! assert_eq!(value, Some(b"world".to_vec()));
//!
//! // Delete
//! db.delete(b"hello").unwrap();
//!
//! // Batch write
//! let mut batch = regolith::WriteBatch::new();
//! batch.put(b"key1", b"val1");
//! batch.put(b"key2", b"val2");
//! batch.delete(b"key3");
//! db.write(batch).unwrap();
//!
//! // Snapshot reads
//! let snap = db.snapshot();
//! db.put(b"key1", b"val_new").unwrap();
//! // Snapshot still sees old value
//! assert_eq!(snap.get(b"key1").unwrap(), Some(b"val1".to_vec()));
//! ```

// `unsafe` is confined to the modules that need raw pointers to hand
// out zero-copy views of bytes the engine already owns. Every other
// module inherits the crate-level deny.
#![deny(unsafe_code)]
#![warn(missing_docs)]

mod allocate;
mod backup;
mod checkpoint;
mod column_family;
mod conflict;
mod encryption;
mod engine;
pub mod env;
mod error;
mod event_listener;
mod io_queue;
mod iter;
mod log_layout;
mod options;
mod per_thread;
mod perf_context;
mod portability;
mod rate_limiter;
mod slice;
mod snapshot_wait;
mod sst_file_writer;
mod statistics;
mod stream_writer;
pub mod sync;
mod tailing;
#[cfg(all(feature = "testing", not(target_family = "wasm")))]
pub mod testing;
mod transaction;
mod ttl;
mod txn_buffer;

pub use backup::{BackupEngine, BackupId, BackupInfo, Backups, UnreadableBackup};
pub use checkpoint::Checkpoint;
pub use column_family::{ColumnFamilyHandle, DEFAULT_CF_NAME};
pub use conflict::{Access, Conflict, WriteKind};
pub use encryption::{KeyId, KeyMaterial, KeyProvider};
pub use engine::compaction::CompactionOutcome;
#[cfg(target_os = "wasi")]
pub use env::WasiEnv;
pub use env::{Capabilities, Env, MemEnv, StdEnv};
pub use error::Error;
pub use event_listener::{
    BackgroundErrorReason, CompactionJobInfo, EventListener, ExternalFileIngestionInfo,
    FlushJobInfo, TableFileCreationInfo, TableFileCreationReason, TableFileDeletionInfo,
    WalTailDiscardedInfo,
};
#[cfg(not(target_arch = "wasm32"))]
pub use io_queue::through_stalls;
pub use io_queue::{
    CommitTicket, IoBudget, IoProgress, IoQueue, IoUnit, IoWait, JobTicket, LockWait, QueueId,
    ReadMode, StallWait, WouldBlock,
};
pub use iter::Iter;
pub use log_layout::LogLayout;
pub use options::{
    ArenaProfile, CompactionDecision, CompactionFilter, CompactionStyle, CompressionType,
    DEFAULT_MAX_BACKGROUND_COMPACTIONS, DEFAULT_MAX_KEY_SIZE, DEFAULT_MAX_VALUE_SIZE,
    DEFAULT_TRANSACTION_KEYS_INLINE, DurabilityMode, FifoCompactionOptions, FixedLengthPrefix,
    MAX_BLOCK_CACHE_SHARD_BITS, MAX_BLOOM_BITS_PER_KEY, MergeOperator, Options, PrefixExtractor,
    UniversalCompactionOptions, WriteOptions,
};
pub use perf_context::{PerfContext, PerfContextSnapshot, PerfLevel};
pub use rate_limiter::{Priority, RateLimiter, TokenBucketRateLimiter};
pub use slice::DbSlice;
pub use snapshot_wait::SnapshotWait;
pub use sst_file_writer::{IngestOptions, SstFileMeta, SstFileWriter};
pub use statistics::{Histogram, HistogramSnapshot, Statistics, StatsLevel, Ticker};
pub use stream_writer::{StreamOptions, StreamingWriter};
pub use tailing::TailingIter;
pub use transaction::{
    AbortReason, CommitInfo, CommitReceipt, IsolationLevel, KeyClass, KeyClassifier,
    OptimisticTransactionDb, Page, RetryPolicy, ScanCheck, ScanDirection, TransactError,
    Transaction, TransactionDb, TransactionError, TransactionHooks, TxResult, TxnCursor,
    TxnOptions, TxnScanStream, ready,
};

/// The transactional API and the traits a caller implements, in one import:
/// `use regolith::prelude::*;`.
///
/// Every trait a caller implements is here ([`KeyClassifier`],
/// [`MergeOperator`], [`LogLayout`], [`TransactionHooks`], [`EventListener`],
/// [`KeyProvider`], [`CompactionFilter`], [`Env`]), with the types its
/// required methods name. The file traits an [`Env`] hands out stay in
/// [`crate::env`].
pub mod prelude {
    pub use crate::{
        AbortReason, CommitInfo, CommitReceipt, CommitTicket, CompactionDecision, CompactionFilter,
        Conflict, Db, Env, EventListener, IoBudget, IoQueue, IsolationLevel, JobTicket, KeyClass,
        KeyClassifier, KeyId, KeyMaterial, KeyProvider, LockWait, LogLayout, MergeOperator,
        OptimisticTransactionDb, Options, QueueId, ReadMode, RetryPolicy, StallWait, TransactError,
        Transaction, TransactionError, TransactionHooks, TxResult, TxnOptions, WouldBlock,
    };
}
pub use ttl::{DbWithTtl, TtlCompactionFilter, strip_timestamp};

#[cfg(loom)]
#[doc(hidden)]
pub mod loom_exports {
    //! Model-checking entry points, published only in a `--cfg loom`
    //! build.
    //!
    //! `tests/loom_memtable.rs` is an integration test and so sees only
    //! the public API, while everything the models drive - the arena,
    //! the skip list, the memtable, the read horizon - is crate-private.
    //! The models therefore live inside the crate, next to the code they
    //! check, and this module is the seam that lets the test target call
    //! them. It does not exist in an ordinary build.

    pub use crate::engine::commit::loom_model as pipeline;
    pub use crate::engine::commit::loom_turn_wait as turn_wait;
    pub use crate::engine::commit::loom_write_unit as write_unit;
    pub use crate::engine::loom_model::{
        arena, families, gate, handoff, io_queue, jobs, park, skiplist, slice, snapshots, step,
        tombstones, version,
    };
    pub use crate::engine::open_transactions::loom_model as claim_slots;
    pub use crate::env::open_file_limit::loom_model as open_files;
    pub use crate::sync::loom_model as sync;
    pub use crate::transaction::lock_loom as locks;
}

#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub mod fuzzing {
    //! Fuzz-only entry points for private on-disk decoders.
    //!
    //! These helpers intentionally swallow decoder results: fuzz targets
    //! care that arbitrary bytes never panic or trigger undefined behavior.

    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    /// Decode arbitrary bytes as an SSTable data block.
    pub fn decode_block(data: &[u8]) {
        let _ = crate::engine::block::Block::decode_data_block(data.to_vec());
    }

    /// Decode arbitrary bytes as an SSTable range-tombstone block.
    pub fn decode_range_tombstones(data: &[u8]) {
        let _ = crate::engine::sstable::decode_range_tombstone_block(data);
    }

    /// Replay arbitrary bytes as a WAL file.
    pub fn replay_wal(data: &[u8]) {
        with_temp_file("wal", "log", data, |path| {
            let Ok(mut iter) = crate::engine::wal_replay::WalReplayIter::open(
                &crate::env::std_env(),
                path,
                crate::engine::wal_replay::WalPosition::Newest,
                None,
            ) else {
                return;
            };
            while matches!(iter.next_entry(), Ok(Some(_))) {}
        });
    }

    /// Open arbitrary bytes as a complete SSTable file.
    pub fn open_sst(data: &[u8]) {
        with_temp_file("sst", "sst", data, |path| {
            // `open_with`, not `open`: the latter is `#[cfg(test)]`, so under
            // the `fuzzing` feature it does not exist and the crate does not build.
            let _ = crate::engine::sstable::SsTableReader::open_with(
                &crate::env::std_env(),
                path,
                0,
                crate::engine::sstable::MetadataPolicy::Pinned,
                None,
            );
        });
    }

    /// Replay arbitrary bytes as a MANIFEST file.
    pub fn replay_manifest(data: &[u8]) {
        with_temp_dir("manifest", |db_dir| {
            let sst_dir = db_dir.join("sst");
            let manifest_path = db_dir.join("MANIFEST");
            if fs::create_dir_all(&sst_dir).is_ok() && fs::write(&manifest_path, data).is_ok() {
                let _ = crate::engine::manifest::VersionSet::open(db_dir, &sst_dir);
            }
        });
    }

    fn with_temp_file(label: &str, extension: &str, data: &[u8], f: impl FnOnce(&Path)) {
        let path = temp_path(label).with_extension(extension);
        if fs::write(&path, data).is_ok() {
            f(&path);
        }
        let _ = fs::remove_file(path);
    }

    fn with_temp_dir(label: &str, f: impl FnOnce(&Path)) {
        let path = temp_path(label);
        if fs::create_dir_all(&path).is_ok() {
            f(&path);
        }
        let _ = fs::remove_dir_all(path);
    }

    fn temp_path(label: &str) -> PathBuf {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("regolith-fuzz-{label}-{}-{id}", std::process::id()))
    }
}

use column_family::{
    CfRegistry, DEFAULT_CF_ID, META_CF_ID, cf_lower_bound, cf_upper_bound, meta, prefix_key,
};
use engine::lookup_key::LookupKey;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use engine::RegolithEngine;

fn invalid_cf_handle_error(cf: &ColumnFamilyHandle) -> Error {
    column_family::invalid_handle_error(cf)
}

fn invalid_input_error(message: impl Into<String>) -> Error {
    Error::invalid_argument(message)
}

fn invalid_cf_id_error(cf_id: u32) -> Error {
    column_family::dropped_family_error(cf_id)
}

fn invalid_cf_id_io_error(cf_id: u32) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("column family id {cf_id} is not live"),
    )
}

fn map_point_read_error(err: std::io::Error, prefixed_key: &[u8]) -> Error {
    let user_key = prefixed_key.get(4..).unwrap_or(prefixed_key);
    map_point_read_error_for(err, user_key)
}

/// [`map_point_read_error`] for a caller that already holds the user
/// key. Stripping the column-family prefix a second time would eat
/// four bytes of the key itself and report a truncated one, so the
/// strip lives in exactly one place and both entries share the
/// decision below.
fn map_point_read_error_for(err: std::io::Error, user_key: &[u8]) -> Error {
    if err.kind() == std::io::ErrorKind::InvalidData
        && err.to_string().starts_with("merge operator ")
    {
        Error::MergeFailed(user_key.to_vec())
    } else {
        Error::from(err)
    }
}

fn strip_cf_prefix_key(key: &[u8]) -> Result<Vec<u8>> {
    key.get(4..)
        .map(|user_key| user_key.to_vec())
        .ok_or_else(|| Error::corruption("internal key is shorter than the column-family prefix"))
}

fn strip_cf_prefix_entries(raw: Vec<(Vec<u8>, Vec<u8>)>) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    raw.into_iter()
        .map(|(k, v)| strip_cf_prefix_key(&k).map(|user_key| (user_key, v)))
        .collect()
}

/// One bounded page of ordered scan results.
///
/// Returned by [`Db::scan_page`], [`Db::scan_page_cf`],
/// [`Snapshot::scan_page`], and [`Snapshot::scan_page_cf`] when callers
/// want an explicit memory cap without manually driving an iterator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanPage {
    /// Key-value pairs returned for this page, ordered by key.
    pub entries: Vec<(Vec<u8>, Vec<u8>)>,
    /// Inclusive start key to pass to the next page request, or
    /// `None` when the requested range has been exhausted.
    ///
    /// This key is the first matching key that was not returned in
    /// [`ScanPage::entries`].
    pub next_start: Option<Vec<u8>>,
}

/// Whether the column family a prefixed key names is live in `cfs`: the one
/// check a write's keys and an ingested file's keys share.
fn check_prefixed_cf(cfs: &CfRegistry, prefixed_key: &[u8]) -> std::io::Result<()> {
    let cf_id = prefixed_cf_id(prefixed_key)?;
    if cfs.contains_id(cf_id) {
        Ok(())
    } else {
        Err(invalid_cf_id_io_error(cf_id))
    }
}

fn prefixed_cf_id(prefixed_key: &[u8]) -> std::io::Result<u32> {
    let prefix = prefixed_key.get(..4).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "prefixed key is shorter than the column-family id",
        )
    })?;
    let mut bytes = [0; 4];
    bytes.copy_from_slice(prefix);
    Ok(u32::from_be_bytes(bytes))
}

/// Minimal snapshot of the currently-configured options, returned
/// by `Db::get_property("regolith.options")`. Deliberately small -
/// regolith doesn't retain the full `Options` past `Db::open`, and
/// the Debug impl of this struct is the property's string value.
#[derive(Debug)]
#[allow(dead_code)]
struct OptionsSnapshot {
    durability: engine::DurabilityMode,
    default_cf: &'static str,
    read_only: bool,
    max_key_size: usize,
    max_value_size: usize,
    transaction_keys_inline: usize,
}

/// Format a raw engine key for inclusion in a property string.
/// Internal keys in regolith carry a 4-byte CF prefix; if the key
/// is long enough we strip it and ASCII-escape the remainder.
/// Anything non-printable (or keys too short to strip) falls
/// back to a hex rendering so the output stays single-line.
fn format_key_for_display(key: &[u8]) -> String {
    let payload = if key.len() > 4 { &key[4..] } else { key };
    if payload.iter().all(|&b| b.is_ascii_graphic() || b == b' ') {
        format!("\"{}\"", String::from_utf8_lossy(payload))
    } else {
        let hex: String = payload.iter().map(|b| format!("{b:02x}")).collect();
        format!("0x{hex}")
    }
}

/// A half-open key range `[start, end)` passed to the approximate-size
/// APIs. Borrowed; cheap to construct inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range<'a> {
    /// Inclusive lower bound.
    pub start: &'a [u8],
    /// Exclusive upper bound.
    pub end: &'a [u8],
}

impl<'a> Range<'a> {
    /// Construct a new `[start, end)` range.
    pub fn new(start: &'a [u8], end: &'a [u8]) -> Self {
        Self { start, end }
    }
}

/// Approximate memtable stats returned by
/// [`Db::get_approximate_memtable_stats`]. `count` is the number of
/// raw entries (including every version and every tombstone) for
/// user keys in the queried range; `size` is the sum of
/// `internal_key.len() + value.len()` over those entries. Both
/// values are exact with respect to the current active memtable -
/// this method walks the skip list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemTableStats {
    /// Number of raw entries in the range.
    pub count: u64,
    /// Approximate total bytes in the range.
    pub size: u64,
}

/// Result type for regolith operations.
pub type Result<T> = std::result::Result<T, Error>;

/// A key-value database backed by an LSM-tree.
///
/// No write waits on a write stall. A write that a stop threshold stops
/// (too many level-0 tables, too many memtables, or pending compaction
/// bytes over the hard limit) applies nothing and returns
/// [`Error::WouldBlock`] with a [`WouldBlock::Stall`] wait, completed on the
/// writer's own [`IoQueue`] once background work clears the stall; run the
/// write again then, or wrap it in [`through_stalls`] to wait on this
/// thread. A slowdown threshold delays nothing. A transaction's commit is
/// the exception, since it cannot be run again: it waits the stall out
/// ([`Transaction::commit`], [`Transaction::commit_nowait`]).
pub struct Db {
    engine: Arc<RegolithEngine>,
    durability: engine::DurabilityMode,
    cfs: Arc<CfRegistry>,
    read_only: bool,
    max_key_size: usize,
    max_value_size: usize,
    transaction_keys_inline: usize,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db")
            .field("durability", &self.durability)
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

impl Db {
    /// Open or create a database at the given path.
    ///
    /// On a fresh database the default column family (`"default"`)
    /// is created automatically and every non-`*_cf` method uses
    /// it. Callers who want logical keyspace isolation can then
    /// call [`Db::create_column_family`] for additional CFs; those
    /// calls persist into the database and survive reopen. Invalid
    /// option combinations fail before any filesystem work starts.
    ///
    /// Returns [`Error::Io`] with [`std::io::ErrorKind::Unsupported`]
    /// when the platform cannot start the background compaction
    /// thread, as a single-threaded target does.
    ///
    /// # Databases written by 0.1.x
    ///
    /// A database written by a 0.1.x release can hold a level below level 0
    /// whose tables overlap in key range: that release placed an ingested
    /// table carrying a range tombstone by the range of its keys and
    /// recorded it with the wider range of the tombstone. Such a database
    /// is valid and opens. The first open moves the tables of that level,
    /// and of every level above it down to level 1, into level 0, oldest
    /// level first, where tables may overlap. No table is rewritten and
    /// nothing is lost: every read returns the newest version, and a key
    /// that a range tombstone of a moved table covers and that is older
    /// than it reads as deleted, as that tombstone says (0.1.x could still
    /// return it from a level it read first). The open logs one warning,
    /// counts the moved tables in [`Ticker::ManifestTablesDemoted`] and in
    /// the `regolith.manifest.tables_demoted` property, and records the move
    /// before it returns, so the next open has nothing to move. Compaction
    /// then brings the tables back down as it does any level 0 table; until
    /// it has, they count toward the level 0 write thresholds. A read-only
    /// open makes the same move in its own view, every time, and writes
    /// nothing.
    ///
    /// The same releases could leave an ingested table's range delete in the
    /// bottom level below older keys it deletes, with no level overlapping. A
    /// point read then found such a key before the range delete and returned
    /// it, while a scan did not; a compaction of the bottom level would drop
    /// the range delete and bring the key back for good. The first open of a
    /// database that does not record this check copies every range delete
    /// that has a newer level, a level 0 table read before it or the
    /// recovered log holding keys in its range in front of every key, so the
    /// key reads as deleted everywhere. A copy deletes only what its range
    /// delete deletes, so the copies of range deletes that were in front
    /// already change no read. The open logs one warning, counts the copies
    /// in [`Ticker::ManifestRangeDeletesCopied`] and in the
    /// `regolith.manifest.range_deletes_copied` property, and records the
    /// check before it returns; a database this release creates records it
    /// from the start and is never checked. The copies are one level 0
    /// table after the first flush, which compaction carries down; where the
    /// copied ranges are wide, that is a one-time rewrite of the data inside
    /// them. A read-only open copies in its own view, every time, and writes
    /// nothing.
    pub fn open<P: AsRef<Path>>(path: P, opts: Options) -> Result<Self> {
        opts.validate()?;
        let read_only = opts.read_only;
        let max_key_size = opts.max_key_size;
        let max_value_size = opts.max_value_size;
        let transaction_keys_inline = opts.transaction_keys_inline;
        let durability = match opts.durability {
            DurabilityMode::Immediate => engine::DurabilityMode::Immediate,
            DurabilityMode::Eventual => engine::DurabilityMode::Eventual,
        };
        let engine_opts = opts.to_engine_options();
        let engine = if read_only {
            RegolithEngine::open_read_only(path.as_ref(), engine_opts)?
        } else {
            RegolithEngine::open(path.as_ref(), engine_opts)?
        };
        let cfs = Arc::new(CfRegistry::new());
        // Attached before the first write, so every write is fenced against
        // the registry when it is decided, from the start.
        engine.attach_families(Arc::clone(&cfs));
        let db = Self {
            engine,
            durability,
            cfs,
            read_only,
            max_key_size,
            max_value_size,
            transaction_keys_inline,
        };
        db.load_cf_registry()?;
        Ok(db)
    }

    /// Open an existing database in read-only mode.
    ///
    /// The handle replays any existing WAL files into memory so reads
    /// see committed-but-unflushed writes, but it does not create,
    /// rewrite, compact, or delete files. Mutating APIs return
    /// [`Error::ReadOnly`]. A database a 0.1.x release left with an
    /// overlapping level, or with a range delete below older keys it
    /// deletes, reads correctly here too, see [`Db::open`].
    pub fn open_read_only<P: AsRef<Path>>(path: P, mut opts: Options) -> Result<Self> {
        opts.read_only = true;
        Self::open(path, opts)
    }

    /// Populate the in-memory [`CfRegistry`] from the on-disk
    /// metadata CF, creating the default CF entry if this is a
    /// fresh database. Called once from [`Db::open`].
    fn load_cf_registry(&self) -> Result<()> {
        // Scan every `name:*` entry in the meta CF to rebuild the
        // name→id map. The default CF is not persisted to disk -
        // it's always injected into the in-memory registry with a
        // hardcoded id so an empty database stays byte-free on
        // disk. User-created CFs are the only thing that produces
        // on-disk metadata writes.
        let mut entries: Vec<(String, u32)> = Vec::new();
        let pairs = collect_range(
            self.engine.new_iter_latest(),
            Some(&meta::name_scan_prefix()),
            Some(&meta::name_scan_upper()),
        )?;
        for (key, value) in pairs {
            // A meta value that is not a 4-byte id is not one of ours.
            let Ok(id_bytes) = <[u8; 4]>::try_from(value.as_slice()) else {
                continue;
            };
            let Some(name) = meta::name_from_key(&key) else {
                continue;
            };
            entries.push((name.to_string(), u32::from_be_bytes(id_bytes)));
        }

        // Recover `next_id`. Absent on a fresh database.
        let next_id_raw = self
            .engine
            .get_latest(&meta::next_id_key())
            .map_err(Error::from)?;
        let next_id = next_id_raw
            .and_then(|bytes| <[u8; 4]>::try_from(bytes.as_slice()).ok())
            .map_or(DEFAULT_CF_ID + 1, u32::from_be_bytes);

        // Inject the default CF into the in-memory registry so
        // `Db::default_cf()` always succeeds. It's never persisted
        // to the meta CF - any re-open computes the same id.
        entries.push((DEFAULT_CF_NAME.to_string(), DEFAULT_CF_ID));
        self.cfs.load(entries, next_id);
        Ok(())
    }

    /// Get the value for a key from the default column family.
    /// Returns `None` if the key doesn't exist.
    ///
    /// This is [`Db::get_slice`] followed by [`DbSlice::into_vec`].
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(self.get_slice(key)?.map(DbSlice::into_vec))
    }

    /// Read a value from the default column family without copying it.
    ///
    /// The returned [`DbSlice`] borrows bytes the database already owns
    /// and holds a reference count on their owner, so nothing is copied
    /// on the way out. Holding one pins that owner: see [`DbSlice`].
    ///
    /// `Option<DbSlice>` does not compare against `Option<Vec<u8>>`
    /// (`core`'s `PartialEq` for `Option` is homogeneous), so this
    /// method is additive and [`Db::get`] keeps its signature.
    pub fn get_slice(&self, key: &[u8]) -> Result<Option<DbSlice>> {
        self.lookup_slice_latest(DEFAULT_CF_ID, key)
    }

    /// [`Db::get_slice`] scoped to a column family.
    pub fn get_slice_cf(&self, cf: &ColumnFamilyHandle, key: &[u8]) -> Result<Option<DbSlice>> {
        self.validate_cf_handle(cf)?;
        self.lookup_slice_latest(cf.id(), key)
    }

    /// Whether a live value exists for `key` in the default column
    /// family.
    ///
    /// Reads the same sources [`Db::get`] does and pays the same block
    /// reads: a bloom filter is probabilistic, so ruling a key *in*
    /// still requires reading/decompressing the full data block. It skips
    /// copying out the winning value and pins no value after returning.
    /// On a bloom-negative lookup `has` and
    /// `get` cost the same and neither touches a block.
    ///
    /// When a [`MergeOperator`] is configured this method does
    /// materialize: the operator decides inside `full_merge` whether a
    /// value exists at all, so there is no way to answer without
    /// collapsing the chain.
    pub fn has(&self, key: &[u8]) -> Result<bool> {
        Ok(self.get_size(key)?.is_some())
    }

    /// [`Db::has`] scoped to a column family.
    pub fn has_cf(&self, cf: &ColumnFamilyHandle, key: &[u8]) -> Result<bool> {
        Ok(self.get_size_cf(cf, key)?.is_some())
    }

    /// Length in bytes of the live value for `key` in the default
    /// column family, or `None` when there is none.
    ///
    /// Same sources and same block reads as [`Db::get`], without the
    /// copy. See [`Db::has`] for what that does and does not save, and
    /// for the [`MergeOperator`] caveat.
    pub fn get_size(&self, key: &[u8]) -> Result<Option<usize>> {
        self.lookup_size_latest(DEFAULT_CF_ID, key, None)
    }

    /// [`Db::get_size`] scoped to a column family.
    pub fn get_size_cf(&self, cf: &ColumnFamilyHandle, key: &[u8]) -> Result<Option<usize>> {
        self.validate_cf_handle(cf)?;
        self.lookup_size_latest(cf.id(), key, None)
    }

    /// Like [`Db::get_size`], with an opt-in allocation guard per SST data block.
    /// The on-disk frame size plus decoded backing-buffer capacity must not exceed
    /// `max_data_block_bytes`, even on cache hits. Oversized blocks return
    /// [`Error::DataBlockLimitExceeded`], not an exact value size or absence.
    ///
    /// This is NOT a total-memory or time bound: metadata (including indexes,
    /// filters and range tombstones), keys, restart arrays, validation overhead,
    /// allocator overhead and other concurrent/retained allocations are excluded.
    /// Memtable values do not incur a data-block charge. A configured merge
    /// operator is rejected with [`Error::InvalidArgument`] without invoking it.
    /// A limit error can precede detection of corruption in an oversized frame.
    pub fn get_size_with_limit(
        &self,
        key: &[u8],
        max_data_block_bytes: usize,
    ) -> Result<Option<usize>> {
        self.lookup_size_latest(DEFAULT_CF_ID, key, Some(max_data_block_bytes))
    }

    /// [`Db::get_size_with_limit`] scoped to a column family, with the same exclusions.
    pub fn get_size_cf_with_limit(
        &self,
        cf: &ColumnFamilyHandle,
        key: &[u8],
        max_data_block_bytes: usize,
    ) -> Result<Option<usize>> {
        self.validate_cf_handle(cf)?;
        self.lookup_size_latest(cf.id(), key, Some(max_data_block_bytes))
    }

    /// [`Db::lookup_slice`] for a read of the newest visible value.
    ///
    /// The engine samples the read horizon against the view it walks,
    /// which a caller cannot do for it: building a `LookupKey` here
    /// would fix the sequence before the engine loads its view, and a
    /// compaction in between can drop the newest version at or below
    /// it, so the read reports the key absent or an older value.
    fn lookup_slice_latest(&self, cf_id: u32, key: &[u8]) -> Result<Option<DbSlice>> {
        let stats = self.stats();
        let _scope = statistics::TimeScope::new(stats, Histogram::DbGet);
        if let Some(s) = stats {
            s.add(Ticker::KeysRead, 1);
        }
        perf_context::record_get_call();
        let result = self
            .engine
            .get_slice_latest(cf_id, key)
            .map_err(|err| map_point_read_error_for(err, key));
        if let (Some(s), Ok(Some(v))) = (stats, &result) {
            s.add(Ticker::BytesRead, v.len() as u64);
            s.record(Histogram::BytesPerRead, v.len() as u64);
        }
        result
    }

    /// The length-only twin of [`Db::lookup_slice_latest`].
    fn lookup_size_latest(
        &self,
        cf_id: u32,
        key: &[u8],
        limit: Option<usize>,
    ) -> Result<Option<usize>> {
        let stats = self.stats();
        let _scope = statistics::TimeScope::new(stats, Histogram::DbGet);
        if let Some(s) = stats {
            s.add(Ticker::KeysRead, 1);
        }
        perf_context::record_get_call();
        self.engine
            .get_size_latest(cf_id, key, limit)
            .map_err(|err| map_point_read_error_for(err, key))
    }

    /// Helper that exposes a borrowed reference to the engine's
    /// `Statistics` (if any) so instrumented methods can call
    /// `stats.add(..)` / `stats.record(..)` through a single
    /// `Option::is_some` check.
    fn stats(&self) -> Option<&Statistics> {
        self.engine.statistics()
    }

    /// Look up a batch of keys in the default column family.
    /// Returns a vector with one entry per input key (preserving
    /// order and duplicates); each entry is `None` if the key does
    /// not exist or is tombstoned.
    ///
    /// All keys in a single call see the **same** consistent view -
    /// a concurrent writer cannot make two keys disagree about
    /// visibility.
    pub fn multi_get(&self, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>> {
        let owned: Vec<Vec<u8>> = keys.iter().map(|k| prefix_key(DEFAULT_CF_ID, k)).collect();
        let refs: Vec<&[u8]> = owned.iter().map(|k| k.as_slice()).collect();
        self.engine.multi_get_latest(&refs).map_err(Error::from)
    }

    /// Set a key-value pair in the default column family using
    /// the database-global durability mode and default write
    /// options.
    pub fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        self.put_opt(&WriteOptions::default(), key, value)
    }

    /// Set a key-value pair in the default column family with an
    /// explicit [`WriteOptions`] override.
    pub fn put_opt(&self, opts: &WriteOptions, key: &[u8], value: &[u8]) -> Result<()> {
        self.ensure_writable()?;
        self.validate_write_kv_sizes(key, value)?;
        self.admit_write(opts)?;
        let _scope = statistics::TimeScope::new(self.stats(), Histogram::DbWrite);
        perf_context::record_write_call();
        let (dm, disable_wal) = self.resolve_write_opts(opts);
        self.engine
            .apply_single_put(
                prefix_key(DEFAULT_CF_ID, key),
                value.to_vec(),
                dm,
                disable_wal,
            )
            .map_err(Error::from)
            .map(|_| ())
    }

    /// Delete a key from the default column family using the
    /// database-global durability mode.
    pub fn delete(&self, key: &[u8]) -> Result<()> {
        self.delete_opt(&WriteOptions::default(), key)
    }

    /// Delete a key from the default column family with an
    /// explicit [`WriteOptions`] override.
    pub fn delete_opt(&self, opts: &WriteOptions, key: &[u8]) -> Result<()> {
        self.ensure_writable()?;
        self.validate_key_size(key)?;
        self.admit_write(opts)?;
        let mut batch = BTreeMap::new();
        batch.insert(prefix_key(DEFAULT_CF_ID, key), None);
        let (dm, disable_wal) = self.resolve_write_opts(opts);
        self.engine
            .apply_grouped_batch(batch, Vec::new(), Vec::new(), dm, disable_wal)
            .map(|_| ())
            .map_err(Error::from)
    }

    /// Layer a merge operand on top of `key` in the default
    /// column family.
    ///
    /// Requires an [`Options::merge_operator`] to be configured, and returns
    /// [`Error::NoMergeOperator`] without writing when none is. The
    /// operand is written cheaply (no read-modify-write); readers
    /// collapse the chain of merges plus any base value via the
    /// configured operator at visibility time.
    pub fn merge(&self, key: &[u8], operand: &[u8]) -> Result<()> {
        self.merge_opt(&WriteOptions::default(), key, operand)
    }

    /// [`Db::merge`] with an explicit [`WriteOptions`] override.
    pub fn merge_opt(&self, opts: &WriteOptions, key: &[u8], operand: &[u8]) -> Result<()> {
        self.ensure_writable()?;
        self.engine.require_merge_operator()?;
        self.validate_write_kv_sizes(key, operand)?;
        self.admit_write(opts)?;
        let (dm, disable_wal) = self.resolve_write_opts(opts);
        self.engine
            .apply_grouped_batch(
                BTreeMap::new(),
                Vec::new(),
                vec![(prefix_key(DEFAULT_CF_ID, key), operand.to_vec())],
                dm,
                disable_wal,
            )
            .map(|_| ())
            .map_err(Error::from)
    }

    /// Delete every key in `[start, end)` in the default column
    /// family.
    ///
    /// Range deletes are cheap regardless of how many keys the range
    /// covers - internally they are stored as a single range-tombstone
    /// record rather than as one point tombstone per key. The delete
    /// is durable under the same rules as [`Db::put`] / [`Db::delete`]
    /// and is atomic with respect to concurrent readers.
    ///
    /// If `start >= end` the call is a no-op.
    pub fn delete_range(&self, start: &[u8], end: &[u8]) -> Result<()> {
        self.delete_range_opt(&WriteOptions::default(), start, end)
    }

    /// Delete every key in `[start, end)` in the default column
    /// family with an explicit [`WriteOptions`] override.
    ///
    /// An empty range is still a write: a read-only or closed handle
    /// rejects it with [`Error::ReadOnly`] or [`Error::Closed`] rather
    /// than returning `Ok`.
    pub fn delete_range_opt(&self, opts: &WriteOptions, start: &[u8], end: &[u8]) -> Result<()> {
        self.ensure_writable()?;
        if start >= end {
            return Ok(());
        }
        self.validate_key_size(start)?;
        self.validate_key_size(end)?;
        self.admit_write(opts)?;
        let (dm, disable_wal) = self.resolve_write_opts(opts);
        self.engine
            .apply_grouped_batch(
                BTreeMap::new(),
                vec![(
                    prefix_key(DEFAULT_CF_ID, start),
                    prefix_key(DEFAULT_CF_ID, end),
                )],
                Vec::new(),
                dm,
                disable_wal,
            )
            .map(|_| ())
            .map_err(Error::from)
    }

    /// Keys a transaction buffers before indexing them. See
    /// [`Options::transaction_keys_inline`].
    pub(crate) fn transaction_keys_inline(&self) -> usize {
        self.transaction_keys_inline
    }

    /// Open a write stream that bounds its own memory.
    ///
    /// [`WriteBatch`] costs memory proportional to its input, which a
    /// caller feeding an unbounded stream cannot afford. A
    /// [`StreamingWriter`] costs the configured budget instead, at the
    /// price of atomicity across the whole stream. Read
    /// [`StreamingWriter`]'s own documentation before choosing it: that
    /// tradeoff is the entire reason the type exists.
    pub fn streaming_writer(&self, opts: StreamOptions) -> StreamingWriter<'_> {
        StreamingWriter::new(self, opts)
    }

    /// Apply a batch of writes atomically using the database-global
    /// durability mode.
    ///
    /// Refused with [`Error::InvalidArgument`], with nothing applied and
    /// no sequence number used, when the batch's write-ahead log record
    /// would exceed 1 GiB (see [`WriteBatch`]).
    pub fn write(&self, batch: WriteBatch) -> Result<()> {
        self.write_opt(&WriteOptions::default(), batch)
    }

    /// Apply a batch of writes atomically with an explicit
    /// [`WriteOptions`] override.
    ///
    /// Apply a batch and return the sequence it committed at.
    ///
    /// The returned sequence is the engine's visibility horizon once the batch
    /// is durable and applied: a [`Snapshot`] taken afterwards reports at least
    /// this value from [`Snapshot::sequence`], and one taken before reports
    /// less. An upper layer can therefore order its own versions against regolith's
    /// without holding a lock across the write, because the horizon publishes
    /// atomically inside this call.
    ///
    /// An empty batch commits nothing and returns the current horizon.
    pub fn write_sequenced(&self, batch: WriteBatch) -> Result<u64> {
        self.write_sequenced_opt(&WriteOptions::default(), batch)
    }

    /// [`Db::write_sequenced`] with explicit [`WriteOptions`].
    pub fn write_sequenced_opt(&self, opts: &WriteOptions, batch: WriteBatch) -> Result<u64> {
        self.write_opt_inner(opts, batch)
    }

    /// The newest sequence visible to a snapshot taken now.
    ///
    /// Monotonic and never decreasing for an open database.
    pub fn latest_sequence(&self) -> u64 {
        self.engine.snapshot_seq()
    }

    /// Apply a batch of writes atomically with explicit [`WriteOptions`].
    ///
    /// Use [`Db::write_sequenced_opt`] instead when the caller needs the
    /// sequence the batch committed at.
    pub fn write_opt(&self, opts: &WriteOptions, batch: WriteBatch) -> Result<()> {
        self.write_opt_inner(opts, batch).map(|_| ())
    }

    fn write_opt_inner(&self, opts: &WriteOptions, batch: WriteBatch) -> Result<u64> {
        // Checked before the empty-batch shortcut: an empty batch is
        // still a write, so a read-only or closed handle rejects it
        // rather than reporting a horizon it could not have advanced.
        self.ensure_writable()?;
        if batch.is_empty() {
            // Nothing committed: report the current horizon, which is what a
            // snapshot taken now would read at.
            return Ok(self.engine.snapshot_seq());
        }
        let (dm, disable_wal) = self.resolve_write_opts(opts);
        self.validate_batch_cf_liveness(&batch)?;
        self.validate_batch_sizes(&batch, disable_wal)?;
        self.admit_write(opts)?;
        perf_context::record_write_call();
        let _scope = statistics::TimeScope::new(self.stats(), Histogram::DbWrite);
        self.engine
            .submit_batch(batch.ops, dm, disable_wal)
            .map_err(Error::from)
    }

    /// Apply a batch of writes atomically with an explicit
    /// [`DurabilityMode`] override. Retained for backwards
    /// compatibility - prefer [`Db::write_opt`] for new code.
    pub fn write_with_durability(
        &self,
        batch: WriteBatch,
        durability: DurabilityMode,
    ) -> Result<()> {
        let opts = WriteOptions {
            sync: matches!(durability, DurabilityMode::Immediate),
            ..WriteOptions::default()
        };
        self.write_opt(&opts, batch)
    }

    /// Resolve a [`WriteOptions`] into the pair the engine's
    /// `apply_batch` actually consumes: a concrete
    /// `engine::DurabilityMode` and a `disable_wal` bool. `sync: true`
    /// maps to `Immediate` regardless of the database-global default;
    /// otherwise the default wins. `low_pri` is accepted but is
    /// currently a no-op; `no_slowdown` is handled separately by
    /// the write-stall pre-check.
    fn resolve_write_opts(&self, opts: &WriteOptions) -> (engine::DurabilityMode, bool) {
        let dm = if opts.sync {
            engine::DurabilityMode::Immediate
        } else {
            self.durability
        };
        (dm, opts.disable_wal)
    }

    /// Run the write-stall admission. Never waits: a write a stop stopped
    /// returns [`Error::WouldBlock`] with a [`StallWait`] for the stall to
    /// clear, and one that set `opts.no_slowdown` returns [`Error::Busy`]
    /// under any stall.
    fn admit_write(&self, opts: &WriteOptions) -> Result<()> {
        self.engine.admit_write(opts.no_slowdown)
    }

    fn ensure_open(&self) -> Result<()> {
        if self.engine.is_closed() {
            Err(Error::Closed)
        } else {
            Ok(())
        }
    }

    fn ensure_writable(&self) -> Result<()> {
        self.ensure_open()?;
        if self.read_only {
            Err(Error::ReadOnly)
        } else {
            Ok(())
        }
    }

    fn validate_key_size(&self, key: &[u8]) -> Result<()> {
        if key.len() <= self.max_key_size {
            return Ok(());
        }

        Err(invalid_input_error(format!(
            "key length {} exceeds configured max_key_size {}",
            key.len(),
            self.max_key_size
        )))
    }

    fn validate_value_size(&self, value: &[u8]) -> Result<()> {
        if value.len() <= self.max_value_size {
            return Ok(());
        }

        Err(invalid_input_error(format!(
            "value length {} exceeds configured max_value_size {}",
            value.len(),
            self.max_value_size
        )))
    }

    fn validate_write_kv_sizes(&self, key: &[u8], value: &[u8]) -> Result<()> {
        self.validate_key_size(key)?;
        self.validate_value_size(value)
    }

    fn validate_prefixed_key_size(&self, prefixed_key: &[u8]) -> Result<()> {
        let user_key_len = prefixed_key.len().saturating_sub(4);
        if user_key_len <= self.max_key_size {
            return Ok(());
        }

        Err(invalid_input_error(format!(
            "key length {} exceeds configured max_key_size {}",
            user_key_len, self.max_key_size
        )))
    }

    /// `disable_wal` skips the record-length check, since the write
    /// produces no WAL record; key and value limits still apply.
    fn validate_batch_sizes(&self, batch: &WriteBatch, disable_wal: bool) -> Result<()> {
        let mut record = engine::wal::RecordLen::default();
        for op in &batch.ops {
            match op {
                WriteBatchOp::Put { key, value } => {
                    self.validate_prefixed_key_size(key)?;
                    self.validate_value_size(value)?;
                    record.put(key, value);
                }
                WriteBatchOp::Delete { key } => {
                    self.validate_prefixed_key_size(key)?;
                    record.delete(key);
                }
                WriteBatchOp::DeleteRange { start, end } => {
                    self.validate_prefixed_key_size(start)?;
                    self.validate_prefixed_key_size(end)?;
                    record.delete_range(start, end);
                }
                WriteBatchOp::Merge { key, operand } => {
                    self.engine.require_merge_operator()?;
                    self.validate_prefixed_key_size(key)?;
                    self.validate_value_size(operand)?;
                    record.merge(key, operand);
                }
            }
        }
        if !disable_wal {
            engine::wal::check_write_len(record.framed())?;
        }
        Ok(())
    }

    fn validate_cf_handle(&self, cf: &ColumnFamilyHandle) -> Result<()> {
        if self.cfs.is_live_handle(cf) {
            Ok(())
        } else {
            Err(invalid_cf_handle_error(cf))
        }
    }

    fn is_live_cf_handle(&self, cf: &ColumnFamilyHandle) -> bool {
        self.cfs.is_live_handle(cf)
    }

    /// Check one prefixed key's column family, remembering the last id
    /// found live so a run of ops in one column family costs one registry
    /// lookup.
    fn validate_prefixed_cf(&self, live: &mut Option<u32>, prefixed_key: &[u8]) -> Result<()> {
        let cf_id = prefixed_cf_id(prefixed_key).map_err(Error::from)?;
        if *live == Some(cf_id) {
            return Ok(());
        }
        if !self.cfs.contains_id(cf_id) {
            return Err(invalid_cf_id_error(cf_id));
        }
        *live = Some(cf_id);
        Ok(())
    }

    fn validate_batch_cf_liveness(&self, batch: &WriteBatch) -> Result<()> {
        // The early check, at the API boundary: a batch naming a family
        // already gone is refused before any work. A batch is usually one
        // column family, or a few long runs of one, and every distinct run
        // reaches the registry once. A drop that lands after this check is
        // caught by the fence made when the batch's slot is decided
        // (`engine::commit::families`), which refuses the batch whole, so a
        // batch racing a drop is applied whole before the tombstone or
        // refused whole, never split across it.
        let mut live = None;
        for op in &batch.ops {
            match op {
                WriteBatchOp::Put { key, .. }
                | WriteBatchOp::Delete { key }
                | WriteBatchOp::Merge { key, .. } => self.validate_prefixed_cf(&mut live, key)?,
                WriteBatchOp::DeleteRange { start, end } => {
                    self.validate_prefixed_cf(&mut live, start)?;
                    self.validate_prefixed_cf(&mut live, end)?;
                }
            }
        }
        Ok(())
    }

    /// Create a point-in-time snapshot for consistent reads.
    ///
    /// Snapshots also pin what they read: as long as a `Snapshot` at
    /// seq `S` is alive, compaction keeps every version a read at `S`
    /// needs. It still drops what no reader can see, folds
    /// merge operands between one snapshot and the next, and runs the
    /// [`CompactionFilter`] (see its note on snapshots). Dropping the
    /// returned `Snapshot` releases the pin and may allow subsequent
    /// compactions to reclaim more space.
    ///
    /// A snapshot held for long also defers the compaction of deleted
    /// ranges: a table holding a deletion newer than the oldest live
    /// snapshot is not compacted for its deletions until that snapshot is
    /// dropped (see [`CompactionStyle`]), so scans over a range deleted
    /// since then keep walking its deletions meanwhile.
    pub fn snapshot(&self) -> Snapshot {
        let (seq, pin) = self.engine.register_snapshot_at_horizon();
        Snapshot {
            engine: Arc::clone(&self.engine),
            cfs: Arc::clone(&self.cfs),
            seq,
            mode: ReadMode::Blocking,
            pin,
        }
    }

    /// A new I/O queue for the calling thread.
    ///
    /// Hand its [`IoQueue::id`] to the read handles this thread uses, in
    /// [`ReadMode::CacheOnly`] through [`Snapshot::with_read_mode`] or
    /// [`TxnOptions::read_mode`]: their reads then never touch the device,
    /// and return [`Error::WouldBlock`] when they need a block the cache
    /// does not hold. The thread polls the queue with [`IoQueue::poll`] when
    /// it has nothing else to run, which reads those blocks, and runs the
    /// reads again. regolith adds no thread for this: each thread does its
    /// own I/O.
    ///
    /// One queue per thread; only the thread holding it polls it. A queue
    /// costs nothing while it waits on nothing, and a database that never
    /// opens one pays nothing for the mechanism.
    pub fn io_queue(&self) -> IoQueue {
        self.engine.io_queue()
    }

    /// Scan a key range lazily, holding one entry rather than the range.
    ///
    /// The streaming counterpart to [`Db::scan`], and what a caller
    /// should reach for by default: a scan that stops early pays only for
    /// what it read, and memory does not grow with the size of the range.
    /// Values come back as [`DbSlice`], so no value bytes are copied.
    ///
    /// The scan is served from a snapshot pinned when it is created, so
    /// concurrent writes cannot shift the range underneath it.
    ///
    /// Each item is a `Result`. A scan that fails part-way ends with one
    /// `Err`, then `None`, so a failure is never read as the end of the
    /// range.
    ///
    /// ```
    /// # use regolith::{Db, Options};
    /// # let db = Db::open("users", Options::memory())?;
    /// db.put(b"user:ada", b"1815")?;
    /// db.put(b"user:bob", b"1970")?;
    /// db.put(b"visit:1", b"ada")?;
    ///
    /// // `;` is the byte after `:`, so this is every key under `user:`.
    /// let mut users = Vec::new();
    /// for entry in db.scan_stream(Some(b"user:"), Some(b"user;"))? {
    ///     let (key, value) = entry?;
    ///     users.push((key, value.len()));
    /// }
    /// assert_eq!(users, [(b"user:ada".to_vec(), 4), (b"user:bob".to_vec(), 4)]);
    /// # Ok::<(), regolith::Error>(())
    /// ```
    pub fn scan_stream(&self, start: Option<&[u8]>, end: Option<&[u8]>) -> Result<ScanStream> {
        Ok(self.snapshot().into_scan_stream(start, end))
    }

    /// Scan a key range in the default column family.
    ///
    /// Returns all key-value pairs where `start <= key < end`, with
    /// keys in their user-visible form (no CF prefix). This
    /// materializes the entire range into memory, so it is bounded by
    /// the size of the range rather than by the caller. Prefer
    /// [`Db::scan_stream`], or [`Db::scan_page`] when the caller wants an
    /// explicit page size.
    pub fn scan(
        &self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let lo = match start {
            Some(s) => prefix_key(DEFAULT_CF_ID, s),
            None => cf_lower_bound(DEFAULT_CF_ID),
        };
        let hi = match end {
            Some(e) => prefix_key(DEFAULT_CF_ID, e),
            None => cf_upper_bound(DEFAULT_CF_ID),
        };
        // `new_iter_latest` loads the published view and *then* samples the
        // horizon. Sampling first and building the iterator after leaves a
        // window where a compaction no snapshot pins can drop the newest
        // version at or below the sampled sequence, after which the scan
        // finds only versions it must filter out and a key reads absent.
        let raw = collect_range(self.engine.new_iter_latest(), Some(&lo), Some(&hi))?;
        strip_cf_prefix_entries(raw)
    }

    /// Scan a bounded page in the default column family.
    ///
    /// At most `limit` entries are materialized. When
    /// [`ScanPage::next_start`] is `Some`, pass that key back as
    /// `start` to continue the scan. Each `Db` call captures its own
    /// read snapshot; create a [`Snapshot`] and call
    /// [`Snapshot::scan_page`] for a stable multi-page walk while
    /// writes continue.
    pub fn scan_page(
        &self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        limit: usize,
    ) -> Result<ScanPage> {
        let lo = match start {
            Some(s) => prefix_key(DEFAULT_CF_ID, s),
            None => cf_lower_bound(DEFAULT_CF_ID),
        };
        let hi = match end {
            Some(e) => prefix_key(DEFAULT_CF_ID, e),
            None => cf_upper_bound(DEFAULT_CF_ID),
        };
        collect_page(self.engine.new_iter_latest(), &lo, &hi, limit).and_then(strip_cf_prefix_page)
    }

    /// Create a streaming iterator over the default column family.
    ///
    /// The iterator captures a consistent view at the moment it is
    /// created: later writes are invisible to this iterator, and
    /// concurrent background compaction cannot invalidate it. Keys from
    /// the iterator have the CF prefix stripped and appear exactly as
    /// the caller supplied them on put.
    ///
    /// A fresh iterator is not positioned; call one of
    /// [`CfIter::seek_to_first`], [`CfIter::seek`], or
    /// [`CfIter::seek_for_prev`] before reading.
    pub fn iter(&self) -> CfIter<'_> {
        let default = self.default_cf();
        self.iter_cf(&default)
    }

    /// Create a raw streaming iterator over the entire engine
    /// keyspace, including the reserved metadata CF. Internal -
    /// used by [`Db::iter_cf`] via `CfIter`.
    fn raw_iter(&self) -> Iter<'_> {
        Iter::from_internal(self.engine.new_iter_latest()).with_stats(self.engine.statistics_arc())
    }

    /// Delete all data in the database.
    ///
    /// A call that does its own I/O and waits on this thread: for the
    /// compaction pass or job in flight, and for the commits in
    /// flight, to end. Call it off a thread that must not block. On
    /// `wasm32`, whose one thread cannot wait for itself, a call made from
    /// inside a compaction or a checkpoint (a listener, a compaction filter)
    /// fails with an I/O error of kind
    /// [`WouldBlock`](std::io::ErrorKind::WouldBlock) instead.
    pub fn drop_all(&self) -> Result<()> {
        self.ensure_writable()?;
        self.engine.drop_all().map_err(Error::from)
    }

    /// Compact every SSTable overlapping the default column-family
    /// user-key range `[start, end)` down to the bottommost non-empty
    /// level, as a job: the call returns a [`JobTicket`] at once and never
    /// waits on the compaction gate.
    ///
    /// Passing `None` for either bound means "unbounded" on that side,
    /// so `compact_range(None, None)` compacts the entire default
    /// column family.
    ///
    /// The job runs on the compaction worker, or, with no worker, on this
    /// thread's [`IoQueue`] when it polls; a thread with neither runs it
    /// before this returns. It flushes the active memtable contents that
    /// fall in the range to L0 first, and is serialized with the background
    /// compaction so the two can't fight over the same inputs. Neither
    /// starves the other: a compaction pass waits out a bounded number of
    /// jobs like this one, and a job a bounded number of passes
    /// ([`sync::MAX_BYPASS`]). A poll never waits for a pass in flight
    /// either: until the job may go on it stays pending on the queue, whose
    /// owner is woken when it may. The ticket
    /// completes on this thread's queue (or, with none, when the job
    /// finished): await it, poll the queue, or [`JobTicket::wait`] for it
    /// from a thread with no queue. An invalid bound fails the ticket at
    /// once.
    ///
    /// On a database encrypted at rest ([`Options::key_provider`]) it also
    /// re-seals the whole database, whatever the range: every table not
    /// sealed under the provider's current key, the active write-ahead log,
    /// and a manifest holding a batch under another key are rewritten under
    /// it. This is the one-time re-encryption after encryption is turned on
    /// and after a key rotation; once its ticket is ready, no file names
    /// another key.
    pub fn compact_range(&self, start: Option<&[u8]>, end: Option<&[u8]>) -> JobTicket {
        let bounds = || -> Result<(Vec<u8>, Vec<u8>)> {
            self.ensure_writable()?;
            if let Some(start) = start {
                self.validate_key_size(start)?;
            }
            if let Some(end) = end {
                self.validate_key_size(end)?;
            }
            let lower = match start {
                Some(s) => prefix_key(DEFAULT_CF_ID, s),
                None => cf_lower_bound(DEFAULT_CF_ID),
            };
            let upper = match end {
                Some(e) => prefix_key(DEFAULT_CF_ID, e),
                None => cf_upper_bound(DEFAULT_CF_ID),
            };
            Ok((lower, upper))
        };
        match bounds() {
            Ok((lower, upper)) => self
                .engine
                .start_foreground(engine::Work::CompactRange { lower, upper }),
            Err(err) => JobTicket::ready(Arc::downgrade(&self.engine), Err(err)),
        }
    }

    /// Rotate the active memtable and see it written to a level-0
    /// SSTable before returning.
    ///
    /// A no-op when the active memtable holds no entries and no range
    /// tombstones. This is the same flush a full memtable triggers on
    /// the write path, made explicit so a caller can decide when to
    /// pay for it. This thread writes the table, unless a compaction
    /// worker gets to the sealed memtable first: the worker then writes
    /// it, paced by [`Options::rate_limiter`] like the rest of its work,
    /// and this call returns once the table is in level 0. With no
    /// worker it is always this thread.
    ///
    /// Flushing does not compact: the new file lands in L0 and stays
    /// there until a compaction job merges it. See [`Db::compact_step`]
    /// and [`Db::compact_range`].
    pub fn flush(&self) -> Result<()> {
        self.ensure_writable()?;
        self.engine.flush_active_memtable().map_err(Error::from)
    }

    /// Perform at most one pending compaction job on this thread.
    ///
    /// Returns `Ok(true)` when a job ran. Callers running with
    /// [`Options::max_background_compactions`] set to `0` use this to
    /// keep the level structure healthy outside the write path. With
    /// [`Options::inline_compaction`] on, a writer a stall stops performs
    /// the same job itself; with it off, this (or [`Db::flush`]) is what
    /// clears a stall, and the stopped writes' [`StallWait`]s complete when
    /// it does.
    ///
    /// The returned [`CompactionOutcome`] separates the two reasons a
    /// pass can do nothing, because a caller has to act on them
    /// differently. [`CompactionOutcome::Idle`] means nothing was over
    /// its compaction trigger, no deleted range was worth a pass (see
    /// [`CompactionStyle`]), and the tree is settled.
    /// [`CompactionOutcome::Contended`] means work may be pending but
    /// another thread is in the way, so the right response is to try again
    /// later rather than conclude the engine is done: another pass holds the
    /// input files, or a [`Db::compact_range`], an ingest, a checkpoint, a
    /// backup or a [`Db::drop_all`] holds the compaction gate alone or is
    /// owed it. This call never waits for that thread.
    ///
    /// A drain therefore looks like this, and stops only on `Idle`:
    ///
    /// ```
    /// # use regolith::{CompactionOutcome, Db, Options};
    /// # fn main() -> regolith::Result<()> {
    /// # let db = Db::open("rows", Options::memory())?;
    /// # for round in 0..8u8 {
    /// #     db.put(b"row", &[round])?;
    /// #     db.flush()?;
    /// # }
    /// loop {
    ///     match db.compact_step()? {
    ///         CompactionOutcome::DidWork => continue,
    ///         CompactionOutcome::Contended => std::thread::yield_now(),
    ///         CompactionOutcome::Idle => break,
    ///     }
    /// }
    /// # assert_eq!(db.compact_step()?, CompactionOutcome::Idle);
    /// # assert_eq!(db.get(b"row")?, Some(vec![7]));
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// A size-triggered compaction reduces the file count it merges, and
    /// a compaction picked for its deletions moves them one level down or
    /// discards them, so a loop driven by `DidWork` cannot be fed forever
    /// by its own output.
    ///
    /// Safe to call with background workers running: the job is picked
    /// under the same engine-wide compaction gate and the same claims on
    /// tables the workers use, so the two can never pick overlapping
    /// inputs.
    pub fn compact_step(&self) -> Result<CompactionOutcome> {
        self.ensure_writable()?;
        self.engine.run_one_compaction_pass().map_err(Error::from)
    }

    /// Return the string value of a named property, or `None` if
    /// `name` isn't recognized. See the module-level docs for the
    /// full list of supported properties; the most useful ones
    /// are `"regolith.stats"`, `"regolith.sstables"`,
    /// `"regolith.levelstats"`, and `"regolith.options"`.
    pub fn get_property(&self, name: &str) -> Option<String> {
        match name {
            "regolith.stats" => Some(self.format_stats_property()),
            "regolith.sstables" => Some(self.format_sstables_property()),
            "regolith.levelstats" => Some(self.format_levelstats_property()),
            "regolith.options" => Some(format!("{:#?}", self.options_snapshot())),
            _ => {
                // Integer properties surfaced through the string
                // API too - every int property's string form is
                // just its decimal number.
                self.get_int_property(name).map(|v| v.to_string())
            }
        }
    }

    /// The statistics this database was opened with
    /// ([`Options::statistics`]), or `None` when it was opened with none.
    ///
    /// It is the object the caller configured, not a copy: every handle on
    /// this database adds to it, a ticker read through it is the total they
    /// all added, and [`Statistics::reset`] through it zeroes it for all of
    /// them. Clone the `Arc` to keep it past the database handle. A database
    /// opened without statistics never makes one, so this stays `None` for
    /// its whole life. What a read sees while updates run is stated on
    /// [`Statistics`].
    ///
    /// ```
    /// # use std::sync::Arc;
    /// # use regolith::{Db, Options, Statistics, Ticker};
    /// let options = Options::memory().statistics(Some(Arc::new(Statistics::new())));
    /// let db = Db::open("statistics-doc", options)?;
    /// db.put(b"k", b"v")?;
    /// let stats = db.statistics().expect("opened with statistics");
    /// assert_eq!(stats.get_ticker(Ticker::KeysWritten), 1);
    /// # Ok::<(), regolith::Error>(())
    /// ```
    pub fn statistics(&self) -> Option<&Arc<Statistics>> {
        self.engine.statistics_shared()
    }

    /// What the environment this database was opened on can
    /// actually do.
    ///
    /// Read this when a durability or isolation guarantee matters.
    /// regolith keeps working on a host without directory fsync, without
    /// hard links, or without cross-process locking, but the
    /// guarantee is narrower there, and this is where it says so
    /// instead of the database quietly claiming more than it
    /// provides. On the default [`crate::env::StdEnv`] every flag is
    /// `true` on Linux, macOS, and Windows.
    pub fn capabilities(&self) -> env::Capabilities {
        self.engine.capabilities()
    }

    /// Return the integer value of a named property, or `None` if
    /// `name` isn't recognized or doesn't have an integer form.
    ///
    /// `regolith.manifest.tables_demoted` is the number of tables this open
    /// moved to level 0 out of levels a 0.1.x release left overlapping (see
    /// [`Db::open`]): zero once a read-write open has recorded the move, and
    /// the same count at every read-only open until then.
    /// `regolith.manifest.range_deletes_copied` is the number of range
    /// deletes this open copied in front of keys an earlier release may have
    /// left them behind (see [`Db::open`]): zero once a read-write open has
    /// recorded the check, and the same count at every read-only open until
    /// then.
    ///
    /// `regolith.disk-available-bytes` and `regolith.disk-available-inodes`
    /// are what the environment's [`env::Env::disk_space`] reports, and
    /// `None` where it reports nothing: the default [`env::StdEnv`] reports
    /// them on Unix, and not on Windows or WASI.
    ///
    /// `regolith.num-uncounted-tables` is the number of tables whose
    /// deletions have not been counted yet (see [`CompactionStyle`]).
    pub fn get_int_property(&self, name: &str) -> Option<u64> {
        if let Some(level_str) = name.strip_prefix("regolith.num-files-at-level") {
            let level: usize = level_str.parse().ok()?;
            return Some(self.engine.num_files_at_level(level));
        }
        match name {
            "regolith.manifest.tables_demoted" => Some(self.engine.tables_demoted_at_open()),
            "regolith.manifest.range_deletes_copied" => {
                Some(self.engine.range_deletes_copied_at_open())
            }
            "regolith.total-sst-files-size" => Some(self.engine.total_sst_size()),
            "regolith.cur-size-active-mem-table" => Some(self.engine.active_memtable_size()),
            "regolith.memtable-reserved-bytes" => Some(self.engine.memtables_reserved_size()),
            "regolith.arena-pool-bytes" => Some(self.engine.arena_pool_size()),
            "regolith.cur-size-all-mem-tables" => {
                Some(self.engine.active_memtable_size() + self.engine.frozen_memtables_size())
            }
            "regolith.num-entries-active-mem-table" => {
                // Approximate: the memtable exposes `approximate_size`
                // in bytes but no direct entry count. Estimate by
                // assuming a 48-byte average entry (internal key +
                // value). This is a rough indicator, not an exact
                // count.
                let bytes = self.engine.active_memtable_size();
                Some(bytes / 48)
            }
            "regolith.num-entries-imm-mem-tables" => {
                let bytes = self.engine.frozen_memtables_size();
                Some(bytes / 48)
            }
            "regolith.estimate-num-keys" => {
                // Lower-bound estimate: exact SST entry count plus
                // a rough guess for the memtable contribution.
                let sst = self.engine.total_sst_num_entries();
                let mem_bytes =
                    self.engine.active_memtable_size() + self.engine.frozen_memtables_size();
                Some(sst + mem_bytes / 48)
            }
            "regolith.estimate-live-data-size" => Some(self.engine.total_sst_size()),
            "regolith.num-snapshots" => Some(self.engine.live_snapshot_count()),
            "regolith.oldest-snapshot-time" => self.engine.oldest_snapshot_time_unix(),
            "regolith.block-cache-usage" => Some(self.engine.block_cache_usage() as u64),
            "regolith.block-cache-capacity" => Some(self.engine.block_cache_capacity() as u64),
            "regolith.pinned-metadata-bytes" => Some(self.engine.pinned_metadata_bytes() as u64),
            "regolith.disk-available-bytes" => self.engine.disk_space().map(|d| d.available_bytes),
            "regolith.disk-available-inodes" => {
                self.engine.disk_space().map(|d| d.available_inodes)
            }
            "regolith.background-errors" => Some(self.engine.background_error_count()),
            // Tables whose deletions have not been counted yet: tables an
            // earlier version or a restored backup left, which are counted
            // in the background. A table that cannot be read stays counted
            // here.
            "regolith.num-uncounted-tables" => Some(self.engine.current_version().uncounted as u64),
            _ => None,
        }
    }

    /// Format the multi-line `regolith.stats` property: counters +
    /// histograms (when statistics are enabled) plus per-level
    /// file counts and compaction aggregates.
    fn format_stats_property(&self) -> String {
        let mut out = String::new();
        out.push_str("== regolith engine stats ==\n");
        out.push_str(&self.format_levelstats_property());
        if let Some(stats) = self.engine.statistics() {
            out.push('\n');
            out.push_str(&stats.dump());
        } else {
            out.push_str("\n(no Statistics object configured - see Options::statistics)\n");
        }
        out
    }

    /// Format the `regolith.levelstats` property: one row per
    /// level with file count and total size in bytes.
    fn format_levelstats_property(&self) -> String {
        let version = self.engine.current_version();
        let mut out = String::from("Level  Files     Size(B)\n");
        for (lvl, files) in version.levels.iter().enumerate() {
            let count = files.len();
            let size: u64 = files.iter().map(|f| f.meta.file_size).sum();
            out.push_str(&format!("{lvl:5}  {count:5}  {size:10}\n"));
        }
        out
    }

    /// Format the `regolith.sstables` property: one row per live
    /// SSTable with its level, file id, size, and key range.
    fn format_sstables_property(&self) -> String {
        let version = self.engine.current_version();
        let mut out =
            String::from("Level    FileID       Size(B)     Entries  SmallestKey..LargestKey\n");
        for (lvl, files) in version.levels.iter().enumerate() {
            for f in files {
                // Strip the CF prefix for display when the key
                // has room for it; otherwise show the raw bytes.
                let smallest = format_key_for_display(&f.meta.smallest_key);
                let largest = format_key_for_display(&f.meta.largest_key);
                out.push_str(&format!(
                    "{lvl:5}  {:8}  {:12}  {:10}  {}..{}\n",
                    f.meta.file_id, f.meta.file_size, f.meta.num_entries, smallest, largest
                ));
            }
        }
        out
    }

    /// A minimal snapshot of the engine options. We deliberately
    /// do not carry the full `Options` struct around past
    /// construction, so this returns a small struct with just
    /// the observable knobs.
    fn options_snapshot(&self) -> OptionsSnapshot {
        OptionsSnapshot {
            durability: self.durability,
            default_cf: DEFAULT_CF_NAME,
            read_only: self.read_only,
            max_key_size: self.max_key_size,
            max_value_size: self.max_value_size,
            transaction_keys_inline: self.transaction_keys_inline,
        }
    }

    /// Return the approximate on-disk bytes in each of the given
    /// ranges, in the same order as `ranges`. Each range is
    /// scoped to the default column family.
    ///
    /// Computed index-only: no data-block decompression happens,
    /// so the cost is sub-linear in the range size. Accuracy is
    /// bounded by one data-block worth of bytes per range
    /// boundary (partially-covered blocks at `start` and `end` are
    /// included whole). Active-memtable contents are **not**
    /// included - call [`Db::get_approximate_memtable_stats`] for
    /// those.
    pub fn get_approximate_sizes(&self, ranges: &[Range<'_>]) -> Vec<u64> {
        ranges
            .iter()
            .map(|r| self.approximate_size_in_range(&self.default_cf(), r))
            .collect()
    }

    /// CF-scoped variant of [`Db::get_approximate_sizes`].
    pub fn get_approximate_sizes_cf(
        &self,
        cf: &ColumnFamilyHandle,
        ranges: &[Range<'_>],
    ) -> Vec<u64> {
        if !self.is_live_cf_handle(cf) {
            return vec![0; ranges.len()];
        }
        ranges
            .iter()
            .map(|r| self.approximate_size_in_range(cf, r))
            .collect()
    }

    fn approximate_size_in_range(&self, cf: &ColumnFamilyHandle, r: &Range<'_>) -> u64 {
        if r.start >= r.end {
            return 0;
        }
        let lo = prefix_key(cf.id(), r.start);
        let hi = prefix_key(cf.id(), r.end);
        self.engine.approximate_size_in_range(&lo, &hi)
    }

    /// Exact count + approximate size of entries in the active
    /// memtable whose user key falls in `range`, scoped to the
    /// default column family. Frozen memtables are not included.
    pub fn get_approximate_memtable_stats(&self, range: Range<'_>) -> MemTableStats {
        self.memtable_stats_in(&self.default_cf(), &range)
    }

    /// CF-scoped variant of [`Db::get_approximate_memtable_stats`].
    pub fn get_approximate_memtable_stats_cf(
        &self,
        cf: &ColumnFamilyHandle,
        range: Range<'_>,
    ) -> MemTableStats {
        if !self.is_live_cf_handle(cf) {
            return MemTableStats::default();
        }
        self.memtable_stats_in(cf, &range)
    }

    fn memtable_stats_in(&self, cf: &ColumnFamilyHandle, range: &Range<'_>) -> MemTableStats {
        if range.start >= range.end {
            return MemTableStats::default();
        }
        let lo = prefix_key(cf.id(), range.start);
        let hi = prefix_key(cf.id(), range.end);
        let (count, size) = self.engine.approximate_memtable_stats(&lo, &hi);
        MemTableStats { count, size }
    }

    /// Bulk-ingest one or more externally-built SSTable files, as a job:
    /// the call returns a [`JobTicket`] at once and never waits on the
    /// compaction gate. The job runs where [`Db::compact_range`]'s does,
    /// and its ticket completes the same way. Each
    /// file must have been produced by [`SstFileWriter`], or hold at most
    /// one entry per key as it does; a file holding a key twice is
    /// refused. On success every ingested file is placed at the
    /// appropriate level and its keys become visible to new reads and
    /// iterators. See [`IngestOptions`] for the snapshot-consistency,
    /// placement and move rules.
    ///
    /// Each file takes a fresh sequence number, and every entry in it
    /// reads at that sequence. The file is not rewritten: it is copied
    /// into the database's own SSTable directory, or linked when
    /// [`IngestOptions::move_files`] is set, with no lock held, so writers
    /// never wait for the copy. They wait only for the flush of a memtable
    /// holding a key of the file's range, when there is one, and for the
    /// manifest edit that installs the file. So the ingested entries order
    /// after every write acknowledged before the call and before every
    /// write acknowledged after it, no snapshot sees them appear under it,
    /// and a write running alongside the call orders on one side of it.
    /// Files are installed one at a time, each at its own sequence; an
    /// error leaves the files before it installed.
    ///
    /// On a database encrypted at rest ([`Options::key_provider`]) a file
    /// sealed under any key the provider has is installed as it is, while
    /// a plaintext file is written in sealed under the current key instead
    /// of copied or linked, with no lock held: an encrypted database holds
    /// no table in plaintext. A sealed file under a key the provider lacks
    /// is refused with [`Error::UnknownKey`].
    pub fn ingest_external_files(
        &self,
        files: &[std::path::PathBuf],
        opts: IngestOptions,
    ) -> JobTicket {
        if let Err(err) = self.ensure_writable() {
            return JobTicket::ready(Arc::downgrade(&self.engine), Err(err));
        }
        let cfs = Arc::clone(&self.cfs);
        self.engine.start_foreground(engine::Work::Ingest {
            files: files.to_vec(),
            options: opts,
            check: Box::new(move |user_key| check_prefixed_cf(&cfs, user_key)),
        })
    }

    /// Flush all data to disk and shut down background threads.
    ///
    /// After a successful close, result-returning operations on this
    /// handle fail with [`Error::Closed`]. Calling `close` more than
    /// once is allowed.
    ///
    /// Close does not give the directory back: it stays locked, and opening
    /// it again fails, until this handle and every snapshot, iterator and
    /// transaction made from it have been dropped.
    ///
    /// Close syncs the write-ahead log and ends it with a record saying it
    /// is complete, so every write survives a power cut after `close`
    /// returns, under either durability mode. A close whose sync fails
    /// returns the error and leaves the database refusing writes until it
    /// is reopened, since what reached the device is unknown; the next
    /// open recovers it as it would after a crash. A database dropped
    /// without `close` is recovered the same way.
    ///
    /// A transaction still open when `close` begins is aborted: its
    /// [`Transaction::on_abort`](crate::Transaction::on_abort) callbacks and
    /// the database's [`TransactionHooks::on_abort`] run on this thread with
    /// [`AbortReason::Closed`], before the data is flushed, and the
    /// transaction's later commit fails with [`Error::Closed`]. A transaction
    /// that had begun to commit is left to finish. Those callbacks must not
    /// call back into the database.
    pub fn close(&self) -> Result<()> {
        self.engine.close().map_err(Error::from)
    }

    /// How many [`Snapshot`]s and snapshot-backed iterators are live, and a
    /// wait for the last of them to be released. The call itself never
    /// waits: it returns a [`SnapshotWait`] at once.
    ///
    /// A snapshot pins the SSTables it can see, so closing a database
    /// while one is outstanding leaves those files on disk and their
    /// readers open. An embedder that wants a clean shutdown has to wait
    /// for its readers to finish, and the handle is that wait: ask it
    /// ([`SnapshotWait::pinned`]), await it from a task, or block on it by
    /// name ([`SnapshotWait::wait`]). Either wait is for the release
    /// itself rather than a polled counter, so it costs nothing while it
    /// waits and adds no latency once the last reader is done.
    ///
    /// Counts pins, not handles. An iterator taken from a snapshot pins
    /// it again for as long as the iterator lives, so a `Snapshot` that
    /// has already been dropped can still be counted here, which is
    /// exactly the case an embedder tracking its own transaction objects
    /// would miss. An open transaction pins its snapshot too, until its
    /// commit is decided or it is rolled back; a
    /// [`Transaction::commit_nowait`](crate::Transaction::commit_nowait)
    /// commit holds it until the commit lands.
    ///
    /// The handle reports a count rather than an error because whether
    /// outstanding readers are a failure is the caller's decision:
    /// [`Db::close`] does not require this wait, and a database closed with
    /// snapshots live is consistent, just not tidy.
    ///
    /// ```
    /// # use std::time::Duration;
    /// # use regolith::{Db, Options};
    /// # fn main() -> regolith::Result<()> {
    /// # let dir = tempfile::tempdir().unwrap();
    /// let db = Db::open(dir.path(), Options::default())?;
    /// db.put(b"k", b"v")?;
    ///
    /// let snapshot = db.snapshot();
    /// let readers = db.wait_for_snapshots();
    /// assert_eq!(readers.pinned(), 1);
    /// assert_eq!(readers.wait(Duration::from_millis(50)), 1);
    ///
    /// drop(snapshot);
    /// assert!(readers.is_ready());
    /// assert_eq!(readers.wait(Duration::from_secs(5)), 0);
    /// db.close()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn wait_for_snapshots(&self) -> SnapshotWait {
        SnapshotWait::new(Arc::clone(&self.engine))
    }

    /// Test-only: number of SSTable files at `level`.
    #[cfg(test)]
    pub(crate) fn level_file_count(&self, level: usize) -> usize {
        self.engine.level_file_count(level)
    }

    /// Create a hard-linked [`Checkpoint`] of the database, as a job: the
    /// call returns a [`JobTicket`] at once and never waits on the
    /// compaction gate. The job runs where [`Db::compact_range`]'s does,
    /// and its ticket completes the same way.
    ///
    /// The job briefly flushes the active memtable and compacts the manifest
    /// before any files are linked; concurrent writers continue to make
    /// progress.
    pub fn checkpoint<P: AsRef<Path>>(&self, target_dir: P) -> JobTicket {
        if let Err(err) = self.ensure_writable() {
            return JobTicket::ready(Arc::downgrade(&self.engine), Err(err));
        }
        self.engine.start_foreground(engine::Work::Checkpoint {
            target: target_dir.as_ref().to_path_buf(),
        })
    }

    // ── column families ─────────────────────────────────────────────────

    /// Return a handle to the default column family. Always
    /// present - [`Db::open`] creates it if the database didn't
    /// already contain one.
    pub fn default_cf(&self) -> ColumnFamilyHandle {
        ColumnFamilyHandle {
            name: Arc::new(DEFAULT_CF_NAME.to_string()),
            id: DEFAULT_CF_ID,
        }
    }

    /// Look up a column family by name. Returns `None` when no CF
    /// with that name has been created (or if the CF was dropped).
    pub fn column_family(&self, name: &str) -> Option<ColumnFamilyHandle> {
        self.cfs.get(name)
    }

    /// Return the names of every live column family, including
    /// `"default"`. Order is unspecified.
    pub fn list_column_families(&self) -> Vec<String> {
        let mut names = self.cfs.names();
        names.sort();
        names
    }

    /// Create a new column family with `name`. The name must be
    /// non-empty and unique; creating a CF with an existing name
    /// returns the existing handle (idempotent).
    ///
    /// The new CF is persisted to the on-disk metadata before this
    /// call returns, so it survives a crash and a reopen. It becomes
    /// visible only once that write is committed, and concurrent creates
    /// of one name return the same handle. Ids are never reused: a name
    /// created again after a drop is a new family, and a handle to the
    /// old one stays refused.
    pub fn create_column_family(&self, name: &str) -> Result<ColumnFamilyHandle> {
        self.ensure_writable()?;
        if name.is_empty() {
            return Err(Error::invalid_argument(
                "column family name must not be empty",
            ));
        }
        if let Some(existing) = self.cfs.get(name) {
            return Ok(existing);
        }
        self.validate_prefixed_key_size(&meta::name_key(name))?;
        // Before the ordered step, so a write that cannot be admitted leaves
        // no name registered.
        self.admit_write(&WriteOptions::default())?;
        // The id is drawn, persisted and published in one ordered step, so a
        // racing create of the same name returns this family.
        self.engine
            .create_family(name, self.durability)
            .map_err(Error::from)
    }

    /// Drop a column family. Every key stored in the CF is removed
    /// via a single range tombstone (O(1) write work regardless of
    /// key count) and the CF name is unregistered so future
    /// lookups via [`Db::column_family`] return `None`. Space is
    /// physically reclaimed by the next compaction over the range.
    ///
    /// Dropping the default column family is not allowed and
    /// returns an error.
    ///
    /// A write to the family that races the drop either commits before
    /// it, and the range tombstone deletes it, or is refused with
    /// [`Error::InvalidColumnFamily`]; it never lands after the
    /// tombstone. Every later write, read or drop through a handle to the
    /// family is refused the same way. Two drops of one family race to
    /// one success.
    pub fn drop_column_family(&self, cf: ColumnFamilyHandle) -> Result<()> {
        self.ensure_writable()?;
        if cf.id() == DEFAULT_CF_ID {
            return Err(Error::invalid_argument(
                "cannot drop the default column family",
            ));
        }
        if cf.id() == META_CF_ID {
            return Err(Error::invalid_argument(
                "cannot drop the reserved metadata column family",
            ));
        }
        self.validate_cf_handle(&cf)?;
        self.admit_write(&WriteOptions::default())?;
        // The range tombstone and the meta delete commit as one group, so a
        // crash mid-drop leaves the CF fully present or fully removed, and
        // the family is retired in the same ordered step: a write racing the
        // drop is either deleted by the tombstone or refused, never committed
        // after it.
        self.engine
            .drop_family(&cf, self.durability)
            .map_err(Error::from)
    }

    /// Read `key` from column family `cf`. Same semantics as
    /// [`Db::get`] but scoped to the CF's keyspace.
    pub fn get_cf(&self, cf: &ColumnFamilyHandle, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(self.get_slice_cf(cf, key)?.map(DbSlice::into_vec))
    }

    /// Batched point lookup across a single CF.
    pub fn multi_get_cf(
        &self,
        cf: &ColumnFamilyHandle,
        keys: &[&[u8]],
    ) -> Result<Vec<Option<Vec<u8>>>> {
        self.validate_cf_handle(cf)?;
        let owned: Vec<Vec<u8>> = keys.iter().map(|k| prefix_key(cf.id(), k)).collect();
        let refs: Vec<&[u8]> = owned.iter().map(|k| k.as_slice()).collect();
        self.engine.multi_get_latest(&refs).map_err(Error::from)
    }

    /// Write `key → value` in column family `cf`.
    pub fn put_cf(&self, cf: &ColumnFamilyHandle, key: &[u8], value: &[u8]) -> Result<()> {
        self.ensure_writable()?;
        self.validate_cf_handle(cf)?;
        self.validate_write_kv_sizes(key, value)?;
        self.admit_write(&WriteOptions::default())?;
        let mut batch = BTreeMap::new();
        batch.insert(prefix_key(cf.id(), key), Some(value.to_vec()));
        self.engine
            .apply_grouped_batch(batch, Vec::new(), Vec::new(), self.durability, false)
            .map(|_| ())
            .map_err(Error::from)
    }

    /// Delete `key` in column family `cf`.
    pub fn delete_cf(&self, cf: &ColumnFamilyHandle, key: &[u8]) -> Result<()> {
        self.ensure_writable()?;
        self.validate_cf_handle(cf)?;
        self.validate_key_size(key)?;
        self.admit_write(&WriteOptions::default())?;
        let mut batch = BTreeMap::new();
        batch.insert(prefix_key(cf.id(), key), None);
        self.engine
            .apply_grouped_batch(batch, Vec::new(), Vec::new(), self.durability, false)
            .map(|_| ())
            .map_err(Error::from)
    }

    /// Delete every key in `[start, end)` in column family `cf`.
    ///
    /// An empty range is still a write: a read-only or closed handle
    /// rejects it with [`Error::ReadOnly`] or [`Error::Closed`] rather
    /// than returning `Ok`.
    pub fn delete_range_cf(&self, cf: &ColumnFamilyHandle, start: &[u8], end: &[u8]) -> Result<()> {
        self.ensure_writable()?;
        if start >= end {
            return Ok(());
        }
        self.validate_cf_handle(cf)?;
        self.validate_key_size(start)?;
        self.validate_key_size(end)?;
        self.admit_write(&WriteOptions::default())?;
        self.engine
            .apply_grouped_batch(
                BTreeMap::new(),
                vec![(prefix_key(cf.id(), start), prefix_key(cf.id(), end))],
                Vec::new(),
                self.durability,
                false,
            )
            .map(|_| ())
            .map_err(Error::from)
    }

    /// Layer a merge operand on top of `key` in column family `cf`.
    /// Requires [`Options::merge_operator`] to be set, and returns
    /// [`Error::NoMergeOperator`] without writing when it is not.
    pub fn merge_cf(&self, cf: &ColumnFamilyHandle, key: &[u8], operand: &[u8]) -> Result<()> {
        self.ensure_writable()?;
        self.engine.require_merge_operator()?;
        self.validate_cf_handle(cf)?;
        self.validate_write_kv_sizes(key, operand)?;
        self.admit_write(&WriteOptions::default())?;
        self.engine
            .apply_grouped_batch(
                BTreeMap::new(),
                Vec::new(),
                vec![(prefix_key(cf.id(), key), operand.to_vec())],
                self.durability,
                false,
            )
            .map(|_| ())
            .map_err(Error::from)
    }

    /// Scan a key range inside column family `cf`.
    ///
    /// Returned keys have the CF prefix stripped - they appear
    /// exactly as the caller supplied them on put. This convenience
    /// method materializes the entire range into memory. Prefer
    /// [`Db::iter_cf`] for streaming scans or [`Db::scan_page_cf`]
    /// when the caller needs an explicit page size.
    pub fn scan_cf(
        &self,
        cf: &ColumnFamilyHandle,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.validate_cf_handle(cf)?;
        let lo = match start {
            Some(s) => prefix_key(cf.id(), s),
            None => cf_lower_bound(cf.id()),
        };
        let hi = match end {
            Some(e) => prefix_key(cf.id(), e),
            None => cf_upper_bound(cf.id()),
        };
        // `new_iter_latest` loads the published view and *then* samples the
        // horizon. Sampling first and building the iterator after leaves a
        // window where a compaction no snapshot pins can drop the newest
        // version at or below the sampled sequence, after which the scan
        // finds only versions it must filter out and a key reads absent.
        let raw = collect_range(self.engine.new_iter_latest(), Some(&lo), Some(&hi))?;
        strip_cf_prefix_entries(raw)
    }

    /// Scan a bounded page inside column family `cf`.
    ///
    /// At most `limit` entries are materialized, and returned keys have
    /// the CF prefix stripped. When [`ScanPage::next_start`] is `Some`,
    /// pass that key back as `start` to continue the scan.
    pub fn scan_page_cf(
        &self,
        cf: &ColumnFamilyHandle,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        limit: usize,
    ) -> Result<ScanPage> {
        self.validate_cf_handle(cf)?;
        let lo = match start {
            Some(s) => prefix_key(cf.id(), s),
            None => cf_lower_bound(cf.id()),
        };
        let hi = match end {
            Some(e) => prefix_key(cf.id(), e),
            None => cf_upper_bound(cf.id()),
        };
        collect_page(self.engine.new_iter_latest(), &lo, &hi, limit).and_then(strip_cf_prefix_page)
    }

    /// Streaming iterator bounded to column family `cf`. The
    /// returned keys have the CF prefix stripped.
    pub fn iter_cf<'a>(&'a self, cf: &ColumnFamilyHandle) -> CfIter<'a> {
        let inner = self.raw_iter();
        if self.is_live_cf_handle(cf) {
            CfIter::new(inner, cf.id())
        } else {
            CfIter::invalid(inner, cf)
        }
    }

    /// Create a forward-only [`TailingIter`] over the default
    /// column family. Unlike [`Db::iter`], a tailing iterator
    /// sees writes that arrive after it was created and does not
    /// pin the database at a point in time - see [`TailingIter`]
    /// for the ordering rules.
    pub fn iter_tailing(&self) -> TailingIter {
        tailing::new_default(Arc::clone(&self.engine))
    }

    /// Create a forward-only [`TailingIter`] scoped to column
    /// family `cf`.
    pub fn iter_tailing_cf(&self, cf: &ColumnFamilyHandle) -> TailingIter {
        let engine = Arc::clone(&self.engine);
        if self.is_live_cf_handle(cf) {
            tailing::new_for_cf(engine, cf)
        } else {
            tailing::new_empty(engine, cf)
        }
    }

    pub(crate) fn engine(&self) -> &RegolithEngine {
        &self.engine
    }

    /// Clone the engine `Arc` - used by transaction facade types
    /// that need to carry an engine reference around independent
    /// of the owning `Db`'s lifetime. Internal-only.
    pub(crate) fn engine_arc(&self) -> Arc<RegolithEngine> {
        Arc::clone(&self.engine)
    }

    /// Database-global durability mode. Used by transaction
    /// commit code to choose fsync semantics.
    pub(crate) fn durability(&self) -> engine::DurabilityMode {
        self.durability
    }
}

/// Streaming iterator scoped to a single column family. Wraps a
/// regular [`Iter`] and bounds the scan to the CF's prefix range,
/// stripping the 4-byte CF prefix from every key before returning
/// it. Created by [`Db::iter_cf`] / [`Snapshot::iter_cf`].
pub struct CfIter<'a> {
    inner: Iter<'a>,
    cf_id: u32,
    upper_bound: Vec<u8>,
    valid_cf: bool,
    /// Why this iterator has no column family to read, when it has none.
    ///
    /// A handle that is not live is a detected error, and the rest of the CF
    /// read surface returns it. An iterator cannot, so it is held here and
    /// handed back by [`CfIter::status`]. `None` on every live iterator, so
    /// the path that works allocates nothing.
    invalid_cf: Option<Box<str>>,
}

impl<'a> CfIter<'a> {
    fn new(inner: Iter<'a>, cf_id: u32) -> Self {
        Self {
            invalid_cf: None,
            inner,
            cf_id,
            upper_bound: cf_upper_bound(cf_id),
            valid_cf: true,
        }
    }

    fn invalid(inner: Iter<'a>, cf: &ColumnFamilyHandle) -> Self {
        Self {
            inner,
            cf_id: DEFAULT_CF_ID,
            upper_bound: cf_upper_bound(DEFAULT_CF_ID),
            valid_cf: false,
            invalid_cf: Some(
                format!(
                    "column family handle '{}' with id {} is not live",
                    cf.name(),
                    cf.id()
                )
                .into_boxed_str(),
            ),
        }
    }

    /// Position the cursor at the first key in the CF.
    pub fn seek_to_first(&mut self) {
        if !self.valid_cf {
            return;
        }
        let lo = self.cf_id.to_be_bytes();
        self.inner.seek(&lo);
    }

    /// Position the cursor at the last key in the CF (or before
    /// the CF's upper bound if the CF is empty).
    pub fn seek_to_last(&mut self) {
        if !self.valid_cf {
            return;
        }
        self.inner.seek_to_last_before(&self.upper_bound);
    }

    /// Position the cursor at the first key `>= target` in the CF.
    pub fn seek(&mut self, target: &[u8]) {
        if !self.valid_cf {
            return;
        }
        self.inner.seek(&prefix_key(self.cf_id, target));
    }

    /// Position the cursor at the first key `>= target` in the CF, and end
    /// forward iteration before `upper_bound`, or at the end of the CF when
    /// there is none. See [`Iter::seek_bounded`].
    pub fn seek_bounded(&mut self, target: &[u8], upper_bound: Option<&[u8]>) {
        if !self.valid_cf {
            return;
        }
        let upper_bound = match upper_bound {
            Some(bound) => prefix_key(self.cf_id, bound),
            None => self.upper_bound.clone(),
        };
        self.inner
            .seek_bounded(&prefix_key(self.cf_id, target), &upper_bound);
    }

    /// Position the cursor at the last key `<= target` in the CF.
    pub fn seek_for_prev(&mut self, target: &[u8]) {
        if !self.valid_cf {
            return;
        }
        self.inner.seek_for_prev(&prefix_key(self.cf_id, target));
    }

    /// Position the cursor at the last key strictly below `end` in the CF,
    /// and set the scan direction to reverse.
    pub(crate) fn seek_before(&mut self, end: &[u8]) {
        if !self.valid_cf {
            return;
        }
        self.inner.seek_to_last_before(&prefix_key(self.cf_id, end));
    }

    /// Position the cursor at the first key in this CF that starts
    /// with `prefix`, and bound subsequent forward iteration to
    /// that prefix. Delegates to the underlying [`Iter::seek_prefix`],
    /// with `prefix` first re-scoped to include the CF prefix.
    pub fn seek_prefix(&mut self, prefix: &[u8]) {
        if !self.valid_cf {
            return;
        }
        self.inner.seek_prefix(&prefix_key(self.cf_id, prefix));
    }

    /// Advance the cursor forward. Invalidates the iterator if
    /// the next key crosses the CF's upper bound.
    pub fn next(&mut self) {
        if !self.valid_cf {
            return;
        }
        self.inner.next();
    }

    /// Move the cursor backward. Invalidates the iterator if
    /// the previous key crosses the CF's lower bound.
    pub fn prev(&mut self) {
        if !self.valid_cf {
            return;
        }
        self.inner.prev();
    }

    /// Run the seek or step a [`WouldBlock`] interrupted again, once the
    /// queue it names was polled. See [`Iter::resume`].
    pub fn resume(&mut self) {
        if !self.valid_cf {
            return;
        }
        self.inner.resume();
    }

    /// Whether the caller has positioned this cursor at all. See
    /// [`Iter::positioned`].
    pub fn positioned(&self) -> bool {
        self.inner.positioned()
    }

    /// Whether the cursor is positioned on a visible key within
    /// the CF.
    pub fn valid(&self) -> bool {
        if !self.valid_cf {
            return false;
        }
        let Some(k) = self.inner.key() else {
            return false;
        };
        if k < self.cf_id.to_be_bytes().as_slice() {
            return false;
        }
        if k >= self.upper_bound.as_slice() {
            return false;
        }
        true
    }

    /// Current key, with the CF prefix stripped.
    pub fn key(&self) -> Option<&[u8]> {
        if !self.valid() {
            return None;
        }
        self.inner.key().and_then(|k| k.get(4..))
    }

    /// Current value.
    pub fn value(&self) -> Option<&[u8]> {
        if !self.valid() {
            return None;
        }
        self.inner.value()
    }

    /// Current value as a [`DbSlice`], which outlives the cursor
    /// moving on. See [`Iter::value_slice`].
    pub fn value_slice(&self) -> Option<DbSlice> {
        if !self.valid() {
            return None;
        }
        self.inner.value_slice()
    }

    /// Why the walk stopped, or why it never started.
    ///
    /// A handle that is not live is reported here rather than dropped. The
    /// rest of the CF read surface (`get_cf`, `scan_cf`, `scan_page_cf`,
    /// `multi_get_cf`) returns that as an `Err`; an iterator has no way to,
    /// so without this a dropped column family, or a handle belonging to a
    /// different `Db`, would read as an empty one and report success.
    pub fn status(&self) -> Result<()> {
        if let Some(reason) = &self.invalid_cf {
            return Err(Error::invalid_column_family(reason.to_string()));
        }
        self.inner.status()
    }
}

/// Owned streaming iterator over a [`Snapshot`] in the default column
/// family. This is useful for adapters that need to return an owned
/// iterator object without tying the type to a borrowed snapshot
/// lifetime.
pub struct OwnedSnapshotIter {
    inner: CfIter<'static>,
    _snapshot: Snapshot,
}

impl OwnedSnapshotIter {
    fn new(snapshot: Snapshot) -> Self {
        let inner = Iter::<'static>::from_internal(snapshot.engine.new_iter_at(snapshot.seq))
            .with_stats(snapshot.engine.statistics_arc())
            .with_mode(snapshot.mode);
        Self {
            inner: CfIter::new(inner, DEFAULT_CF_ID),
            _snapshot: snapshot,
        }
    }

    /// Position the cursor at the first key in the default CF.
    pub fn seek_to_first(&mut self) {
        self.inner.seek_to_first();
    }

    /// Position the cursor at the last key in the default CF.
    pub fn seek_to_last(&mut self) {
        self.inner.seek_to_last();
    }

    /// Position the cursor at the first key `>= target`.
    pub fn seek(&mut self, target: &[u8]) {
        self.inner.seek(target);
    }

    /// Position the cursor at the first key `>= target`, and end forward
    /// iteration before `upper_bound`. See [`Iter::seek_bounded`].
    pub fn seek_bounded(&mut self, target: &[u8], upper_bound: &[u8]) {
        self.inner.seek_bounded(target, Some(upper_bound));
    }

    /// Position the cursor at the last key `<= target`.
    pub fn seek_for_prev(&mut self, target: &[u8]) {
        self.inner.seek_for_prev(target);
    }

    /// Position the cursor at the first key with `prefix`.
    pub fn seek_prefix(&mut self, prefix: &[u8]) {
        self.inner.seek_prefix(prefix);
    }

    /// Advance the cursor forward.
    pub fn next(&mut self) {
        self.inner.next();
    }

    /// Move the cursor backward.
    pub fn prev(&mut self) {
        self.inner.prev();
    }

    /// Run the seek or step a [`WouldBlock`] interrupted again, once the
    /// queue it names was polled. See [`Iter::resume`].
    ///
    /// A cursor made from a [`ReadMode::CacheOnly`] snapshot stops on a block
    /// the cache does not hold: [`valid`](Self::valid) turns `false` and
    /// [`status`](Self::status) reports [`Error::WouldBlock`]. This carries
    /// the walk on from exactly where it stopped, so a walk interrupted any
    /// number of times yields the same entries as one that never was.
    pub fn resume(&mut self) {
        self.inner.resume();
    }

    /// Whether the caller has positioned this cursor at all. See
    /// [`Iter::positioned`].
    pub fn positioned(&self) -> bool {
        self.inner.positioned()
    }

    /// Whether the cursor is positioned on a visible key.
    pub fn valid(&self) -> bool {
        self.inner.valid()
    }

    /// Current key.
    pub fn key(&self) -> Option<&[u8]> {
        self.inner.key()
    }

    /// Current value.
    pub fn value(&self) -> Option<&[u8]> {
        self.inner.value()
    }

    /// Current value as a [`DbSlice`], which outlives the cursor
    /// moving on. See [`Iter::value_slice`].
    pub fn value_slice(&self) -> Option<DbSlice> {
        self.inner.value_slice()
    }

    /// Propagate any I/O error from the underlying iterator.
    pub fn status(&self) -> Result<()> {
        self.inner.status()
    }
}

/// A lazy, bounded scan over a key range.
///
/// Returned by [`Db::scan_stream`], [`Snapshot::scan_stream`] and
/// [`Snapshot::into_scan_stream`]. Holds one entry at a time rather than
/// the range, so a caller that stops early pays only for what it read: the
/// opposite of [`Db::scan`], which reads the whole range up front. The
/// value is a [`DbSlice`], so no value bytes are copied.
///
/// Each item is a `Result`: a scan that fails in the middle of the range
/// (a corrupt block, a failed read, a key the merge operator declines) ends
/// with one `Err` item, so a failed scan is never read as a short one.
/// After the `Err`, or after the end of the range, the stream returns
/// `None`. The entries before the `Err` are a prefix of the range and the
/// rest was never read; `collect::<Result<Vec<_>>>()` keeps the rows only
/// when the whole range was read.
///
/// The one `Err` that does not end the stream is [`Error::WouldBlock`], from
/// a stream of a [`ReadMode::CacheOnly`] snapshot: poll the queue it names
/// and take the next item, and the scan carries on exactly where it stopped.
///
/// The scan runs against a pinned snapshot, so writes that land while it
/// is being drained are invisible to it and the range cannot shift
/// underneath the cursor.
pub struct ScanStream {
    entries: Entries<OwnedSnapshotIter>,
    /// Exclusive upper bound in user-visible form, or `None` for the end
    /// of the column family.
    end: Option<Vec<u8>>,
    done: bool,
}

impl Iterator for ScanStream {
    type Item = Result<(Vec<u8>, DbSlice)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        // `Entries` hands a failure out once and is finished after it, so
        // passing the item on is all it takes to keep that contract here.
        let (key, value) = match self.entries.next()? {
            Ok(entry) => entry,
            Err(error) => return Some(Err(error)),
        };
        if let Some(end) = &self.end
            && key.as_slice() >= end.as_slice()
        {
            // Keys come out ascending, so the first key at or past the
            // bound ends the scan; nothing after it can be in range.
            self.done = true;
            return None;
        }
        Some(Ok((key, value)))
    }
}

impl std::fmt::Debug for ScanStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanStream")
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

/// Ordered entries drained from a cursor, from its current position on.
///
/// Returned by the `IntoIterator` impls on the cursors and by
/// [`OwnedSnapshotIter::entries`] / [`OwnedSnapshotIter::entries_rev`].
/// The value is a [`DbSlice`], so iterating copies keys but never value
/// bytes: a key is reassembled from its prefix-compressed form into a
/// buffer the cursor owns and has to be copied out, while a value is
/// stored whole and can be handed over by reference.
///
/// Each item is a `Result`: a walk that failed in the middle of the range
/// ends with one `Err` item, so a failed scan is never read as a short one.
/// After it, or after the end of the range, the iterator returns `None`.
///
/// [`Error::WouldBlock`] is the one error that does not end the walk: a
/// cursor read in [`ReadMode::CacheOnly`] hands it out when it needs a block
/// the cache does not hold. Poll the queue it names, then ask for the next
/// item: the cursor resumes where it stopped, and the items that follow are
/// exactly those an uninterrupted walk would have yielded.
///
/// This is the seam to build a `Stream` on. regolith's IO is synchronous
/// and it requires no async runtime, so wrapping a ready iterator with
/// `futures::stream::iter` belongs where the async context is rather than
/// here, where it would add a dependency and never yield.
pub struct Entries<C> {
    cursor: C,
    started: bool,
    reverse: bool,
    /// The range ended or the walk failed; nothing more is read.
    ended: bool,
    /// The last item was a `WouldBlock`: the next one resumes the cursor
    /// instead of stepping it.
    blocked: bool,
}

impl<C> Entries<C> {
    fn new(cursor: C, reverse: bool) -> Self {
        Self {
            cursor,
            started: false,
            reverse,
            ended: false,
            blocked: false,
        }
    }

    /// Give the cursor back, positioned wherever iteration stopped.
    pub fn into_cursor(self) -> C {
        self.cursor
    }
}

/// A cursor already positioned by `seek` is left where it is on the first
/// step, so seeking and then iterating resumes from the seek instead of
/// restarting at the end of the range.
macro_rules! impl_entries {
    ($cursor:ty $(, $lt:lifetime)?) => {
        impl$(<$lt>)? Iterator for Entries<$cursor> {
            type Item = Result<(Vec<u8>, DbSlice)>;

            fn next(&mut self) -> Option<Self::Item> {
                if self.ended {
                    return None;
                }
                if self.blocked {
                    // The step or seek the last item interrupted runs again.
                    self.blocked = false;
                    self.cursor.resume();
                } else if self.started {
                    if self.reverse {
                        self.cursor.prev();
                    } else {
                        self.cursor.next();
                    }
                } else {
                    self.started = true;
                    // `positioned`, not `valid`. A cursor the caller seeked
                    // past the end of the range is invalid but positioned,
                    // and seeking it again would hand back the very rows the
                    // caller seeked away from.
                    if !self.cursor.positioned() {
                        if self.reverse {
                            self.cursor.seek_to_last();
                        } else {
                            self.cursor.seek_to_first();
                        }
                    }
                }
                if !self.cursor.valid() {
                    // A cursor goes invalid for two reasons that look
                    // identical from the position alone: the range ended, or
                    // the walk failed. The status tells them apart, and a
                    // failure is handed out as an item so it cannot pass for
                    // the end of the range. A `WouldBlock` pauses the walk
                    // rather than ending it.
                    return match self.cursor.status() {
                        Ok(()) => {
                            self.ended = true;
                            None
                        }
                        Err(error @ Error::WouldBlock(_)) => {
                            self.blocked = true;
                            Some(Err(error))
                        }
                        Err(error) => {
                            self.ended = true;
                            Some(Err(error))
                        }
                    };
                }
                let key = self.cursor.key()?.to_vec();
                let value = self.cursor.value_slice()?;
                Some(Ok((key, value)))
            }
        }

        impl$(<$lt>)? Entries<$cursor> {
            /// Why the walk stopped.
            ///
            /// `Ok(())` means the range ended. An error means it did not:
            /// the entries handed out are a prefix of the range, and the
            /// rest was not read. Iterating alone cannot tell the two
            /// apart, so a caller that must not silently lose rows checks
            /// this once the iteration finishes.
            pub fn status(&self) -> Result<()> {
                self.cursor.status()
            }
        }

        impl$(<$lt>)? IntoIterator for $cursor {
            type Item = Result<(Vec<u8>, DbSlice)>;
            type IntoIter = Entries<$cursor>;

            fn into_iter(self) -> Self::IntoIter {
                Entries::new(self, false)
            }
        }
    };
}

impl_entries!(OwnedSnapshotIter);
impl_entries!(CfIter<'a>, 'a);

impl OwnedSnapshotIter {
    /// Iterate forward from the cursor's current position.
    pub fn entries(self) -> Entries<Self> {
        Entries::new(self, false)
    }

    /// Iterate backward from the cursor's current position.
    pub fn entries_rev(self) -> Entries<Self> {
        Entries::new(self, true)
    }
}

impl<'a> CfIter<'a> {
    /// Iterate forward from the cursor's current position.
    pub fn entries(self) -> Entries<CfIter<'a>> {
        Entries::new(self, false)
    }

    /// Iterate backward from the cursor's current position.
    pub fn entries_rev(self) -> Entries<CfIter<'a>> {
        Entries::new(self, true)
    }
}

/// A point-in-time snapshot for consistent reads.
///
/// A snapshot reads in a [`ReadMode`], `Blocking` unless
/// [`Snapshot::with_read_mode`] says otherwise, and every iterator, cursor and
/// stream made from it reads in the same mode.
pub struct Snapshot {
    engine: Arc<RegolithEngine>,
    cfs: Arc<CfRegistry>,
    seq: u64,
    /// Where this snapshot's block-cache misses go.
    mode: ReadMode,
    /// Where `seq` is pinned in the engine's snapshot registry. The drop
    /// releases exactly there, whichever thread it runs on.
    pin: engine::SnapshotPin,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        // Release the pin this snapshot held in the engine's
        // compaction GC registry. Compaction is now free to drop any
        // version it was keeping alive for this snapshot's sake,
        // subject to other live snapshots that may still pin older
        // seqs.
        self.engine.release_snapshot(self.pin.take());
    }
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot")
            .field("seq", &self.seq)
            .finish_non_exhaustive()
    }
}

impl Snapshot {
    fn clone_pin(&self) -> Self {
        Self {
            engine: Arc::clone(&self.engine),
            cfs: Arc::clone(&self.cfs),
            seq: self.seq,
            mode: self.mode,
            pin: self.engine.clone_snapshot_pin(&self.pin),
        }
    }

    /// This snapshot, reading in `mode` from now on.
    ///
    /// Under [`ReadMode::CacheOnly`] no read through this snapshot touches
    /// the device: a point read, a scan, an iterator step or a stream item
    /// that needs a block the cache does not hold returns
    /// [`Error::WouldBlock`], and the block is read when the queue the mode
    /// names is polled. Run the read again after that poll; an iterator or a
    /// stream resumes where it stopped. A point read run again reads the same
    /// sequence, so it sees what the first attempt would have seen.
    pub fn with_read_mode(mut self, mode: ReadMode) -> Self {
        self.mode = mode;
        self
    }

    /// The mode this snapshot reads in.
    pub fn read_mode(&self) -> ReadMode {
        self.mode
    }

    /// Run `read` in this snapshot's mode. `Blocking` sets nothing.
    #[inline]
    fn reading<T>(&self, read: impl FnOnce() -> T) -> T {
        let _scope = self.mode.scope();
        read()
    }

    fn validate_cf_handle(&self, cf: &ColumnFamilyHandle) -> Result<()> {
        if self.cfs.is_live_handle(cf) {
            Ok(())
        } else {
            Err(invalid_cf_handle_error(cf))
        }
    }

    fn is_live_cf_handle(&self, cf: &ColumnFamilyHandle) -> bool {
        self.cfs.is_live_handle(cf)
    }

    /// The engine sequence this snapshot reads at.
    ///
    /// Every write with a sequence at or below this value is visible here, and
    /// every later write is not. Pairs with [`Db::write_sequenced`], which
    /// returns the sequence a batch committed at, so an upper layer can order
    /// its own versions against regolith's without serializing commits behind a
    /// lock of its own: the horizon publishes atomically inside the write, and
    /// a snapshot captures it atomically here.
    pub fn sequence(&self) -> u64 {
        self.seq
    }

    /// Get the value for a key at this snapshot (default CF).
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(self.get_slice(key)?.map(DbSlice::into_vec))
    }

    /// Read a value at this snapshot without copying it. See
    /// [`Db::get_slice`] and [`DbSlice`].
    pub fn get_slice(&self, key: &[u8]) -> Result<Option<DbSlice>> {
        self.lookup_slice(&LookupKey::new(DEFAULT_CF_ID, key, self.seq))
    }

    /// [`Snapshot::get_slice`] scoped to a column family.
    pub fn get_slice_cf(&self, cf: &ColumnFamilyHandle, key: &[u8]) -> Result<Option<DbSlice>> {
        self.validate_cf_handle(cf)?;
        self.lookup_slice(&LookupKey::new(cf.id(), key, self.seq))
    }

    fn lookup_slice(&self, lk: &LookupKey) -> Result<Option<DbSlice>> {
        self.reading(|| self.engine.get_slice(lk))
            .map_err(|err| map_point_read_error(err, lk.prefixed_user_key()))
    }

    /// Whether a live value exists for `key` at this snapshot. See
    /// [`Db::has`] for what this does and does not avoid.
    pub fn has(&self, key: &[u8]) -> Result<bool> {
        Ok(self.get_size(key)?.is_some())
    }

    /// [`Snapshot::has`] scoped to a column family.
    pub fn has_cf(&self, cf: &ColumnFamilyHandle, key: &[u8]) -> Result<bool> {
        Ok(self.get_size_cf(cf, key)?.is_some())
    }

    /// Length in bytes of the live value for `key` at this snapshot,
    /// or `None` when there is none. See [`Db::get_size`].
    pub fn get_size(&self, key: &[u8]) -> Result<Option<usize>> {
        self.lookup_size(&LookupKey::new(DEFAULT_CF_ID, key, self.seq), None)
    }

    /// [`Snapshot::get_size`] scoped to a column family.
    pub fn get_size_cf(&self, cf: &ColumnFamilyHandle, key: &[u8]) -> Result<Option<usize>> {
        self.validate_cf_handle(cf)?;
        self.lookup_size(&LookupKey::new(cf.id(), key, self.seq), None)
    }

    /// Snapshot-scoped [`Db::get_size_with_limit`], with the same per-block guard
    /// and exclusions; not a total-memory or time bound.
    pub fn get_size_with_limit(
        &self,
        key: &[u8],
        max_data_block_bytes: usize,
    ) -> Result<Option<usize>> {
        self.lookup_size(
            &LookupKey::new(DEFAULT_CF_ID, key, self.seq),
            Some(max_data_block_bytes),
        )
    }

    /// [`Snapshot::get_size_with_limit`] scoped to a column family.
    pub fn get_size_cf_with_limit(
        &self,
        cf: &ColumnFamilyHandle,
        key: &[u8],
        max_data_block_bytes: usize,
    ) -> Result<Option<usize>> {
        self.validate_cf_handle(cf)?;
        self.lookup_size(
            &LookupKey::new(cf.id(), key, self.seq),
            Some(max_data_block_bytes),
        )
    }

    fn lookup_size(&self, lk: &LookupKey, limit: Option<usize>) -> Result<Option<usize>> {
        self.reading(|| self.engine.get_size(lk, limit))
            .map_err(|err| map_point_read_error(err, lk.prefixed_user_key()))
    }

    /// Batched point lookup anchored at this snapshot (default CF).
    pub fn multi_get(&self, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>> {
        let owned: Vec<Vec<u8>> = keys.iter().map(|k| prefix_key(DEFAULT_CF_ID, k)).collect();
        let refs: Vec<&[u8]> = owned.iter().map(|k| k.as_slice()).collect();
        self.reading(|| self.engine.multi_get_at(&refs, self.seq))
            .map_err(Error::from)
    }

    /// Scan a key range at this snapshot (default CF).
    ///
    /// This convenience method materializes the entire range into
    /// memory. Prefer [`Snapshot::iter`] for streaming scans or
    /// [`Snapshot::scan_page`] when the caller needs an explicit page
    /// size.
    pub fn scan(
        &self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let lo = match start {
            Some(s) => prefix_key(DEFAULT_CF_ID, s),
            None => cf_lower_bound(DEFAULT_CF_ID),
        };
        let hi = match end {
            Some(e) => prefix_key(DEFAULT_CF_ID, e),
            None => cf_upper_bound(DEFAULT_CF_ID),
        };
        let raw = self
            .reading(|| collect_range(self.engine.new_iter_at(self.seq), Some(&lo), Some(&hi)))?;
        strip_cf_prefix_entries(raw)
    }

    /// Scan a bounded page at this snapshot (default CF).
    ///
    /// At most `limit` entries are materialized. When
    /// [`ScanPage::next_start`] is `Some`, pass that key back as
    /// `start` to continue the same snapshot-consistent range.
    pub fn scan_page(
        &self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        limit: usize,
    ) -> Result<ScanPage> {
        let lo = match start {
            Some(s) => prefix_key(DEFAULT_CF_ID, s),
            None => cf_lower_bound(DEFAULT_CF_ID),
        };
        let hi = match end {
            Some(e) => prefix_key(DEFAULT_CF_ID, e),
            None => cf_upper_bound(DEFAULT_CF_ID),
        };
        self.reading(|| collect_page(self.engine.new_iter_at(self.seq), &lo, &hi, limit))
            .and_then(strip_cf_prefix_page)
    }

    /// Create a streaming iterator anchored at this snapshot
    /// (default CF). Keys returned have the CF prefix stripped.
    pub fn iter(&self) -> CfIter<'_> {
        CfIter::new(
            Iter::from_internal(self.engine.new_iter_at(self.seq))
                .with_stats(self.engine.statistics_arc())
                .with_mode(self.mode),
            DEFAULT_CF_ID,
        )
    }

    /// Scan a key range lazily against this snapshot.
    ///
    /// The streaming counterpart to [`Snapshot::scan`]. The whole walk
    /// sees one point in time, because the snapshot stays pinned for as
    /// long as the stream lives.
    pub fn scan_stream(&self, start: Option<&[u8]>, end: Option<&[u8]>) -> ScanStream {
        self.clone_pin().into_scan_stream(start, end)
    }

    /// [`Snapshot::scan_stream`], consuming the snapshot instead of
    /// pinning a second handle on it.
    pub fn into_scan_stream(self, start: Option<&[u8]>, end: Option<&[u8]>) -> ScanStream {
        let mut cursor = self.into_owned_iter();
        match (start, end) {
            (start, Some(end)) => cursor.seek_bounded(start.unwrap_or_default(), end),
            (Some(start), None) => cursor.seek(start),
            (None, None) => cursor.seek_to_first(),
        }
        let end = end.map(<[u8]>::to_vec);
        ScanStream {
            entries: Entries::new(cursor, false),
            end,
            done: false,
        }
    }

    /// Create an owned streaming iterator at this snapshot's sequence
    /// (default CF). The iterator takes its own snapshot pin, so the
    /// original snapshot remains usable for later reads.
    pub fn owned_iter(&self) -> OwnedSnapshotIter {
        OwnedSnapshotIter::new(self.clone_pin())
    }

    /// Consume this snapshot and create an owned streaming iterator
    /// (default CF). The iterator keeps the snapshot pin alive for its
    /// own lifetime, so it can be stored in trait objects that cannot
    /// express a borrow from `Snapshot`.
    pub fn into_owned_iter(self) -> OwnedSnapshotIter {
        OwnedSnapshotIter::new(self)
    }

    /// CF-scoped get at this snapshot.
    pub fn get_cf(&self, cf: &ColumnFamilyHandle, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(self.get_slice_cf(cf, key)?.map(DbSlice::into_vec))
    }

    /// CF-scoped multi_get at this snapshot.
    pub fn multi_get_cf(
        &self,
        cf: &ColumnFamilyHandle,
        keys: &[&[u8]],
    ) -> Result<Vec<Option<Vec<u8>>>> {
        self.validate_cf_handle(cf)?;
        let owned: Vec<Vec<u8>> = keys.iter().map(|k| prefix_key(cf.id(), k)).collect();
        let refs: Vec<&[u8]> = owned.iter().map(|k| k.as_slice()).collect();
        self.reading(|| self.engine.multi_get_at(&refs, self.seq))
            .map_err(Error::from)
    }

    /// CF-scoped scan at this snapshot.
    ///
    /// Returned keys have the CF prefix stripped. This convenience
    /// method materializes the entire range into memory. Prefer
    /// [`Snapshot::iter_cf`] for streaming scans or
    /// [`Snapshot::scan_page_cf`] when the caller needs an explicit
    /// page size.
    pub fn scan_cf(
        &self,
        cf: &ColumnFamilyHandle,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.validate_cf_handle(cf)?;
        let lo = match start {
            Some(s) => prefix_key(cf.id(), s),
            None => cf_lower_bound(cf.id()),
        };
        let hi = match end {
            Some(e) => prefix_key(cf.id(), e),
            None => cf_upper_bound(cf.id()),
        };
        let raw = self
            .reading(|| collect_range(self.engine.new_iter_at(self.seq), Some(&lo), Some(&hi)))?;
        strip_cf_prefix_entries(raw)
    }

    /// CF-scoped bounded page at this snapshot.
    ///
    /// At most `limit` entries are materialized, and returned keys have
    /// the CF prefix stripped. When [`ScanPage::next_start`] is `Some`,
    /// pass that key back as `start` to continue the same
    /// snapshot-consistent range.
    pub fn scan_page_cf(
        &self,
        cf: &ColumnFamilyHandle,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        limit: usize,
    ) -> Result<ScanPage> {
        self.validate_cf_handle(cf)?;
        let lo = match start {
            Some(s) => prefix_key(cf.id(), s),
            None => cf_lower_bound(cf.id()),
        };
        let hi = match end {
            Some(e) => prefix_key(cf.id(), e),
            None => cf_upper_bound(cf.id()),
        };
        self.reading(|| collect_page(self.engine.new_iter_at(self.seq), &lo, &hi, limit))
            .and_then(strip_cf_prefix_page)
    }

    /// CF-scoped streaming iterator at this snapshot.
    pub fn iter_cf<'a>(&'a self, cf: &ColumnFamilyHandle) -> CfIter<'a> {
        let inner = Iter::from_internal(self.engine.new_iter_at(self.seq))
            .with_stats(self.engine.statistics_arc())
            .with_mode(self.mode);
        if self.is_live_cf_handle(cf) {
            CfIter::new(inner, cf.id())
        } else {
            CfIter::invalid(inner, cf)
        }
    }
}

/// Collect a bounded range of `(user_key, value)` pairs via the streaming
/// iterator. This is the engine of `Db::scan` / `Snapshot::scan`; the
/// dedicated method exists so both callers share one merge implementation.
fn collect_range(
    mut iter: crate::engine::iterator::RegolithIterator,
    start: Option<&[u8]>,
    end: Option<&[u8]>,
) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    match (start, end) {
        (start, Some(end)) => iter.seek_bounded(start.unwrap_or_default(), end),
        (Some(start), None) => iter.seek(start),
        (None, None) => iter.seek_to_first(),
    }
    iter.status().map_err(Error::from)?;

    let mut out = Vec::new();
    while iter.valid() {
        let (Some(k), Some(v)) = (iter.key(), iter.value()) else {
            break;
        };
        if let Some(e) = end
            && k >= e
        {
            break;
        }
        out.push((k.to_vec(), v.to_vec()));
        iter.next();
    }
    iter.status().map_err(Error::from)?;
    Ok(out)
}

fn collect_page(
    mut iter: crate::engine::iterator::RegolithIterator,
    start: &[u8],
    end: &[u8],
    limit: usize,
) -> Result<ScanPage> {
    if limit == 0 {
        return Err(invalid_input_error(
            "scan page limit must be greater than zero",
        ));
    }

    iter.seek_bounded(start, end);
    iter.status().map_err(Error::from)?;

    let mut entries = Vec::new();
    let mut next_start = None;
    while iter.valid() {
        let (Some(k), Some(v)) = (iter.key(), iter.value()) else {
            break;
        };
        if k >= end {
            break;
        }
        if entries.len() == limit {
            next_start = Some(k.to_vec());
            break;
        }
        entries.push((k.to_vec(), v.to_vec()));
        iter.next();
    }
    iter.status().map_err(Error::from)?;
    Ok(ScanPage {
        entries,
        next_start,
    })
}

fn strip_cf_prefix_page(page: ScanPage) -> Result<ScanPage> {
    let entries = strip_cf_prefix_entries(page.entries)?;
    let next_start = page
        .next_start
        .map(|k| strip_cf_prefix_key(&k))
        .transpose()?;
    Ok(ScanPage {
        entries,
        next_start,
    })
}

/// One ordered operation in a [`WriteBatch`].
#[derive(Clone, Debug)]
pub(crate) enum WriteBatchOp {
    /// Write a value for `key`.
    Put { key: Vec<u8>, value: Vec<u8> },
    /// Delete the point value for `key`.
    Delete { key: Vec<u8> },
    /// Delete every key in `[start, end)`.
    DeleteRange { start: Vec<u8>, end: Vec<u8> },
    /// Add one merge operand for `key`.
    Merge { key: Vec<u8>, operand: Vec<u8> },
}

impl WriteBatchOp {
    /// Heap bytes this op holds. Drives the streaming writer's flush
    /// budget, so it counts what is buffered now, not what the op will
    /// encode to later.
    pub(crate) fn buffered_bytes(&self) -> usize {
        match self {
            Self::Put { key, value } => key.len() + value.len(),
            Self::Delete { key } => key.len(),
            Self::DeleteRange { start, end } => start.len() + end.len(),
            Self::Merge { key, operand } => key.len() + operand.len(),
        }
    }
}

/// A batch of write operations to apply atomically.
///
/// A batch is one record in the write-ahead log, and a record may be at
/// most 1 GiB: roughly the batch's keys and values plus a few tens of
/// bytes per operation. [`Db::write`] refuses a larger batch with
/// [`Error::InvalidArgument`] before applying any of it; split it into
/// smaller batches. A batch written with `WriteOptions::disable_wal`
/// logs nothing and is not subject to this limit.
#[derive(Clone, Debug, Default)]
pub struct WriteBatch {
    ops: Vec<WriteBatchOp>,
}

impl WriteBatch {
    /// Create an empty write batch.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a put operation to the batch (default column family).
    pub fn put(&mut self, key: &[u8], value: &[u8]) {
        self.put_owned(key, value.to_vec());
    }

    /// [`WriteBatch::put`] for a value the caller already owns.
    ///
    /// Takes the buffer instead of copying it, which is what a producer
    /// that just built the bytes wants. The key is still copied because
    /// it is rewritten with a column-family prefix, so there is nothing
    /// to hand over.
    pub fn put_owned(&mut self, key: &[u8], value: Vec<u8>) {
        self.ops.push(WriteBatchOp::Put {
            key: prefix_key(DEFAULT_CF_ID, key),
            value,
        });
    }

    /// Bytes this batch is holding: the key and value of every buffered
    /// operation. What a caller bounds when it decides to flush.
    pub fn buffered_bytes(&self) -> usize {
        self.ops.iter().map(WriteBatchOp::buffered_bytes).sum()
    }

    /// Add a delete operation to the batch (default column family).
    pub fn delete(&mut self, key: &[u8]) {
        self.ops.push(WriteBatchOp::Delete {
            key: prefix_key(DEFAULT_CF_ID, key),
        });
    }

    /// Delete every key in the half-open range `[start, end)` in
    /// the default column family.
    ///
    /// When the batch is applied, the range delete is ordered with
    /// the other batch operations, so later puts inside the range
    /// remain live while earlier puts are shadowed. Calls with
    /// `start >= end` are ignored.
    pub fn delete_range(&mut self, start: &[u8], end: &[u8]) {
        if start >= end {
            return;
        }
        self.ops.push(WriteBatchOp::DeleteRange {
            start: prefix_key(DEFAULT_CF_ID, start),
            end: prefix_key(DEFAULT_CF_ID, end),
        });
    }

    /// Add a merge operand for `key` in the default column family.
    /// Requires the database to be configured with a
    /// [`MergeOperator`]; the operand is layered on top of any
    /// existing value or merge chain and collapsed at read time.
    /// Multiple merges on the same key in a single batch are
    /// allowed and applied in insertion order.
    ///
    /// A batch does not know its database, so the operator is checked when
    /// the batch is applied: [`Db::write`] returns [`Error::NoMergeOperator`]
    /// and applies none of the batch when the database has no operator.
    pub fn merge(&mut self, key: &[u8], operand: &[u8]) {
        self.ops.push(WriteBatchOp::Merge {
            key: prefix_key(DEFAULT_CF_ID, key),
            operand: operand.to_vec(),
        });
    }

    /// Add a put scoped to column family `cf`.
    pub fn put_cf(&mut self, cf: &ColumnFamilyHandle, key: &[u8], value: &[u8]) {
        self.ops.push(WriteBatchOp::Put {
            key: prefix_key(cf.id(), key),
            value: value.to_vec(),
        });
    }

    /// Add a delete scoped to column family `cf`.
    pub fn delete_cf(&mut self, cf: &ColumnFamilyHandle, key: &[u8]) {
        self.ops.push(WriteBatchOp::Delete {
            key: prefix_key(cf.id(), key),
        });
    }

    /// Add a range delete scoped to column family `cf`.
    pub fn delete_range_cf(&mut self, cf: &ColumnFamilyHandle, start: &[u8], end: &[u8]) {
        if start >= end {
            return;
        }
        self.ops.push(WriteBatchOp::DeleteRange {
            start: prefix_key(cf.id(), start),
            end: prefix_key(cf.id(), end),
        });
    }

    /// Add a merge operand scoped to column family `cf`. As for
    /// [`WriteBatch::merge`], applying the batch to a database with no merge
    /// operator returns [`Error::NoMergeOperator`].
    pub fn merge_cf(&mut self, cf: &ColumnFamilyHandle, key: &[u8], operand: &[u8]) {
        self.ops.push(WriteBatchOp::Merge {
            key: prefix_key(cf.id(), key),
            operand: operand.to_vec(),
        });
    }

    /// Insert an already-prefixed put (internal use by wrappers
    /// like `DbWithTtl` that iterate a source batch's raw entries
    /// and rebuild a new batch without re-applying the CF prefix).
    pub(crate) fn insert_raw_put(&mut self, prefixed_key: Vec<u8>, value: Vec<u8>) {
        self.ops.push(WriteBatchOp::Put {
            key: prefixed_key,
            value,
        });
    }

    /// Insert an already-prefixed delete.
    pub(crate) fn insert_raw_delete(&mut self, prefixed_key: Vec<u8>) {
        self.ops.push(WriteBatchOp::Delete { key: prefixed_key });
    }

    /// Insert an already-prefixed range delete.
    pub(crate) fn insert_raw_range_delete(
        &mut self,
        prefixed_start: Vec<u8>,
        prefixed_end: Vec<u8>,
    ) {
        self.ops.push(WriteBatchOp::DeleteRange {
            start: prefixed_start,
            end: prefixed_end,
        });
    }

    /// Insert an already-prefixed merge operand.
    pub(crate) fn insert_raw_merge(&mut self, prefixed_key: Vec<u8>, operand: Vec<u8>) {
        self.ops.push(WriteBatchOp::Merge {
            key: prefixed_key,
            operand,
        });
    }

    /// Number of point operations in the batch. Repeated operations
    /// on the same key are counted separately. Range deletes and
    /// merges are counted separately via
    /// [`WriteBatch::range_delete_count`] and
    /// [`WriteBatch::merge_count`].
    pub fn len(&self) -> usize {
        self.ops
            .iter()
            .filter(|op| matches!(op, WriteBatchOp::Put { .. } | WriteBatchOp::Delete { .. }))
            .count()
    }

    /// Number of range-delete operations in the batch.
    pub fn range_delete_count(&self) -> usize {
        self.ops
            .iter()
            .filter(|op| matches!(op, WriteBatchOp::DeleteRange { .. }))
            .count()
    }

    /// Number of merge operations in the batch.
    pub fn merge_count(&self) -> usize {
        self.ops
            .iter()
            .filter(|op| matches!(op, WriteBatchOp::Merge { .. }))
            .count()
    }

    /// Whether the batch contains no operations of any kind.
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

#[cfg(test)]
mod write_limit_tests;

#[cfg(all(test, not(target_family = "wasm")))]
mod cf_race_tests;

#[cfg(all(test, not(target_family = "wasm")))]
mod prefix_form_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn open_tmp() -> (Db, TempDir) {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), Options::default()).unwrap();
        (db, dir)
    }

    fn first_wal_path(dir: &TempDir) -> PathBuf {
        let mut entries: Vec<_> = std::fs::read_dir(dir.path().join("wal"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("log"))
            .collect();
        entries.sort_by_key(|entry| entry.path());
        entries.into_iter().next().unwrap().path()
    }

    #[test]
    fn test_db_open_rejects_second_writer_on_same_directory() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), Options::default()).unwrap();

        let err = Db::open(dir.path(), Options::default()).unwrap_err();
        match err {
            Error::Io(io) => {
                assert_eq!(io.kind(), std::io::ErrorKind::AlreadyExists);
                assert!(io.to_string().contains("already locked"));
            }
            other => panic!("expected lock I/O error, got {other:?}"),
        }

        db.put(b"k", b"v").unwrap();
        drop(db);

        let reopened = Db::open(dir.path(), Options::default()).unwrap();
        assert_eq!(reopened.get(b"k").unwrap(), Some(b"v".to_vec()));
    }

    #[test]
    fn test_db_open_rejects_invalid_options() {
        let dir = TempDir::new().unwrap();
        let err = Db::open(dir.path(), Options::default().write_buffer_size(0)).unwrap_err();
        match err {
            Error::InvalidArgument(message) => assert!(message.contains("write_buffer_size")),
            other => panic!("expected invalid argument, got {other:?}"),
        }
    }

    #[test]
    fn test_open_read_only_replays_wal_without_mutating_files() {
        let dir = TempDir::new().unwrap();
        {
            let opts = Options::default().durability(DurabilityMode::Immediate);
            let db = Db::open(dir.path(), opts).unwrap();
            db.put(b"wal_only", b"value").unwrap();
        }

        let manifest_len = std::fs::metadata(dir.path().join("MANIFEST"))
            .unwrap()
            .len();
        let mut wal_names_before: Vec<_> = std::fs::read_dir(dir.path().join("wal"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        wal_names_before.sort();

        let ro = Db::open_read_only(dir.path(), Options::default()).unwrap();
        assert_eq!(ro.get(b"wal_only").unwrap(), Some(b"value".to_vec()));

        let err = ro.put(b"blocked", b"write").unwrap_err();
        match err {
            Error::ReadOnly => {}
            other => panic!("expected read-only error, got {other:?}"),
        }

        let writer_err = Db::open(dir.path(), Options::default()).unwrap_err();
        match writer_err {
            Error::Io(io) => assert_eq!(io.kind(), std::io::ErrorKind::AlreadyExists),
            other => panic!("expected lock conflict, got {other:?}"),
        }
        ro.close().unwrap();
        drop(ro);

        assert_eq!(
            std::fs::metadata(dir.path().join("MANIFEST"))
                .unwrap()
                .len(),
            manifest_len
        );
        let mut wal_names_after: Vec<_> = std::fs::read_dir(dir.path().join("wal"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        wal_names_after.sort();
        assert_eq!(wal_names_after, wal_names_before);
    }

    #[test]
    fn test_open_read_only_missing_db_errors() {
        let dir = TempDir::new().unwrap();
        let err = Db::open_read_only(dir.path(), Options::default()).unwrap_err();
        match err {
            Error::Io(io) => assert_eq!(io.kind(), std::io::ErrorKind::NotFound),
            other => panic!("expected missing DB error, got {other:?}"),
        }
    }

    #[test]
    fn test_close_transitions_handle_to_closed_state() {
        let (db, _dir) = open_tmp();
        db.put(b"k", b"v").unwrap();
        let snap = db.snapshot();

        db.close().unwrap();
        db.close().unwrap();

        match db.get(b"k").unwrap_err() {
            Error::Closed => {}
            other => panic!("expected closed error from get, got {other:?}"),
        }
        match db.put(b"after", b"close").unwrap_err() {
            Error::Closed => {}
            other => panic!("expected closed error from put, got {other:?}"),
        }
        match db.write(WriteBatch::new()).unwrap_err() {
            Error::Closed => {}
            other => panic!("expected closed error from empty write, got {other:?}"),
        }
        match db.delete_range(b"z", b"a").unwrap_err() {
            Error::Closed => {}
            other => panic!("expected closed error from no-op range delete, got {other:?}"),
        }
        let default_cf = db.default_cf();
        match db.delete_range_cf(&default_cf, b"z", b"a").unwrap_err() {
            Error::Closed => {}
            other => panic!("expected closed error from no-op cf range delete, got {other:?}"),
        }
        match snap.get(b"k").unwrap_err() {
            Error::Closed => {}
            other => panic!("expected closed error from snapshot get, got {other:?}"),
        }

        let mut iter = db.iter();
        iter.seek_to_first();
        match iter.status().unwrap_err() {
            Error::Closed => {}
            other => panic!("expected closed error from iterator, got {other:?}"),
        }
    }

    #[test]
    fn test_failed_close_can_be_retried() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), Options::default()).unwrap();
        db.put(b"k", b"v").unwrap();

        // The log close creates after its flush: the open's log is 1 and the
        // flush's table 2.
        let blocked_wal_path = dir.path().join("wal").join("wal_000003.log");
        std::fs::create_dir(&blocked_wal_path).unwrap();

        match db.close().unwrap_err() {
            Error::Io(_) => {}
            other => panic!("expected close I/O error, got {other:?}"),
        }

        assert_eq!(db.get(b"k").unwrap(), Some(b"v".to_vec()));
        // The failed close left the database taking writes again.
        db.put(b"after", b"close").unwrap();

        std::fs::remove_dir(&blocked_wal_path).unwrap();
        db.close().unwrap();
        db.close().unwrap();

        let sst_count = std::fs::read_dir(dir.path().join("sst"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "sst"))
            .count();
        assert!(sst_count > 0);

        drop(db);
        let reopened = Db::open(dir.path(), Options::default()).unwrap();
        assert_eq!(reopened.get(b"k").unwrap(), Some(b"v".to_vec()));
        assert_eq!(reopened.get(b"after").unwrap(), Some(b"close".to_vec()));
    }

    #[test]
    fn test_configured_key_value_size_limits_are_enforced() {
        let dir = TempDir::new().unwrap();
        let opts = Options::default()
            .max_key_size(8)
            .max_value_size(4)
            .merge_operator(Some(Arc::new(AppendMerge)));
        let db = Db::open(dir.path(), opts).unwrap();

        db.put(b"abc", b"1234").unwrap();
        assert!(db.put(b"toolongky", b"1").is_err());
        assert!(db.put(b"a", b"12345").is_err());
        assert!(db.delete(b"toolongky").is_err());
        assert!(db.merge(b"toolongky", b"1").is_err());
        assert!(db.merge(b"a", b"12345").is_err());

        let mut batch = WriteBatch::new();
        batch.put(b"ok", b"1");
        batch.put(b"toolong", b"2");
        db.write(batch).unwrap();
        assert_eq!(db.get(b"ok").unwrap(), Some(b"1".to_vec()));

        let mut batch = WriteBatch::new();
        batch.put(b"ok2", b"1");
        batch.put(b"toolongky", b"2");
        assert!(db.write(batch).is_err());
        assert_eq!(db.get(b"ok2").unwrap(), None);

        let cf = db.create_column_family("cf").unwrap();
        assert!(db.put_cf(&cf, b"toolongky", b"1").is_err());
    }

    /// Options that force flushes early so tests can exercise the SSTable path.
    fn tiny_flush_opts() -> Options {
        Options::default().write_buffer_size(4 * 1024)
    }

    /// Write enough filler bytes to push the active memtable past
    /// `write_buffer_size`, forcing a flush to L0.
    fn force_flush(db: &Db, tag: &str) {
        let payload = vec![0u8; 512];
        for i in 0..32 {
            let key = format!("__flush_{}_{:04}", tag, i);
            crate::through_stalls(|| db.put(key.as_bytes(), &payload)).unwrap();
        }
    }

    fn force_flush_with_prefix(db: &Db, prefix: &str) {
        let payload = vec![0u8; 512];
        for i in 0..32 {
            let key = format!("{prefix}_{i:04}");
            crate::through_stalls(|| db.put(key.as_bytes(), &payload)).unwrap();
        }
    }

    #[test]
    fn test_basic_crud() {
        let (db, _dir) = open_tmp();

        db.put(b"key1", b"value1").unwrap();
        assert_eq!(db.get(b"key1").unwrap(), Some(b"value1".to_vec()));

        db.put(b"key1", b"value2").unwrap();
        assert_eq!(db.get(b"key1").unwrap(), Some(b"value2".to_vec()));

        db.delete(b"key1").unwrap();
        assert_eq!(db.get(b"key1").unwrap(), None);

        assert_eq!(db.get(b"nonexistent").unwrap(), None);
    }

    #[test]
    fn test_write_batch() {
        let (db, _dir) = open_tmp();

        let mut batch = WriteBatch::new();
        batch.put(b"a", b"1");
        batch.put(b"b", b"2");
        batch.put(b"c", b"3");
        db.write(batch).unwrap();

        assert_eq!(db.get(b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(db.get(b"b").unwrap(), Some(b"2".to_vec()));
        assert_eq!(db.get(b"c").unwrap(), Some(b"3".to_vec()));
    }

    #[test]
    fn test_write_batch_uses_single_wal_batch_record() {
        let (db, dir) = open_tmp();

        let mut batch = WriteBatch::new();
        batch.put(b"a", b"1");
        batch.put(b"b", b"2");
        db.write(batch).unwrap();

        // One group record after the stamp holds both operations, so
        // replay restores the batch whole or not at all.
        use crate::engine::wal_frame::{HEADER_LEN, KIND_GROUP, STAMP_LEN};
        let wal = std::fs::read(first_wal_path(&dir)).unwrap();
        assert_eq!(&wal[0..4], b"REGO", "the log must carry its stamp");
        assert_eq!(wal[STAMP_LEN + 4], KIND_GROUP);
        let len = u32::from_le_bytes(wal[STAMP_LEN..STAMP_LEN + 4].try_into().unwrap()) as usize;
        assert_eq!(STAMP_LEN + HEADER_LEN + len, wal.len(), "one record");
        assert!(len > 2 * 4, "the record holds both operations");
    }

    #[test]
    fn test_snapshot_isolation() {
        let (db, _dir) = open_tmp();

        db.put(b"key", b"v1").unwrap();
        let snap = db.snapshot();

        db.put(b"key", b"v2").unwrap();

        assert_eq!(snap.get(b"key").unwrap(), Some(b"v1".to_vec()));
        assert_eq!(db.get(b"key").unwrap(), Some(b"v2".to_vec()));
    }

    #[test]
    fn test_scan() {
        let (db, _dir) = open_tmp();

        db.put(b"a", b"1").unwrap();
        db.put(b"b", b"2").unwrap();
        db.put(b"c", b"3").unwrap();
        db.put(b"d", b"4").unwrap();

        let results = db.scan(Some(b"b"), Some(b"d")).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0], (b"b".to_vec(), b"2".to_vec()));
        assert_eq!(results[1], (b"c".to_vec(), b"3".to_vec()));
    }

    #[test]
    fn test_scan_page_limits_and_resumes() {
        let (db, _dir) = open_tmp();

        for c in b'a'..=b'e' {
            db.put(&[c], &[c]).unwrap();
        }

        let page = db.scan_page(Some(b"b"), Some(b"e"), 2).unwrap();
        assert_eq!(
            page,
            ScanPage {
                entries: vec![
                    (b"b".to_vec(), b"b".to_vec()),
                    (b"c".to_vec(), b"c".to_vec()),
                ],
                next_start: Some(b"d".to_vec()),
            }
        );

        let next = db
            .scan_page(page.next_start.as_deref(), Some(b"e"), 2)
            .unwrap();
        assert_eq!(
            next,
            ScanPage {
                entries: vec![(b"d".to_vec(), b"d".to_vec())],
                next_start: None,
            }
        );

        let exact_end = db.scan_page(Some(b"b"), Some(b"d"), 2).unwrap();
        assert_eq!(
            exact_end,
            ScanPage {
                entries: vec![
                    (b"b".to_vec(), b"b".to_vec()),
                    (b"c".to_vec(), b"c".to_vec()),
                ],
                next_start: None,
            }
        );
    }

    #[test]
    fn test_scan_page_rejects_zero_limit() {
        let (db, _dir) = open_tmp();

        let err = db.scan_page(None, None, 0).unwrap_err();
        match err {
            Error::InvalidArgument(message) => assert!(message.contains("greater than zero")),
            other => panic!("expected invalid argument error, got {other:?}"),
        }
    }

    #[test]
    fn test_strip_cf_prefix_page_rejects_short_internal_key() {
        let page = ScanPage {
            entries: vec![(vec![0, 1, 2], b"value".to_vec())],
            next_start: None,
        };

        match strip_cf_prefix_page(page).unwrap_err() {
            Error::Corruption(source) => assert_eq!(source.kind(), std::io::ErrorKind::InvalidData),
            other => panic!("expected corruption error, got {other:?}"),
        }
    }

    #[test]
    fn test_snapshot_scan_page_is_stable() {
        let (db, _dir) = open_tmp();

        db.put(b"a", b"1").unwrap();
        db.put(b"b", b"2").unwrap();
        let snap = db.snapshot();

        db.put(b"a", b"new").unwrap();
        db.delete(b"b").unwrap();
        db.put(b"c", b"3").unwrap();

        let first = snap.scan_page(None, None, 1).unwrap();
        assert_eq!(
            first,
            ScanPage {
                entries: vec![(b"a".to_vec(), b"1".to_vec())],
                next_start: Some(b"b".to_vec()),
            }
        );

        let second = snap
            .scan_page(first.next_start.as_deref(), None, 2)
            .unwrap();
        assert_eq!(
            second,
            ScanPage {
                entries: vec![(b"b".to_vec(), b"2".to_vec())],
                next_start: None,
            }
        );
    }

    #[test]
    fn test_drop_all() {
        let (db, _dir) = open_tmp();

        db.put(b"key1", b"val1").unwrap();
        db.put(b"key2", b"val2").unwrap();
        db.drop_all().unwrap();

        assert_eq!(db.get(b"key1").unwrap(), None);
        assert_eq!(db.get(b"key2").unwrap(), None);

        db.put(b"key3", b"val3").unwrap();
        assert_eq!(db.get(b"key3").unwrap(), Some(b"val3".to_vec()));
    }

    #[test]
    fn test_drop_all_reopen_ignores_leftover_old_wal() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        crate::through_stalls(|| db.put(b"flushed", b"old")).unwrap();
        force_flush(&db, "drop-all-wal-floor");
        crate::through_stalls(|| db.put(b"unflushed", b"old")).unwrap();
        db.drop_all().unwrap();
        drop(db);

        let stale_wal_path = dir.path().join("wal").join("wal_000001.log");
        let mut stale_wal = engine::wal::Wal::create(&stale_wal_path).unwrap();
        stale_wal
            .append_put(&prefix_key(DEFAULT_CF_ID, b"resurrect"), b"bad", 99)
            .unwrap();
        stale_wal.sync_data().unwrap();

        let db = Db::open(dir.path(), Options::default()).unwrap();
        assert_eq!(db.get(b"flushed").unwrap(), None);
        assert_eq!(db.get(b"unflushed").unwrap(), None);
        assert_eq!(db.get(b"resurrect").unwrap(), None);
        assert!(db.scan(None, None).unwrap().is_empty());
    }

    #[test]
    fn test_snapshot_isolation_across_flush() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        crate::through_stalls(|| db.put(b"key", b"v1")).unwrap();
        let snap = db.snapshot();

        crate::through_stalls(|| db.put(b"key", b"v2")).unwrap();
        force_flush(&db, "snap");

        assert_eq!(snap.get(b"key").unwrap(), Some(b"v1".to_vec()));
        assert_eq!(db.get(b"key").unwrap(), Some(b"v2".to_vec()));
    }

    #[test]
    fn test_delete_persists_across_flush() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        crate::through_stalls(|| db.put(b"key", b"v1")).unwrap();
        force_flush(&db, "a");

        crate::through_stalls(|| db.delete(b"key")).unwrap();
        force_flush(&db, "b");

        assert_eq!(db.get(b"key").unwrap(), None);
    }

    #[test]
    fn test_crash_recovery_without_close() {
        let dir = TempDir::new().unwrap();

        {
            let db = Db::open(dir.path(), Options::default()).unwrap();
            db.put(b"a", b"1").unwrap();
            db.put(b"b", b"2").unwrap();
            db.delete(b"a").unwrap();
            db.put(b"c", b"3").unwrap();
            // Drop without close() - simulates a crash; the WAL must be replayed.
        }

        let db = Db::open(dir.path(), Options::default()).unwrap();
        assert_eq!(db.get(b"a").unwrap(), None);
        assert_eq!(db.get(b"b").unwrap(), Some(b"2".to_vec()));
        assert_eq!(db.get(b"c").unwrap(), Some(b"3".to_vec()));
    }

    #[test]
    fn test_recovered_wal_survives_second_crash_before_flush() {
        let dir = TempDir::new().unwrap();

        {
            let db = Db::open(dir.path(), Options::default()).unwrap();
            db.put(b"a", b"1").unwrap();
            db.put(b"b", b"2").unwrap();
            db.put(b"d", b"4").unwrap();
            db.delete(b"a").unwrap();
            db.delete_range(b"d", b"f").unwrap();
            // Drop without close so the first reopen must recover from WAL.
        }

        {
            let db = Db::open(dir.path(), Options::default()).unwrap();
            assert_eq!(db.get(b"a").unwrap(), None);
            assert_eq!(db.get(b"b").unwrap(), Some(b"2".to_vec()));
            assert_eq!(db.get(b"d").unwrap(), None);
            // Drop again before the recovered memtable can flush. The
            // recovered state must have been rewritten to the active WAL.
        }

        let db = Db::open(dir.path(), Options::default()).unwrap();
        assert_eq!(db.get(b"a").unwrap(), None);
        assert_eq!(db.get(b"b").unwrap(), Some(b"2".to_vec()));
        assert_eq!(db.get(b"d").unwrap(), None);
    }

    // ─── Streaming iterator tests ────────────────────────────────────────

    fn collect_iter(db: &Db) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut it = db.iter();
        it.seek_to_first();
        let mut out = Vec::new();
        while it.valid() {
            out.push((it.key().unwrap().to_vec(), it.value().unwrap().to_vec()));
            it.next();
        }
        it.status().unwrap();
        out
    }

    #[test]
    fn test_iter_empty_db() {
        let (db, _dir) = open_tmp();
        let mut it = db.iter();
        it.seek_to_first();
        assert!(!it.valid());
        it.seek(b"anything");
        assert!(!it.valid());
        assert!(it.status().is_ok());
    }

    #[test]
    fn test_iter_basic_forward() {
        let (db, _dir) = open_tmp();
        for i in 0..10 {
            let k = format!("k{:02}", i);
            let v = format!("v{}", i);
            db.put(k.as_bytes(), v.as_bytes()).unwrap();
        }
        let items = collect_iter(&db);
        assert_eq!(items.len(), 10);
        for (i, (k, v)) in items.iter().enumerate() {
            assert_eq!(k, format!("k{:02}", i).as_bytes());
            assert_eq!(v, format!("v{}", i).as_bytes());
        }
    }

    #[test]
    fn test_owned_snapshot_iter_streams_from_snapshot() {
        let (db, _dir) = open_tmp();
        db.put(b"a", b"1").unwrap();
        db.put(b"b", b"2").unwrap();
        let snapshot = db.snapshot();
        db.put(b"c", b"3").unwrap();

        let mut it = snapshot.owned_iter();
        assert_eq!(snapshot.get(b"b").unwrap(), Some(b"2".to_vec()));

        it.seek_to_first();
        assert!(it.valid());
        assert_eq!(it.key(), Some(b"a".as_ref()));
        assert_eq!(it.value(), Some(b"1".as_ref()));
        it.next();
        assert_eq!(it.key(), Some(b"b".as_ref()));
        it.next();
        assert!(!it.valid());

        it.seek_to_last();
        assert_eq!(it.key(), Some(b"b".as_ref()));
        it.prev();
        assert_eq!(it.key(), Some(b"a".as_ref()));
        it.status().unwrap();
    }

    #[test]
    fn test_iter_seek_exact_and_between() {
        let (db, _dir) = open_tmp();
        db.put(b"a", b"1").unwrap();
        db.put(b"c", b"3").unwrap();
        db.put(b"e", b"5").unwrap();

        let mut it = db.iter();

        it.seek(b"a");
        assert!(it.valid());
        assert_eq!(it.key(), Some(b"a".as_ref()));

        it.seek(b"b");
        assert_eq!(it.key(), Some(b"c".as_ref()));

        it.seek(b"c");
        assert_eq!(it.key(), Some(b"c".as_ref()));

        it.seek(b"f");
        assert!(!it.valid());
    }

    #[test]
    fn test_iter_seek_for_prev() {
        let (db, _dir) = open_tmp();
        db.put(b"a", b"1").unwrap();
        db.put(b"c", b"3").unwrap();
        db.put(b"e", b"5").unwrap();

        let mut it = db.iter();

        it.seek_for_prev(b"e");
        assert_eq!(it.key(), Some(b"e".as_ref()));

        it.seek_for_prev(b"d");
        assert_eq!(it.key(), Some(b"c".as_ref()));

        it.seek_for_prev(b"a");
        assert_eq!(it.key(), Some(b"a".as_ref()));

        it.seek_for_prev(b"0");
        assert!(!it.valid());
    }

    #[test]
    fn test_iter_continues_after_seek() {
        let (db, _dir) = open_tmp();
        for c in b'a'..=b'j' {
            db.put(&[c], &[c]).unwrap();
        }

        let mut it = db.iter();
        it.seek(b"d");
        let mut keys = Vec::new();
        while it.valid() {
            keys.push(it.key().unwrap().to_vec());
            it.next();
        }
        assert_eq!(
            keys,
            vec![
                b"d".to_vec(),
                b"e".to_vec(),
                b"f".to_vec(),
                b"g".to_vec(),
                b"h".to_vec(),
                b"i".to_vec(),
                b"j".to_vec(),
            ]
        );
    }

    #[test]
    fn test_iter_across_memtable_and_l0() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        for i in 0..10 {
            let k = format!("old{:02}", i);
            crate::through_stalls(|| db.put(k.as_bytes(), b"old")).unwrap();
        }
        force_flush(&db, "to-l0");

        for i in 0..5 {
            let k = format!("new{:02}", i);
            crate::through_stalls(|| db.put(k.as_bytes(), b"new")).unwrap();
        }

        let items = collect_iter(&db);
        let olds = items.iter().filter(|(k, _)| k.starts_with(b"old")).count();
        let news = items.iter().filter(|(k, _)| k.starts_with(b"new")).count();
        assert_eq!(olds, 10);
        assert_eq!(news, 5);

        let sorted: Vec<_> = items.iter().map(|(k, _)| k.clone()).collect();
        let mut expected = sorted.clone();
        expected.sort();
        assert_eq!(sorted, expected);
    }

    #[test]
    fn test_iter_tombstone_hides_older_level_entry() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        crate::through_stalls(|| db.put(b"kept", b"v1")).unwrap();
        crate::through_stalls(|| db.put(b"gone", b"v1")).unwrap();
        force_flush(&db, "a");

        crate::through_stalls(|| db.delete(b"gone")).unwrap();

        let items = collect_iter(&db);
        let keys: Vec<_> = items.iter().map(|(k, _)| k.clone()).collect();
        assert!(keys.contains(&b"kept".to_vec()));
        assert!(!keys.contains(&b"gone".to_vec()));
    }

    #[test]
    fn test_iter_latest_version_wins_across_levels() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        crate::through_stalls(|| db.put(b"k", b"v1")).unwrap();
        force_flush(&db, "a");
        crate::through_stalls(|| db.put(b"k", b"v2")).unwrap();

        let mut it = db.iter();
        it.seek(b"k");
        assert_eq!(it.key(), Some(b"k".as_ref()));
        assert_eq!(it.value(), Some(b"v2".as_ref()));
    }

    #[test]
    fn test_iter_honors_snapshot_isolation() {
        let (db, _dir) = open_tmp();
        db.put(b"k", b"v1").unwrap();
        let snap = db.snapshot();
        db.put(b"k", b"v2").unwrap();

        let mut it = snap.iter();
        it.seek(b"k");
        assert_eq!(it.value(), Some(b"v1".as_ref()));
    }

    #[test]
    fn test_iter_snapshot_ignores_tombstone_newer_than_snap() {
        let (db, _dir) = open_tmp();
        db.put(b"k", b"v1").unwrap();
        let snap = db.snapshot();
        db.delete(b"k").unwrap();

        let mut it = snap.iter();
        it.seek(b"k");
        assert_eq!(it.value(), Some(b"v1".as_ref()));
    }

    #[test]
    fn test_iter_consistency_with_scan() {
        let (db, _dir) = open_tmp();
        for i in 0..100 {
            let k = format!("k{:03}", i);
            let v = format!("v{}", i);
            db.put(k.as_bytes(), v.as_bytes()).unwrap();
        }

        let scan = db.scan(Some(b"k020"), Some(b"k050")).unwrap();

        let mut it = db.iter();
        it.seek(b"k020");
        let mut from_iter = Vec::new();
        while it.valid() {
            let k = it.key().unwrap();
            if k >= b"k050".as_ref() {
                break;
            }
            from_iter.push((k.to_vec(), it.value().unwrap().to_vec()));
            it.next();
        }

        assert_eq!(scan, from_iter);
    }

    #[test]
    fn test_iter_large_scan_10k_keys_after_flush() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        const N: usize = 10_000;
        for i in 0..N {
            let k = format!("key_{:06}", i);
            // Ten thousand writes into a 4 KiB buffer can outrun the
            // worker's compactions and meet the L0 stop, which a write
            // reports rather than waits out; this test is about the scan.
            through_stalls(|| db.put(k.as_bytes(), b"v")).unwrap();
        }

        let mut it = db.iter();
        it.seek(b"key_");
        let mut count = 0;
        while it.valid() {
            let k = it.key().unwrap();
            if !k.starts_with(b"key_") {
                it.next();
                continue;
            }
            count += 1;
            it.next();
        }
        assert_eq!(count, N);
    }

    // ─── Snapshot-pinning GC tests ──────────────────────────────────────

    /// Thin wrapper around the engine's test-only persisted-versions
    /// accessor. Returns `(seq, value_type)` for every copy of
    /// `user_key` currently sitting in an SSTable at any level.
    fn all_versions_of(db: &Db, user_key: &[u8]) -> Vec<(u64, u8)> {
        // The helper walks raw engine keys, so re-apply the
        // default-CF prefix before querying.
        let prefixed = prefix_key(DEFAULT_CF_ID, user_key);
        db.engine.all_persisted_versions_of(&prefixed).unwrap()
    }

    #[test]
    fn test_gc_drops_old_versions_without_snapshot() {
        // With no live snapshot, compact_range(None, None) should
        // leave only the newest version of each user key on disk.
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        for v in 0..10 {
            crate::through_stalls(|| db.put(b"k", format!("v{}", v).as_bytes())).unwrap();
        }

        db.compact_range(None, None).wait().unwrap();

        let versions = all_versions_of(&db, b"k");
        assert_eq!(
            versions.len(),
            1,
            "expected a single surviving version, found {:?}",
            versions
        );
        assert_eq!(db.get(b"k").unwrap(), Some(b"v9".to_vec()));
    }

    #[test]
    fn test_gc_preserves_versions_pinned_by_snapshot() {
        // Take a snapshot at seq 5, then write more versions. After
        // compaction the snapshot must still read its view, which
        // requires preserving the version it pinned.
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        crate::through_stalls(|| db.put(b"k", b"v1")).unwrap();
        crate::through_stalls(|| db.put(b"k", b"v2")).unwrap();
        crate::through_stalls(|| db.put(b"k", b"v3")).unwrap();
        let snap = db.snapshot();
        // `snap` now pins seq=3 - the snapshot sees v3.

        for v in 4..10 {
            crate::through_stalls(|| db.put(b"k", format!("v{}", v).as_bytes())).unwrap();
        }

        db.compact_range(None, None).wait().unwrap();

        assert_eq!(snap.get(b"k").unwrap(), Some(b"v3".to_vec()));
        assert_eq!(db.get(b"k").unwrap(), Some(b"v9".to_vec()));
    }

    #[test]
    fn test_gc_releases_pin_when_snapshot_drops() {
        // Pinning a snapshot and then dropping it should fully
        // release the horizon so the next compaction can collapse
        // the key to a single surviving version.
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        for v in 0..5 {
            crate::through_stalls(|| db.put(b"k", format!("v{}", v).as_bytes())).unwrap();
        }

        {
            let _snap = db.snapshot();
            assert_eq!(db.engine.oldest_live_seq(), 5);
        }
        // Pin released.
        assert_eq!(db.engine.oldest_live_seq(), u64::MAX);

        for v in 5..10 {
            crate::through_stalls(|| db.put(b"k", format!("v{}", v).as_bytes())).unwrap();
        }

        db.compact_range(None, None).wait().unwrap();

        let versions = all_versions_of(&db, b"k");
        assert_eq!(versions.len(), 1);
        assert_eq!(db.get(b"k").unwrap(), Some(b"v9".to_vec()));
    }

    #[test]
    fn test_gc_with_multiple_live_snapshots_keeps_every_snapshots_view() {
        // With two snapshots live, compaction keeps the version each
        // one reads, so the older snapshot's view and the newer
        // snapshot's both survive.
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        crate::through_stalls(|| db.put(b"k", b"v1")).unwrap();
        crate::through_stalls(|| db.put(b"k", b"v2")).unwrap();
        let old_snap = db.snapshot(); // pins seq 2
        crate::through_stalls(|| db.put(b"k", b"v3")).unwrap();
        crate::through_stalls(|| db.put(b"k", b"v4")).unwrap();
        let new_snap = db.snapshot(); // pins seq 4
        crate::through_stalls(|| db.put(b"k", b"v5")).unwrap();
        crate::through_stalls(|| db.put(b"k", b"v6")).unwrap();

        db.compact_range(None, None).wait().unwrap();

        // Both snapshots must still return their respective versions.
        assert_eq!(old_snap.get(b"k").unwrap(), Some(b"v2".to_vec()));
        assert_eq!(new_snap.get(b"k").unwrap(), Some(b"v4".to_vec()));
        assert_eq!(db.get(b"k").unwrap(), Some(b"v6".to_vec()));
    }

    #[test]
    fn test_gc_preserves_tombstone_hiding_older_entries() {
        // A tombstone newer than any live snapshot still needs to
        // survive compaction while it hides an older version that
        // survives with it: here one a snapshot below it keeps, which
        // reads must still resolve to "deleted" at the head. With
        // nothing left beneath it, it retires (E27).
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        for v in 0..5 {
            crate::through_stalls(|| db.put(b"k", format!("v{}", v).as_bytes())).unwrap();
        }
        let before = db.snapshot();
        crate::through_stalls(|| db.delete(b"k")).unwrap();

        db.compact_range(None, None).wait().unwrap();

        assert_eq!(db.get(b"k").unwrap(), None);
        assert_eq!(before.get(b"k").unwrap(), Some(b"v4".to_vec()));

        // The newest surviving version is a tombstone - look for it
        // on disk.
        let versions = all_versions_of(&db, b"k");
        assert!(!versions.is_empty());
        // Highest seq is the tombstone.
        let (_, vt) = *versions.iter().max_by_key(|(seq, _)| *seq).unwrap();
        const VALUE_TYPE_DELETION: u8 = 0;
        assert_eq!(vt, VALUE_TYPE_DELETION);
    }

    #[test]
    fn test_range_tombstone_pruning_preserves_snapshot_visible_value() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        crate::through_stalls(|| db.put(b"k", b"old")).unwrap();
        let snap = db.snapshot();
        crate::through_stalls(|| db.delete_range(b"a", b"z")).unwrap();

        db.compact_range(None, None).wait().unwrap();

        assert_eq!(snap.get(b"k").unwrap(), Some(b"old".to_vec()));
        assert_eq!(db.get(b"k").unwrap(), None);
    }

    #[test]
    fn test_gc_across_many_user_keys() {
        // Stress the multi-group path: many distinct user keys each
        // with several versions. No snapshot is live so each key
        // should collapse to exactly one surviving version.
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        for i in 0..200 {
            for v in 0..3 {
                crate::through_stalls(|| {
                    db.put(
                        format!("k{:03}", i).as_bytes(),
                        format!("v{}_{}", i, v).as_bytes(),
                    )
                })
                .unwrap();
            }
        }

        db.compact_range(None, None).wait().unwrap();

        for i in 0..200 {
            let k = format!("k{:03}", i);
            let versions = all_versions_of(&db, k.as_bytes());
            assert_eq!(versions.len(), 1, "key {} survived with {:?}", k, versions);
            assert_eq!(
                db.get(k.as_bytes()).unwrap(),
                Some(format!("v{}_2", i).into_bytes())
            );
        }
    }

    // ─── compact_range tests ────────────────────────────────────────────

    fn level_file_count(db: &Db, level: usize) -> usize {
        db.engine.level_file_count(level)
    }

    fn total_file_count(db: &Db) -> usize {
        db.engine.total_file_count()
    }

    #[test]
    fn test_compact_range_empty_db() {
        let (db, _dir) = open_tmp();
        // No data, no files. compact_range is a no-op and must succeed.
        db.compact_range(None, None).wait().unwrap();
        assert_eq!(total_file_count(&db), 0);
    }

    #[test]
    fn test_compact_range_full_preserves_reads() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        for i in 0..500 {
            let k = format!("k{:04}", i);
            crate::through_stalls(|| db.put(k.as_bytes(), format!("v{}", i).as_bytes())).unwrap();
        }

        db.compact_range(None, None).wait().unwrap();

        // Every key is still readable after the compaction.
        for i in 0..500 {
            let k = format!("k{:04}", i);
            assert_eq!(
                db.get(k.as_bytes()).unwrap(),
                Some(format!("v{}", i).into_bytes())
            );
        }
    }

    #[test]
    fn test_compact_range_flushes_active_memtable() {
        // Writes that are still in the memtable when compact_range is
        // called must be flushed to L0 before the walk, so the active
        // memtable is empty afterwards.
        let (db, _dir) = open_tmp();
        for i in 0..10 {
            let k = format!("m{:02}", i);
            db.put(k.as_bytes(), b"v").unwrap();
        }
        assert!(!db.engine.active_memtable_is_empty());

        db.compact_range(None, None).wait().unwrap();

        assert!(db.engine.active_memtable_is_empty());
        // And data is still readable through the SSTable path.
        for i in 0..10 {
            let k = format!("m{:02}", i);
            assert_eq!(db.get(k.as_bytes()).unwrap(), Some(b"v".to_vec()));
        }
    }

    #[test]
    fn test_compact_range_drains_l0() {
        // After a full compact_range, nothing should remain at L0 -
        // every file must have been pushed down to L1+.
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        for i in 0..200 {
            let k = format!("k{:04}", i);
            crate::through_stalls(|| db.put(k.as_bytes(), b"v")).unwrap();
        }

        db.compact_range(None, None).wait().unwrap();

        assert_eq!(level_file_count(&db, 0), 0);
        // Some higher level must hold the data.
        assert!(total_file_count(&db) > 0);
    }

    #[test]
    fn test_compact_range_bounded_preserves_all_data() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        // Three disjoint ranges: low (a*), mid (m*), high (z*).
        for i in 0..100 {
            crate::through_stalls(|| db.put(format!("a{:03}", i).as_bytes(), b"a")).unwrap();
        }
        for i in 0..100 {
            crate::through_stalls(|| db.put(format!("m{:03}", i).as_bytes(), b"m")).unwrap();
        }
        for i in 0..100 {
            crate::through_stalls(|| db.put(format!("z{:03}", i).as_bytes(), b"z")).unwrap();
        }

        // Only compact the mid range.
        db.compact_range(Some(b"m"), Some(b"n")).wait().unwrap();

        // Every key must still be readable regardless of the range.
        for i in 0..100 {
            assert_eq!(
                db.get(format!("a{:03}", i).as_bytes()).unwrap(),
                Some(b"a".to_vec())
            );
            assert_eq!(
                db.get(format!("m{:03}", i).as_bytes()).unwrap(),
                Some(b"m".to_vec())
            );
            assert_eq!(
                db.get(format!("z{:03}", i).as_bytes()).unwrap(),
                Some(b"z".to_vec())
            );
        }
    }

    #[test]
    fn test_compact_range_bounded_compacts_default_cf_files() {
        let dir = TempDir::new().unwrap();
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .l0_compaction_trigger(1_000);
        let db = Db::open(dir.path(), opts).unwrap();
        let payload = vec![0u8; 512];

        for i in 0..32 {
            crate::through_stalls(|| db.put(format!("m{i:04}").as_bytes(), &payload)).unwrap();
        }
        force_flush_with_prefix(&db, "m_flush");

        let l0_before = level_file_count(&db, 0);
        assert!(l0_before > 0);

        db.compact_range(Some(b"m"), Some(b"n")).wait().unwrap();

        assert_eq!(level_file_count(&db, 0), 0);
        assert!(total_file_count(&db) > 0);
        assert_eq!(db.get(b"m0000").unwrap(), Some(payload));
    }

    #[test]
    fn test_compact_range_reclaims_space_after_overwrite() {
        // Write N keys, overwrite them, force flush, then compact_range.
        // The number of distinct entries after compaction should be N
        // (one per user key) - the old overwritten versions got merged
        // away by deduplication during compaction.
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        for i in 0..200 {
            let k = format!("k{:03}", i);
            crate::through_stalls(|| db.put(k.as_bytes(), b"v1")).unwrap();
        }
        for i in 0..200 {
            let k = format!("k{:03}", i);
            crate::through_stalls(|| db.put(k.as_bytes(), b"v2")).unwrap();
        }

        db.compact_range(None, None).wait().unwrap();

        for i in 0..200 {
            let k = format!("k{:03}", i);
            assert_eq!(db.get(k.as_bytes()).unwrap(), Some(b"v2".to_vec()));
        }
    }

    #[test]
    fn test_compact_range_runs_alongside_background_compaction() {
        // Write enough to trigger background compactions, then while
        // the engine is still churning, fire a foreground compact_range.
        // Both must complete without corruption.
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        const N: usize = 2_000;
        for i in 0..N {
            let k = format!("key_{:05}", i);
            crate::through_stalls(|| db.put(k.as_bytes(), b"v")).unwrap();
        }

        db.compact_range(None, None).wait().unwrap();

        // After the foreground compaction, every key is still there.
        for i in 0..N {
            let k = format!("key_{:05}", i);
            assert_eq!(db.get(k.as_bytes()).unwrap(), Some(b"v".to_vec()));
        }
    }

    #[test]
    fn test_compact_range_iterator_still_correct() {
        // compact_range shouldn't perturb an iterator built after it.
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        for i in 0..300 {
            let k = format!("k{:04}", i);
            crate::through_stalls(|| db.put(k.as_bytes(), b"v")).unwrap();
        }

        db.compact_range(None, None).wait().unwrap();

        let mut it = db.iter();
        it.seek_to_first();
        let mut count = 0;
        while it.valid() {
            if it.key().unwrap().starts_with(b"k") {
                count += 1;
            }
            it.next();
        }
        assert_eq!(count, 300);
    }

    #[test]
    fn test_compact_range_tombstones_are_preserved() {
        // Tombstones must survive compaction until the bottommost level
        // drops them - for now compaction preserves all versions, so a
        // deleted key is still absent to reads.
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        for i in 0..50 {
            let k = format!("k{:02}", i);
            crate::through_stalls(|| db.put(k.as_bytes(), b"v")).unwrap();
        }
        // Delete half of them.
        for i in (0..50).step_by(2) {
            let k = format!("k{:02}", i);
            crate::through_stalls(|| db.delete(k.as_bytes())).unwrap();
        }

        db.compact_range(None, None).wait().unwrap();

        for i in 0..50 {
            let k = format!("k{:02}", i);
            let expected = if i % 2 == 0 {
                None
            } else {
                Some(b"v".to_vec())
            };
            assert_eq!(db.get(k.as_bytes()).unwrap(), expected);
        }
    }

    #[test]
    fn test_compaction_range_tombstone_bounds_cover_point_keys_in_other_files() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        crate::through_stalls(|| db.put(b"b", b"old")).unwrap();
        force_flush_with_prefix(&db, "__old_flush");
        db.compact_range(None, None).wait().unwrap();

        crate::through_stalls(|| db.delete_range(b"a", b"z")).unwrap();
        crate::through_stalls(|| db.put(b"m", b"new")).unwrap();
        force_flush_with_prefix(&db, "zz_new_flush");
        db.compact_range(None, None).wait().unwrap();

        assert_eq!(db.get(b"b").unwrap(), None);
        assert_eq!(db.get(b"m").unwrap(), Some(b"new".to_vec()));
    }

    #[test]
    fn test_compaction_splits_range_tombstones_around_point_outputs() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        crate::through_stalls(|| db.put(b"b", b"old-left")).unwrap();
        crate::through_stalls(|| db.put(b"y", b"old-right")).unwrap();
        db.compact_range(None, None).wait().unwrap();

        // A snapshot older than the tombstone holds it at the bottom level,
        // where it would otherwise retire (E27), so the pass writes it out.
        let before = db.snapshot();
        crate::through_stalls(|| db.delete_range(b"a", b"z")).unwrap();
        crate::through_stalls(|| db.put(b"m", b"new")).unwrap();
        db.compact_range(None, None).wait().unwrap();

        assert_eq!(db.get(b"b").unwrap(), None);
        assert_eq!(db.get(b"m").unwrap(), Some(b"new".to_vec()));
        assert_eq!(db.get(b"y").unwrap(), None);
        assert_eq!(before.get(b"b").unwrap(), Some(b"old-left".to_vec()));

        let version = db.engine.current_version();
        let rt_only_files = version
            .levels
            .iter()
            .flatten()
            .filter(|file| file.meta.num_entries == 0)
            .count();
        assert!(
            rt_only_files >= 2,
            "range tombstone gaps should be emitted separately, got {rt_only_files}"
        );
    }

    // ─── MultiGet tests ─────────────────────────────────────────────────

    #[test]
    fn test_multi_get_empty_batch() {
        let (db, _dir) = open_tmp();
        db.put(b"x", b"y").unwrap();
        let results = db.multi_get(&[]).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_multi_get_all_hit() {
        let (db, _dir) = open_tmp();
        db.put(b"a", b"1").unwrap();
        db.put(b"b", b"2").unwrap();
        db.put(b"c", b"3").unwrap();

        let keys: &[&[u8]] = &[b"a", b"b", b"c"];
        let results = db.multi_get(keys).unwrap();
        assert_eq!(
            results,
            vec![
                Some(b"1".to_vec()),
                Some(b"2".to_vec()),
                Some(b"3".to_vec())
            ]
        );
    }

    #[test]
    fn test_multi_get_all_miss() {
        let (db, _dir) = open_tmp();
        db.put(b"a", b"1").unwrap();

        let keys: &[&[u8]] = &[b"x", b"y", b"z"];
        let results = db.multi_get(keys).unwrap();
        assert_eq!(results, vec![None, None, None]);
    }

    #[test]
    fn test_multi_get_mixed_hit_miss() {
        let (db, _dir) = open_tmp();
        db.put(b"a", b"1").unwrap();
        db.put(b"c", b"3").unwrap();

        let keys: &[&[u8]] = &[b"a", b"b", b"c", b"d"];
        let results = db.multi_get(keys).unwrap();
        assert_eq!(
            results,
            vec![Some(b"1".to_vec()), None, Some(b"3".to_vec()), None]
        );
    }

    #[test]
    fn test_multi_get_preserves_input_order() {
        let (db, _dir) = open_tmp();
        db.put(b"a", b"1").unwrap();
        db.put(b"b", b"2").unwrap();
        db.put(b"c", b"3").unwrap();

        // Reverse order input.
        let keys: &[&[u8]] = &[b"c", b"a", b"b"];
        let results = db.multi_get(keys).unwrap();
        assert_eq!(
            results,
            vec![
                Some(b"3".to_vec()),
                Some(b"1".to_vec()),
                Some(b"2".to_vec())
            ]
        );
    }

    #[test]
    fn test_multi_get_duplicates_in_input() {
        let (db, _dir) = open_tmp();
        db.put(b"a", b"1").unwrap();
        db.put(b"b", b"2").unwrap();

        let keys: &[&[u8]] = &[b"a", b"b", b"a", b"missing", b"a"];
        let results = db.multi_get(keys).unwrap();
        assert_eq!(
            results,
            vec![
                Some(b"1".to_vec()),
                Some(b"2".to_vec()),
                Some(b"1".to_vec()),
                None,
                Some(b"1".to_vec()),
            ]
        );
    }

    #[test]
    fn test_multi_get_honors_tombstones() {
        let (db, _dir) = open_tmp();
        db.put(b"a", b"1").unwrap();
        db.put(b"b", b"2").unwrap();
        db.put(b"c", b"3").unwrap();
        db.delete(b"b").unwrap();

        let keys: &[&[u8]] = &[b"a", b"b", b"c"];
        let results = db.multi_get(keys).unwrap();
        assert_eq!(
            results,
            vec![Some(b"1".to_vec()), None, Some(b"3".to_vec())]
        );
    }

    #[test]
    fn test_multi_get_tombstone_hides_older_level_entry() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        crate::through_stalls(|| db.put(b"keep", b"v")).unwrap();
        crate::through_stalls(|| db.put(b"gone", b"v")).unwrap();
        force_flush(&db, "x");
        crate::through_stalls(|| db.delete(b"gone")).unwrap();

        let keys: &[&[u8]] = &[b"keep", b"gone"];
        let results = db.multi_get(keys).unwrap();
        assert_eq!(results, vec![Some(b"v".to_vec()), None]);
    }

    #[test]
    fn test_multi_get_spans_memtable_and_l0() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        crate::through_stalls(|| db.put(b"from_l0_1", b"v1")).unwrap();
        crate::through_stalls(|| db.put(b"from_l0_2", b"v2")).unwrap();
        force_flush(&db, "x");

        crate::through_stalls(|| db.put(b"from_mem_1", b"v3")).unwrap();
        crate::through_stalls(|| db.put(b"from_mem_2", b"v4")).unwrap();

        let keys: &[&[u8]] = &[b"from_mem_1", b"from_l0_1", b"from_mem_2", b"from_l0_2"];
        let results = db.multi_get(keys).unwrap();
        assert_eq!(
            results,
            vec![
                Some(b"v3".to_vec()),
                Some(b"v1".to_vec()),
                Some(b"v4".to_vec()),
                Some(b"v2".to_vec())
            ]
        );
    }

    #[test]
    fn test_multi_get_snapshot_isolation() {
        let (db, _dir) = open_tmp();
        db.put(b"a", b"a1").unwrap();
        db.put(b"b", b"b1").unwrap();

        let snap = db.snapshot();

        db.put(b"a", b"a2").unwrap();
        db.put(b"c", b"c1").unwrap();
        db.delete(b"b").unwrap();

        let keys: &[&[u8]] = &[b"a", b"b", b"c"];
        let results = snap.multi_get(keys).unwrap();
        assert_eq!(
            results,
            vec![Some(b"a1".to_vec()), Some(b"b1".to_vec()), None],
        );
    }

    #[test]
    fn test_multi_get_consistency_with_get() {
        // For any batch, multi_get must return the same results as a
        // loop of individual get calls at the same snapshot.
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        for i in 0..500 {
            let k = format!("k{:04}", i);
            crate::through_stalls(|| db.put(k.as_bytes(), format!("v{}", i).as_bytes())).unwrap();
        }
        // Delete some.
        for i in (0..500).step_by(7) {
            let k = format!("k{:04}", i);
            crate::through_stalls(|| db.delete(k.as_bytes())).unwrap();
        }

        // Snapshot so individual gets and multi_get see the same thing.
        let snap = db.snapshot();

        let keys_owned: Vec<String> = (0..500)
            .step_by(3)
            .map(|i| format!("k{:04}", i))
            .chain(std::iter::once("missing_key".to_string()))
            .collect();
        let keys: Vec<&[u8]> = keys_owned.iter().map(|s| s.as_bytes()).collect();

        let individual: Vec<_> = keys.iter().map(|k| snap.get(k).unwrap()).collect();
        let batched = snap.multi_get(&keys).unwrap();

        assert_eq!(individual, batched);
        assert_eq!(individual.len(), keys.len());
    }

    #[test]
    fn test_multi_get_large_batch_after_flush() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        const N: usize = 2_000;
        for i in 0..N {
            let k = format!("key_{:05}", i);
            crate::through_stalls(|| db.put(k.as_bytes(), b"v")).unwrap();
        }

        let keys_owned: Vec<String> = (0..N).map(|i| format!("key_{:05}", i)).collect();
        let keys: Vec<&[u8]> = keys_owned.iter().map(|s| s.as_bytes()).collect();
        let results = db.multi_get(&keys).unwrap();
        assert_eq!(results.len(), N);
        for r in &results {
            assert_eq!(r.as_deref(), Some(b"v".as_ref()));
        }
    }

    #[test]
    fn test_multi_get_compacted_level_range_tombstones() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        for i in 0..30 {
            let key = format!("k{:02}", i);
            let value = format!("v{:02}", i);
            crate::through_stalls(|| db.put(key.as_bytes(), value.as_bytes())).unwrap();
        }
        force_flush(&db, "base");
        crate::through_stalls(|| db.delete_range(b"k10", b"k20")).unwrap();
        db.compact_range(None, None).wait().unwrap();

        let version = db.engine.current_version();
        assert!(
            version.levels.iter().skip(1).any(|level| !level.is_empty()),
            "test must exercise L1+ files"
        );

        let keys_owned = ["k09", "k10", "k15", "k20", "k15", "k09", "missing", "k29"];
        let keys: Vec<&[u8]> = keys_owned.iter().map(|key| key.as_bytes()).collect();
        let individual: Vec<_> = keys.iter().map(|key| db.get(key).unwrap()).collect();
        let batched = db.multi_get(&keys).unwrap();

        assert_eq!(batched, individual);
        assert_eq!(batched[0], Some(b"v09".to_vec()));
        assert_eq!(batched[1], None);
        assert_eq!(batched[2], None);
        assert_eq!(batched[3], Some(b"v20".to_vec()));
        assert_eq!(batched[4], None);
        assert_eq!(batched[5], Some(b"v09".to_vec()));
        assert_eq!(batched[6], None);
        assert_eq!(batched[7], Some(b"v29".to_vec()));
    }

    // ─── Reverse iteration tests ─────────────────────────────────────────

    fn collect_reverse(db: &Db) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut it = db.iter();
        it.seek_to_last();
        let mut out = Vec::new();
        while it.valid() {
            out.push((it.key().unwrap().to_vec(), it.value().unwrap().to_vec()));
            it.prev();
        }
        it.status().unwrap();
        out
    }

    #[test]
    fn test_iter_seek_to_last_empty() {
        let (db, _dir) = open_tmp();
        let mut it = db.iter();
        it.seek_to_last();
        assert!(!it.valid());
    }

    #[test]
    fn test_iter_reverse_walk_basic() {
        let (db, _dir) = open_tmp();
        for i in 0..10 {
            let k = format!("k{:02}", i);
            db.put(k.as_bytes(), b"v").unwrap();
        }
        let items = collect_reverse(&db);
        assert_eq!(items.len(), 10);
        for (i, (k, _)) in items.iter().enumerate() {
            assert_eq!(k, format!("k{:02}", 9 - i).as_bytes());
        }
    }

    #[test]
    fn test_iter_prev_latest_version() {
        let (db, _dir) = open_tmp();
        db.put(b"a", b"a1").unwrap();
        db.put(b"b", b"b1").unwrap();
        db.put(b"b", b"b2").unwrap();
        db.put(b"c", b"c1").unwrap();

        let mut it = db.iter();
        it.seek_to_last();
        assert_eq!(it.key(), Some(b"c".as_ref()));
        it.prev();
        assert_eq!(it.key(), Some(b"b".as_ref()));
        assert_eq!(it.value(), Some(b"b2".as_ref()));
        it.prev();
        assert_eq!(it.key(), Some(b"a".as_ref()));
        it.prev();
        assert!(!it.valid());
    }

    #[test]
    fn test_iter_seek_for_prev_then_prev() {
        let (db, _dir) = open_tmp();
        db.put(b"a", b"1").unwrap();
        db.put(b"c", b"3").unwrap();
        db.put(b"e", b"5").unwrap();
        db.put(b"g", b"7").unwrap();

        let mut it = db.iter();
        it.seek_for_prev(b"f");
        assert_eq!(it.key(), Some(b"e".as_ref()));
        it.prev();
        assert_eq!(it.key(), Some(b"c".as_ref()));
        it.prev();
        assert_eq!(it.key(), Some(b"a".as_ref()));
        it.prev();
        assert!(!it.valid());
    }

    #[test]
    fn test_iter_reverse_across_flush_levels() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        for i in 0..20 {
            let k = format!("k{:02}", i);
            crate::through_stalls(|| db.put(k.as_bytes(), b"v")).unwrap();
        }
        force_flush(&db, "a");
        for i in 20..30 {
            let k = format!("k{:02}", i);
            crate::through_stalls(|| db.put(k.as_bytes(), b"v")).unwrap();
        }

        let items = collect_reverse(&db);
        let k_count = items.iter().filter(|(k, _)| k.starts_with(b"k")).count();
        assert_eq!(k_count, 30);
        let mut prev_k: Option<Vec<u8>> = None;
        for (k, _) in items.iter().filter(|(k, _)| k.starts_with(b"k")) {
            if let Some(p) = &prev_k {
                assert!(k < p, "not descending: {:?} after {:?}", k, p);
            }
            prev_k = Some(k.clone());
        }
    }

    #[test]
    fn test_iter_reverse_hides_tombstoned_user_key() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        crate::through_stalls(|| db.put(b"keep", b"v")).unwrap();
        crate::through_stalls(|| db.put(b"gone", b"v")).unwrap();
        force_flush(&db, "a");
        crate::through_stalls(|| db.delete(b"gone")).unwrap();

        let items = collect_reverse(&db);
        let keys: Vec<_> = items.iter().map(|(k, _)| k.clone()).collect();
        assert!(keys.contains(&b"keep".to_vec()));
        assert!(!keys.contains(&b"gone".to_vec()));
    }

    #[test]
    fn test_iter_reverse_honors_snapshot_isolation() {
        let (db, _dir) = open_tmp();
        db.put(b"k", b"v1").unwrap();
        let snap = db.snapshot();
        db.put(b"k", b"v2").unwrap();

        let mut it = snap.iter();
        it.seek_to_last();
        assert_eq!(it.key(), Some(b"k".as_ref()));
        assert_eq!(it.value(), Some(b"v1".as_ref()));
    }

    #[test]
    fn test_iter_direction_flip_forward_to_reverse() {
        let (db, _dir) = open_tmp();
        for c in b'a'..=b'e' {
            db.put(&[c], &[c]).unwrap();
        }

        let mut it = db.iter();
        it.seek_to_first();
        assert_eq!(it.key(), Some(b"a".as_ref()));
        it.next();
        assert_eq!(it.key(), Some(b"b".as_ref()));
        it.next();
        assert_eq!(it.key(), Some(b"c".as_ref()));

        it.prev();
        assert_eq!(it.key(), Some(b"b".as_ref()));
        it.prev();
        assert_eq!(it.key(), Some(b"a".as_ref()));
        it.prev();
        assert!(!it.valid());
    }

    #[test]
    fn test_iter_direction_flip_reverse_to_forward() {
        let (db, _dir) = open_tmp();
        for c in b'a'..=b'e' {
            db.put(&[c], &[c]).unwrap();
        }

        let mut it = db.iter();
        it.seek_to_last();
        assert_eq!(it.key(), Some(b"e".as_ref()));
        it.prev();
        assert_eq!(it.key(), Some(b"d".as_ref()));
        it.prev();
        assert_eq!(it.key(), Some(b"c".as_ref()));

        it.next();
        assert_eq!(it.key(), Some(b"d".as_ref()));
        it.next();
        assert_eq!(it.key(), Some(b"e".as_ref()));
        it.next();
        assert!(!it.valid());
    }

    #[test]
    fn test_iter_reverse_scan_10k_keys_after_flush() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        const N: usize = 10_000;
        for i in 0..N {
            let k = format!("key_{:06}", i);
            // Ten thousand writes into a 4 KiB buffer can outrun the
            // worker's compactions and meet the L0 stop, which a write
            // reports rather than waits out; this test is about the scan.
            through_stalls(|| db.put(k.as_bytes(), b"v")).unwrap();
        }

        let mut it = db.iter();
        it.seek_for_prev(b"key_~"); // '~' sorts after digits
        let mut count = 0;
        let mut prev: Option<Vec<u8>> = None;
        while it.valid() {
            let k = it.key().unwrap().to_vec();
            if !k.starts_with(b"key_") {
                it.prev();
                continue;
            }
            if let Some(p) = &prev {
                assert!(k < *p, "not descending: {:?} after {:?}", k, p);
            }
            prev = Some(k);
            count += 1;
            it.prev();
        }
        assert_eq!(count, N);
        assert!(it.status().is_ok());
    }

    #[test]
    fn test_iter_reverse_seek_past_end_of_multi_block_sst() {
        // Regression: SsTableLevelIter::seek_for_prev used to fall back
        // to block 0 when the target exceeded every entry in the SST.
        // The correct fallback is the *last* block, so reverse walks
        // that start past the end actually visit every user key.
        //
        // Forces a multi-block SSTable with a small `block_size`, flushes
        // to L0 via `close()` so the data is guaranteed to be on disk,
        // then reopens and runs `seek_for_prev` with a target larger
        // than every key.
        let dir = TempDir::new().unwrap();
        let opts = Options::default()
            .block_size(128)
            .write_buffer_size(64 * 1024);
        {
            let db = Db::open(dir.path(), opts.clone()).unwrap();
            for i in 0..60u32 {
                let k = format!("k{:03}", i);
                crate::through_stalls(|| db.put(k.as_bytes(), b"v")).unwrap();
            }
            db.close().unwrap();
        }

        let db = Db::open(dir.path(), opts).unwrap();
        let mut it = db.iter();
        it.seek_for_prev(b"~"); // '~' sorts after 'k'

        let mut seen = Vec::new();
        while it.valid() {
            seen.push(it.key().unwrap().to_vec());
            it.prev();
        }
        assert_eq!(seen.len(), 60);
        assert_eq!(seen.first().map(|k| k.as_slice()), Some(&b"k059"[..]));
        assert_eq!(seen.last().map(|k| k.as_slice()), Some(&b"k000"[..]));
    }

    #[test]
    fn test_iter_seek_for_prev_on_tombstoned_key() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        crate::through_stalls(|| db.put(b"a", b"a1")).unwrap();
        crate::through_stalls(|| db.put(b"b", b"b1")).unwrap();
        crate::through_stalls(|| db.put(b"c", b"c1")).unwrap();
        force_flush(&db, "x");
        crate::through_stalls(|| db.delete(b"b")).unwrap();

        let mut it = db.iter();
        it.seek_for_prev(b"b");
        // `b` is tombstoned, so reverse-seek to `b` should skip past it
        // and land on `a`.
        assert_eq!(it.key(), Some(b"a".as_ref()));
    }

    #[test]
    fn test_iter_survives_drop_all() {
        // drop_all unlinks every SSTable file. An iterator captured before
        // drop_all holds its own Arc<SsTableReader>s (each with an open
        // File), so OS fd refcounting keeps the bytes alive and the
        // iterator continues to produce its original view.
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();

        for i in 0..20 {
            let k = format!("pin{:03}", i);
            crate::through_stalls(|| db.put(k.as_bytes(), b"v")).unwrap();
        }
        force_flush(&db, "pinned");

        let mut it = db.iter();
        it.seek_to_first();

        db.drop_all().unwrap();

        let mut seen_pinned = 0;
        while it.valid() {
            if it.key().unwrap().starts_with(b"pin") {
                seen_pinned += 1;
            }
            it.next();
        }
        assert_eq!(seen_pinned, 20);
        assert!(it.status().is_ok());
    }

    #[test]
    fn test_persistence() {
        let dir = TempDir::new().unwrap();

        {
            let db = Db::open(dir.path(), Options::default()).unwrap();
            db.put(b"persist", b"data").unwrap();
            db.close().unwrap();
        }

        {
            let db = Db::open(dir.path(), Options::default()).unwrap();
            assert_eq!(db.get(b"persist").unwrap(), Some(b"data".to_vec()));
        }
    }

    // ── delete_range ────────────────────────────────────────────────────────

    #[test]
    fn test_delete_range_basic() {
        let (db, _dir) = open_tmp();
        for c in b'a'..=b'j' {
            db.put(&[c], &[c]).unwrap();
        }
        db.delete_range(b"c", b"g").unwrap();

        assert_eq!(db.get(b"a").unwrap(), Some(b"a".to_vec()));
        assert_eq!(db.get(b"b").unwrap(), Some(b"b".to_vec()));
        assert_eq!(db.get(b"c").unwrap(), None);
        assert_eq!(db.get(b"d").unwrap(), None);
        assert_eq!(db.get(b"e").unwrap(), None);
        assert_eq!(db.get(b"f").unwrap(), None);
        assert_eq!(db.get(b"g").unwrap(), Some(b"g".to_vec())); // end exclusive
        assert_eq!(db.get(b"j").unwrap(), Some(b"j".to_vec()));
    }

    #[test]
    fn test_delete_range_no_op_for_empty_or_inverted() {
        let (db, _dir) = open_tmp();
        db.put(b"a", b"1").unwrap();
        // Inverted range should be a silent no-op.
        db.delete_range(b"z", b"a").unwrap();
        // Equal bounds should also be a no-op (half-open empty range).
        db.delete_range(b"a", b"a").unwrap();
        assert_eq!(db.get(b"a").unwrap(), Some(b"1".to_vec()));
    }

    #[test]
    fn test_delete_range_then_put_inside_range() {
        let (db, _dir) = open_tmp();
        db.put(b"k", b"old").unwrap();
        db.delete_range(b"a", b"z").unwrap();
        // A put after the range delete must win - it has a higher seq.
        db.put(b"k", b"new").unwrap();
        assert_eq!(db.get(b"k").unwrap(), Some(b"new".to_vec()));
    }

    #[test]
    fn test_delete_range_put_then_range_delete_then_overwrite() {
        let (db, _dir) = open_tmp();
        db.put(b"k", b"v1").unwrap();
        db.delete_range(b"a", b"z").unwrap();
        assert_eq!(db.get(b"k").unwrap(), None);
        db.put(b"k", b"v2").unwrap();
        assert_eq!(db.get(b"k").unwrap(), Some(b"v2".to_vec()));
    }

    #[test]
    fn test_delete_range_snapshot_isolation() {
        let (db, _dir) = open_tmp();
        db.put(b"k", b"v1").unwrap();
        let snap = db.snapshot();
        db.delete_range(b"a", b"z").unwrap();
        assert_eq!(db.get(b"k").unwrap(), None);
        // Snapshot is anchored before the range delete.
        assert_eq!(snap.get(b"k").unwrap(), Some(b"v1".to_vec()));
    }

    #[test]
    fn test_delete_range_survives_flush() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();
        for i in 0..20 {
            crate::through_stalls(|| db.put(format!("key_{:02}", i).as_bytes(), b"v")).unwrap();
        }
        crate::through_stalls(|| db.delete_range(b"key_05", b"key_15")).unwrap();
        force_flush(&db, "rt");
        for i in 0..20 {
            let key = format!("key_{:02}", i);
            let got = db.get(key.as_bytes()).unwrap();
            if (5..15).contains(&i) {
                assert_eq!(got, None, "key {} should be deleted", key);
            } else {
                assert_eq!(got, Some(b"v".to_vec()), "key {} should survive", key);
            }
        }
    }

    #[test]
    fn test_delete_range_survives_compaction() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();
        for i in 0..30 {
            crate::through_stalls(|| db.put(format!("key_{:02}", i).as_bytes(), b"v")).unwrap();
        }
        crate::through_stalls(|| db.delete_range(b"key_10", b"key_20")).unwrap();
        // Force several flushes + a manual compaction down to L1+.
        for tag in 0..6 {
            force_flush(&db, &format!("c{}", tag));
        }
        db.compact_range(None, None).wait().unwrap();

        for i in 0..30 {
            let key = format!("key_{:02}", i);
            let got = db.get(key.as_bytes()).unwrap();
            if (10..20).contains(&i) {
                assert_eq!(got, None, "key {} should be deleted post-compact", key);
            } else {
                assert_eq!(
                    got,
                    Some(b"v".to_vec()),
                    "key {} should survive compact",
                    key
                );
            }
        }
    }

    #[test]
    fn test_delete_range_iterator_skips_deleted() {
        let (db, _dir) = open_tmp();
        for c in b'a'..=b'h' {
            db.put(&[c], &[c]).unwrap();
        }
        db.delete_range(b"c", b"f").unwrap();

        let results = db.scan(None, None).unwrap();
        let keys: Vec<u8> = results.iter().map(|(k, _)| k[0]).collect();
        assert_eq!(keys, vec![b'a', b'b', b'f', b'g', b'h']);
    }

    #[test]
    fn test_delete_range_reverse_iterator_skips_deleted() {
        let (db, _dir) = open_tmp();
        for c in b'a'..=b'h' {
            db.put(&[c], &[c]).unwrap();
        }
        db.delete_range(b"c", b"f").unwrap();

        let mut iter = db.iter();
        iter.seek_to_last();
        let mut keys = Vec::new();
        while iter.valid() {
            keys.push(iter.key().unwrap()[0]);
            iter.prev();
        }
        assert_eq!(keys, vec![b'h', b'g', b'f', b'b', b'a']);
    }

    #[test]
    fn test_delete_range_multi_get_honors_rt() {
        let (db, _dir) = open_tmp();
        for c in b'a'..=b'f' {
            db.put(&[c], &[c]).unwrap();
        }
        db.delete_range(b"b", b"e").unwrap();

        let keys: Vec<&[u8]> = vec![b"a", b"b", b"c", b"d", b"e", b"f"];
        let got = db.multi_get(&keys).unwrap();
        assert_eq!(got[0], Some(b"a".to_vec()));
        assert_eq!(got[1], None);
        assert_eq!(got[2], None);
        assert_eq!(got[3], None);
        assert_eq!(got[4], Some(b"e".to_vec()));
        assert_eq!(got[5], Some(b"f".to_vec()));
    }

    #[test]
    fn test_delete_range_crash_recovery() {
        let dir = TempDir::new().unwrap();
        {
            let db = Db::open(dir.path(), Options::default()).unwrap();
            for c in b'a'..=b'e' {
                db.put(&[c], &[c]).unwrap();
            }
            db.delete_range(b"b", b"d").unwrap();
            // Drop without close - only the WAL has the range delete.
        }
        let db = Db::open(dir.path(), Options::default()).unwrap();
        assert_eq!(db.get(b"a").unwrap(), Some(b"a".to_vec()));
        assert_eq!(db.get(b"b").unwrap(), None);
        assert_eq!(db.get(b"c").unwrap(), None);
        assert_eq!(db.get(b"d").unwrap(), Some(b"d".to_vec()));
        assert_eq!(db.get(b"e").unwrap(), Some(b"e".to_vec()));
    }

    #[test]
    fn test_delete_range_in_write_batch() {
        let (db, _dir) = open_tmp();
        for c in b'a'..=b'f' {
            db.put(&[c], &[c]).unwrap();
        }
        let mut batch = WriteBatch::new();
        batch.put(b"x", b"x");
        batch.delete_range(b"b", b"e");
        batch.put(b"y", b"y");
        db.write(batch).unwrap();

        assert_eq!(db.get(b"a").unwrap(), Some(b"a".to_vec()));
        assert_eq!(db.get(b"b").unwrap(), None);
        assert_eq!(db.get(b"c").unwrap(), None);
        assert_eq!(db.get(b"d").unwrap(), None);
        assert_eq!(db.get(b"e").unwrap(), Some(b"e".to_vec()));
        assert_eq!(db.get(b"x").unwrap(), Some(b"x".to_vec()));
        assert_eq!(db.get(b"y").unwrap(), Some(b"y".to_vec()));
    }

    #[test]
    fn test_write_batch_delete_range_then_put_inside_range_keeps_put() {
        let (db, _dir) = open_tmp();
        db.put(b"k", b"old").unwrap();

        let mut batch = WriteBatch::new();
        batch.delete_range(b"a", b"z");
        batch.put(b"k", b"new");
        db.write(batch).unwrap();

        assert_eq!(db.get(b"k").unwrap(), Some(b"new".to_vec()));
    }

    #[test]
    fn test_write_batch_put_then_delete_range_inside_range_deletes_put() {
        let (db, _dir) = open_tmp();

        let mut batch = WriteBatch::new();
        batch.put(b"k", b"new");
        batch.delete_range(b"a", b"z");
        db.write(batch).unwrap();

        assert_eq!(db.get(b"k").unwrap(), None);
    }

    #[test]
    fn test_write_batch_order_survives_wal_replay() {
        let dir = TempDir::new().unwrap();
        {
            let db = Db::open(dir.path(), Options::default()).unwrap();
            db.put(b"k", b"old").unwrap();

            let mut batch = WriteBatch::new();
            batch.delete_range(b"a", b"z");
            batch.put(b"k", b"new");
            db.write(batch).unwrap();
            // Drop without an explicit close so reopen must recover
            // the ordered batch from the WAL.
        }

        let db = Db::open(dir.path(), Options::default()).unwrap();
        assert_eq!(db.get(b"k").unwrap(), Some(b"new".to_vec()));
    }

    #[test]
    fn test_delete_range_overlapping_ranges() {
        let (db, _dir) = open_tmp();
        for c in b'a'..=b'j' {
            db.put(&[c], &[c]).unwrap();
        }
        db.delete_range(b"b", b"e").unwrap();
        db.delete_range(b"d", b"h").unwrap();

        assert_eq!(db.get(b"a").unwrap(), Some(b"a".to_vec()));
        for c in b'b'..=b'g' {
            assert_eq!(db.get(&[c]).unwrap(), None, "key {} deleted", c as char);
        }
        assert_eq!(db.get(b"h").unwrap(), Some(b"h".to_vec()));
    }

    // ── compression codecs ──────────────────────────────────────────────────

    fn compression_opts(codec: CompressionType) -> Options {
        Options::default()
            .write_buffer_size(4 * 1024)
            .compression(codec)
    }

    fn write_and_read_back(opts: Options) {
        let dir = TempDir::new().unwrap();
        let payload: Vec<u8> = (0..256).map(|i| (i % 31) as u8).collect();
        {
            let db = Db::open(dir.path(), opts.clone()).unwrap();
            for i in 0..200 {
                let key = format!("key_{:04}", i);
                crate::through_stalls(|| db.put(key.as_bytes(), &payload)).unwrap();
            }
            // Force a flush so reads must go through the SSTable codec path.
            force_flush(&db, "comp");
            for i in 0..200 {
                let key = format!("key_{:04}", i);
                assert_eq!(
                    db.get(key.as_bytes()).unwrap().as_deref(),
                    Some(payload.as_slice()),
                    "round-trip failed for {key}"
                );
            }
            db.close().unwrap();
        }
        // Reopen to verify the on-disk codec is decoded correctly by a
        // fresh reader.
        let db = Db::open(dir.path(), opts).unwrap();
        for i in 0..200 {
            let key = format!("key_{:04}", i);
            assert_eq!(
                db.get(key.as_bytes()).unwrap().as_deref(),
                Some(payload.as_slice())
            );
        }
    }

    #[test]
    fn test_compression_none_roundtrip() {
        write_and_read_back(compression_opts(CompressionType::None));
    }

    #[test]
    fn test_compression_lz4_roundtrip() {
        write_and_read_back(compression_opts(CompressionType::Lz4));
    }

    #[test]
    fn test_compression_snappy_roundtrip() {
        write_and_read_back(compression_opts(CompressionType::Snappy));
    }

    #[test]
    fn test_compression_per_level_mixed_codecs() {
        // L0 = Snappy, L1+ = Lz4. After a flush + manual compaction the
        // database must hold blocks compressed with both codecs and
        // still read back correctly.
        let dir = TempDir::new().unwrap();
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .compression(CompressionType::Lz4)
            .compression_per_level(Some(vec![
                CompressionType::Snappy, // L0
                CompressionType::Lz4,    // L1
                CompressionType::None,   // L2 (unused here, just to exercise the slot)
            ]));
        let payload: Vec<u8> = (0..256).map(|i| (i % 17) as u8).collect();
        {
            let db = Db::open(dir.path(), opts.clone()).unwrap();
            for i in 0..300 {
                let key = format!("k_{:04}", i);
                crate::through_stalls(|| db.put(key.as_bytes(), &payload)).unwrap();
            }
            force_flush(&db, "mix");
            // Push everything down to L1 with the manual compaction path.
            db.compact_range(None, None).wait().unwrap();
            for i in 0..300 {
                let key = format!("k_{:04}", i);
                assert_eq!(
                    db.get(key.as_bytes()).unwrap().as_deref(),
                    Some(payload.as_slice())
                );
            }
            db.close().unwrap();
        }
        // Reopen and re-read so the test exercises a fresh reader
        // hitting both codecs through the level layout we just built.
        let db = Db::open(dir.path(), opts).unwrap();
        for i in 0..300 {
            let key = format!("k_{:04}", i);
            assert_eq!(
                db.get(key.as_bytes()).unwrap().as_deref(),
                Some(payload.as_slice())
            );
        }
    }

    #[test]
    fn test_compression_per_level_falls_back_to_default() {
        // Override only L0; L1+ should fall back to `compression`.
        let dir = TempDir::new().unwrap();
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .compression(CompressionType::Snappy)
            .compression_per_level(Some(vec![CompressionType::None]));
        let db = Db::open(dir.path(), opts).unwrap();
        for i in 0..50 {
            crate::through_stalls(|| db.put(format!("k_{i:03}").as_bytes(), b"v")).unwrap();
        }
        force_flush(&db, "fb");
        db.compact_range(None, None).wait().unwrap();
        for i in 0..50 {
            assert_eq!(
                db.get(format!("k_{i:03}").as_bytes()).unwrap(),
                Some(b"v".to_vec())
            );
        }
    }

    // ── compaction filter ───────────────────────────────────────────────────

    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    /// Test filter that drops every entry whose user key ends in an
    /// odd ASCII digit. Also counts invocations so tests can verify
    /// the filter actually ran.
    struct DropOddKeysFilter {
        calls: AtomicUsize,
    }

    impl CompactionFilter for DropOddKeysFilter {
        fn filter(&self, _level: usize, key: &[u8], _value: &[u8]) -> CompactionDecision {
            self.calls.fetch_add(1, AtomicOrdering::Relaxed);
            match key.last() {
                Some(b) if b.is_ascii_digit() && (b - b'0') % 2 == 1 => CompactionDecision::Remove,
                _ => CompactionDecision::Keep,
            }
        }
        fn name(&self) -> &'static str {
            "drop_odd_keys"
        }
    }

    /// Test filter that uppercases every ASCII-lowercase byte in the
    /// value. Exercises `Change`.
    struct UppercaseValuesFilter;

    impl CompactionFilter for UppercaseValuesFilter {
        fn filter(&self, _level: usize, _key: &[u8], value: &[u8]) -> CompactionDecision {
            let up: Vec<u8> = value.iter().map(|b| b.to_ascii_uppercase()).collect();
            if up == value {
                CompactionDecision::Keep
            } else {
                CompactionDecision::Change(up)
            }
        }
        fn name(&self) -> &'static str {
            "uppercase_values"
        }
    }

    /// Filter that drops every range tombstone it sees.
    struct DropRangeTombstonesFilter;

    impl CompactionFilter for DropRangeTombstonesFilter {
        fn filter(&self, _level: usize, _key: &[u8], _value: &[u8]) -> CompactionDecision {
            CompactionDecision::Keep
        }
        fn filter_range_delete(
            &self,
            _level: usize,
            _start: &[u8],
            _end: &[u8],
        ) -> CompactionDecision {
            CompactionDecision::Remove
        }
        fn name(&self) -> &'static str {
            "drop_range_tombstones"
        }
    }

    #[test]
    fn test_compaction_filter_removes_matching_entries() {
        let dir = TempDir::new().unwrap();
        let filter = Arc::new(DropOddKeysFilter {
            calls: AtomicUsize::new(0),
        });
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .compaction_filter(Some(filter.clone()));
        let db = Db::open(dir.path(), opts).unwrap();
        // 20 keys: k0..k9 written twice so compaction has work. Use
        // longer payloads so the tiny write buffer triggers flushes.
        let payload = vec![b'v'; 512];
        for _round in 0..4 {
            for i in 0..10 {
                crate::through_stalls(|| db.put(format!("k{i}").as_bytes(), &payload)).unwrap();
            }
        }
        db.compact_range(None, None).wait().unwrap();

        // After compaction, odd-suffix keys are gone.
        for i in 0..10 {
            let got = db.get(format!("k{i}").as_bytes()).unwrap();
            if i % 2 == 1 {
                assert_eq!(got, None, "k{i} should be filtered");
            } else {
                assert_eq!(got, Some(payload.clone()), "k{i} should survive");
            }
        }
        assert!(
            filter.calls.load(AtomicOrdering::Relaxed) > 0,
            "filter should have been invoked"
        );
    }

    #[test]
    fn test_compaction_filter_rewrites_values() {
        let dir = TempDir::new().unwrap();
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .compaction_filter(Some(Arc::new(UppercaseValuesFilter)));
        let db = Db::open(dir.path(), opts).unwrap();
        for i in 0..20 {
            crate::through_stalls(|| db.put(format!("k{i:02}").as_bytes(), b"hello world"))
                .unwrap();
        }
        // Force enough flushes + manual compaction to run the filter.
        force_flush(&db, "filter");
        db.compact_range(None, None).wait().unwrap();

        for i in 0..20 {
            assert_eq!(
                db.get(format!("k{i:02}").as_bytes()).unwrap(),
                Some(b"HELLO WORLD".to_vec())
            );
        }
    }

    #[test]
    fn test_compaction_filter_runs_in_every_snapshot_stripe() {
        let dir = TempDir::new().unwrap();
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .compaction_filter(Some(Arc::new(UppercaseValuesFilter)));
        let db = Db::open(dir.path(), opts).unwrap();
        for i in 0..20 {
            crate::through_stalls(|| db.put(format!("k{i:02}").as_bytes(), b"hello")).unwrap();
        }
        let snap = db.snapshot();
        for i in 0..20 {
            crate::through_stalls(|| db.put(format!("k{i:02}").as_bytes(), b"world")).unwrap();
        }
        force_flush(&db, "snap_filter");
        db.compact_range(None, None).wait().unwrap();

        // Compaction keeps one version per side of the snapshot, and
        // the filter runs on both: the snapshot reads its own version,
        // filtered, and the live db reads the newer one, filtered.
        for i in 0..20 {
            let key = format!("k{i:02}");
            assert_eq!(snap.get(key.as_bytes()).unwrap(), Some(b"HELLO".to_vec()));
            assert_eq!(db.get(key.as_bytes()).unwrap(), Some(b"WORLD".to_vec()));
        }
    }

    #[test]
    fn test_compaction_filter_drops_range_tombstones() {
        let dir = TempDir::new().unwrap();
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .compaction_filter(Some(Arc::new(DropRangeTombstonesFilter)));
        let db = Db::open(dir.path(), opts).unwrap();
        for c in b'a'..=b'f' {
            crate::through_stalls(|| db.put(&[c], &[c])).unwrap();
        }
        crate::through_stalls(|| db.delete_range(b"b", b"e")).unwrap();
        // Before compaction, the range-delete is honored - no snapshot
        // pinning, so the read path sees the memtable RT directly.
        for c in b'b'..=b'd' {
            assert_eq!(db.get(&[c]).unwrap(), None);
        }
        force_flush(&db, "drop_rt");
        db.compact_range(None, None).wait().unwrap();

        // After compaction the filter dropped the RT, so the original
        // values come back (they were never actually overwritten).
        for c in b'a'..=b'f' {
            assert_eq!(
                db.get(&[c]).unwrap(),
                Some(vec![c]),
                "key {} restored",
                c as char
            );
        }
    }

    fn prefix_opts() -> Options {
        Options::default()
            .write_buffer_size(4 * 1024)
            .prefix_extractor(Some(std::sync::Arc::new(FixedLengthPrefix(10))))
    }

    #[test]
    fn test_seek_prefix_basic() {
        let (db, _dir) = open_tmp();
        db.put(b"tenant_001:k1", b"1").unwrap();
        db.put(b"tenant_001:k2", b"2").unwrap();
        db.put(b"tenant_002:k1", b"3").unwrap();
        db.put(b"tenant_010:k1", b"4").unwrap();

        let mut it = db.iter();
        it.seek_prefix(b"tenant_001");
        let mut got: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        while it.valid() {
            got.push((it.key().unwrap().to_vec(), it.value().unwrap().to_vec()));
            it.next();
        }
        assert_eq!(
            got,
            vec![
                (b"tenant_001:k1".to_vec(), b"1".to_vec()),
                (b"tenant_001:k2".to_vec(), b"2".to_vec()),
            ]
        );
    }

    #[test]
    fn test_seek_prefix_absent_returns_empty() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), prefix_opts()).unwrap();
        for i in 0..200 {
            let key = format!("tenant_001:k{:04}", i);
            crate::through_stalls(|| db.put(key.as_bytes(), b"v")).unwrap();
        }
        force_flush(&db, "p");

        let mut it = db.iter();
        it.seek_prefix(b"tenant_999");
        assert!(!it.valid(), "expected no keys under an absent prefix");
    }

    #[test]
    fn test_seek_prefix_uses_extracted_bloom_probe_for_longer_prefix() {
        let stats = Arc::new(Statistics::new());
        let dir = TempDir::new().unwrap();
        let db = Db::open(
            dir.path(),
            Options::default()
                .write_buffer_size(4 * 1024)
                .l0_compaction_trigger(1_000)
                .bloom_bits_per_key(64)
                .prefix_extractor(Some(Arc::new(FixedLengthPrefix(8))))
                .statistics(Some(stats.clone())),
        )
        .unwrap();

        crate::through_stalls(|| db.put(b"aaaa:item", b"v")).unwrap();
        crate::through_stalls(|| db.put(b"zzzz:item", b"v")).unwrap();
        force_flush(&db, "long_prefix_probe");

        stats.reset();
        let mut it = db.iter();
        it.seek_prefix(b"bbbb:item");
        assert!(!it.valid(), "absent long prefix should return empty");
        it.status().unwrap();
        assert_eq!(
            stats.get_ticker(Ticker::BlockCacheMiss),
            0,
            "prefix bloom should skip the SSTable before loading a data block"
        );
    }

    #[test]
    fn test_seek_prefix_across_flush_boundary() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), prefix_opts()).unwrap();

        // First generation → flushed to L0.
        crate::through_stalls(|| db.put(b"tenant_001:a", b"1a")).unwrap();
        crate::through_stalls(|| db.put(b"tenant_002:a", b"2a")).unwrap();
        force_flush(&db, "p1");

        // Second generation → stays in memtable at iteration time.
        crate::through_stalls(|| db.put(b"tenant_001:b", b"1b")).unwrap();
        crate::through_stalls(|| db.put(b"tenant_002:b", b"2b")).unwrap();

        let mut it = db.iter();
        it.seek_prefix(b"tenant_001");
        let mut keys: Vec<Vec<u8>> = Vec::new();
        while it.valid() {
            keys.push(it.key().unwrap().to_vec());
            it.next();
        }
        assert_eq!(
            keys,
            vec![b"tenant_001:a".to_vec(), b"tenant_001:b".to_vec()]
        );
    }

    #[test]
    fn test_seek_prefix_after_compact_range() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), prefix_opts()).unwrap();

        for i in 0..50 {
            crate::through_stalls(|| db.put(format!("tenant_001:{:04}", i).as_bytes(), b"v"))
                .unwrap();
            crate::through_stalls(|| db.put(format!("tenant_002:{:04}", i).as_bytes(), b"v"))
                .unwrap();
        }
        force_flush(&db, "c1");
        db.compact_range(None, None).wait().unwrap();

        let mut it = db.iter();
        it.seek_prefix(b"tenant_002");
        let mut count = 0;
        while it.valid() {
            let k = it.key().unwrap();
            assert!(
                k.starts_with(b"tenant_002"),
                "got unexpected key {:?}",
                std::str::from_utf8(k).unwrap_or("<non-utf8>")
            );
            count += 1;
            it.next();
        }
        assert_eq!(count, 50);
    }

    #[test]
    fn test_seek_prefix_mixed_with_without_extractor() {
        // Open with no extractor, flush some data (file A has no prefix
        // bloom), then reopen with an extractor and write new data
        // (file B has a prefix bloom). Reads through the extractor-
        // configured DB must still return correct results across both
        // files.
        let dir = TempDir::new().unwrap();
        {
            let db = Db::open(dir.path(), Options::default().write_buffer_size(4 * 1024)).unwrap();
            crate::through_stalls(|| db.put(b"tenant_001:old", b"old")).unwrap();
            force_flush(&db, "a");
        }

        let db = Db::open(dir.path(), prefix_opts()).unwrap();
        crate::through_stalls(|| db.put(b"tenant_001:new", b"new")).unwrap();
        crate::through_stalls(|| db.put(b"tenant_002:new", b"new")).unwrap();
        force_flush(&db, "b");

        let mut it = db.iter();
        it.seek_prefix(b"tenant_001");
        let mut keys: Vec<Vec<u8>> = Vec::new();
        while it.valid() {
            keys.push(it.key().unwrap().to_vec());
            it.next();
        }
        assert_eq!(
            keys,
            vec![b"tenant_001:new".to_vec(), b"tenant_001:old".to_vec()]
        );
    }

    #[test]
    fn test_compaction_filter_none_is_noop() {
        let (db, _dir) = open_tmp();
        for i in 0..10 {
            db.put(format!("k{i}").as_bytes(), b"v").unwrap();
        }
        db.compact_range(None, None).wait().unwrap();
        for i in 0..10 {
            assert_eq!(
                db.get(format!("k{i}").as_bytes()).unwrap(),
                Some(b"v".to_vec())
            );
        }
    }

    // ── per-write WriteOptions ──────────────────────────────────────────────

    #[test]
    fn test_write_options_defaults_unchanged() {
        // `put_opt` with a default-constructed WriteOptions must
        // behave identically to `put`.
        let (db, _dir) = open_tmp();
        db.put_opt(&WriteOptions::default(), b"a", b"1").unwrap();
        assert_eq!(db.get(b"a").unwrap(), Some(b"1".to_vec()));
    }

    #[test]
    fn test_write_options_sync_override_persists_across_reopen() {
        // With Eventual default, a sync write should still land on
        // disk such that a reopen recovers it. (Eventual alone
        // already survives a clean close - this test's real content
        // is that the sync flag doesn't break the normal code path.)
        let dir = TempDir::new().unwrap();
        let opts = Options::default().durability(DurabilityMode::Eventual);
        {
            let db = Db::open(dir.path(), opts.clone()).unwrap();
            db.put_opt(&WriteOptions::sync(), b"critical", b"payload")
                .unwrap();
            // Deliberately skip close() - sync must have forced the
            // WAL to durable storage already.
        }
        let db = Db::open(dir.path(), opts).unwrap();
        assert_eq!(db.get(b"critical").unwrap(), Some(b"payload".to_vec()));
    }

    #[test]
    fn test_write_options_disable_wal_loses_data_on_drop_without_flush() {
        // disable_wal skips the WAL append entirely. Without a clean
        // close(), a reopen cannot recover the write because neither
        // the WAL nor an SSTable has it.
        let dir = TempDir::new().unwrap();
        let opts = Options::default();
        {
            let db = Db::open(dir.path(), opts.clone()).unwrap();
            db.put_opt(&WriteOptions::disable_wal(), b"ephemeral", b"ghost")
                .unwrap();
            // No close() - simulate a crash. The memtable holds the
            // write but nothing on disk does.
        }
        let db = Db::open(dir.path(), opts).unwrap();
        assert_eq!(db.get(b"ephemeral").unwrap(), None);
    }

    #[test]
    fn test_write_options_disable_wal_visible_within_session() {
        // Within the same process, a disable_wal write is visible
        // to subsequent reads via the memtable - only a crash
        // erases it.
        let (db, _dir) = open_tmp();
        db.put_opt(&WriteOptions::disable_wal(), b"k", b"v")
            .unwrap();
        assert_eq!(db.get(b"k").unwrap(), Some(b"v".to_vec()));
    }

    #[test]
    fn test_write_options_disable_wal_survives_clean_close() {
        // A clean close() flushes the memtable to an SSTable before
        // shutting down. A disable_wal write still made it into the
        // memtable, so close() + reopen recovers it via the SSTable
        // (not the WAL).
        let dir = TempDir::new().unwrap();
        let opts = Options::default();
        {
            let db = Db::open(dir.path(), opts.clone()).unwrap();
            db.put_opt(&WriteOptions::disable_wal(), b"bulk", b"loaded")
                .unwrap();
            db.close().unwrap();
        }
        let db = Db::open(dir.path(), opts).unwrap();
        assert_eq!(db.get(b"bulk").unwrap(), Some(b"loaded".to_vec()));
    }

    #[test]
    fn test_close_flushes_range_tombstone_only_memtable() {
        // A disable_wal range delete lives only in the active memtable
        // until close. Clean close must flush it even though there are
        // no point entries in that memtable.
        let dir = TempDir::new().unwrap();
        let opts = Options::default();

        {
            let db = Db::open(dir.path(), opts.clone()).unwrap();
            db.put(b"k", b"v").unwrap();
            db.close().unwrap();
        }

        {
            let db = Db::open(dir.path(), opts.clone()).unwrap();
            assert_eq!(db.get(b"k").unwrap(), Some(b"v".to_vec()));
            db.delete_range_opt(&WriteOptions::disable_wal(), b"a", b"z")
                .unwrap();
            assert_eq!(db.get(b"k").unwrap(), None);
            db.close().unwrap();
        }

        let db = Db::open(dir.path(), opts).unwrap();
        assert_eq!(db.get(b"k").unwrap(), None);
    }

    #[test]
    fn test_write_options_batch_overrides() {
        let (db, _dir) = open_tmp();
        let mut batch = WriteBatch::new();
        batch.put(b"a", b"1");
        batch.put(b"b", b"2");
        batch.delete(b"ghost");
        db.write_opt(&WriteOptions::sync(), batch).unwrap();
        assert_eq!(db.get(b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(db.get(b"b").unwrap(), Some(b"2".to_vec()));
    }

    #[test]
    fn test_write_options_delete_and_delete_range_opts() {
        let (db, _dir) = open_tmp();
        for c in b'a'..=b'f' {
            db.put(&[c], &[c]).unwrap();
        }
        db.delete_opt(&WriteOptions::sync(), b"c").unwrap();
        db.delete_range_opt(&WriteOptions::sync(), b"d", b"f")
            .unwrap();
        assert_eq!(db.get(b"a").unwrap(), Some(b"a".to_vec()));
        assert_eq!(db.get(b"c").unwrap(), None);
        assert_eq!(db.get(b"d").unwrap(), None);
        assert_eq!(db.get(b"e").unwrap(), None);
        assert_eq!(db.get(b"f").unwrap(), Some(b"f".to_vec()));
    }

    #[test]
    fn test_write_options_low_pri_and_no_slowdown_pass_through_when_not_stalling() {
        // `low_pri` is accepted and ignored. `no_slowdown` only bites
        // while the engine is stalling, and an idle engine is not.
        let (db, _dir) = open_tmp();
        let opts = WriteOptions {
            low_pri: true,
            no_slowdown: true,
            ..WriteOptions::default()
        };
        db.put_opt(&opts, b"k", b"v").unwrap();
        assert_eq!(db.get(b"k").unwrap(), Some(b"v".to_vec()));
    }

    // ── merge operator ──────────────────────────────────────────────────────

    /// Integer-counter merge operator: every operand is the 8-byte
    /// big-endian i64 delta to add. `full_merge` sums them (starting
    /// from `base` if present) and emits the new counter value.
    /// `partial_merge` folds two deltas by adding them.
    struct CounterMerge;

    impl MergeOperator for CounterMerge {
        fn full_merge(
            &self,
            _key: &[u8],
            base: Option<&[u8]>,
            operands: &[&[u8]],
        ) -> Option<Vec<u8>> {
            let mut total: i64 = match base {
                Some(b) if b.len() == 8 => i64::from_be_bytes(b.try_into().unwrap()),
                Some(_) => return None,
                None => 0,
            };
            for op in operands {
                if op.len() != 8 {
                    return None;
                }
                total = total.wrapping_add(i64::from_be_bytes((*op).try_into().unwrap()));
            }
            Some(total.to_be_bytes().to_vec())
        }

        fn partial_merge(&self, _key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
            if left.len() != 8 || right.len() != 8 {
                return None;
            }
            let l = i64::from_be_bytes(left.try_into().unwrap());
            let r = i64::from_be_bytes(right.try_into().unwrap());
            Some(l.wrapping_add(r).to_be_bytes().to_vec())
        }

        fn name(&self) -> &'static str {
            "CounterMerge"
        }
    }

    /// String-append merge operator: every operand is raw bytes;
    /// `full_merge` concatenates the base (if any) with every
    /// operand in oldest-first order.
    struct AppendMerge;

    impl MergeOperator for AppendMerge {
        fn full_merge(
            &self,
            _key: &[u8],
            base: Option<&[u8]>,
            operands: &[&[u8]],
        ) -> Option<Vec<u8>> {
            let mut out: Vec<u8> = base.map(|b| b.to_vec()).unwrap_or_default();
            for op in operands {
                out.extend_from_slice(op);
            }
            Some(out)
        }

        fn name(&self) -> &'static str {
            "AppendMerge"
        }
    }

    fn counter_opts() -> Options {
        Options::default()
            .write_buffer_size(4 * 1024)
            .merge_operator(Some(Arc::new(CounterMerge)))
    }

    fn encode_i64(n: i64) -> Vec<u8> {
        n.to_be_bytes().to_vec()
    }

    #[test]
    fn test_merge_counter_basic_chain_of_one() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), counter_opts()).unwrap();
        crate::through_stalls(|| db.merge(b"counter", &encode_i64(5))).unwrap();
        assert_eq!(db.get(b"counter").unwrap(), Some(encode_i64(5)));
    }

    #[test]
    fn test_merge_counter_chain_of_two() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), counter_opts()).unwrap();
        crate::through_stalls(|| db.put(b"counter", &encode_i64(10))).unwrap();
        crate::through_stalls(|| db.merge(b"counter", &encode_i64(3))).unwrap();
        assert_eq!(db.get(b"counter").unwrap(), Some(encode_i64(13)));
    }

    #[test]
    fn test_merge_counter_chain_of_ten() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), counter_opts()).unwrap();
        crate::through_stalls(|| db.put(b"counter", &encode_i64(100))).unwrap();
        for i in 1..=10 {
            crate::through_stalls(|| db.merge(b"counter", &encode_i64(i))).unwrap();
        }
        // 100 + (1+2+...+10) = 155
        assert_eq!(db.get(b"counter").unwrap(), Some(encode_i64(155)));
    }

    #[test]
    fn test_merge_counter_chain_of_1000() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), counter_opts()).unwrap();
        for _ in 0..1000 {
            crate::through_stalls(|| db.merge(b"counter", &encode_i64(1))).unwrap();
        }
        assert_eq!(db.get(b"counter").unwrap(), Some(encode_i64(1000)));
    }

    #[test]
    fn test_merge_without_base_defaults_to_none() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), counter_opts()).unwrap();
        // No put - counter starts at 0 (base=None).
        crate::through_stalls(|| db.merge(b"counter", &encode_i64(7))).unwrap();
        crate::through_stalls(|| db.merge(b"counter", &encode_i64(5))).unwrap();
        assert_eq!(db.get(b"counter").unwrap(), Some(encode_i64(12)));
    }

    #[test]
    fn test_merge_failure_surfaces_key() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), counter_opts()).unwrap();
        crate::through_stalls(|| db.merge(b"counter", b"not an i64")).unwrap();

        let err = db.get(b"counter").unwrap_err();
        match err {
            Error::MergeFailed(key) => assert_eq!(key, b"counter".to_vec()),
            other => panic!("expected merge failure, got {other:?}"),
        }
    }

    #[test]
    fn test_merge_snapshot_isolation() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), counter_opts()).unwrap();
        crate::through_stalls(|| db.put(b"counter", &encode_i64(10))).unwrap();
        let snap = db.snapshot();
        crate::through_stalls(|| db.merge(b"counter", &encode_i64(5))).unwrap();
        // Live read sees 15; snapshot still sees 10.
        assert_eq!(db.get(b"counter").unwrap(), Some(encode_i64(15)));
        assert_eq!(snap.get(b"counter").unwrap(), Some(encode_i64(10)));
    }

    #[test]
    fn test_merge_survives_flush() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), counter_opts()).unwrap();
        crate::through_stalls(|| db.put(b"counter", &encode_i64(0))).unwrap();
        for i in 1..=20 {
            crate::through_stalls(|| db.merge(b"counter", &encode_i64(i))).unwrap();
        }
        // Push past the tiny write buffer so the chain crosses a
        // flush boundary (memtable → L0).
        force_flush(&db, "merge");
        // Sum = 1+2+...+20 = 210
        assert_eq!(db.get(b"counter").unwrap(), Some(encode_i64(210)));
    }

    #[test]
    fn test_merge_survives_compaction_and_collapses() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), counter_opts()).unwrap();
        crate::through_stalls(|| db.put(b"counter", &encode_i64(0))).unwrap();
        for i in 1..=50 {
            crate::through_stalls(|| db.merge(b"counter", &encode_i64(i))).unwrap();
        }
        for tag in 0..4 {
            force_flush(&db, &format!("c{tag}"));
        }
        db.compact_range(None, None).wait().unwrap();
        // Sum 1..=50 = 1275
        assert_eq!(db.get(b"counter").unwrap(), Some(encode_i64(1275)));
    }

    #[test]
    fn test_merge_tombstone_interaction() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), counter_opts()).unwrap();
        // Value=10, then two merges, then delete, then two more merges.
        crate::through_stalls(|| db.put(b"k", &encode_i64(10))).unwrap();
        crate::through_stalls(|| db.merge(b"k", &encode_i64(5))).unwrap();
        crate::through_stalls(|| db.merge(b"k", &encode_i64(3))).unwrap();
        crate::through_stalls(|| db.delete(b"k")).unwrap();
        crate::through_stalls(|| db.merge(b"k", &encode_i64(7))).unwrap();
        crate::through_stalls(|| db.merge(b"k", &encode_i64(1))).unwrap();
        // Reads layer the two latest merges on top of the deletion
        // (which resets the base to None → 0): 0 + 7 + 1 = 8.
        assert_eq!(db.get(b"k").unwrap(), Some(encode_i64(8)));
    }

    #[test]
    fn test_merge_range_tombstone_interaction() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), counter_opts()).unwrap();
        crate::through_stalls(|| db.put(b"k", &encode_i64(10))).unwrap();
        crate::through_stalls(|| db.merge(b"k", &encode_i64(5))).unwrap();
        crate::through_stalls(|| db.delete_range(b"j", b"l")).unwrap(); // hides the base
        crate::through_stalls(|| db.merge(b"k", &encode_i64(7))).unwrap();
        // After the RT, only the latest merge (7) applies to a None base.
        assert_eq!(db.get(b"k").unwrap(), Some(encode_i64(7)));
    }

    #[test]
    fn test_merge_write_batch() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), counter_opts()).unwrap();
        let mut batch = WriteBatch::new();
        batch.put(b"a", &encode_i64(1));
        batch.merge(b"a", &encode_i64(2));
        batch.merge(b"a", &encode_i64(3));
        batch.put(b"b", &encode_i64(100));
        crate::through_stalls(|| db.write(batch.clone())).unwrap();
        assert_eq!(db.get(b"a").unwrap(), Some(encode_i64(6)));
        assert_eq!(db.get(b"b").unwrap(), Some(encode_i64(100)));
    }

    #[test]
    fn test_merge_append_operator() {
        let dir = TempDir::new().unwrap();
        let opts = Options::default().merge_operator(Some(Arc::new(AppendMerge)));
        let db = Db::open(dir.path(), opts).unwrap();
        db.put(b"s", b"hello").unwrap();
        db.merge(b"s", b" ").unwrap();
        db.merge(b"s", b"world").unwrap();
        assert_eq!(db.get(b"s").unwrap(), Some(b"hello world".to_vec()));
    }

    #[test]
    fn test_merge_iterator_sees_collapsed_value() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), counter_opts()).unwrap();
        crate::through_stalls(|| db.put(b"a", &encode_i64(0))).unwrap();
        crate::through_stalls(|| db.merge(b"a", &encode_i64(5))).unwrap();
        crate::through_stalls(|| db.put(b"b", &encode_i64(100))).unwrap();
        crate::through_stalls(|| db.merge(b"b", &encode_i64(10))).unwrap();
        crate::through_stalls(|| db.merge(b"b", &encode_i64(2))).unwrap();

        let pairs = db.scan(None, None).unwrap();
        assert_eq!(
            pairs,
            vec![
                (b"a".to_vec(), encode_i64(5)),
                (b"b".to_vec(), encode_i64(112)),
            ]
        );
    }

    #[test]
    fn test_merge_iterator_reverse() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), counter_opts()).unwrap();
        crate::through_stalls(|| db.put(b"a", &encode_i64(0))).unwrap();
        crate::through_stalls(|| db.merge(b"a", &encode_i64(1))).unwrap();
        crate::through_stalls(|| db.put(b"b", &encode_i64(0))).unwrap();
        crate::through_stalls(|| db.merge(b"b", &encode_i64(2))).unwrap();
        crate::through_stalls(|| db.merge(b"b", &encode_i64(3))).unwrap();

        let mut iter = db.iter();
        iter.seek_to_last();
        let mut collected = Vec::new();
        while iter.valid() {
            collected.push((iter.key().unwrap().to_vec(), iter.value().unwrap().to_vec()));
            iter.prev();
        }
        assert_eq!(
            collected,
            vec![
                (b"b".to_vec(), encode_i64(5)),
                (b"a".to_vec(), encode_i64(1)),
            ]
        );
    }

    #[test]
    fn test_merge_crash_recovery() {
        let dir = TempDir::new().unwrap();
        {
            let db = Db::open(dir.path(), counter_opts()).unwrap();
            crate::through_stalls(|| db.put(b"counter", &encode_i64(0))).unwrap();
            crate::through_stalls(|| db.merge(b"counter", &encode_i64(7))).unwrap();
            crate::through_stalls(|| db.merge(b"counter", &encode_i64(3))).unwrap();
            // No close - memtable flush didn't happen; WAL must
            // survive the chain.
        }
        let db = Db::open(dir.path(), counter_opts()).unwrap();
        assert_eq!(db.get(b"counter").unwrap(), Some(encode_i64(10)));
    }

    #[test]
    fn test_merge_operator_name_plumbs_through() {
        // Surface-area smoke test: the configured operator's `name`
        // is reachable via Options::debug.
        let opts = counter_opts();
        let dbg = format!("{opts:?}");
        assert!(dbg.contains("CounterMerge"));
    }

    // ── column families ─────────────────────────────────────────────────

    #[test]
    fn test_cf_default_exists_on_open() {
        let (db, _dir) = open_tmp();
        let default = db.default_cf();
        assert_eq!(default.name(), DEFAULT_CF_NAME);
        assert!(db.column_family(DEFAULT_CF_NAME).is_some());
        assert_eq!(db.list_column_families(), vec![DEFAULT_CF_NAME.to_string()]);
    }

    #[test]
    fn test_cf_create_and_lookup() {
        let (db, _dir) = open_tmp();
        let users = db.create_column_family("users").unwrap();
        let orders = db.create_column_family("orders").unwrap();
        assert_ne!(users, orders);
        assert_eq!(db.column_family("users"), Some(users.clone()));
        assert_eq!(db.column_family("orders"), Some(orders.clone()));
        assert!(db.column_family("missing").is_none());

        let mut names = db.list_column_families();
        names.sort();
        assert_eq!(names, vec!["default", "orders", "users"]);
    }

    #[test]
    fn test_cf_create_is_idempotent() {
        let (db, _dir) = open_tmp();
        let a = db.create_column_family("x").unwrap();
        let b = db.create_column_family("x").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn test_cf_put_get_isolated_from_default() {
        let (db, _dir) = open_tmp();
        let users = db.create_column_family("users").unwrap();
        db.put(b"k", b"default_val").unwrap();
        db.put_cf(&users, b"k", b"users_val").unwrap();
        assert_eq!(db.get(b"k").unwrap(), Some(b"default_val".to_vec()));
        assert_eq!(
            db.get_cf(&users, b"k").unwrap(),
            Some(b"users_val".to_vec())
        );
    }

    #[test]
    fn test_cf_writes_to_a_invisible_from_b() {
        let (db, _dir) = open_tmp();
        let a = db.create_column_family("a").unwrap();
        let b = db.create_column_family("b").unwrap();
        db.put_cf(&a, b"shared_key", b"alpha").unwrap();
        assert_eq!(
            db.get_cf(&a, b"shared_key").unwrap(),
            Some(b"alpha".to_vec())
        );
        assert_eq!(db.get_cf(&b, b"shared_key").unwrap(), None);
    }

    #[test]
    fn test_cf_delete_cf() {
        let (db, _dir) = open_tmp();
        let cf = db.create_column_family("c").unwrap();
        db.put_cf(&cf, b"k", b"v").unwrap();
        db.delete_cf(&cf, b"k").unwrap();
        assert_eq!(db.get_cf(&cf, b"k").unwrap(), None);
    }

    #[test]
    fn test_cf_scan_strips_prefix() {
        let (db, _dir) = open_tmp();
        let cf = db.create_column_family("s").unwrap();
        db.put_cf(&cf, b"a", b"1").unwrap();
        db.put_cf(&cf, b"b", b"2").unwrap();
        db.put_cf(&cf, b"c", b"3").unwrap();
        let pairs = db.scan_cf(&cf, None, None).unwrap();
        assert_eq!(
            pairs,
            vec![
                (b"a".to_vec(), b"1".to_vec()),
                (b"b".to_vec(), b"2".to_vec()),
                (b"c".to_vec(), b"3".to_vec()),
            ]
        );
        // Bounded scan.
        let pairs = db.scan_cf(&cf, Some(b"b"), Some(b"c")).unwrap();
        assert_eq!(pairs, vec![(b"b".to_vec(), b"2".to_vec())]);
    }

    #[test]
    fn test_cf_iter_bounded_to_cf() {
        let (db, _dir) = open_tmp();
        let a = db.create_column_family("a").unwrap();
        let b = db.create_column_family("b").unwrap();
        db.put_cf(&a, b"a1", b"A1").unwrap();
        db.put_cf(&a, b"a2", b"A2").unwrap();
        db.put_cf(&b, b"b1", b"B1").unwrap();
        db.put(b"d1", b"D1").unwrap();

        let mut iter = db.iter_cf(&a);
        iter.seek_to_first();
        let mut keys = Vec::new();
        while iter.valid() {
            keys.push(iter.key().unwrap().to_vec());
            iter.next();
        }
        assert_eq!(keys, vec![b"a1".to_vec(), b"a2".to_vec()]);
    }

    #[test]
    fn test_cf_iter_reverse() {
        let (db, _dir) = open_tmp();
        let cf = db.create_column_family("rev").unwrap();
        db.put_cf(&cf, b"a", b"1").unwrap();
        db.put_cf(&cf, b"b", b"2").unwrap();
        db.put_cf(&cf, b"c", b"3").unwrap();

        let mut iter = db.iter_cf(&cf);
        iter.seek_to_last();
        let mut keys = Vec::new();
        while iter.valid() {
            keys.push(iter.key().unwrap().to_vec());
            iter.prev();
        }
        assert_eq!(keys, vec![b"c".to_vec(), b"b".to_vec(), b"a".to_vec()]);
    }

    #[test]
    fn test_cf_drop_removes_all_keys_in_cf() {
        let (db, _dir) = open_tmp();
        let cf = db.create_column_family("tmp").unwrap();
        db.put_cf(&cf, b"a", b"1").unwrap();
        db.put_cf(&cf, b"b", b"2").unwrap();
        db.put_cf(&cf, b"c", b"3").unwrap();
        db.put(b"default_key", b"default_val").unwrap();

        db.drop_column_family(cf.clone()).unwrap();

        // The CF name is unregistered.
        assert!(db.column_family("tmp").is_none());
        // Default CF survives.
        assert_eq!(
            db.get(b"default_key").unwrap(),
            Some(b"default_val".to_vec())
        );
        // Re-creating with the same name yields a fresh, empty CF.
        let cf2 = db.create_column_family("tmp").unwrap();
        assert_eq!(db.get_cf(&cf2, b"a").unwrap(), None);
    }

    #[test]
    fn test_cf_stale_handle_is_rejected_after_drop() {
        let (db, _dir) = open_tmp();
        let cf = db.create_column_family("tmp").unwrap();
        db.put_cf(&cf, b"k", b"v").unwrap();
        db.drop_column_family(cf.clone()).unwrap();

        let err = db.get_cf(&cf, b"k").unwrap_err();
        match err {
            Error::InvalidColumnFamily(message) => assert!(message.contains("tmp")),
            other => panic!("expected invalid column family error, got {other:?}"),
        }
        assert!(db.multi_get_cf(&cf, &[b"k"]).is_err());
        assert!(db.put_cf(&cf, b"k", b"ghost").is_err());
        assert!(db.delete_cf(&cf, b"k").is_err());
        assert!(db.delete_range_cf(&cf, b"a", b"z").is_err());
        assert!(db.merge_cf(&cf, b"k", b"operand").is_err());
        assert!(db.scan_cf(&cf, None, None).is_err());
        assert_eq!(
            db.get_approximate_sizes_cf(&cf, &[Range::new(b"a", b"z")]),
            vec![0]
        );
        assert_eq!(
            db.get_approximate_memtable_stats_cf(&cf, Range::new(b"a", b"z")),
            MemTableStats::default()
        );

        let mut iter = db.iter_cf(&cf);
        iter.seek_to_first();
        assert!(!iter.valid());

        let mut tail = db.iter_tailing_cf(&cf);
        tail.seek_to_first();
        assert!(!tail.valid());
    }

    #[test]
    fn test_cf_stale_handle_cannot_write_into_recreated_cf_name() {
        let (db, _dir) = open_tmp();
        let stale = db.create_column_family("tmp").unwrap();
        db.put_cf(&stale, b"k", b"old").unwrap();
        db.drop_column_family(stale.clone()).unwrap();

        let live = db.create_column_family("tmp").unwrap();
        assert_ne!(stale, live);
        assert!(db.put_cf(&stale, b"k", b"ghost").is_err());
        db.put_cf(&live, b"k", b"new").unwrap();

        assert!(db.get_cf(&stale, b"k").is_err());
        assert_eq!(db.get_cf(&live, b"k").unwrap(), Some(b"new".to_vec()));
    }

    #[test]
    fn test_cf_write_batch_rejects_stale_handle_ops() {
        let (db, _dir) = open_tmp();
        let stale = db.create_column_family("tmp").unwrap();
        db.drop_column_family(stale.clone()).unwrap();

        let mut batch = WriteBatch::new();
        batch.put_cf(&stale, b"k", b"ghost");
        let err = db.write(batch).unwrap_err();
        match err {
            Error::InvalidColumnFamily(message) => assert!(message.contains("column family id")),
            other => panic!("expected invalid column family error, got {other:?}"),
        }

        let live = db.create_column_family("tmp").unwrap();
        assert_eq!(db.get_cf(&live, b"k").unwrap(), None);
    }

    #[test]
    fn test_cf_write_batch_rejects_dropped_cf_in_first_op() {
        let (db, _dir) = open_tmp();
        let keep = db.create_column_family("keep").unwrap();
        let gone = db.create_column_family("gone").unwrap();
        db.drop_column_family(gone.clone()).unwrap();

        let mut batch = WriteBatch::new();
        batch.put_cf(&gone, b"g", b"ghost");
        batch.put(b"d", b"dv");
        batch.put_cf(&keep, b"k1", b"v1");
        batch.put_cf(&keep, b"k2", b"v2");
        let err = db.write(batch).unwrap_err();
        match err {
            Error::InvalidColumnFamily(message) => assert!(message.contains("column family id")),
            other => panic!("expected invalid column family error, got {other:?}"),
        }

        assert_eq!(db.get(b"d").unwrap(), None);
        assert_eq!(db.get_cf(&keep, b"k1").unwrap(), None);
    }

    #[test]
    fn test_cf_write_batch_rejects_dropped_cf_in_last_op() {
        let (db, _dir) = open_tmp();
        let keep = db.create_column_family("keep").unwrap();
        let gone = db.create_column_family("gone").unwrap();
        db.drop_column_family(gone.clone()).unwrap();

        let mut batch = WriteBatch::new();
        batch.put(b"d1", b"v1");
        batch.put(b"d2", b"v2");
        batch.put_cf(&keep, b"k", b"kv");
        batch.delete_cf(&gone, b"g");
        let err = db.write(batch).unwrap_err();
        match err {
            Error::InvalidColumnFamily(message) => assert!(message.contains("column family id")),
            other => panic!("expected invalid column family error, got {other:?}"),
        }

        assert_eq!(db.get(b"d1").unwrap(), None);
        assert_eq!(db.get_cf(&keep, b"k").unwrap(), None);
    }

    #[test]
    fn test_cf_write_batch_rejects_dropped_cf_between_live_runs() {
        let (db, _dir) = open_tmp();
        let keep = db.create_column_family("keep").unwrap();
        let gone = db.create_column_family("gone").unwrap();
        db.drop_column_family(gone.clone()).unwrap();

        let mut batch_a = WriteBatch::new();
        batch_a.put_cf(&keep, b"k1", b"v1");
        batch_a.put_cf(&keep, b"k2", b"v2");
        batch_a.put_cf(&gone, b"g", b"ghost");
        batch_a.put_cf(&keep, b"k3", b"v3");
        batch_a.put(b"d", b"dv");
        let err = db.write(batch_a).unwrap_err();
        match err {
            Error::InvalidColumnFamily(message) => assert!(message.contains("column family id")),
            other => panic!("expected invalid column family error, got {other:?}"),
        }
        assert_eq!(db.get_cf(&keep, b"k1").unwrap(), None);
        assert_eq!(db.get_cf(&keep, b"k2").unwrap(), None);
        assert_eq!(db.get_cf(&keep, b"k3").unwrap(), None);
        assert_eq!(db.get(b"d").unwrap(), None);

        let mut batch_b = WriteBatch::new();
        batch_b.put_cf(&keep, b"k1", b"v1");
        batch_b.insert_raw_range_delete(prefix_key(keep.id(), b"a"), prefix_key(gone.id(), b"z"));
        let err = db.write(batch_b).unwrap_err();
        match err {
            Error::InvalidColumnFamily(message) => assert!(message.contains("column family id")),
            other => panic!("expected invalid column family error, got {other:?}"),
        }
        assert_eq!(db.get_cf(&keep, b"k1").unwrap(), None);

        let mut batch_c = WriteBatch::new();
        batch_c.put_cf(&keep, b"k1", b"v1");
        batch_c.insert_raw_range_delete(prefix_key(gone.id(), b"a"), prefix_key(keep.id(), b"z"));
        let err = db.write(batch_c).unwrap_err();
        match err {
            Error::InvalidColumnFamily(message) => assert!(message.contains("column family id")),
            other => panic!("expected invalid column family error, got {other:?}"),
        }
        assert_eq!(db.get_cf(&keep, b"k1").unwrap(), None);
    }

    #[test]
    fn test_cf_write_batch_with_alternating_live_cfs_lands() {
        let (db, _dir) = open_tmp();
        let a = db.create_column_family("a").unwrap();
        let b = db.create_column_family("b").unwrap();

        let mut batch = WriteBatch::new();
        batch.put_cf(&a, b"1", b"a1");
        batch.put_cf(&b, b"1", b"b1");
        batch.put_cf(&a, b"2", b"a2");
        batch.put(b"d", b"dv");
        batch.put_cf(&b, b"2", b"b2");
        batch.delete_range_cf(&a, b"x", b"y");
        db.write(batch).unwrap();

        assert_eq!(db.get_cf(&a, b"1").unwrap(), Some(b"a1".to_vec()));
        assert_eq!(db.get_cf(&b, b"1").unwrap(), Some(b"b1".to_vec()));
        assert_eq!(db.get_cf(&a, b"2").unwrap(), Some(b"a2".to_vec()));
        assert_eq!(db.get(b"d").unwrap(), Some(b"dv".to_vec()));
        assert_eq!(db.get_cf(&b, b"2").unwrap(), Some(b"b2".to_vec()));
    }

    #[test]
    fn test_cf_write_batch_rejects_reserved_meta_cf_id() {
        let (db, _dir) = open_tmp();
        let before = db.engine.get_latest(&meta::next_id_key()).unwrap();

        let mut batch = WriteBatch::new();
        batch.insert_raw_put(prefix_key(META_CF_ID, b"next_id"), vec![9, 9, 9, 9]);
        let err = db.write(batch).unwrap_err();
        match err {
            Error::InvalidColumnFamily(message) => assert!(message.contains("column family id")),
            other => panic!("expected invalid column family error, got {other:?}"),
        }

        assert_eq!(db.engine.get_latest(&meta::next_id_key()).unwrap(), before);
        assert_eq!(db.list_column_families(), vec![DEFAULT_CF_NAME.to_string()]);
        let after = db.create_column_family("after").unwrap();
        assert!(db.list_column_families().contains(&"after".to_string()));
        assert_eq!(db.get_cf(&after, b"k").unwrap(), None);
    }

    #[test]
    fn test_cf_ingest_rejects_stale_handle_entries() {
        let (db, dir) = open_tmp();
        let stale = db.create_column_family("tmp").unwrap();
        db.drop_column_family(stale.clone()).unwrap();

        let path = dir.path().join("stale-cf.sst");
        let mut writer = SstFileWriter::create(&path, &Options::default()).unwrap();
        writer.put_cf(&stale, b"k", b"ghost").unwrap();
        writer.finish().unwrap();

        assert!(
            db.ingest_external_files(&[path], IngestOptions::default())
                .wait()
                .is_err()
        );
        let live = db.create_column_family("tmp").unwrap();
        assert_eq!(db.get_cf(&live, b"k").unwrap(), None);
    }

    #[test]
    fn test_cf_snapshot_rejects_stale_handle_after_drop() {
        let (db, _dir) = open_tmp();
        let cf = db.create_column_family("tmp").unwrap();
        db.put_cf(&cf, b"k", b"v").unwrap();
        let snap = db.snapshot();
        db.drop_column_family(cf.clone()).unwrap();

        assert!(snap.get_cf(&cf, b"k").is_err());
        assert!(snap.multi_get_cf(&cf, &[b"k"]).is_err());
        assert!(snap.scan_cf(&cf, None, None).is_err());
        let mut iter = snap.iter_cf(&cf);
        iter.seek_to_first();
        assert!(!iter.valid());
    }

    #[test]
    fn test_cf_cannot_drop_default() {
        let (db, _dir) = open_tmp();
        let default = db.default_cf();
        assert!(db.drop_column_family(default).is_err());
    }

    #[test]
    fn test_cf_survives_reopen() {
        let dir = TempDir::new().unwrap();
        {
            let db = Db::open(dir.path(), Options::default()).unwrap();
            let cf = db.create_column_family("persistent").unwrap();
            db.put_cf(&cf, b"k", b"v").unwrap();
            db.close().unwrap();
        }
        let db = Db::open(dir.path(), Options::default()).unwrap();
        let cf = db
            .column_family("persistent")
            .expect("CF must survive reopen");
        assert_eq!(db.get_cf(&cf, b"k").unwrap(), Some(b"v".to_vec()));
    }

    #[test]
    fn test_cf_dropped_cf_does_not_survive_reopen() {
        let dir = TempDir::new().unwrap();
        {
            let db = Db::open(dir.path(), Options::default()).unwrap();
            let cf = db.create_column_family("doomed").unwrap();
            db.put_cf(&cf, b"k", b"v").unwrap();
            db.drop_column_family(cf).unwrap();
            db.close().unwrap();
        }
        let db = Db::open(dir.path(), Options::default()).unwrap();
        assert!(db.column_family("doomed").is_none());
    }

    #[test]
    fn test_cf_write_batch_cross_cf_atomic() {
        let (db, _dir) = open_tmp();
        let a = db.create_column_family("a").unwrap();
        let b = db.create_column_family("b").unwrap();
        let mut batch = WriteBatch::new();
        batch.put_cf(&a, b"k1", b"v_a1");
        batch.put_cf(&b, b"k1", b"v_b1");
        batch.put(b"k1", b"v_default");
        batch.delete_cf(&a, b"ghost");
        db.write(batch).unwrap();

        assert_eq!(db.get_cf(&a, b"k1").unwrap(), Some(b"v_a1".to_vec()));
        assert_eq!(db.get_cf(&b, b"k1").unwrap(), Some(b"v_b1".to_vec()));
        assert_eq!(db.get(b"k1").unwrap(), Some(b"v_default".to_vec()));
    }

    #[test]
    fn test_cf_write_batch_survives_crash_recovery() {
        let dir = TempDir::new().unwrap();
        {
            let db = Db::open(dir.path(), Options::default()).unwrap();
            let cf = db.create_column_family("txn").unwrap();
            let mut batch = WriteBatch::new();
            batch.put_cf(&cf, b"a", b"1");
            batch.put_cf(&cf, b"b", b"2");
            batch.put(b"default_k", b"default_v");
            db.write(batch).unwrap();
            // No close - simulate a crash. WAL must survive.
        }
        let db = Db::open(dir.path(), Options::default()).unwrap();
        let cf = db.column_family("txn").expect("CF must survive");
        assert_eq!(db.get_cf(&cf, b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(db.get_cf(&cf, b"b").unwrap(), Some(b"2".to_vec()));
        assert_eq!(db.get(b"default_k").unwrap(), Some(b"default_v".to_vec()));
    }

    #[test]
    fn test_cf_snapshot_isolation_per_cf() {
        let (db, _dir) = open_tmp();
        let a = db.create_column_family("a").unwrap();
        db.put_cf(&a, b"k", b"v0").unwrap();
        let snap = db.snapshot();
        db.put_cf(&a, b"k", b"v1").unwrap();
        assert_eq!(snap.get_cf(&a, b"k").unwrap(), Some(b"v0".to_vec()));
        assert_eq!(db.get_cf(&a, b"k").unwrap(), Some(b"v1".to_vec()));
    }

    #[test]
    fn test_cf_scan_across_cfs_is_isolated() {
        let (db, _dir) = open_tmp();
        let a = db.create_column_family("a").unwrap();
        let b = db.create_column_family("b").unwrap();
        db.put_cf(&a, b"apple", b"A").unwrap();
        db.put_cf(&b, b"apple", b"B").unwrap();
        db.put(b"apple", b"D").unwrap();

        assert_eq!(
            db.scan_cf(&a, None, None).unwrap(),
            vec![(b"apple".to_vec(), b"A".to_vec())]
        );
        assert_eq!(
            db.scan_cf(&b, None, None).unwrap(),
            vec![(b"apple".to_vec(), b"B".to_vec())]
        );
        assert_eq!(
            db.scan(None, None).unwrap(),
            vec![(b"apple".to_vec(), b"D".to_vec())]
        );
    }

    #[test]
    fn test_cf_scan_page_is_scoped_and_resumable() {
        let (db, _dir) = open_tmp();
        let cf = db.create_column_family("paged").unwrap();

        db.put(b"a", b"default").unwrap();
        db.put_cf(&cf, b"a", b"1").unwrap();
        db.put_cf(&cf, b"b", b"2").unwrap();
        db.put_cf(&cf, b"c", b"3").unwrap();

        let first = db.scan_page_cf(&cf, None, None, 2).unwrap();
        assert_eq!(
            first,
            ScanPage {
                entries: vec![
                    (b"a".to_vec(), b"1".to_vec()),
                    (b"b".to_vec(), b"2".to_vec()),
                ],
                next_start: Some(b"c".to_vec()),
            }
        );

        let second = db
            .scan_page_cf(&cf, first.next_start.as_deref(), None, 2)
            .unwrap();
        assert_eq!(
            second,
            ScanPage {
                entries: vec![(b"c".to_vec(), b"3".to_vec())],
                next_start: None,
            }
        );
    }

    #[test]
    fn test_cf_multi_get_cf() {
        let (db, _dir) = open_tmp();
        let cf = db.create_column_family("mg").unwrap();
        db.put_cf(&cf, b"a", b"1").unwrap();
        db.put_cf(&cf, b"b", b"2").unwrap();
        let keys: Vec<&[u8]> = vec![b"a", b"missing", b"b"];
        let got = db.multi_get_cf(&cf, &keys).unwrap();
        assert_eq!(got, vec![Some(b"1".to_vec()), None, Some(b"2".to_vec())]);
    }

    #[test]
    fn test_cf_delete_range_cf() {
        let (db, _dir) = open_tmp();
        let cf = db.create_column_family("r").unwrap();
        for c in b'a'..=b'f' {
            db.put_cf(&cf, &[c], &[c]).unwrap();
        }
        db.delete_range_cf(&cf, b"b", b"e").unwrap();
        assert_eq!(db.get_cf(&cf, b"a").unwrap(), Some(b"a".to_vec()));
        assert_eq!(db.get_cf(&cf, b"b").unwrap(), None);
        assert_eq!(db.get_cf(&cf, b"c").unwrap(), None);
        assert_eq!(db.get_cf(&cf, b"d").unwrap(), None);
        assert_eq!(db.get_cf(&cf, b"e").unwrap(), Some(b"e".to_vec()));
        assert_eq!(db.get_cf(&cf, b"f").unwrap(), Some(b"f".to_vec()));
    }

    #[test]
    fn test_cf_create_empty_name_errors() {
        let (db, _dir) = open_tmp();
        assert!(db.create_column_family("").is_err());
    }

    #[test]
    fn test_cf_many_cfs_all_isolated() {
        let (db, _dir) = open_tmp();
        let mut handles = Vec::new();
        for i in 0..10 {
            handles.push(db.create_column_family(&format!("cf{i}")).unwrap());
        }
        for (i, h) in handles.iter().enumerate() {
            db.put_cf(h, b"k", format!("v{i}").as_bytes()).unwrap();
        }
        for (i, h) in handles.iter().enumerate() {
            assert_eq!(
                db.get_cf(h, b"k").unwrap(),
                Some(format!("v{i}").into_bytes())
            );
        }
    }

    // ── get_approximate_sizes / get_approximate_memtable_stats ──────────

    #[test]
    fn test_approximate_sizes_empty_db() {
        let (db, _dir) = open_tmp();
        let sizes = db.get_approximate_sizes(&[Range::new(b"a", b"z")]);
        assert_eq!(sizes, vec![0]);
    }

    #[test]
    fn test_approximate_sizes_empty_range_returns_zero() {
        let (db, _dir) = open_tmp();
        db.put(b"k", b"v").unwrap();
        // Inverted / empty range must not panic and must be 0.
        assert_eq!(db.get_approximate_sizes(&[Range::new(b"z", b"a")]), vec![0]);
        assert_eq!(db.get_approximate_sizes(&[Range::new(b"k", b"k")]), vec![0]);
    }

    #[test]
    fn test_approximate_memtable_stats_exact_for_memtable() {
        let (db, _dir) = open_tmp();
        for c in b'a'..=b'e' {
            db.put(&[c], b"v").unwrap();
        }
        let stats = db.get_approximate_memtable_stats(Range::new(b"b", b"e"));
        assert_eq!(stats.count, 3, "count must be exact");
        // Each entry is [4-byte cf prefix][1-byte key] as
        // internal-key + 9-byte seq/type suffix + 1-byte value.
        // The size must be strictly > 0 and < (5 full entries * 50).
        assert!(stats.size > 0);
        assert!(stats.size < 500);
    }

    #[test]
    fn test_approximate_memtable_stats_empty_range() {
        let (db, _dir) = open_tmp();
        db.put(b"k", b"v").unwrap();
        let stats = db.get_approximate_memtable_stats(Range::new(b"m", b"n"));
        assert_eq!(stats, MemTableStats::default());
    }

    #[test]
    fn test_approximate_memtable_stats_counts_every_version() {
        let (db, _dir) = open_tmp();
        db.put(b"k", b"v1").unwrap();
        db.put(b"k", b"v2").unwrap();
        db.put(b"k", b"v3").unwrap();
        let stats = db.get_approximate_memtable_stats(Range::new(b"k", b"l"));
        // Three versions of the same user key.
        assert_eq!(stats.count, 3);
    }

    #[test]
    fn test_approximate_sizes_after_flush_within_factor_of_2() {
        // Write enough data to materialize into L0, then check the
        // approximate size against the on-disk file size. The
        // accuracy contract is "within a factor of 2".
        //
        // Use a high-entropy payload so LZ4 can't crush it - a
        // zero-filled payload compresses to near-nothing and would
        // undercut the accuracy window we're checking.
        let dir = TempDir::new().unwrap();
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .compression(CompressionType::None);
        let db = Db::open(dir.path(), opts).unwrap();
        let payload: Vec<u8> = (0..256).map(|i| (i % 251) as u8).collect();
        for i in 0..100 {
            crate::through_stalls(|| db.put(format!("k_{i:04}").as_bytes(), &payload)).unwrap();
        }
        force_flush(&db, "sizes");
        let sizes = db.get_approximate_sizes(&[Range::new(b"k_0000", b"k_9999")]);
        assert!(sizes[0] > 0, "whole-range size must be > 0 after flush");
        // Raw on-disk footprint of the point data: 100 entries,
        // each ≈ 256-byte value + ~20-byte key/overhead + a bit
        // of block framing, so ~28-30k. The approximation
        // includes whole covered blocks, so a 2x window covers
        // it comfortably.
        let approx = sizes[0];
        assert!(approx > 10_000, "approx={approx} too small; expected > 10k");
        assert!(
            approx < 1_000_000,
            "approx={approx} absurdly large; expected < 1M"
        );
    }

    #[test]
    fn test_approximate_sizes_multi_range_preserves_order() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();
        let payload = vec![0u8; 256];
        for i in 0..200 {
            crate::through_stalls(|| db.put(format!("k_{i:04}").as_bytes(), &payload)).unwrap();
        }
        force_flush(&db, "multi");
        let ranges = vec![
            Range::new(b"k_0000", b"k_0050"),
            Range::new(b"k_0050", b"k_0100"),
            Range::new(b"k_0100", b"k_0200"),
        ];
        let sizes = db.get_approximate_sizes(&ranges);
        assert_eq!(sizes.len(), 3);
        // Every range should contain some bytes.
        for (i, &s) in sizes.iter().enumerate() {
            assert!(s > 0, "range {i} size was 0");
        }
    }

    #[test]
    fn test_approximate_sizes_cf_scoped() {
        let (db, _dir) = open_tmp();
        let cf = db.create_column_family("scoped").unwrap();
        // Put into default CF but not into `scoped` - the
        // scoped CF's whole-range size must be 0.
        for i in 0..20 {
            db.put(format!("k{i}").as_bytes(), b"v").unwrap();
        }
        let default_sizes = db.get_approximate_sizes(&[Range::new(b"a", b"z")]);
        let cf_sizes = db.get_approximate_sizes_cf(&cf, &[Range::new(b"a", b"z")]);
        // Memtable contents aren't in approximate_sizes, but they
        // aren't on disk either - the default-CF whole-range
        // matches the scoped-CF whole-range (both 0) unless a
        // flush happened. With default write_buffer_size, 20 small
        // writes don't trigger a flush.
        assert_eq!(default_sizes[0], 0);
        assert_eq!(cf_sizes[0], 0);

        // Memtable-stats however sees the default CF entries but
        // not the scoped CF.
        let default_mt = db.get_approximate_memtable_stats(Range::new(b"a", b"z"));
        let cf_mt = db.get_approximate_memtable_stats_cf(&cf, Range::new(b"a", b"z"));
        assert_eq!(default_mt.count, 20);
        assert_eq!(cf_mt.count, 0);
    }

    // ── atomic flush across column families ────────────────────────────

    #[test]
    fn test_atomic_flush_multi_cf_batch_survives_crash() {
        // A WriteBatch that touches multiple CFs must be
        // all-or-nothing across a crash, even when the write
        // lands in the memtable without an explicit flush.
        let dir = TempDir::new().unwrap();
        {
            let db = Db::open(dir.path(), Options::default()).unwrap();
            let cf_a = db.create_column_family("a").unwrap();
            let cf_b = db.create_column_family("b").unwrap();
            let mut batch = WriteBatch::new();
            batch.put_cf(&cf_a, b"k1", b"a1");
            batch.put_cf(&cf_a, b"k2", b"a2");
            batch.put_cf(&cf_b, b"k1", b"b1");
            batch.put_cf(&cf_b, b"k2", b"b2");
            batch.put(b"default_k", b"default_v");
            db.write(batch).unwrap();
            // Drop without close - simulate a crash. WAL is the
            // source of truth; recovery must restore every key.
        }
        let db = Db::open(dir.path(), Options::default()).unwrap();
        let cf_a = db.column_family("a").expect("cf a survives reopen");
        let cf_b = db.column_family("b").expect("cf b survives reopen");
        assert_eq!(db.get_cf(&cf_a, b"k1").unwrap(), Some(b"a1".to_vec()));
        assert_eq!(db.get_cf(&cf_a, b"k2").unwrap(), Some(b"a2".to_vec()));
        assert_eq!(db.get_cf(&cf_b, b"k1").unwrap(), Some(b"b1".to_vec()));
        assert_eq!(db.get_cf(&cf_b, b"k2").unwrap(), Some(b"b2".to_vec()));
        assert_eq!(db.get(b"default_k").unwrap(), Some(b"default_v".to_vec()));
    }

    #[test]
    fn test_atomic_flush_cross_cf_survives_rotate_and_flush() {
        // Drive the memtable past its flush threshold while a
        // multi-CF batch is in flight. The rotated memtable
        // produces one L0 SSTable that contains every CF's half
        // of the batch atomically.
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();
        let cf_a = db.create_column_family("a").unwrap();
        let cf_b = db.create_column_family("b").unwrap();

        // Seed enough filler to push the tiny 4KB buffer over
        // on the next write.
        for i in 0..16 {
            crate::through_stalls(|| {
                db.put_cf(&cf_a, format!("fill_a_{i:02}").as_bytes(), &[0u8; 256])
            })
            .unwrap();
            crate::through_stalls(|| {
                db.put_cf(&cf_b, format!("fill_b_{i:02}").as_bytes(), &[0u8; 256])
            })
            .unwrap();
        }

        let mut batch = WriteBatch::new();
        batch.put_cf(&cf_a, b"pivot", b"A_PIVOT");
        batch.put_cf(&cf_b, b"pivot", b"B_PIVOT");
        crate::through_stalls(|| db.write(batch.clone())).unwrap();
        force_flush(&db, "atomic");

        assert_eq!(
            db.get_cf(&cf_a, b"pivot").unwrap(),
            Some(b"A_PIVOT".to_vec())
        );
        assert_eq!(
            db.get_cf(&cf_b, b"pivot").unwrap(),
            Some(b"B_PIVOT".to_vec())
        );
    }

    #[test]
    fn test_atomic_flush_empty_cf_mixed_with_populated() {
        // Creating a CF and leaving it empty while another CF
        // gets flushed must not corrupt the empty CF.
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();
        let cf_populated = db.create_column_family("populated").unwrap();
        let cf_empty = db.create_column_family("empty").unwrap();

        for i in 0..50 {
            crate::through_stalls(|| db.put_cf(&cf_populated, format!("k{i:02}").as_bytes(), b"v"))
                .unwrap();
        }
        force_flush(&db, "empty_mix");

        for i in 0..50 {
            assert_eq!(
                db.get_cf(&cf_populated, format!("k{i:02}").as_bytes())
                    .unwrap(),
                Some(b"v".to_vec())
            );
        }
        assert_eq!(db.get_cf(&cf_empty, b"anything").unwrap(), None);

        // The empty CF still accepts new writes after the flush.
        crate::through_stalls(|| db.put_cf(&cf_empty, b"new", b"fresh")).unwrap();
        assert_eq!(
            db.get_cf(&cf_empty, b"new").unwrap(),
            Some(b"fresh".to_vec())
        );
    }

    #[test]
    fn test_atomic_flush_option_accepted() {
        // The flag is a no-op for API parity - both values must
        // open cleanly and produce the same atomic behavior.
        let dir = TempDir::new().unwrap();
        let opts = Options::default().atomic_flush(true);
        let db = Db::open(dir.path(), opts).unwrap();
        let cf = db.create_column_family("cf1").unwrap();
        db.put_cf(&cf, b"k", b"v").unwrap();
        assert_eq!(db.get_cf(&cf, b"k").unwrap(), Some(b"v".to_vec()));
    }

    #[test]
    fn test_atomic_flush_close_with_pending_multi_cf_writes() {
        // A clean close with pending multi-CF writes must flush
        // the active memtable to L0 before returning, so every
        // CF's state is durable on reopen.
        let dir = TempDir::new().unwrap();
        {
            let db = Db::open(dir.path(), Options::default()).unwrap();
            let cf_a = db.create_column_family("a").unwrap();
            let cf_b = db.create_column_family("b").unwrap();
            let mut batch = WriteBatch::new();
            batch.put_cf(&cf_a, b"k", b"a");
            batch.put_cf(&cf_b, b"k", b"b");
            db.write(batch).unwrap();
            db.close().unwrap();
        }
        let db = Db::open(dir.path(), Options::default()).unwrap();
        let cf_a = db.column_family("a").unwrap();
        let cf_b = db.column_family("b").unwrap();
        assert_eq!(db.get_cf(&cf_a, b"k").unwrap(), Some(b"a".to_vec()));
        assert_eq!(db.get_cf(&cf_b, b"k").unwrap(), Some(b"b".to_vec()));
    }

    // ── event listeners ─────────────────────────────────────────────────

    /// Test listener that counts every callback it receives and
    /// records enough detail for assertions.
    #[derive(Default)]
    struct CountingListener {
        flush_completed: AtomicUsize,
        compaction_begin: AtomicUsize,
        compaction_completed: AtomicUsize,
        table_file_created: AtomicUsize,
        table_file_deleted: AtomicUsize,
        external_file_ingested: AtomicUsize,
        background_error: AtomicUsize,
        last_flush_file_id: AtomicUsize,
        last_compaction_output_count: AtomicUsize,
    }

    impl EventListener for CountingListener {
        fn on_flush_completed(&self, info: &FlushJobInfo) {
            self.flush_completed.fetch_add(1, AtomicOrdering::Relaxed);
            self.last_flush_file_id
                .store(info.file_id as usize, AtomicOrdering::Relaxed);
        }
        fn on_compaction_begin(&self, _info: &CompactionJobInfo) {
            self.compaction_begin.fetch_add(1, AtomicOrdering::Relaxed);
        }
        fn on_compaction_completed(&self, info: &CompactionJobInfo) {
            self.compaction_completed
                .fetch_add(1, AtomicOrdering::Relaxed);
            self.last_compaction_output_count
                .store(info.output_files.len(), AtomicOrdering::Relaxed);
        }
        fn on_table_file_created(&self, _info: &TableFileCreationInfo) {
            self.table_file_created
                .fetch_add(1, AtomicOrdering::Relaxed);
        }
        fn on_table_file_deleted(&self, _info: &TableFileDeletionInfo) {
            self.table_file_deleted
                .fetch_add(1, AtomicOrdering::Relaxed);
        }
        fn on_external_file_ingested(&self, _info: &ExternalFileIngestionInfo) {
            self.external_file_ingested
                .fetch_add(1, AtomicOrdering::Relaxed);
        }
        fn on_background_error(&self, _reason: BackgroundErrorReason, _err: &Error) {
            self.background_error.fetch_add(1, AtomicOrdering::Relaxed);
        }
    }

    #[test]
    fn test_listener_fires_on_flush() {
        let listener = Arc::new(CountingListener::default());
        let dir = TempDir::new().unwrap();
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .listeners(vec![listener.clone() as Arc<dyn EventListener>]);
        let db = Db::open(dir.path(), opts).unwrap();
        force_flush(&db, "listener");

        assert!(
            listener.flush_completed.load(AtomicOrdering::Relaxed) >= 1,
            "flush callback should fire at least once"
        );
        assert!(
            listener.table_file_created.load(AtomicOrdering::Relaxed) >= 1,
            "table_file_created should fire for every flushed file"
        );
        assert_ne!(
            listener.last_flush_file_id.load(AtomicOrdering::Relaxed),
            0,
            "file id recorded"
        );
    }

    #[test]
    fn test_listener_fires_on_compaction() {
        let listener = Arc::new(CountingListener::default());
        let dir = TempDir::new().unwrap();
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .listeners(vec![listener.clone() as Arc<dyn EventListener>]);
        let db = Db::open(dir.path(), opts).unwrap();
        // Drive enough writes to generate L0 files, then manually
        // compact the range so the compaction callbacks fire on
        // the calling thread.
        for i in 0..400 {
            crate::through_stalls(|| db.put(format!("k_{i:04}").as_bytes(), b"v")).unwrap();
        }
        force_flush(&db, "listener");
        db.compact_range(None, None).wait().unwrap();

        let begin = listener.compaction_begin.load(AtomicOrdering::Relaxed);
        let complete = listener.compaction_completed.load(AtomicOrdering::Relaxed);
        assert!(
            begin >= 1,
            "compaction_begin must fire at least once, got {begin}"
        );
        assert_eq!(
            begin, complete,
            "begin and completed must fire in matched pairs"
        );
        assert!(
            listener.table_file_created.load(AtomicOrdering::Relaxed) >= 2,
            "flush + compaction both produce files"
        );
        assert!(
            listener.table_file_deleted.load(AtomicOrdering::Relaxed) >= 1,
            "old L0 files must be unlinked after compaction"
        );
    }

    #[test]
    fn test_listener_fires_on_ingest() {
        let listener = Arc::new(CountingListener::default());
        let dir = TempDir::new().unwrap();
        let opts = Options::default().listeners(vec![listener.clone() as Arc<dyn EventListener>]);
        let db = Db::open(dir.path(), opts.clone()).unwrap();

        let sst_path = dir.path().join("ingest.sst");
        {
            let mut w = SstFileWriter::create(&sst_path, &opts).unwrap();
            for i in 0..10 {
                w.put(format!("ik_{i:02}").as_bytes(), b"iv").unwrap();
            }
            w.finish().unwrap();
        }
        db.ingest_external_files(&[sst_path], IngestOptions::default())
            .wait()
            .unwrap();

        assert_eq!(
            listener
                .external_file_ingested
                .load(AtomicOrdering::Relaxed),
            1,
            "external_file_ingested fires once per ingested file"
        );
        assert!(
            listener.table_file_created.load(AtomicOrdering::Relaxed) >= 1,
            "ingest re-emits the file and fires table_file_created"
        );
    }

    #[test]
    fn test_listener_multiple_listeners_all_fire() {
        let a = Arc::new(CountingListener::default());
        let b = Arc::new(CountingListener::default());
        let dir = TempDir::new().unwrap();
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .listeners(vec![
                a.clone() as Arc<dyn EventListener>,
                b.clone() as Arc<dyn EventListener>,
            ]);
        let db = Db::open(dir.path(), opts).unwrap();
        force_flush(&db, "multi");

        assert!(a.flush_completed.load(AtomicOrdering::Relaxed) >= 1);
        assert!(b.flush_completed.load(AtomicOrdering::Relaxed) >= 1);
    }

    #[test]
    fn test_listener_none_configured_is_noop() {
        // Sanity check: with no listeners, all paths still work
        // and nothing panics.
        let (db, _dir) = open_tmp();
        db.put(b"k", b"v").unwrap();
        force_flush(&db, "none");
        db.compact_range(None, None).wait().unwrap();
    }

    #[test]
    fn test_listener_compaction_job_info_contains_input_files() {
        // Capture the last CompactionJobInfo on `on_compaction_completed`
        // and assert it carries the expected input file ids.
        struct CaptureListener {
            captured: Mutex<Option<CompactionJobInfo>>,
        }
        impl EventListener for CaptureListener {
            fn on_compaction_completed(&self, info: &CompactionJobInfo) {
                *self.captured.lock() = Some(info.clone());
            }
        }
        use crate::sync::internal::Mutex;

        let listener = Arc::new(CaptureListener {
            captured: Mutex::new(None),
        });
        let dir = TempDir::new().unwrap();
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .listeners(vec![listener.clone() as Arc<dyn EventListener>]);
        let db = Db::open(dir.path(), opts).unwrap();
        for i in 0..400 {
            crate::through_stalls(|| db.put(format!("k_{i:04}").as_bytes(), b"v")).unwrap();
        }
        force_flush(&db, "capture");
        db.compact_range(None, None).wait().unwrap();

        let captured = listener.captured.lock().clone();
        let info = captured.expect("compaction_completed must have fired");
        assert!(
            !info.input_files_input_level.is_empty(),
            "at least one L0 input file was picked"
        );
        assert_eq!(info.output_level, info.input_level + 1);
        assert!(!info.output_files.is_empty(), "compaction produced outputs");
    }

    // ── statistics ──────────────────────────────────────────────────────

    fn stats_opts(stats: Arc<Statistics>) -> Options {
        Options::default().statistics(Some(stats))
    }

    fn tiny_flush_stats_opts(stats: Arc<Statistics>) -> Options {
        Options::default()
            .write_buffer_size(4 * 1024)
            .statistics(Some(stats))
    }

    #[test]
    fn test_stats_keys_written_and_bytes_written() {
        let stats = Arc::new(Statistics::new());
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), stats_opts(stats.clone())).unwrap();
        db.put(b"k1", b"value1").unwrap();
        db.put(b"k2", b"value2").unwrap();
        assert_eq!(stats.get_ticker(Ticker::KeysWritten), 2);
        // Expected bytes = 2 + 6 + 2 + 6 = 16
        assert_eq!(stats.get_ticker(Ticker::BytesWritten), 16);
    }

    #[test]
    fn test_stats_keys_read_and_bytes_read() {
        let stats = Arc::new(Statistics::new());
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), stats_opts(stats.clone())).unwrap();
        db.put(b"k", b"value").unwrap();
        db.get(b"k").unwrap();
        db.get(b"missing").unwrap();
        assert_eq!(stats.get_ticker(Ticker::KeysRead), 2);
        // Only the found value contributes to BytesRead.
        assert_eq!(stats.get_ticker(Ticker::BytesRead), 5);
        let get_hist = stats.get_histogram_snapshot(Histogram::DbGet);
        assert_eq!(get_hist.count, 2);
    }

    #[test]
    fn test_stats_delete_counter() {
        let stats = Arc::new(Statistics::new());
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), stats_opts(stats.clone())).unwrap();
        db.put(b"k", b"v").unwrap();
        db.delete(b"k").unwrap();
        assert_eq!(stats.get_ticker(Ticker::KeysDeleted), 1);
    }

    #[test]
    fn test_stats_block_cache_hit_and_miss_populate() {
        // After a deterministic flush + compact_range (so no
        // concurrent background compaction can race the reads
        // and contaminate the counters), every point lookup
        // that reaches a data block fires either a hit or a
        // miss on the block cache. We don't assert the strict
        // `adds == misses` invariant here - LRU eviction plus
        // any lingering background work can perturb that
        // equality on fast machines. The weaker "both hits and
        // misses see traffic" is the observable contract.
        let stats = Arc::new(Statistics::new());
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_stats_opts(stats.clone())).unwrap();
        for i in 0..200 {
            crate::through_stalls(|| db.put(format!("k_{i:05}").as_bytes(), b"value")).unwrap();
        }
        force_flush(&db, "cache");
        // Drain any pending compaction before measuring.
        db.compact_range(None, None).wait().unwrap();
        stats.reset();
        // Read the same few keys twice: the first read is a
        // miss + add, the second is a hit.
        for _ in 0..2 {
            for i in 0..5 {
                db.get(format!("k_{i:05}").as_bytes()).unwrap();
            }
        }
        let hits = stats.get_ticker(Ticker::BlockCacheHit);
        let misses = stats.get_ticker(Ticker::BlockCacheMiss);
        let adds = stats.get_ticker(Ticker::BlockCacheAdd);
        assert!(misses > 0, "expected at least one block cache miss");
        assert!(hits > 0, "expected at least one block cache hit");
        // `adds` tracks inserts after a miss - it can never
        // exceed `misses`.
        assert!(adds <= misses, "adds={adds} misses={misses}");
    }

    #[test]
    fn test_stats_bloom_filter_useful_increments_on_absent_key() {
        // Deterministic layout: write 200 keys spaced on even
        // suffixes (so the resulting SST covers `[k_00000,
        // k_00398]`), compact to L1, then query odd suffixes
        // within that range. The partition_point-based file
        // lookup lands on the single L1 file for every query
        // and the bloom has a chance to say "not present".
        let stats = Arc::new(Statistics::new());
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_stats_opts(stats.clone())).unwrap();
        for i in 0..200 {
            let even = i * 2;
            crate::through_stalls(|| db.put(format!("k_{even:05}").as_bytes(), b"v")).unwrap();
        }
        force_flush(&db, "bloom");
        db.compact_range(None, None).wait().unwrap();
        stats.reset();
        // Query 100 absent (odd-suffix) keys inside the range.
        // With ~10 bits/key the false-positive rate is ~1%, so
        // almost all queries will register as "useful".
        for i in 0..100 {
            let odd = i * 2 + 1;
            db.get(format!("k_{odd:05}").as_bytes()).unwrap();
        }
        let useful = stats.get_ticker(Ticker::BloomFilterUseful);
        assert!(
            useful > 0,
            "bloom filter should have ruled out at least one absent key"
        );
    }

    #[test]
    fn test_stats_bloom_filter_full_positive_on_present_key() {
        let stats = Arc::new(Statistics::new());
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_stats_opts(stats.clone())).unwrap();
        for i in 0..100 {
            crate::through_stalls(|| db.put(format!("k_{i:04}").as_bytes(), b"v")).unwrap();
        }
        force_flush(&db, "bloom_pos");
        db.compact_range(None, None).wait().unwrap();
        stats.reset();
        for i in 0..100 {
            db.get(format!("k_{i:04}").as_bytes()).unwrap();
        }
        let pos = stats.get_ticker(Ticker::BloomFilterFullPositive);
        assert!(
            pos > 0,
            "bloom filter should have returned 'maybe' and we found the key"
        );
    }

    #[test]
    fn test_stats_flush_and_compaction_counters() {
        let stats = Arc::new(Statistics::new());
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_stats_opts(stats.clone())).unwrap();
        for i in 0..200 {
            crate::through_stalls(|| db.put(format!("k_{i:04}").as_bytes(), b"v")).unwrap();
        }
        force_flush(&db, "fcstats");
        db.compact_range(None, None).wait().unwrap();
        assert!(stats.get_ticker(Ticker::FlushCount) >= 1);
        assert!(stats.get_ticker(Ticker::FlushBytesWritten) > 0);
        assert!(stats.get_ticker(Ticker::CompactionCount) >= 1);
        assert!(stats.get_ticker(Ticker::CompactionBytesRead) > 0);
        assert!(stats.get_ticker(Ticker::CompactionBytesWritten) > 0);
        assert!(stats.get_histogram_snapshot(Histogram::FlushTime).count > 0);
        assert!(
            stats
                .get_histogram_snapshot(Histogram::CompactionTime)
                .count
                > 0
        );
    }

    #[test]
    fn test_stats_wal_counters() {
        let stats = Arc::new(Statistics::new());
        let dir = TempDir::new().unwrap();
        let opts = Options::default()
            .statistics(Some(stats.clone()))
            .durability(DurabilityMode::Immediate);
        let db = Db::open(dir.path(), opts).unwrap();
        db.put(b"k", b"v").unwrap();
        db.put(b"k2", b"v2").unwrap();
        assert!(stats.get_ticker(Ticker::WalBytesWritten) > 0);
        // Immediate durability fsyncs per call.
        assert_eq!(stats.get_ticker(Ticker::WalSyncCount), 2);
        assert!(stats.get_histogram_snapshot(Histogram::WalWriteTime).count >= 2);
    }

    #[test]
    fn test_stats_iter_seek_and_next_counters() {
        let stats = Arc::new(Statistics::new());
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), stats_opts(stats.clone())).unwrap();
        db.put(b"a", b"1").unwrap();
        db.put(b"b", b"2").unwrap();
        db.put(b"c", b"3").unwrap();
        let mut it = db.iter();
        it.seek_to_first();
        while it.valid() {
            it.next();
        }
        assert!(stats.get_ticker(Ticker::IterSeekCount) >= 1);
        // Two `next` calls produced keys (b, c); the third
        // invalidated and doesn't count.
        assert_eq!(stats.get_ticker(Ticker::IterNextCount), 2);
    }

    #[test]
    fn test_stats_snapshot_register_release() {
        let stats = Arc::new(Statistics::new());
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), stats_opts(stats.clone())).unwrap();
        {
            let _snap = db.snapshot();
        }
        assert_eq!(stats.get_ticker(Ticker::SnapshotsRegistered), 1);
        assert_eq!(stats.get_ticker(Ticker::SnapshotsReleased), 1);
    }

    #[test]
    fn test_stats_reset_clears_everything() {
        let stats = Arc::new(Statistics::new());
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), stats_opts(stats.clone())).unwrap();
        db.put(b"k", b"v").unwrap();
        assert!(stats.get_ticker(Ticker::KeysWritten) > 0);
        stats.reset();
        assert_eq!(stats.get_ticker(Ticker::KeysWritten), 0);
        assert_eq!(stats.get_ticker(Ticker::BytesWritten), 0);
    }

    #[test]
    fn test_stats_none_configured_is_noop() {
        // Sanity: with statistics disabled every hot path still
        // works and nothing panics.
        let (db, _dir) = open_tmp();
        db.put(b"k", b"v").unwrap();
        assert_eq!(db.get(b"k").unwrap(), Some(b"v".to_vec()));
    }

    #[test]
    fn test_stats_dump_is_non_empty_and_contains_every_ticker() {
        let stats = Arc::new(Statistics::new());
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), stats_opts(stats.clone())).unwrap();
        db.put(b"k", b"v").unwrap();
        let dump = stats.dump();
        for ticker_name in [
            "regolith.write.bytes",
            "regolith.write.keys",
            "regolith.bloom.useful",
            "regolith.compaction.count",
            "regolith.flush.count",
        ] {
            assert!(dump.contains(ticker_name), "dump missing {ticker_name}");
        }
    }

    // ── properties API ──────────────────────────────────────────────────

    #[test]
    fn test_property_unknown_name_returns_none() {
        let (db, _dir) = open_tmp();
        assert!(db.get_property("not.a.real.property").is_none());
        assert!(db.get_int_property("not.a.real.property").is_none());
    }

    #[test]
    fn test_property_num_files_at_level() {
        let (db, _dir) = open_tmp();
        assert_eq!(db.get_int_property("regolith.num-files-at-level0"), Some(0));
        assert_eq!(db.get_int_property("regolith.num-files-at-level6"), Some(0));
        // Out-of-range level is a valid query that returns 0.
        assert_eq!(
            db.get_int_property("regolith.num-files-at-level99"),
            Some(0)
        );
    }

    #[test]
    fn test_property_level_counts_after_flush_and_compact() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();
        for i in 0..200 {
            crate::through_stalls(|| db.put(format!("k_{i:04}").as_bytes(), b"v")).unwrap();
        }
        force_flush(&db, "props");
        // At this point we expect some L0 files.
        let l0_before = db.get_int_property("regolith.num-files-at-level0").unwrap();
        assert!(l0_before > 0 || db.get_int_property("regolith.num-files-at-level1").unwrap() > 0);

        // Drain everything to the deepest level.
        db.compact_range(None, None).wait().unwrap();
        assert_eq!(
            db.get_int_property("regolith.num-files-at-level0"),
            Some(0),
            "L0 should be empty after compact_range"
        );
    }

    #[test]
    fn test_property_total_sst_size_after_flush() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();
        assert_eq!(
            db.get_int_property("regolith.total-sst-files-size"),
            Some(0)
        );
        for i in 0..100 {
            crate::through_stalls(|| db.put(format!("k_{i:04}").as_bytes(), b"v")).unwrap();
        }
        force_flush(&db, "size");
        let size = db
            .get_int_property("regolith.total-sst-files-size")
            .unwrap();
        assert!(size > 0, "SST size should be > 0 after a flush");
    }

    #[test]
    fn test_property_cur_size_active_mem_table() {
        let (db, _dir) = open_tmp();
        assert_eq!(
            db.get_int_property("regolith.cur-size-active-mem-table"),
            Some(0)
        );
        for i in 0..50 {
            db.put(format!("k_{i:03}").as_bytes(), b"value").unwrap();
        }
        let size = db
            .get_int_property("regolith.cur-size-active-mem-table")
            .unwrap();
        assert!(size > 0, "active memtable should have non-zero size");
    }

    #[test]
    fn test_property_cur_size_all_mem_tables_aggregates() {
        let (db, _dir) = open_tmp();
        db.put(b"k", b"v").unwrap();
        let active = db
            .get_int_property("regolith.cur-size-active-mem-table")
            .unwrap();
        let all = db
            .get_int_property("regolith.cur-size-all-mem-tables")
            .unwrap();
        assert!(all >= active, "all mem tables must be >= active");
    }

    #[test]
    fn test_property_estimate_num_keys() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();
        for i in 0..100 {
            crate::through_stalls(|| db.put(format!("k_{i:04}").as_bytes(), b"v")).unwrap();
        }
        force_flush(&db, "estimate");
        db.compact_range(None, None).wait().unwrap();
        let estimate = db.get_int_property("regolith.estimate-num-keys").unwrap();
        // Exact count per SST includes the flush filler + the 100
        // writes; the property is a lower bound, so > 50 is a
        // safe floor.
        assert!(estimate > 50, "estimate-num-keys={estimate} too low");
    }

    #[test]
    fn test_property_num_snapshots_and_oldest_snapshot_time() {
        let (db, _dir) = open_tmp();
        assert_eq!(db.get_int_property("regolith.num-snapshots"), Some(0));
        assert!(
            db.get_int_property("regolith.oldest-snapshot-time")
                .is_none(),
            "oldest-snapshot-time should be None when no snapshots are live"
        );
        let _snap_a = db.snapshot();
        let _snap_b = db.snapshot();
        assert_eq!(db.get_int_property("regolith.num-snapshots"), Some(2));
        assert!(
            db.get_int_property("regolith.oldest-snapshot-time")
                .is_some()
        );
    }

    #[test]
    fn test_property_background_errors_returns_zero() {
        let (db, _dir) = open_tmp();
        // No background errors on a fresh db.
        assert_eq!(db.get_int_property("regolith.background-errors"), Some(0));
    }

    #[test]
    fn test_property_stats_string_includes_level_header_and_counters() {
        let stats = Arc::new(Statistics::new());
        let opts = Options::default().statistics(Some(stats));
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();
        db.put(b"k", b"v").unwrap();
        let text = db.get_property("regolith.stats").unwrap();
        assert!(text.contains("== regolith engine stats =="));
        assert!(text.contains("Level  Files     Size(B)"));
        assert!(text.contains("regolith.write.keys"));
    }

    #[test]
    fn test_property_stats_string_without_statistics_configured() {
        let (db, _dir) = open_tmp();
        let text = db.get_property("regolith.stats").unwrap();
        assert!(text.contains("== regolith engine stats =="));
        assert!(text.contains("(no Statistics object configured"));
    }

    #[test]
    fn test_property_sstables_lists_files_after_flush() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();
        for i in 0..50 {
            crate::through_stalls(|| db.put(format!("k_{i:03}").as_bytes(), b"v")).unwrap();
        }
        force_flush(&db, "ssts");
        let text = db.get_property("regolith.sstables").unwrap();
        assert!(text.contains("Level    FileID"));
        // Should list at least one file with non-zero size.
        assert!(
            text.lines().any(|l| l.contains("\"k_")),
            "expected a file line to include a user key from the writes"
        );
    }

    #[test]
    fn test_property_levelstats_format() {
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), tiny_flush_opts()).unwrap();
        for i in 0..20 {
            crate::through_stalls(|| db.put(format!("k_{i:03}").as_bytes(), b"v")).unwrap();
        }
        force_flush(&db, "lvl");
        let text = db.get_property("regolith.levelstats").unwrap();
        assert!(text.starts_with("Level  Files     Size(B)"));
        // Every level row is present, not just the populated ones.
        for lvl in 0..7 {
            assert!(
                text.contains(&format!("{lvl:5}")),
                "level {lvl} should appear in levelstats"
            );
        }
    }

    #[test]
    fn test_property_options_debug_dump() {
        let (db, _dir) = open_tmp();
        let text = db.get_property("regolith.options").unwrap();
        assert!(text.contains("OptionsSnapshot"));
        assert!(text.contains("default"));
    }

    #[test]
    fn test_property_integer_forms_available_via_get_property() {
        // Integer properties should also be reachable via
        // get_property, returning their decimal string form.
        let (db, _dir) = open_tmp();
        assert_eq!(
            db.get_property("regolith.num-files-at-level0").as_deref(),
            Some("0")
        );
        assert_eq!(
            db.get_property("regolith.num-snapshots").as_deref(),
            Some("0")
        );
    }

    #[test]
    fn test_multi_worker_compaction_reads_are_correct() {
        // With 4 background workers, heavy writes produce many L0
        // files that trigger multiple concurrent L1+ compactions.
        // Every key must still read back its latest value after
        // the dust settles.
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .max_background_compactions(4)
            .l0_compaction_trigger(2)
            .target_file_size(8 * 1024);
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();

        let mut expected = std::collections::BTreeMap::new();
        for i in 0..2048 {
            let k = format!("k{i:06}");
            let v = format!("v{i}");
            crate::through_stalls(|| db.put(k.as_bytes(), v.as_bytes())).unwrap();
            expected.insert(k, v);
        }
        // Overwrite a window to exercise dedup across workers.
        for i in 100..300 {
            let k = format!("k{i:06}");
            let v = format!("v{i}-new");
            crate::through_stalls(|| db.put(k.as_bytes(), v.as_bytes())).unwrap();
            expected.insert(k, v);
        }

        // Give background workers time to process L0 files.
        // Use a generous sleep so slow CI runners don't flake.
        std::thread::sleep(std::time::Duration::from_millis(500));
        db.compact_range(None, None).wait().unwrap();

        for (k, v) in &expected {
            assert_eq!(
                db.get(k.as_bytes()).unwrap(),
                Some(v.as_bytes().to_vec()),
                "key {k} must read back its latest value"
            );
        }
    }

    #[test]
    fn test_multi_worker_single_thread_matches_default() {
        // max_background_compactions=1 must behave identically to
        // the default (which is also 1). Sanity check that the
        // RwLock path doesn't break the single-worker case.
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .max_background_compactions(1);
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();
        for i in 0..500 {
            let k = format!("k{i:04}");
            crate::through_stalls(|| db.put(k.as_bytes(), b"v")).unwrap();
        }
        db.compact_range(None, None).wait().unwrap();
        for i in 0..500 {
            let k = format!("k{i:04}");
            assert_eq!(db.get(k.as_bytes()).unwrap(), Some(b"v".to_vec()));
        }
    }

    #[test]
    fn test_partitioned_index_reads_are_correct() {
        // Enable partitioned index with a tiny metadata_block_size
        // so the test actually exercises the two-level path.
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .partitioned_index(true)
            .metadata_block_size(128);
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();

        for i in 0..500 {
            let k = format!("k{i:04}");
            let v = format!("v{i}");
            crate::through_stalls(|| db.put(k.as_bytes(), v.as_bytes())).unwrap();
        }
        db.compact_range(None, None).wait().unwrap();

        for i in 0..500 {
            let k = format!("k{i:04}");
            let v = format!("v{i}");
            assert_eq!(
                db.get(k.as_bytes()).unwrap(),
                Some(v.into_bytes()),
                "key {k} must read back correctly with partitioned index"
            );
        }
    }

    #[test]
    fn test_partitioned_index_scan_matches_flat() {
        // Write the same data with and without partitioned index.
        // Scans must produce identical results.
        let write_and_scan = |partitioned: bool| -> Vec<(Vec<u8>, Vec<u8>)> {
            let dir = TempDir::new().unwrap();
            let opts = Options::default()
                .write_buffer_size(4 * 1024)
                .partitioned_index(partitioned)
                .metadata_block_size(128);
            let db = Db::open(dir.path(), opts).unwrap();
            for i in 0..200 {
                let k = format!("k{i:04}");
                let v = format!("v{i}");
                crate::through_stalls(|| db.put(k.as_bytes(), v.as_bytes())).unwrap();
            }
            db.compact_range(None, None).wait().unwrap();
            db.scan(None, None).unwrap()
        };
        let flat = write_and_scan(false);
        let partitioned = write_and_scan(true);
        assert_eq!(flat, partitioned, "partitioned scan must match flat scan");
    }

    #[test]
    fn test_partitioned_index_survives_reopen() {
        let dir = TempDir::new().unwrap();
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .partitioned_index(true)
            .metadata_block_size(128);
        {
            let db = Db::open(dir.path(), opts.clone()).unwrap();
            for i in 0..200 {
                let k = format!("k{i:04}");
                crate::through_stalls(|| db.put(k.as_bytes(), b"v")).unwrap();
            }
            db.compact_range(None, None).wait().unwrap();
        }
        // Reopen - the V2 SSTables must still be readable.
        let db = Db::open(dir.path(), opts).unwrap();
        for i in 0..200 {
            let k = format!("k{i:04}");
            assert_eq!(db.get(k.as_bytes()).unwrap(), Some(b"v".to_vec()));
        }
    }

    #[test]
    fn test_mixed_v1_v2_sstables_read_correctly() {
        // Write some data with flat index (V1), then switch to
        // partitioned (V2) and write more. Reads that span both
        // file types must work correctly.
        let dir = TempDir::new().unwrap();
        {
            let opts = Options::default()
                .write_buffer_size(4 * 1024)
                .partitioned_index(false);
            let db = Db::open(dir.path(), opts).unwrap();
            for i in 0..100 {
                let k = format!("k{i:04}");
                crate::through_stalls(|| db.put(k.as_bytes(), b"v1")).unwrap();
            }
            db.compact_range(None, None).wait().unwrap();
        }
        {
            let opts = Options::default()
                .write_buffer_size(4 * 1024)
                .partitioned_index(true)
                .metadata_block_size(128);
            let db = Db::open(dir.path(), opts).unwrap();
            for i in 100..200 {
                let k = format!("k{i:04}");
                crate::through_stalls(|| db.put(k.as_bytes(), b"v2")).unwrap();
            }
            // Don't compact - leave V1 files at lower levels and
            // V2 files in L0/L1.
        }
        let opts = Options::default().partitioned_index(true);
        let db = Db::open(dir.path(), opts).unwrap();
        for i in 0..100 {
            let k = format!("k{i:04}");
            assert_eq!(db.get(k.as_bytes()).unwrap(), Some(b"v1".to_vec()));
        }
        for i in 100..200 {
            let k = format!("k{i:04}");
            assert_eq!(db.get(k.as_bytes()).unwrap(), Some(b"v2".to_vec()));
        }
    }

    #[test]
    fn test_streaming_compaction_produces_correct_reads() {
        // A compaction large enough to span multiple output files;
        // read everything back and confirm the streaming path keeps
        // the latest version for every user key.
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .max_subcompactions(4)
            .target_file_size(8 * 1024);
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();

        let expected: Vec<(String, String)> = (0..2048)
            .map(|i| (format!("k{i:06}"), format!("v{i}")))
            .collect();
        for (k, v) in &expected {
            crate::through_stalls(|| db.put(k.as_bytes(), v.as_bytes())).unwrap();
        }
        // Overwrite a window to exercise dedup across input files.
        for i in 100..200 {
            let k = format!("k{i:06}");
            let v = format!("v{i}-new");
            crate::through_stalls(|| db.put(k.as_bytes(), v.as_bytes())).unwrap();
        }

        db.compact_range(None, None).wait().unwrap();

        for (k, v) in &expected {
            let i: usize = k[1..].parse().unwrap();
            let want = if (100..200).contains(&i) {
                format!("v{i}-new")
            } else {
                v.clone()
            };
            assert_eq!(
                db.get(k.as_bytes()).unwrap(),
                Some(want.into_bytes()),
                "key {k} must read back the latest value"
            );
        }
    }

    #[test]
    fn test_streaming_compaction_handles_single_worker_option() {
        // max_subcompactions is accepted for API compatibility, but
        // the streaming compaction path writes from the compaction
        // worker thread.
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .max_subcompactions(1)
            .target_file_size(8 * 1024);
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();
        for i in 0..256 {
            let k = format!("k{i:04}");
            crate::through_stalls(|| db.put(k.as_bytes(), b"v")).unwrap();
        }
        db.compact_range(None, None).wait().unwrap();
        for i in 0..256 {
            let k = format!("k{i:04}");
            assert_eq!(db.get(k.as_bytes()).unwrap(), Some(b"v".to_vec()));
        }
    }

    #[test]
    fn test_streaming_compaction_accepts_subcompaction_option() {
        // The compatibility knob should not change correctness even
        // though output writing is now part of the bounded-memory
        // streaming path.
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .max_subcompactions(8);
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();
        for i in 0..32 {
            let k = format!("k{i:02}");
            crate::through_stalls(|| db.put(k.as_bytes(), b"v")).unwrap();
        }
        db.compact_range(None, None).wait().unwrap();
        for i in 0..32 {
            let k = format!("k{i:02}");
            assert_eq!(db.get(k.as_bytes()).unwrap(), Some(b"v".to_vec()));
        }
    }

    #[test]
    fn test_perf_context_captures_db_get_and_put_activity() {
        // End-to-end: enable PerfContext timing on the current
        // thread, do a few writes and reads, then snapshot. The
        // counters should show one get_count per read, one
        // write_count per put, and non-zero time in both the
        // WAL/memtable write phases and the memtable read phase.
        let (db, _dir) = open_tmp();

        PerfContext::set_level(PerfLevel::EnableTime);
        PerfContext::reset();

        db.put(b"alpha", b"1").unwrap();
        db.put(b"beta", b"2").unwrap();
        db.put(b"gamma", b"3").unwrap();

        let _ = db.get(b"alpha").unwrap();
        let _ = db.get(b"beta").unwrap();

        let snap = PerfContext::capture();
        assert_eq!(snap.write_count, 3, "3 puts → write_count 3");
        assert_eq!(snap.get_count, 2, "2 gets → get_count 2");
        assert!(
            snap.write_wal_time_nanos > 0,
            "WAL phase should record non-zero time under EnableTime"
        );
        assert!(
            snap.write_memtable_time_nanos > 0,
            "memtable write phase should record non-zero time"
        );
        assert!(
            snap.get_from_memtable_time_nanos > 0,
            "memtable read phase should record non-zero time"
        );

        // Disable and confirm subsequent activity is invisible.
        PerfContext::set_level(PerfLevel::Disable);
        let before = snap;
        db.put(b"delta", b"4").unwrap();
        let _ = db.get(b"alpha").unwrap();
        let after = PerfContext::capture();
        assert_eq!(after, before, "Disable level must freeze counters");
    }

    #[test]
    fn test_evict_compaction_data_from_page_cache_is_correctness_neutral() {
        // Enabling the page-cache hint must not change what a
        // compaction produces. On Linux the `posix_fadvise`
        // syscall runs but is a best-effort hint; on other
        // platforms it's a no-op. Either way, the output SSTs
        // contain the same data as the leveled baseline, so
        // readers must see identical values afterward.
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .evict_compaction_data_from_page_cache(true);
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();

        for i in 0..128 {
            let k = format!("k{i:04}");
            let v = format!("v{i}");
            crate::through_stalls(|| db.put(k.as_bytes(), v.as_bytes())).unwrap();
        }
        // Overwrite a few so dedup runs through the hint path.
        for i in 0..32 {
            let k = format!("k{i:04}");
            let v = format!("v{i}-new");
            crate::through_stalls(|| db.put(k.as_bytes(), v.as_bytes())).unwrap();
        }
        db.compact_range(None, None).wait().unwrap();

        for i in 0..128 {
            let k = format!("k{i:04}");
            let expected = if i < 32 {
                format!("v{i}-new")
            } else {
                format!("v{i}")
            };
            assert_eq!(
                db.get(k.as_bytes()).unwrap(),
                Some(expected.into_bytes()),
                "key {k} must still read its latest value"
            );
        }
    }

    #[test]
    fn test_universal_compaction_reads_are_correct_after_merge() {
        // Write a batch under Universal, force a full merge via
        // compact_range, and verify every key is still readable
        // with the most recent value.
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .compaction_style(CompactionStyle::Universal);
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();

        for i in 0..64 {
            let k = format!("k{i:04}");
            let v = format!("v{i}");
            crate::through_stalls(|| db.put(k.as_bytes(), v.as_bytes())).unwrap();
        }
        // Overwrite the first 16 keys so dedup has to pick the
        // newest version during the merge.
        for i in 0..16 {
            let k = format!("k{i:04}");
            let v = format!("v{i}-updated");
            crate::through_stalls(|| db.put(k.as_bytes(), v.as_bytes())).unwrap();
        }

        db.compact_range(None, None).wait().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));

        for i in 0..64 {
            let k = format!("k{i:04}");
            let expected = if i < 16 {
                format!("v{i}-updated")
            } else {
                format!("v{i}")
            };
            assert_eq!(
                db.get(k.as_bytes()).unwrap(),
                Some(expected.into_bytes()),
                "key {k} must read back its latest value"
            );
        }
    }

    #[test]
    fn test_universal_compaction_never_creates_l1_files() {
        // Every Universal merge output should stay at L0 - the
        // level-size push-down rule must not fire for this style.
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .l0_compaction_trigger(1)
            .compaction_style(CompactionStyle::Universal);
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();
        for i in 0..64 {
            let k = format!("k{i:04}");
            crate::through_stalls(|| db.put(k.as_bytes(), &vec![0xCC; 256])).unwrap();
        }
        db.compact_range(None, None).wait().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));

        let l1 = db.get_int_property("regolith.num-files-at-level1").unwrap();
        assert_eq!(l1, 0, "Universal must not produce L1 files, saw {l1}");
        let l0 = db.get_int_property("regolith.num-files-at-level0").unwrap();
        assert!(
            l0 >= 1,
            "Universal compaction should leave at least one L0 file"
        );
    }

    #[test]
    fn test_universal_compaction_full_merge_drops_shadowed_versions() {
        // After a full universal compact_range, we expect the
        // output to be a single L0 file (min cardinality). This
        // exercises the compact_range full-merge path.
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .compaction_style(CompactionStyle::Universal);
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();
        for i in 0..32 {
            let k = format!("k{i:04}");
            crate::through_stalls(|| db.put(k.as_bytes(), &vec![0xAA; 512])).unwrap();
        }
        // Give the background scheduler a moment to potentially
        // kick off work, then force-merge synchronously.
        std::thread::sleep(std::time::Duration::from_millis(50));
        db.compact_range(None, None).wait().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));

        let l0 = db.get_int_property("regolith.num-files-at-level0").unwrap();
        assert_eq!(
            l0, 1,
            "full universal compact_range should fold everything into one L0 file, saw {l0}"
        );
    }

    #[test]
    fn test_fifo_compaction_bounds_total_size() {
        // Tiny memtable + tight FIFO cap: sustained writes should
        // produce many L0 files, and after each flush the oldest
        // ones should be unlinked so the total stays bounded.
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .compaction_style(CompactionStyle::Fifo)
            .fifo_compaction_options(FifoCompactionOptions {
                max_table_files_size: 32 * 1024,
            });
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();

        // Write enough data to produce ~16 flushes of ~4 KB each,
        // well over the 32 KB cap. Each write has a distinct
        // monotonically increasing key so flushes don't overlap.
        let payload = vec![0xEEu8; 256];
        for i in 0..256 {
            let k = format!("k{i:06}");
            crate::through_stalls(|| db.put(k.as_bytes(), &payload)).unwrap();
        }

        // Give the background compaction thread a moment to
        // process the trailing flushes + FIFO drops.
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Force any remaining flushes through and run one more
        // FIFO pass via compact_range (which acquires the
        // compaction gate and drains pending work).
        db.compact_range(None, None).wait().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));

        let total = db
            .get_int_property("regolith.total-sst-files-size")
            .unwrap_or(0);
        assert!(
            total <= 64 * 1024,
            "FIFO cap 32KB should keep total < 64KB slack, got {total}"
        );
        // Meanwhile the newest keys must still be readable (the
        // oldest ones may have been dropped by FIFO).
        assert_eq!(
            db.get(b"k000255").unwrap(),
            Some(payload.clone()),
            "newest key must survive FIFO compaction"
        );
    }

    #[test]
    fn test_fifo_compaction_keeps_at_least_one_file() {
        // A single oversized file must not be deleted - FIFO
        // refuses to drop the last surviving SST because that
        // would wipe the database.
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .compaction_style(CompactionStyle::Fifo)
            .fifo_compaction_options(FifoCompactionOptions {
                max_table_files_size: 1, // 1 byte cap: always over limit
            });
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();
        for i in 0..32 {
            let k = format!("k{i:04}");
            crate::through_stalls(|| db.put(k.as_bytes(), &vec![0xAA; 512])).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        db.compact_range(None, None).wait().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));

        let l0 = db.get_int_property("regolith.num-files-at-level0").unwrap();
        assert!(
            l0 >= 1,
            "FIFO must keep at least one L0 file even when over the cap"
        );
    }

    #[test]
    fn test_fifo_compaction_never_promotes_to_l1() {
        // Under FIFO, the background scheduler should never
        // promote files from L0 to L1. The `l0_compaction_trigger`
        // knob is a level-style knob and must have no effect.
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .l0_compaction_trigger(2)
            .compaction_style(CompactionStyle::Fifo)
            .fifo_compaction_options(FifoCompactionOptions {
                max_table_files_size: 10 * 1024 * 1024,
            });
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();
        for i in 0..64 {
            let k = format!("k{i:04}");
            crate::through_stalls(|| db.put(k.as_bytes(), &vec![0xBB; 256])).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(200));

        let l1 = db.get_int_property("regolith.num-files-at-level1").unwrap();
        assert_eq!(l1, 0, "FIFO must not produce L1 files, saw {l1}");
    }

    #[test]
    fn test_tailing_iter_sees_writes_after_creation() {
        // Initial writes, then a tailing iterator, then more
        // writes - the tailing iterator must surface the later
        // writes once it advances past the initial set.
        let (db, _dir) = open_tmp();
        for i in 0..5 {
            let k = format!("log/{i:04}");
            db.put(k.as_bytes(), format!("v{i}").as_bytes()).unwrap();
        }

        let mut tail = db.iter_tailing();
        tail.seek_to_first();

        // Drain the initial 5 entries.
        let mut seen: Vec<String> = Vec::new();
        while tail.valid() {
            seen.push(String::from_utf8(tail.key().unwrap().to_vec()).unwrap());
            tail.next();
        }
        assert_eq!(seen.len(), 5, "first drain saw {seen:?}");

        // Now push more writes at strictly larger keys.
        for i in 5..10 {
            let k = format!("log/{i:04}");
            db.put(k.as_bytes(), format!("v{i}").as_bytes()).unwrap();
        }

        // Stepping again should refresh the view and surface
        // the new entries without re-emitting the first batch.
        tail.next();
        while tail.valid() {
            seen.push(String::from_utf8(tail.key().unwrap().to_vec()).unwrap());
            tail.next();
        }
        assert_eq!(seen.len(), 10, "tail saw {seen:?}");
        for (i, k) in seen.iter().enumerate() {
            assert_eq!(k, &format!("log/{i:04}"));
        }
    }

    #[test]
    fn test_tailing_iter_survives_flush_and_compaction() {
        // Tiny write_buffer so writes between drains roll
        // memtables and produce L0 files. The tailing iter must
        // pick up those new SSTs on the next refresh.
        let opts = Options::default().write_buffer_size(4 * 1024);
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();

        for i in 0..16 {
            let k = format!("log/{i:04}");
            crate::through_stalls(|| db.put(k.as_bytes(), &vec![0xAA; 256])).unwrap();
        }

        let mut tail = db.iter_tailing();
        tail.seek_to_first();
        let mut seen: usize = 0;
        while tail.valid() {
            seen += 1;
            tail.next();
        }
        assert_eq!(seen, 16);

        // Force a flush and a compaction - the existing tail
        // iter is no longer pinned to anything visible, but a
        // refresh + new writes should still work.
        db.compact_range(None, None).wait().unwrap();
        for i in 16..32 {
            let k = format!("log/{i:04}");
            crate::through_stalls(|| db.put(k.as_bytes(), &vec![0xBB; 256])).unwrap();
        }

        tail.refresh();
        let mut seen_after = 0;
        while tail.valid() {
            seen_after += 1;
            tail.next();
        }
        assert_eq!(
            seen_after, 16,
            "tail should pick up the 16 new entries after refresh"
        );
    }

    #[test]
    fn test_tailing_iter_no_re_emission_after_explicit_refresh() {
        let (db, _dir) = open_tmp();
        for i in 0..3 {
            db.put(format!("k{i}").as_bytes(), b"v").unwrap();
        }
        let mut tail = db.iter_tailing();
        tail.seek_to_first();
        assert!(tail.valid());
        let first_key = tail.key().unwrap().to_vec();
        assert_eq!(first_key, b"k0");
        tail.next();
        assert_eq!(tail.key().unwrap(), b"k1");

        // Explicit refresh in the middle of iteration must NOT
        // re-emit k0.
        tail.refresh();
        // After refresh we should be positioned strictly after
        // the last returned key (k1), so the next valid key is
        // k2.
        assert!(tail.valid());
        assert_eq!(tail.key().unwrap(), b"k2");
        tail.next();
        assert!(!tail.valid(), "no more keys after k2");
    }

    #[test]
    fn test_tailing_iter_cf_scoping() {
        // Tailing iterator scoped to one CF must not surface
        // keys from other CFs.
        let (db, _dir) = open_tmp();
        let cf_logs = db.create_column_family("logs").unwrap();
        let cf_other = db.create_column_family("other").unwrap();

        db.put_cf(&cf_logs, b"a", b"1").unwrap();
        db.put_cf(&cf_other, b"a", b"x").unwrap();
        db.put_cf(&cf_logs, b"b", b"2").unwrap();

        let mut tail = db.iter_tailing_cf(&cf_logs);
        tail.seek_to_first();
        let mut seen = Vec::new();
        while tail.valid() {
            seen.push((tail.key().unwrap().to_vec(), tail.value().unwrap().to_vec()));
            tail.next();
        }
        assert_eq!(
            seen,
            vec![
                (b"a".to_vec(), b"1".to_vec()),
                (b"b".to_vec(), b"2".to_vec())
            ]
        );

        // A write to the other CF must not bleed in even after
        // refresh.
        db.put_cf(&cf_other, b"c", b"y").unwrap();
        tail.refresh();
        assert!(!tail.valid());
    }

    #[test]
    fn test_block_cache_usage_property_reports_nonzero_after_reads() {
        // A cache with a small-but-nonzero budget fills with
        // decompressed data blocks as reads touch SSTables. The
        // `regolith.block-cache-usage` property must report a
        // positive number once at least one read has happened
        // against a file that isn't entirely in the memtable.
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .block_cache_size(1024 * 1024);
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();

        let payload = vec![0xABu8; 256];
        for i in 0..200 {
            let k = format!("k{i:04}");
            crate::through_stalls(|| db.put(k.as_bytes(), &payload)).unwrap();
        }
        // Force a flush so the reads below have to touch SST blocks.
        db.compact_range(None, None).wait().unwrap();

        // Read a few keys to populate the block cache.
        for i in 0..50 {
            let k = format!("k{i:04}");
            let _ = db.get(k.as_bytes()).unwrap();
        }

        let usage = db
            .get_int_property("regolith.block-cache-usage")
            .expect("property must exist");
        assert!(
            usage > 0,
            "expected block-cache-usage > 0 after reads, got {usage}"
        );
        let cap = db
            .get_int_property("regolith.block-cache-capacity")
            .expect("property must exist");
        assert!(
            cap >= 512 * 1024,
            "expected at least 512KB capacity, got {cap}"
        );
        assert!(usage <= cap, "usage {usage} must not exceed capacity {cap}");
    }

    #[test]
    fn test_rate_limiter_throttles_compaction() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        // 100 KB/s sustained, 5 KB burst. Compression is disabled so
        // the on-disk SST size stays proportional to the data we
        // feed in (otherwise LZ4 would collapse the payload to a
        // few KB and nothing meaningful would be throttled). A
        // 16 MB buffer keeps everything in the memtable until
        // compact_range triggers a flush + compaction, both of
        // which the limiter throttles: the job runs on the worker,
        // the only thread the limiter applies to.
        let limiter = Arc::new(TokenBucketRateLimiter::new(100_000, 5_000));
        let opts = Options::default()
            .write_buffer_size(16 * 1024 * 1024)
            .compression(CompressionType::None)
            .rate_limiter(Some(limiter.clone() as Arc<dyn RateLimiter>));

        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();

        // Write ~100 KB of well-dispersed keys so the resulting SST
        // is large enough that the limiter has real work to do.
        for i in 0..200 {
            let k = format!("key-{i:010}");
            // Each value is distinct so prefix compression can't
            // collapse the block.
            let v = format!("value-for-key-{i:010}-payload-{}", i);
            db.put(k.as_bytes(), v.as_bytes()).unwrap();
        }

        let start = Instant::now();
        db.compact_range(None, None).wait().unwrap();
        let elapsed = start.elapsed();

        // With ~10 KB of uncompressed output flushed + compacted at
        // 100 KB/s past a 5 KB burst, we expect the critical path
        // to block for at least 50 ms and in practice more. Assert a
        // conservative floor to confirm the limiter was actually
        // consulted without flaking on CI variance.
        assert!(
            elapsed >= Duration::from_millis(100),
            "compaction with 100KB/s limiter finished in {elapsed:?}, expected >= 100ms"
        );

        // The limiter must have been consulted for background I/O.
        assert!(
            limiter.get_total_bytes_through(Priority::Low) > 0,
            "limiter saw zero background bytes"
        );
        assert_eq!(limiter.get_total_bytes_through(Priority::High), 0);
    }

    /// Wait, with a deadline and a bounded backoff, until the background
    /// has written L0 up to `files` tables, a count at which the caller's
    /// options stall writes, and a write meets that stall.
    ///
    /// The table count alone runs ahead of the stall: a flush publishes its
    /// table first and refreshes the level writers check after it, so a
    /// write between the two is still admitted.
    pub(crate) fn wait_for_l0_files(db: &Db, files: u64) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut pause = std::time::Duration::from_millis(1);
        while db
            .get_int_property("regolith.num-files-at-level0")
            .unwrap_or(0)
            < files
            || db.engine.admit_write(true).is_ok()
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the background never wrote L0 up to {files} tables and stalled writes"
            );
            std::thread::sleep(pause);
            pause = (pause * 2).min(std::time::Duration::from_millis(50));
        }
    }

    /// A slowdown never delays a write: no sleep, so no stall time is
    /// charged, and every write under it lands.
    #[test]
    fn a_slowdown_never_delays_a_write() {
        use std::sync::Arc;

        let stats = Arc::new(Statistics::new());
        let opts = Options::default()
            // Tiny memtable so every handful of puts rolls an L0 file.
            .write_buffer_size(4 * 1024)
            // Disable automatic compaction so L0 can't drain on us.
            .l0_compaction_trigger(1000)
            // Slow down once L0 has 2 files, never stop (high trigger).
            .level0_slowdown_writes_trigger(2)
            .level0_stop_writes_trigger(10_000)
            // Disable the memtable-count trigger for this test so we
            // isolate the L0 slowdown path.
            .max_write_buffer_number(0)
            .statistics(Some(stats.clone()));
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();

        // Write enough data to cross the slowdown trigger and keep
        // going. Each put is ~600 bytes, so after ~7 puts the
        // memtable rolls, and after the 2nd flush L0 hits the
        // slowdown trigger.
        let payload = vec![0xCDu8; 600];
        for i in 0..64 {
            let k = format!("k{i:04}");
            crate::through_stalls(|| db.put(k.as_bytes(), &payload)).unwrap();
        }
        // The worker writes the sealed memtables out off the commit path;
        // the writes after that run under the slowdown.
        wait_for_l0_files(&db, 2);
        assert!(
            db.engine.stall_signal().classify().is_some(),
            "precondition: the slowdown holds"
        );
        for i in 64..128 {
            let k = format!("k{i:04}");
            crate::through_stalls(|| db.put(k.as_bytes(), &payload)).unwrap();
        }
        assert_eq!(
            stats.get_ticker(Ticker::WriteStallMicros),
            0,
            "a slowdown sleeps for nothing, so it charges nothing"
        );
        assert_eq!(db.get(b"k0127").unwrap(), Some(payload));
    }

    #[test]
    fn test_write_stall_no_slowdown_returns_busy() {
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .l0_compaction_trigger(1000)
            .level0_slowdown_writes_trigger(2)
            .level0_stop_writes_trigger(10_000)
            .max_write_buffer_number(0);
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();

        // Build up L0 past the slowdown trigger. The worker writes the
        // sealed memtables out off the commit path.
        let payload = vec![0xEFu8; 600];
        for i in 0..64 {
            let k = format!("k{i:04}");
            crate::through_stalls(|| db.put(k.as_bytes(), &payload)).unwrap();
        }
        wait_for_l0_files(&db, 2);

        // A write with `no_slowdown` must now return Busy rather
        // than sleep or block.
        let wo = WriteOptions {
            no_slowdown: true,
            ..WriteOptions::default()
        };
        let err = db.put_opt(&wo, b"extra", b"value").unwrap_err();
        assert!(
            matches!(err, Error::Busy(_)),
            "expected Error::Busy, got {err:?}"
        );
    }

    /// A write a stop stopped returns at once with a wait, applies nothing,
    /// and the wait completes when a compaction clears the stop; the write
    /// run again lands.
    #[test]
    fn a_stopped_write_returns_a_wait_the_clearing_compaction_completes() {
        use std::sync::Arc;

        // Stop writes entirely once L0 hits 2 files.
        let opts = Options::default()
            .write_buffer_size(4 * 1024)
            .l0_compaction_trigger(1000)
            .level0_slowdown_writes_trigger(0)
            .level0_stop_writes_trigger(2)
            .max_write_buffer_number(0);
        let dir = TempDir::new().unwrap();
        let db = Arc::new(Db::open(dir.path(), opts).unwrap());

        // Fill L0 to the stop trigger. Writes go through until two
        // memtables are sealed; the worker writes them out to L0, and
        // from then on the next write would block, so we time it
        // carefully with a spawned thread.
        let payload = vec![0x12u8; 600];
        let l0 = || {
            db.get_int_property("regolith.num-files-at-level0")
                .unwrap_or(0)
        };
        for i in 0..32 {
            let k = format!("fill{i:04}");
            crate::through_stalls(|| db.put(k.as_bytes(), &payload)).unwrap();
            if l0() + db.engine.frozen_memtables() as u64 >= 2 {
                break;
            }
        }
        wait_for_l0_files(&db, 2);

        let stall = match db.put(b"stopkey", b"stopval") {
            Err(Error::WouldBlock(WouldBlock::Stall(stall))) => stall,
            other => panic!("a stopped write must return its wait, got {other:?}"),
        };
        assert_eq!(stall.queue(), None, "this thread has no queue");
        assert!(!stall.is_ready());
        assert_eq!(db.get(b"stopkey").unwrap(), None, "it applied nothing");

        // compact_range empties L0; the pass that does refreshes the stall
        // and lands its unit, which completes the wait directly.
        db.compact_range(None, None).wait().unwrap();
        assert!(
            stall.is_ready(),
            "the clearing compaction completed the wait"
        );

        crate::through_stalls(|| db.put(b"stopkey", b"stopval")).unwrap();
        assert_eq!(db.get(b"stopkey").unwrap(), Some(b"stopval".to_vec()));
    }

    #[test]
    fn test_rate_limiter_unset_leaves_compaction_uncapped() {
        // Sanity check: with no limiter in Options, compaction still
        // runs and produces correct results. This is the default
        // configuration; the test exists mainly to pin the no-op
        // branch.
        use std::time::Instant;

        let opts = Options::default().write_buffer_size(64 * 1024);
        let dir = TempDir::new().unwrap();
        let db = Db::open(dir.path(), opts).unwrap();

        let payload = vec![0xABu8; 1024];
        for i in 0..256 {
            let k = format!("k{i:06}");
            crate::through_stalls(|| db.put(k.as_bytes(), &payload)).unwrap();
        }

        let start = Instant::now();
        db.compact_range(None, None).wait().unwrap();
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "unthrottled compaction took unreasonably long: {:?}",
            start.elapsed()
        );

        // Reads still work after compaction.
        for i in 0..256 {
            let k = format!("k{i:06}");
            assert_eq!(db.get(k.as_bytes()).unwrap(), Some(payload.clone()));
        }
    }
}
