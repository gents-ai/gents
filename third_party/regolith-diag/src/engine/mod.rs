pub(crate) mod arena;
pub(crate) mod background_health;
mod background_step;
#[cfg(test)]
mod background_step_tests;
pub(crate) mod block;
pub(crate) mod block_cache;
#[cfg(all(test, not(loom)))]
mod blocked_call_tests;
pub(crate) mod bloom;
mod buried_range_deletes;
pub(crate) mod callback;
pub(crate) mod checksum;
#[cfg(test)]
mod close_in_place_tests;
pub(crate) mod commit;
pub(crate) mod compaction;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod compaction_backoff;
pub(crate) mod compaction_gate;
pub(crate) mod disk_check;
mod drain;
pub(crate) mod filter_block;
pub(crate) mod filter_call;
mod flush;
mod foreground;
pub(crate) mod index_block;
mod ingest;
#[cfg(test)]
mod ingest_range_tests;
#[cfg(test)]
mod ingest_window_tests;
pub(crate) mod internal_key;
pub(crate) mod io;
pub(crate) mod iterator;
pub(crate) mod log_retirement;
// The log writeback lane exists only where the host has a way to start the
// writeback (`LogFile::start_writeback`): `sync_file_range` on Linux, a plain
// `fsync` on Apple, where the seal's sync is `F_FULLFSYNC`.
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
mod log_writeback;
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
type WritebackLane = log_writeback::LogWritebackLane;
#[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
type WritebackLane = std::convert::Infallible;
pub(crate) mod lookup_key;
#[cfg(loom)]
pub mod loom_model;
pub(crate) mod manifest;
pub(crate) mod memtable;
pub(crate) mod merge_call;
mod merge_chain;
pub(crate) mod open_transactions;
pub(crate) mod orphan_sweep;
#[cfg(all(test, not(loom)))]
mod park_support;
pub(crate) mod pending_outputs;
#[cfg(all(test, not(loom)))]
mod poll_park_tests;
pub(crate) mod prefix_call;
pub(crate) mod range_tombstone;
pub(crate) mod read_horizon;
mod read_rule;
pub(crate) mod read_view;
pub(crate) mod reclaim;
mod recovery;
pub(crate) mod seal;
pub(crate) mod skiplist;
pub(crate) mod snapshot_registry;
pub(crate) mod source_walk;
pub(crate) mod sstable;
mod stall_state;
mod step;
pub(crate) mod take;
pub(crate) mod version_store;
pub(crate) mod wal;
#[cfg(test)]
mod wal_close_tests;
pub(crate) mod wal_frame;
mod wal_gate;
pub(crate) mod wal_log;
#[cfg(test)]
mod wal_order_tests;
pub(crate) mod wal_replay;
#[cfg(test)]
mod wal_rotation_tests;
mod wal_scan;
pub(crate) mod wal_seal;
pub(crate) mod wal_v1;

use crate::portability::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::sync::OnceCell;
use crate::sync::internal::wait_here;

use background_health::{BackgroundHealth, Job};
use block_cache::BlockCache;
pub(crate) use commit::PendingAppend;
pub(crate) use commit::{NowaitCommit, OwedCommit, PendingCommit, TxnCommit, conclude_settled};
use commit::{Pipeline, StallSignal};
use compaction::{Claims, CompactionOptions, CompactionOutcome, CompactionScheduler};
use compaction_gate::{CompactionGate, Exclusive};
use drain::ActiveFlush;
pub(crate) use foreground::Work;
use lookup_key::{LookupKey, with_key_scratch};
use manifest::{VersionEdit, VersionSet};
use memtable::{MemTable, MemTableConfig};

const STOP_TOO_MANY_MEMTABLES: &str = "stop: too many memtables";
use log_retirement::RetiredLogs;
use read_horizon::ReadHorizon;
pub(crate) use read_rule::ReadRule;
use read_view::{ReadView, ReadViewCell};
use recovery::{
    hoist_range_deletes, replay_logs, report_copied_range_deletes, report_demoted_tables,
    report_discarded_tail, report_dropped_manifest_tail, rewrite_recovered_memtable_to_wal,
};
use skiplist::InsertHint;
pub(crate) use snapshot_registry::SnapshotPin;
use snapshot_registry::SnapshotRegistry;
use source_walk::Source;
use version_store::VersionStore;

use crate::env::{Capabilities, Env, FileLock};
use crate::{Access, Conflict, DbSlice, WriteBatchOp, WriteKind, event_listener};
use sstable::{LiveSst, LookupResult, Materialize, PointValue, SsTableReader, sst_filename};
use wal::{RecordLen, Wal, WalEntry, check_write_len, wal_filename};

/// Controls when data is flushed to disk after a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DurabilityMode {
    Immediate,
    Eventual,
}

/// Outcome of [`RegolithEngine::commit_with_conflict_check`].
/// `Conflict` indicates that another writer changed one of the
/// tracked keys after the sequence the transaction observed it at;
/// the caller typically surfaces this as a retry-able error.
#[derive(Debug)]
pub(crate) enum CommitOutcome {
    /// The commit validated. `seq` is the sequence its writes became visible
    /// at, and `None` for a commit that carried no write.
    Ok { seq: Option<u64> },
    /// The reason, with the key still carrying its column-family prefix.
    Conflict(Conflict),
}

/// Why the engine stopped taking writes, see `RegolithEngine::write_latch`.
enum WriteLatch {
    /// Failed commits could not be rolled back out of the log.
    Wal(std::io::ErrorKind, String),
    /// A caller's code of this trait panicked inside the ordered step.
    CallbackPanicked(&'static str),
}

/// Lay a commit's writes out as one batch: point operations in ascending key
/// order, then range deletes, then merges in ascending key order, each key's
/// operands in the order they were buffered. The commit's conflict check
/// relies on both sorted runs, so this layout is a guarantee and not an
/// accident.
fn grouped_batch_ops(
    point_ops: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    range_deletes: Vec<(Vec<u8>, Vec<u8>)>,
    mut merges: Vec<(Vec<u8>, Vec<u8>)>,
) -> Vec<WriteBatchOp> {
    // Stable on purpose: a key's operands must keep the order they were
    // buffered in, which is the order a read folds them.
    merges.sort_by(|a, b| a.0.cmp(&b.0));
    let mut ops = Vec::with_capacity(point_ops.len() + range_deletes.len() + merges.len());
    for (key, value) in point_ops {
        match value {
            Some(value) => ops.push(WriteBatchOp::Put { key, value }),
            None => ops.push(WriteBatchOp::Delete { key }),
        }
    }
    for (start, end) in range_deletes {
        ops.push(WriteBatchOp::DeleteRange { start, end });
    }
    for (key, operand) in merges {
        ops.push(WriteBatchOp::Merge { key, operand });
    }
    ops
}

/// A key a commit validates because the transaction read it: the key and
/// the earliest sequence the transaction observed it at. Never elided as an
/// idempotent write, since a value derived from a stale read is a lost
/// update even when the bytes match.
#[derive(Clone, Debug)]
pub(crate) struct ConflictKey {
    /// The prefixed key.
    pub key: Vec<u8>,
    /// The sequence the transaction observed it at.
    pub observed_seq: u64,
    /// A read of the key returned a value. Recorded only for a transaction
    /// with a key classifier, which narrows it to `presence_only`; the
    /// commit does not look at it.
    pub found: bool,
    /// What the transaction did with the key, which is what a conflict on it
    /// reports. [`Access::ReadPresence`] marks a read of a key whose bytes
    /// never differ that returned a value, so it is lost only when the key
    /// is gone: see [`ConflictKey::presence_only`].
    pub access: Access,
    /// What counts as a change to the key since the read.
    pub rule: ReadRule,
}

impl ConflictKey {
    /// The read returned a value of a key whose bytes never differ, so a newer
    /// version conflicts only if the newest is a deletion, a covering range
    /// delete included. It does not stand for a write of the same key, which
    /// takes its own check.
    pub fn presence_only(&self) -> bool {
        self.access == Access::ReadPresence
    }
}

/// A key range a validated scan covered, as prefixed keys `[lo, hi)`: no
/// write may have landed in it after `observed_seq`. Sorted and disjoint in a
/// [`ValidationSet`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RangeCheck {
    pub lo: Vec<u8>,
    pub hi: Vec<u8>,
    pub observed_seq: u64,
}

/// What a commit validates on top of the operations it carries.
///
/// The written keys are not listed here: the commit already owns them in
/// its operation list and validates them from there, so listing them again
/// would clone every key a second time. What needs its own entry is a key
/// the transaction read, since a read carries its own anchor (the read's
/// sequence) and its own rule (never elided).
#[derive(Debug)]
pub(crate) struct ValidationSet {
    /// Keys the transaction read that the isolation level validates.
    /// Sorted by key with no duplicates; the commit relies on both to
    /// find a written key here by binary search and to name the same key
    /// on every run of a multi-key conflict. A read is validated in full
    /// (any newer version conflicts) unless it is `presence_only`.
    pub reads: Vec<ConflictKey>,
    /// The sequence every written or merged key not in `reads` is validated
    /// against, or `None` when written keys are not validated at all
    /// (pessimistic mode: the key lock orders them and there is no read to
    /// lose).
    pub writes_at: Option<u64>,
    /// Whether a key this commit only merges into, and did not read, is
    /// validated against the newest write that replaced it rather than the
    /// newest write of any kind. Operands commute, so a newer operand never
    /// invalidates a blind merge; a newer put or delete still does.
    pub blind_merges_commute: bool,
    /// Keys the commit puts or merges that the caller exempts from
    /// validation: the commit skips them without looking them up. Sorted by
    /// key with no duplicates, for the binary search that finds a written
    /// key here, and empty unless the transaction layer's key classifier
    /// named some, so a commit without one pays nothing. Their reads are not
    /// in `reads`, and a key the commit deletes is never listed.
    pub exempt: Vec<Vec<u8>>,
    /// Ranges a validated scan covered, which conflict with any write newer
    /// than their `observed_seq` inside them (see `commit::range_rule`).
    /// Empty unless the transaction used a validated scan.
    pub ranges: Vec<RangeCheck>,
}

fn batch_op_wal_bytes(op: &WriteBatchOp) -> u64 {
    match op {
        WriteBatchOp::Put { key, value } => (key.len() + value.len() + 8) as u64,
        WriteBatchOp::Delete { key } => (key.len() + 8) as u64,
        WriteBatchOp::DeleteRange { start, end } => (start.len() + end.len() + 8) as u64,
        WriteBatchOp::Merge { key, operand } => (key.len() + operand.len() + 8) as u64,
    }
}

fn memtable_needs_flush(memtable: &MemTable) -> bool {
    !memtable.is_empty() || memtable.has_range_tombstones()
}

/// Apply one batch op to `memtable`, threading `hint` through the point
/// ops. `DeleteRange` touches only the tombstone set, not the skip list,
/// so the hint stays valid across it.
fn apply_batch_op_to_memtable<'a>(
    memtable: &'a MemTable,
    hint: &mut InsertHint<'a>,
    op: &WriteBatchOp,
    seq: u64,
) {
    match op {
        WriteBatchOp::Put { key, value } => memtable.put_hinted(hint, key, value, seq),
        WriteBatchOp::Delete { key } => memtable.delete_hinted(hint, key, seq),
        WriteBatchOp::DeleteRange { start, end } => memtable.delete_range(start, end, seq),
        WriteBatchOp::Merge { key, operand } => memtable.merge_hinted(hint, key, operand, seq),
    }
}

/// One version of a key: its sequence, value type and value.
pub(crate) type KeyVersion = (u64, u8, DbSlice);

pub(crate) use merge_chain::MergeChain;

struct MultiGetEntry {
    key: Vec<u8>,
    output_indexes: Vec<usize>,
    max_rt: u64,
    resolved: bool,
}

fn grouped_multi_get_entries(keys: &[&[u8]]) -> Vec<MultiGetEntry> {
    let mut grouped: BTreeMap<Vec<u8>, Vec<usize>> = BTreeMap::new();
    for (idx, key) in keys.iter().enumerate() {
        grouped.entry((*key).to_vec()).or_default().push(idx);
    }
    grouped
        .into_iter()
        .map(|(key, output_indexes)| MultiGetEntry {
            key,
            output_indexes,
            max_rt: 0,
            resolved: false,
        })
        .collect()
}

fn file_covers_key(file: &LiveSst, key: &[u8]) -> bool {
    file.meta.smallest_key.as_slice() <= key && key <= file.meta.largest_key.as_slice()
}

fn resolve_multi_get_value(pseq: u64, popt: Option<DbSlice>, rt_seq: u64) -> Option<Vec<u8>> {
    if pseq > rt_seq {
        popt.map(DbSlice::into_vec)
    } else {
        None
    }
}

fn set_multi_get_result(
    entry: &mut MultiGetEntry,
    results: &mut [Option<Vec<u8>>],
    value: Option<Vec<u8>>,
) {
    for &output_idx in &entry.output_indexes {
        results[output_idx] = value.clone();
    }
    entry.resolved = true;
}

/// Configuration for the Regolith engine.
#[derive(Clone)]
pub(crate) struct EngineOptions {
    pub(crate) write_buffer_size: usize,
    pub(crate) arena_profile: crate::options::ArenaProfile,
    pub(crate) block_size: usize,
    pub(crate) block_cache_size: usize,
    pub(crate) block_cache_num_shard_bits: u32,
    pub(crate) strict_capacity_limit: bool,
    pub(crate) bloom_bits_per_key: usize,
    pub(crate) compression: crate::options::CompressionType,
    pub(crate) compression_per_level: Option<Vec<crate::options::CompressionType>>,
    pub(crate) l0_compaction_trigger: usize,
    pub(crate) level_base_bytes: u64,
    pub(crate) level_size_multiplier: u64,
    pub(crate) target_file_size: u64,
    pub(crate) compaction_filter: Option<Arc<dyn crate::options::CompactionFilter>>,
    pub(crate) prefix_extractor: Option<Arc<dyn crate::options::PrefixExtractor>>,
    pub(crate) merge_operator: Option<Arc<dyn crate::options::MergeOperator>>,
    pub(crate) listeners: Vec<Arc<dyn crate::event_listener::EventListener>>,
    pub(crate) transaction_hooks: Option<Arc<dyn crate::TransactionHooks>>,
    /// Set when the database is encrypted at rest: every file the engine
    /// writes is sealed under its current key.
    pub(crate) keyring: Option<Arc<seal::Keyring>>,
    pub(crate) statistics: Option<Arc<crate::statistics::Statistics>>,
    pub(crate) rate_limiter: Option<Arc<dyn crate::rate_limiter::RateLimiter>>,
    pub(crate) level0_slowdown_writes_trigger: usize,
    pub(crate) level0_stop_writes_trigger: usize,
    pub(crate) soft_pending_compaction_bytes_limit: u64,
    pub(crate) hard_pending_compaction_bytes_limit: u64,
    pub(crate) max_write_buffer_number: usize,
    pub(crate) compaction_style: crate::options::CompactionStyle,
    pub(crate) fifo_compaction_options: crate::options::FifoCompactionOptions,
    pub(crate) universal_compaction_options: crate::options::UniversalCompactionOptions,
    pub(crate) evict_compaction_data_from_page_cache: bool,
    pub(crate) max_background_compactions: usize,
    /// See [`crate::Options::log_writeback`]. Read on Linux and Apple alone,
    /// the hosts with a way to start the writeback.
    #[cfg_attr(
        not(any(target_os = "linux", target_vendor = "apple")),
        allow(dead_code)
    )]
    pub(crate) log_writeback: bool,
    /// See [`crate::Options::inline_compaction`].
    pub(crate) inline_compaction: bool,
    pub(crate) partitioned_index: bool,
    pub(crate) metadata_block_size: usize,
    pub(crate) cache_index_and_filter_blocks: bool,
    pub(crate) read_only: bool,
    pub(crate) max_key_size: usize,
    pub(crate) max_value_size: usize,
    /// The host platform this database runs on. Every filesystem,
    /// clock, and thread call the engine makes goes through it.
    pub(crate) env: Arc<dyn Env>,
}

impl EngineOptions {
    /// How readers this engine opens should hold their index and
    /// filter blocks.
    pub(crate) fn metadata_policy(&self) -> sstable::MetadataPolicy {
        if self.cache_index_and_filter_blocks {
            sstable::MetadataPolicy::Cached
        } else {
            sstable::MetadataPolicy::Pinned
        }
    }

    /// Resolve the codec to use when writing an SSTable destined for
    /// `level`. A per-level override (if any) wins; otherwise fall
    /// back to the default codec.
    pub(crate) fn compression_for_level(&self, level: usize) -> crate::options::CompressionType {
        match &self.compression_per_level {
            Some(per_level) if level < per_level.len() => per_level[level],
            _ => self.compression,
        }
    }

    /// Project these options onto the subset compaction consumes.
    ///
    /// The three places that need a [`CompactionOptions`] - starting
    /// the scheduler, `compact_range`, and the foreground inline pass -
    /// all call this, so a knob added to one cannot go missing from
    /// another.
    pub(crate) fn to_compaction_options(&self) -> CompactionOptions {
        CompactionOptions {
            l0_compaction_trigger: self.l0_compaction_trigger,
            level0_slowdown_writes_trigger: self.level0_slowdown_writes_trigger,
            level0_stop_writes_trigger: self.level0_stop_writes_trigger,
            soft_pending_compaction_bytes_limit: self.soft_pending_compaction_bytes_limit,
            hard_pending_compaction_bytes_limit: self.hard_pending_compaction_bytes_limit,
            level_base_bytes: self.level_base_bytes,
            level_size_multiplier: self.level_size_multiplier,
            target_file_size: self.target_file_size,
            block_size: self.block_size,
            bloom_bits_per_key: self.bloom_bits_per_key,
            cache_index_and_filter_blocks: self.cache_index_and_filter_blocks,
            compression: self.compression,
            compression_per_level: self.compression_per_level.clone(),
            compaction_filter: self.compaction_filter.clone(),
            prefix_extractor: self.prefix_extractor.clone(),
            merge_operator: self.merge_operator.clone(),
            listeners: self.listeners.clone(),
            statistics: self.statistics.clone(),
            rate_limiter: self.rate_limiter.clone(),
            compaction_style: self.compaction_style,
            fifo_compaction_options: self.fifo_compaction_options,
            universal_compaction_options: self.universal_compaction_options,
            evict_compaction_data_from_page_cache: self.evict_compaction_data_from_page_cache,
            max_background_compactions: self.max_background_compactions,
            partitioned_index: self.partitioned_index,
            metadata_block_size: self.metadata_block_size,
            keyring: self.keyring.clone(),
            env: Arc::clone(&self.env),
        }
    }
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            write_buffer_size: 64 * 1024 * 1024,
            arena_profile: crate::options::ArenaProfile::SERVER,
            block_size: 16 * 1024,
            block_cache_size: 512 * 1024 * 1024,
            block_cache_num_shard_bits: 6,
            strict_capacity_limit: false,
            bloom_bits_per_key: 10,
            compression: crate::options::CompressionType::Lz4,
            compression_per_level: None,
            l0_compaction_trigger: compaction::L0_COMPACTION_TRIGGER,
            level_base_bytes: compaction::DEFAULT_LEVEL_BASE_BYTES,
            level_size_multiplier: compaction::LEVEL_SIZE_MULTIPLIER,
            target_file_size: compaction::DEFAULT_TARGET_FILE_SIZE,
            compaction_filter: None,
            prefix_extractor: None,
            merge_operator: None,
            listeners: Vec::new(),
            transaction_hooks: None,
            keyring: None,
            statistics: None,
            rate_limiter: None,
            level0_slowdown_writes_trigger: 20,
            level0_stop_writes_trigger: 36,
            soft_pending_compaction_bytes_limit: 64 * 1024 * 1024 * 1024,
            hard_pending_compaction_bytes_limit: 256 * 1024 * 1024 * 1024,
            max_write_buffer_number: 2,
            compaction_style: crate::options::CompactionStyle::Level,
            fifo_compaction_options: crate::options::FifoCompactionOptions::default(),
            universal_compaction_options: crate::options::UniversalCompactionOptions::default(),
            evict_compaction_data_from_page_cache: false,
            max_background_compactions: 1,
            log_writeback: false,
            inline_compaction: false,
            partitioned_index: false,
            metadata_block_size: 4096,
            cache_index_and_filter_blocks: false,
            read_only: false,
            max_key_size: crate::options::DEFAULT_MAX_KEY_SIZE,
            max_value_size: crate::options::DEFAULT_MAX_VALUE_SIZE,
            env: crate::env::std_env(),
        }
    }
}

const CLOSE_STATE_OPEN: u8 = 0;
const CLOSE_STATE_CLOSING: u8 = 1;
const CLOSE_STATE_CLOSED: u8 = 2;

/// Blocks of the configured block size one I/O queue may owe before a
/// further miss waits for room. Enough for a seek through every level and a
/// deep L0 at once, with each level's index leaf and filter.
const IO_QUEUE_BLOCKS: usize = 256;

/// The core LSM-tree engine.
pub(crate) struct RegolithEngine {
    /// This engine, for the work it leaves on a queue (a sync round, an
    /// ordered unit, an owed step): held weakly, so pending work never keeps a
    /// database open that every caller dropped.
    me: std::sync::Weak<RegolithEngine>,
    /// The published read view: the active memtable, the frozen
    /// memtables and the version every read resolves against. Loading
    /// it is the whole of a read's source acquisition - one shared lock
    /// acquisition and one `Arc` clone - and the three sources it hands
    /// back are consistent with each other by construction.
    view: Arc<ReadViewCell>,
    /// The engine's one memtable arena pool and its sizing policy. Every
    /// memtable this engine builds recycles the others' chunks through it.
    memtable_config: MemTableConfig,
    /// The version set, together with the publication of every version
    /// it installs into `view`.
    versions: Arc<VersionStore>,
    cache: Arc<BlockCache>,
    /// Published read horizon: the highest sequence whose data is fully
    /// applied and durable. Snapshots read this, never the last sequence
    /// the pipeline decided, so a
    /// snapshot taken mid-commit cannot observe a sequence whose WAL and
    /// memtable writes have not landed yet. The ordering that makes that
    /// true lives in [`ReadHorizon`], which is where it is model-checked.
    visible_seq: ReadHorizon,
    close_state: AtomicU8,
    /// One close at a time; a second caller waits for the first and then
    /// reads what it left in `close_state`. A [`crate::sync::Mutex`], so no
    /// thread sleeps in the kernel on it: close is a call that chose to
    /// block, and waits on its own thread. Built without spin (E48): a close
    /// holds it for the whole close, flushes and syncs included, so a second
    /// close queues at once.
    close_lock: crate::sync::Mutex<()>,
    /// Transactions that `close` must abort. See `open_transactions`.
    open_transactions: open_transactions::OpenTransactions,
    wal_id: AtomicU64,
    sst_dir: PathBuf,
    wal_dir: PathBuf,
    /// The compaction workers. Every call on it takes `&self` and none
    /// waits, so nothing guards it.
    compaction: CompactionScheduler,
    /// The engine-wide gate that keeps a compaction pass and the operations
    /// that must not see a table replaced underneath them apart: a pass,
    /// on a worker or on a caller's thread, holds it shared, and
    /// `compact_range`, an ingest, a checkpoint, `drop_all` and a backup's
    /// capture hold it alone (`compaction_gate.rs`).
    compaction_gate: CompactionGate,
    /// Tracks the sequence numbers of every live snapshot so compaction
    /// can cut each key's versions into stripes no snapshot straddles,
    /// and drop or fold what no reader can see. A snapshot registers
    /// itself on creation and releases on drop; a compaction pass reads
    /// the list with [`SnapshotRegistry::live_seqs`] once its inputs are
    /// fixed.
    snapshot_registry: Arc<SnapshotRegistry>,
    /// The database's column-family registry, attached by `Db::open` before
    /// the first write. The commit leader fences every write against it in
    /// the ordered step (`commit::families`); an engine with none fences
    /// nothing. Read with one atomic load once set.
    families: std::sync::OnceLock<Arc<crate::column_family::CfRegistry>>,
    options: EngineOptions,
    /// The lock-free commit pipeline (`commit::pipeline`): the ring every
    /// commit takes a slot of, the state the next commit is decided from
    /// (its sequence, the active log and the offset its record goes to, the
    /// active memtable), and the current sync round. `None` on a database
    /// opened read-only, which takes no write. Operations that rotate the
    /// memtable or the log (`compact_range`, `ingest_external_files`,
    /// `checkpoint_capture`, `drop_all`, `close`) take their turn through
    /// it as ordered units (`commit::ordered`).
    pipeline: Option<Pipeline>,
    /// What writes frozen memtables out, shared with the compaction
    /// workers, which flush on their own (E9) and hold no handle on the
    /// engine. Its `flushing` exclusion serializes every flush.
    flusher: Arc<flush::Flusher>,
    /// Latched write-path failure. Set only when failed commits could
    /// not be rolled back out of the WAL, which leaves the log with
    /// a tail no later write may extend, or when a caller's code panicked
    /// inside the commit's ordered step. Once set, every write fails loud
    /// with the original reason instead of appending after unknown bytes or
    /// state. Written once, by one compare-and-swap, so the first reason is
    /// the one kept; every write checks it with one load.
    write_latch: OnceCell<WriteLatch>,
    /// The write stall: the level writers cache, and the unit writers a
    /// stop threshold stopped wait on (D23). Whoever relieves a threshold
    /// (a flush, a compaction pass on a worker or a caller's thread)
    /// refreshes it, which lands the unit once writes may go on.
    stall_signal: Arc<StallSignal>,
    /// Cached stall level: 0 = none, 1 = slowdown, 2 = stop.
    /// Updated by `rotate_memtable` (after changing L0/memtable
    /// counts) and by the compaction thread (after reducing them).
    /// Writers check this atomic first - the full `stall_state()`
    /// with its lock acquisitions is only called when the cached
    /// level is nonzero, saving 2 lock round-trips per write in
    /// the common no-stall case.
    cached_stall_level: Arc<AtomicU8>,
    /// With no worker, a write stopped by a stall leaves the step that
    /// clears it on its thread's queue, or runs it inline with no queue,
    /// and a slowed write owes one ([`crate::Options::inline_compaction`]).
    inline_compaction: bool,
    /// A compaction worker runs: it flushes what writers seal. Without one,
    /// a writer owes the flush itself (`background_step.rs`).
    has_worker: bool,
    /// What a database with no worker owes the background, as
    /// `background_step::OWED_*` bits: a bounded step of flush or
    /// compaction, the disk check. The next write to leave the commit
    /// pipeline runs it, on its thread's queue or inline (`background_step.rs`).
    owed: AtomicU8,
    /// A job running what is owed waits on some thread's queue: no second
    /// one is left until it ran.
    owed_queued: AtomicBool,
    /// The thread that hands the active log to the host's writeback as it
    /// fills, ahead of its seal, when [`crate::Options::log_writeback`] is on
    /// and background threads run on Linux or Apple (`log_writeback.rs`).
    log_writeback: Option<WritebackLane>,
    /// Where the disk check looks.
    disk_dir: PathBuf,
    /// Tables this open moved out of overlapping levels (`recovery.rs`).
    tables_demoted_at_open: u64,
    /// Range tombstones this open copied in front of older keys they may
    /// delete (`buried_range_deletes.rs`).
    range_deletes_copied_at_open: u64,
    /// A step relieving a stop waits on some thread's queue: no second one
    /// is left until it ran (`background_step.rs`).
    stall_step_queued: AtomicBool,
    /// Steps in a row that found nothing to do while a stop held: past
    /// [`Self::MAX_IDLE_PASSES`] the stop is one no step relieves.
    stall_steps_idle: std::sync::atomic::AtomicUsize,
    /// The tables compaction passes are working on, shared with every
    /// background worker and with [`RegolithEngine::run_one_compaction_pass`].
    /// One record for the whole engine is what stops a pass on a caller's
    /// thread and a worker from picking overlapping inputs
    /// (`compaction/claims.rs`).
    compaction_claims: Arc<Claims>,
    /// Whether background flushes and compactions are currently failing.
    background_health: Arc<BackgroundHealth>,
    /// The host platform. Cloned out of [`EngineOptions`] so the read
    /// and write paths reach it without going through `options`.
    env: Arc<dyn Env>,
    /// The lock on the database directory, shared with every compaction
    /// worker, so the directory is released only once the engine and every
    /// worker are gone. An engine torn down on its own worker (a job held the
    /// last reference) cannot join that worker, which keeps its share until
    /// it has returned: no other open writes the directory while it might.
    _db_lock: Arc<dyn FileLock>,
}

impl RegolithEngine {
    /// Open or create the database at the given path.
    pub(crate) fn open(db_dir: &Path, mut options: EngineOptions) -> std::io::Result<Arc<Self>> {
        options.read_only = false;
        let env = Arc::clone(&options.env);
        let db_lock: Arc<dyn FileLock> = Arc::from(env.lock_file(db_dir, true)?);
        let sst_dir = db_dir.join("sst");
        let wal_dir = db_dir.join("wal");

        env.create_dir_all(&sst_dir)?;
        env.create_dir_all(&wal_dir)?;

        // Every file this open writes is sealed under the current key, so a
        // current key the provider cannot provide refuses here, before
        // anything is written.
        if let Some(keyring) = &options.keyring {
            keyring.current()?;
        }
        let mut version_set = VersionSet::open_with_policy(
            &env,
            db_dir,
            &sst_dir,
            options.metadata_policy(),
            options.keyring.clone(),
        )?;
        if let Some(tail) = version_set.dropped_tail() {
            report_dropped_manifest_tail(&options, version_set.manifest_path(), tail);
        }
        let manifest_path = version_set.manifest_path().to_path_buf();
        let demotion = version_set.take_demotion();
        let check = !version_set.range_deletes_checked();
        let version = version_set.current();
        let latest_seq = version.last_seq;

        // Replay WAL files to recover memtable state
        let memtable_config = MemTableConfig::new(
            options.arena_profile,
            options.write_buffer_size,
            options.max_write_buffer_number,
        );
        let memtable = Arc::new(MemTable::new(&memtable_config)?);
        let wal_files = wal_scan::live_removing_staged(&*env, &wal_dir, version.min_wal_id)?;

        let (latest_seq, discarded) = replay_logs(
            &env,
            &wal_files,
            &memtable,
            latest_seq,
            options.keyring.as_deref(),
        )?;
        if let Some((path, tail)) = discarded {
            // Before the next log exists, so the newest log is complete
            // when it becomes an earlier one (WalRecovery.tla, RED
            // `NoTruncate`): a crash before the old logs are removed below
            // leaves it beside the new one, and an earlier log with a
            // damaged tail refuses the open.
            wal::truncate_durably(&*env, &path, tail.offset)?;
            report_discarded_tail(&options, &path, tail, latest_seq);
        }
        // Into the memtable the new log is written from just below, so
        // they are durable there before the edit that records the move.
        let (moved, copied, latest_seq) =
            hoist_range_deletes(demotion, check, &version, &memtable, latest_seq);

        let wal_id = next_wal_id(version.next_file_id, &wal_files);
        let wal_path = wal_dir.join(wal_filename(wal_id));
        let mut wal = Wal::create_in(&env, &wal_path, options.keyring.as_deref())?;

        rewrite_recovered_memtable_to_wal(&memtable, &mut wal)?;
        // From here on the log is written by position, by whichever thread
        // reaches a decided commit (`wal_log`).
        let (log, log_end) = wal.into_log()?;

        let versions = Arc::new(VersionStore::new(version_set));

        // The new log holds every write the replayed ones did, so they are
        // retired with the logs flushes left behind. Recorded unsynced: if
        // the record is lost, the replayed logs are replayed again beside
        // the new one, which repeats their writes and nothing older.
        //
        // An open that moved overlapping levels records the move in this
        // same batch, which is then synced: the new log already holds the
        // moved tables' range tombstones, so no crash leaves the move
        // without them (LegacyRangeRead.tla, RED `MoveBeforeLog`). The
        // first open of a database written without the record of the range
        // delete check records it here too, synced for the same reason (RED
        // `CheckBeforeLog`); a new database records it unsynced, having
        // nothing to copy.
        //
        // Applied before the view exists, so the view starts at the version
        // this records and the open publishes no view of its own: a
        // publication retires the view it replaces, which costs a padded
        // reclamation batch (`reclaim::submit`) on every open.
        versions.run(|versions| {
            versions.apply(&[
                VersionEdit::SetNextFileId(wal_id + 1),
                VersionEdit::SetMinWalId(wal_id),
            ])
        })?;
        let view = Arc::new(ReadViewCell::new(ReadView {
            active: Arc::clone(&memtable),
            frozen: Vec::new(),
            version: versions.current(),
        }));
        versions.attach_view(Arc::clone(&view));
        let tables_demoted_at_open = moved.map_or(0, |moved| moved.tables);
        if let Some(moved) = moved {
            report_demoted_tables(&options, &manifest_path, moved, false);
        }
        report_copied_range_deletes(&options, &manifest_path, copied, false);
        let removed =
            log_retirement::remove_below(&*env, &wal_dir, wal_id, options.statistics.as_deref());
        let retired_logs = RetiredLogs::new(wal_dir.clone(), !removed);

        let cache = Arc::new(
            BlockCache::with_config(
                options.block_cache_size,
                options.block_cache_num_shard_bits,
                options.strict_capacity_limit,
            )
            .with_stats(options.statistics.clone()),
        );
        let compaction_opts = options.to_compaction_options();

        let compaction_gate = CompactionGate::new();
        let snapshot_registry = Arc::new(SnapshotRegistry::with_env(Arc::clone(&env)));
        let cached_stall_level = Arc::new(AtomicU8::new(0));
        let stall_signal = Arc::new(StallSignal::new(
            Arc::clone(&view),
            options.clone(),
            Arc::clone(&cached_stall_level),
        ));
        let compaction_claims = Arc::new(Claims::new());
        let background_health = Arc::new(BackgroundHealth::relieving(Arc::clone(&stall_signal)));
        // `max_background_compactions == 0` starts no worker, which is
        // how a single-threaded target opens at all. A platform that
        // cannot spawn a worker it was asked for fails the open here
        // instead of aborting; the directory lock and the fresh WAL
        // drop with this return.
        let compaction = CompactionScheduler::start(
            Arc::clone(&db_lock),
            compaction_gate.clone(),
            Arc::clone(&snapshot_registry),
            Arc::clone(&versions),
            Arc::from(sst_dir.as_path()),
            Arc::clone(&cache),
            compaction_opts,
            Arc::clone(&stall_signal),
            Arc::clone(&compaction_claims),
            Arc::clone(&background_health),
        )?;
        let has_worker = compaction.has_workers();
        // The disk check is background work (D15): never on the open path
        // and never on a thread of its own. A worker runs it; with none, the
        // first write owes it, on its thread's queue or inline.
        let owed = if has_worker {
            let check = disk_check::DiskCheck::new(Arc::clone(&env), db_dir.to_path_buf());
            if let Err(check) = compaction.submit(Box::new(check)) {
                check.release();
            }
            0
        } else {
            background_step::OWED_DISK_CHECK
        };
        // The workers flush what writers seal (E9), through a flusher that
        // holds nothing of the engine, so a worker never keeps a database
        // open that every caller dropped.
        let flusher = Arc::new(flush::Flusher::new(
            Arc::clone(&view),
            Arc::clone(&versions),
            sst_dir.clone(),
            retired_logs,
            options.clone(),
            Arc::clone(&background_health),
            Arc::clone(&stall_signal),
        ));
        compaction.set_flush(Box::new({
            let flusher = Arc::clone(&flusher);
            move || flusher.flush_all_frozen()
        }));

        // When the caller asked for it and background threads run, the
        // active log is handed to the host's writeback as it fills, on a
        // thread of its own (E46, `Options::log_writeback`). Off by default:
        // the thread is one more than the compaction threads plan 3.13 and
        // 4.5 promise. Only Linux and Apple have a way to start the writeback
        // (`LogFile::start_writeback`); elsewhere the thread would have
        // nothing to start.
        #[cfg(any(target_os = "linux", target_vendor = "apple"))]
        let log_writeback = if has_worker && options.log_writeback {
            Some(log_writeback::LogWritebackLane::start(&*env)?)
        } else {
            None
        };
        #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
        let log_writeback: Option<WritebackLane> = None;
        let early =
            background_step::early_writeback(log_writeback.as_ref(), options.write_buffer_size);
        let engine = Arc::new_cyclic(|me| Self {
            me: me.clone(),
            view,
            memtable_config,
            versions,
            cache,
            visible_seq: ReadHorizon::new(latest_seq),
            close_state: AtomicU8::new(CLOSE_STATE_OPEN),
            close_lock: crate::sync::Mutex::new_without_spin(()),
            open_transactions: open_transactions::OpenTransactions::new(),
            wal_id: AtomicU64::new(wal_id),
            sst_dir,
            wal_dir,
            compaction,
            compaction_gate,
            snapshot_registry,
            families: std::sync::OnceLock::new(),
            inline_compaction: options.inline_compaction,
            has_worker,
            owed: AtomicU8::new(owed),
            owed_queued: AtomicBool::new(false),
            log_writeback,
            disk_dir: db_dir.to_path_buf(),
            tables_demoted_at_open,
            range_deletes_copied_at_open: copied,
            stall_step_queued: AtomicBool::new(false),
            stall_steps_idle: std::sync::atomic::AtomicUsize::new(0),
            options,
            pipeline: Some(Pipeline::new(
                commit::commit_ring_capacity(),
                commit::After {
                    next_seq: latest_seq + 1,
                    log: Arc::new(log.with_early_writeback(early)),
                    next_off: log_end,
                    mem_used: memtable.approximate_size(),
                    memtable,
                    closed: false,
                    epoch: 0,
                    counters: commit::Counters::empty(),
                },
                me.clone(),
            )),
            flusher,
            write_latch: OnceCell::new(),
            stall_signal,
            cached_stall_level,
            compaction_claims,
            background_health,
            env,
            _db_lock: db_lock,
        });

        #[cfg(any(target_os = "linux", target_vendor = "apple"))]
        if let Some(lane) = &engine.log_writeback {
            let me = engine.me.clone();
            lane.set_hook(Box::new(move || {
                if let Some(engine) = me.upgrade() {
                    engine.write_back_log_ahead();
                }
            }));
        }

        // Arm write back-pressure before the first write. A database
        // reopened with L0 already past its stop trigger would
        // otherwise take the `cached_stall_level == 0` fast path and
        // accept writes with no back-pressure until the first memtable
        // rotation refreshed the cache.
        engine.refresh_stall_level();

        crate::env::db_lock::diag::register_engine(db_dir, &engine);
        Ok(engine)
    }

    pub(crate) fn diag_close_state(&self) -> u8 {
        self.close_state.load(Ordering::Acquire)
    }

    /// Open an existing database without mutating files or starting
    /// background writers.
    pub(crate) fn open_read_only(
        db_dir: &Path,
        mut options: EngineOptions,
    ) -> std::io::Result<Arc<Self>> {
        let env = Arc::clone(&options.env);
        let db_lock: Arc<dyn FileLock> = Arc::from(env.lock_file(db_dir, false)?);
        let sst_dir = db_dir.join("sst");
        let wal_dir = db_dir.join("wal");

        if !env.is_dir(&sst_dir) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "missing SST directory for read-only open: {}",
                    sst_dir.display()
                ),
            ));
        }
        if !env.is_dir(&wal_dir) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "missing WAL directory for read-only open: {}",
                    wal_dir.display()
                ),
            ));
        }

        options.read_only = true;

        let memtable_config = MemTableConfig::new(
            options.arena_profile,
            options.write_buffer_size,
            options.max_write_buffer_number,
        );
        let mut version_set = VersionSet::open_read_only(
            &env,
            db_dir,
            &sst_dir,
            options.metadata_policy(),
            options.keyring.clone(),
        )?;
        if let Some(tail) = version_set.dropped_tail() {
            report_dropped_manifest_tail(&options, version_set.manifest_path(), tail);
        }
        // A read-only open moves overlapping levels in its own view only:
        // the manifest keeps them, and every open moves them again until a
        // read-write one records the move.
        let demotion = version_set.take_demotion();
        let check = !version_set.range_deletes_checked();
        let version = version_set.current();
        let latest_seq = version.last_seq;

        let memtable = Arc::new(MemTable::new(&memtable_config)?);
        let wal_files = wal_scan::live(&*env, &wal_dir, version.min_wal_id)?;

        // A read-only open writes nothing, so the tail stays in the file;
        // the next read-write open drops it again and truncates it.
        let (latest_seq, discarded) = replay_logs(
            &env,
            &wal_files,
            &memtable,
            latest_seq,
            options.keyring.as_deref(),
        )?;
        if let Some((path, tail)) = discarded {
            report_discarded_tail(&options, &path, tail, latest_seq);
        }
        let (moved, copied, latest_seq) =
            hoist_range_deletes(demotion, check, &version, &memtable, latest_seq);
        let tables_demoted_at_open = moved.map_or(0, |moved| moved.tables);
        if let Some(moved) = moved {
            report_demoted_tables(&options, version_set.manifest_path(), moved, true);
        }
        report_copied_range_deletes(&options, version_set.manifest_path(), copied, true);

        let cache = Arc::new(
            BlockCache::with_config(
                options.block_cache_size,
                options.block_cache_num_shard_bits,
                options.strict_capacity_limit,
            )
            .with_stats(options.statistics.clone()),
        );
        let versions = Arc::new(VersionStore::new(version_set));
        let view = Arc::new(ReadViewCell::new(ReadView {
            active: Arc::clone(&memtable),
            frozen: Vec::new(),
            version: versions.current(),
        }));
        versions.attach_view(Arc::clone(&view));
        let snapshot_registry = Arc::new(SnapshotRegistry::with_env(Arc::clone(&env)));
        let cached_stall_level = Arc::new(AtomicU8::new(0));
        let stall_signal = Arc::new(StallSignal::new(
            Arc::clone(&view),
            options.clone(),
            Arc::clone(&cached_stall_level),
        ));
        let wal_id = next_wal_id(version.next_file_id, &wal_files);
        let background_health = Arc::new(BackgroundHealth::relieving(Arc::clone(&stall_signal)));
        // Never flushes: a read-only engine refuses every write before one
        // could seal a memtable.
        let flusher = Arc::new(flush::Flusher::new(
            Arc::clone(&view),
            Arc::clone(&versions),
            sst_dir.clone(),
            RetiredLogs::new(wal_dir.clone(), false),
            options.clone(),
            Arc::clone(&background_health),
            Arc::clone(&stall_signal),
        ));

        Ok(Arc::new_cyclic(|me| Self {
            me: me.clone(),
            view,
            memtable_config,
            versions,
            cache,
            visible_seq: ReadHorizon::new(latest_seq),
            close_state: AtomicU8::new(CLOSE_STATE_OPEN),
            close_lock: crate::sync::Mutex::new_without_spin(()),
            open_transactions: open_transactions::OpenTransactions::new(),
            wal_id: AtomicU64::new(wal_id),
            sst_dir,
            wal_dir,
            compaction: CompactionScheduler::disabled(),
            compaction_gate: CompactionGate::new(),
            snapshot_registry,
            families: std::sync::OnceLock::new(),
            // Writes are refused before they reach the stall, so a
            // read-only engine never stalls one.
            inline_compaction: false,
            has_worker: false,
            owed: AtomicU8::new(0),
            owed_queued: AtomicBool::new(false),
            log_writeback: None,
            disk_dir: db_dir.to_path_buf(),
            tables_demoted_at_open,
            range_deletes_copied_at_open: copied,
            stall_step_queued: AtomicBool::new(false),
            stall_steps_idle: std::sync::atomic::AtomicUsize::new(0),
            options,
            pipeline: None,
            flusher,
            write_latch: OnceCell::new(),
            stall_signal,
            cached_stall_level,
            compaction_claims: Arc::new(Claims::new()),
            background_health,
            env,
            _db_lock: db_lock,
        }))
    }

    /// What the installed environment can actually do.
    pub(crate) fn capabilities(&self) -> Capabilities {
        self.env.capabilities()
    }

    /// The environment this database runs on, for the wrappers
    /// (checkpoints, TTL) that do filesystem or clock work of their
    /// own and must do it on the same host.
    pub(crate) fn env(&self) -> &Arc<dyn Env> {
        &self.env
    }

    /// The keyring this database seals its files through, when it is
    /// encrypted at rest. A backup seals its metadata through it too.
    pub(crate) fn keyring(&self) -> Option<&Arc<seal::Keyring>> {
        self.options.keyring.as_ref()
    }

    /// Microseconds elapsed since `start`, or `None` when this
    /// platform has no monotonic clock.
    ///
    /// A `None` means "not measured". Callers skip the recording
    /// rather than publishing a zero that reads like a measurement.
    fn elapsed_micros(&self, start: Option<u64>) -> Option<u64> {
        crate::env::elapsed_micros(&*self.env, start)
    }

    /// The start reading of a latency sample, or `None` when no latency
    /// histogram is recorded: no statistics, or statistics at
    /// [`crate::StatsLevel::ExceptTimers`]. Then no clock is read.
    fn timer_start(&self) -> Option<u64> {
        self.statistics()
            .filter(|stats| stats.timed())
            .and_then(|_| self.env.now_micros())
    }

    pub(crate) fn snapshot_seq(&self) -> u64 {
        self.visible_seq.visible()
    }

    pub(crate) fn is_read_only(&self) -> bool {
        self.options.read_only
    }

    /// The operator that folds merge operands into a value, if one is
    /// configured.
    pub(crate) fn merge_operator(&self) -> Option<&dyn crate::options::MergeOperator> {
        self.options.merge_operator.as_deref()
    }

    pub(crate) fn max_value_size(&self) -> usize {
        self.options.max_value_size
    }

    /// Refuses a merge write on a database with no operator to fold it, which
    /// reads would otherwise disagree about.
    pub(crate) fn require_merge_operator(&self) -> Result<(), crate::Error> {
        self.merge_operator()
            .map(|_| ())
            .ok_or(crate::Error::NoMergeOperator)
    }

    /// The transactions `close` aborts.
    pub(crate) fn open_transactions(&self) -> &open_transactions::OpenTransactions {
        &self.open_transactions
    }

    /// The database-wide transaction callbacks, if any are installed.
    pub(crate) fn transaction_hooks(&self) -> Option<&Arc<dyn crate::TransactionHooks>> {
        self.options.transaction_hooks.as_ref()
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.close_state.load(Ordering::Acquire) != CLOSE_STATE_OPEN
    }

    fn closed_error() -> std::io::Error {
        crate::Error::Closed.into_io_error()
    }

    fn ensure_open(&self) -> std::io::Result<()> {
        if self.is_closed() {
            Err(Self::closed_error())
        } else {
            Ok(())
        }
    }

    fn read_only_error() -> std::io::Error {
        crate::Error::ReadOnly.into_io_error()
    }

    pub(crate) fn ensure_writable(&self) -> std::io::Result<()> {
        self.ensure_open()?;
        if self.is_latched() {
            return Err(self.latched_error());
        }
        if self.is_read_only() {
            Err(Self::read_only_error())
        } else {
            Ok(())
        }
    }

    /// Latch a write-ahead-log failure the engine cannot recover from
    /// on its own, so every later write fails loud with the reason
    /// rather than appending after a tail nobody can account for.
    pub(crate) fn latch_wal_failure(&self, err: &std::io::Error) {
        self.latch(WriteLatch::Wal(err.kind(), err.to_string()));
    }

    /// Latch the engine read-only because a caller's `callback` panicked
    /// inside the commit's ordered step, which left that step's shared state
    /// unaccounted for. Every later write fails with
    /// [`crate::Error::CallbackPanicked`] until the database is reopened.
    pub(crate) fn latch_callback_panic(&self, callback: &'static str) {
        self.latch(WriteLatch::CallbackPanicked(callback));
    }

    /// Keep the first reason: it is the one that explains the rest. One
    /// compare-and-swap publishes it, and a later reason finds the cell
    /// full and is dropped.
    ///
    /// Every path that latches the engine comes through here, and a latched
    /// engine relieves no stall: each later write fails before it does any
    /// of the work that would. So the unit stopped writers wait on is landed
    /// (E35), and each of them, run again, is told the latch
    /// ([`Self::admit_write`]). The reason is published before the unit is
    /// taken: a writer that registers after the take reads it
    /// ([`Self::stall_wait`]), so none is missed.
    fn latch(&self, latch: WriteLatch) {
        let _ = self.write_latch.set(latch);
        self.stall_signal.land();
    }

    /// Whether a failure latched the engine against writes. One load.
    pub(crate) fn is_latched(&self) -> bool {
        self.write_latch.get().is_some()
    }

    /// Test-only: the compaction gate.
    #[cfg(test)]
    pub(crate) fn compaction_gate(&self) -> &CompactionGate {
        &self.compaction_gate
    }

    /// Test-only: the scheduler, to hand a worker a job of the test's.
    #[cfg(test)]
    pub(crate) fn compaction_scheduler(&self) -> &CompactionScheduler {
        &self.compaction
    }

    /// Test-only: no compaction pass holds a claim.
    #[cfg(test)]
    pub(crate) fn compaction_claims_idle(&self) -> bool {
        self.compaction_claims.is_idle()
    }

    fn latched_error(&self) -> std::io::Error {
        match self.write_latch.get() {
            Some(WriteLatch::Wal(kind, message)) => std::io::Error::new(
                *kind,
                format!("write-ahead log left in an unknown state: {message}"),
            ),
            Some(WriteLatch::CallbackPanicked(callback)) => crate::Error::CallbackPanicked {
                callback,
                latched: true,
            }
            .into_io_error(),
            None => std::io::Error::other("write-ahead log left in an unknown state"),
        }
    }

    /// The last sequence the commit pipeline decided: every commit decided
    /// so far took sequences at or below it. A database opened read-only
    /// decides none, so there it is the published horizon.
    pub(crate) fn last_decided_seq(&self) -> u64 {
        match self.pipeline_state() {
            Some(tip) => tip.after.next_seq.saturating_sub(1),
            None => self.visible_seq.visible(),
        }
    }

    fn validate_prefixed_key_size(&self, key: &[u8]) -> std::io::Result<()> {
        self.validate_user_key_len(key.len().saturating_sub(4))
    }

    /// [`Self::validate_prefixed_key_size`] for a key known by its length,
    /// without the column-family prefix.
    fn validate_user_key_len(&self, user_key_len: usize) -> std::io::Result<()> {
        if user_key_len <= self.options.max_key_size {
            return Ok(());
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "key length {} exceeds configured max_key_size {}",
                user_key_len, self.options.max_key_size
            ),
        ))
    }

    fn validate_value_size(&self, value: &[u8]) -> std::io::Result<()> {
        if value.len() <= self.options.max_value_size {
            return Ok(());
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "value length {} exceeds configured max_value_size {}",
                value.len(),
                self.options.max_value_size
            ),
        ))
    }

    /// `disable_wal` skips the record-length check, since the write
    /// produces no WAL record; key and value limits still apply. Returns the
    /// framed length of the batch's record.
    fn validate_ops_sizes(
        &self,
        ops: &[WriteBatchOp],
        disable_wal: bool,
    ) -> std::io::Result<usize> {
        let mut record = RecordLen::default();
        for op in ops {
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
                    self.validate_prefixed_key_size(key)?;
                    self.validate_value_size(operand)?;
                    record.merge(key, operand);
                }
            }
        }
        if !disable_wal {
            check_write_len(record.framed())?;
        }
        Ok(record.framed())
    }

    /// Borrow the engine's `Statistics` sink if one is configured.
    /// Returning `Option<&Statistics>` lets instrumented call
    /// sites branch on a single `is_some()` check rather than
    /// cloning an `Arc` per operation.
    pub(crate) fn statistics(&self) -> Option<&crate::statistics::Statistics> {
        self.options.statistics.as_deref()
    }

    /// Clone the statistics `Arc` for consumers (like the public
    /// `Iter`) that need to carry the handle across a lifetime
    /// boundary where a borrowed reference wouldn't reach.
    pub(crate) fn statistics_arc(&self) -> Option<Arc<crate::statistics::Statistics>> {
        self.options.statistics.clone()
    }

    /// The statistics `Arc` exactly as the caller configured it, for
    /// [`crate::Db::statistics`] to hand back.
    pub(crate) fn statistics_shared(&self) -> Option<&Arc<crate::statistics::Statistics>> {
        self.options.statistics.as_ref()
    }

    /// The table of units and jobs in flight, and the registry of open
    /// queues, for this database.
    pub(crate) fn io(&self) -> &io::IoRuntime {
        self.cache.io()
    }

    /// The calling thread's queue on this database, if it has one open
    /// ([`io::IoRuntime::current_queue`]). A queue is opened through the
    /// queue table, so a database without one has no queue for any thread,
    /// and asking does not make the table: a plain write that owes a step
    /// asks, and a database no queue was opened on would otherwise build the
    /// table (and free it at close) for that answer.
    pub(crate) fn current_queue(&self) -> Option<Arc<io::shared::QueueShared>> {
        self.cache.opened_io()?.current_queue()
    }

    /// Run `job`, which this thread claimed, whatever scope the caller is
    /// in, then drop it from the job table ([`io::IoRuntime::run_job`]).
    /// Only a queue lists a job there, through the table, so a database no
    /// queue was opened on has nothing to drop, and the unit a close runs
    /// does not build the table to find that out. Looked at after the run,
    /// so a queue opened meanwhile is not missed.
    pub(crate) fn run_job(&self, job: &Arc<io::job::Job>) {
        io::run_claimed(job);
        if let Some(io) = self.cache.opened_io() {
            io.forget_job(job);
        }
    }

    /// A new I/O queue for one thread, on this database's block cache.
    ///
    /// What the queue may owe is bounded by bytes: [`IO_QUEUE_BLOCKS`]
    /// blocks of the configured block size, so the bound follows the block
    /// size the reads are made of.
    pub(crate) fn io_queue(&self) -> crate::IoQueue {
        crate::IoQueue::open(
            Arc::clone(&self.cache),
            self.options.block_size.saturating_mul(IO_QUEUE_BLOCKS),
            Arc::clone(&self.env),
        )
    }

    /// Pin a copy of the snapshot `pin` holds, so compaction keeps every
    /// version it might need to see for as long as the copy lives. The copy
    /// shares the original's entry and is covered from the moment the
    /// original was. Balanced by [`Self::release_snapshot`].
    pub(crate) fn clone_snapshot_pin(&self, pin: &SnapshotPin) -> SnapshotPin {
        let copy = self.snapshot_registry.clone_pin(pin);
        if copy.is_registered()
            && let Some(s) = self.statistics()
        {
            s.add(crate::statistics::Ticker::SnapshotsRegistered, 1);
        }
        copy
    }

    /// Pin a snapshot at the current read horizon by announce, sample and
    /// confirm (see `snapshot_registry`), so a concurrent compaction either
    /// lists the pin or saw a horizon no newer than it. Returns the pinned
    /// sequence and the pin, released by [`Self::release_snapshot`] on
    /// whatever thread drops the handle. A closed database pins nothing.
    pub(crate) fn register_snapshot_at_horizon(&self) -> (u64, SnapshotPin) {
        if self.is_closed() {
            return (self.visible_seq.visible(), SnapshotPin::none());
        }
        let pinned = self.snapshot_registry.pin(&self.visible_seq);
        if let Some(s) = self.statistics() {
            s.add(crate::statistics::Ticker::SnapshotsRegistered, 1);
        }
        pinned
    }

    /// Release a snapshot pin taken by
    /// [`Self::register_snapshot_at_horizon`] or
    /// [`Self::clone_snapshot_pin`], exactly where it was announced.
    pub(crate) fn release_snapshot(&self, pin: SnapshotPin) {
        if !pin.is_registered() {
            return;
        }
        self.snapshot_registry.release(pin);
        if let Some(s) = self.statistics() {
            s.add(crate::statistics::Ticker::SnapshotsReleased, 1);
        }
    }

    /// The smallest live snapshot seq, or `u64::MAX` if no snapshot is
    /// currently pinned.
    pub(crate) fn oldest_live_seq(&self) -> u64 {
        self.snapshot_registry.oldest_live_seq()
    }

    /// Wait for every snapshot pin to be released, returning how many
    /// were still outstanding when `timeout` elapsed.
    pub(crate) fn wait_for_snapshots(&self, timeout: std::time::Duration) -> u64 {
        self.snapshot_registry.wait_until_drained(timeout)
    }

    /// The same wait for a task: over once no snapshot is pinned.
    pub(crate) fn snapshots_drained(&self) -> impl Future<Output = ()> + '_ {
        self.snapshot_registry.until_drained()
    }

    /// Construct a streaming iterator over the latest published read
    /// horizon. The view is loaded before the horizon is sampled, for
    /// the reason spelled out on [`Self::get_latest`].
    pub(crate) fn new_iter_latest(&self) -> iterator::RegolithIterator {
        let closed = self.is_closed();
        let view = self.view.load();
        let snapshot_seq = self.visible_seq.visible();
        self.iter_in_view(&view, snapshot_seq, closed)
    }

    /// Construct a streaming iterator rooted at a caller-pinned
    /// `snapshot_seq`. Captures one published view of the memtables and
    /// the version; no filesystem access happens here - file handles
    /// are already open in the pinned `Arc<LiveSst>`s carried by the
    /// version.
    pub(crate) fn new_iter_at(&self, snapshot_seq: u64) -> iterator::RegolithIterator {
        let closed = self.is_closed();
        let view = self.view.load();
        self.iter_in_view(&view, snapshot_seq, closed)
    }

    fn iter_in_view(
        &self,
        view: &ReadView,
        snapshot_seq: u64,
        closed: bool,
    ) -> iterator::RegolithIterator {
        let mut iter = iterator::RegolithIterator::new(
            Arc::clone(&view.active),
            view.frozen.clone(),
            Arc::clone(&view.version),
            Arc::clone(&self.cache),
            snapshot_seq,
            self.options.prefix_extractor.clone(),
            self.options.merge_operator.clone(),
        );
        if closed {
            iter.set_error(Self::closed_error());
        }
        iter
    }

    /// Point lookup at the latest published read horizon.
    ///
    /// The order of the first two statements is load-bearing: the view
    /// is loaded FIRST and the horizon sampled SECOND. Sampling the
    /// horizon first lets a compaction garbage-collect the newest
    /// version at or below it - compaction cuts its stripes at the
    /// registered snapshots, and a read without a `Snapshot` registers
    /// nothing there - after which the key reads back as absent. A
    /// version that compaction dropped was shadowed by a newer one
    /// that had already been flushed into the version being published,
    /// so a horizon sampled after the view is always at least that
    /// newer version's sequence and the read finds it.
    ///
    /// The read linearizes at the moment it loads the view: a write
    /// that lands between the load and the horizon sample is simply
    /// not part of this read. Read-your-writes still holds, because a
    /// write applies into the active memtable of the view current at
    /// that moment and every later view still exposes that memtable's
    /// data, as `active`, as `frozen`, or folded into the version.
    pub(crate) fn get_latest(&self, key: &[u8]) -> std::io::Result<Option<Vec<u8>>> {
        self.ensure_open()?;
        let view = self.view.load();
        let lk = LookupKey::from_prefixed(key, self.visible_seq.visible());
        if self.options.merge_operator.is_some() {
            return Ok(self.get_with_merge(&lk, &view)?.map(DbSlice::into_vec));
        }
        Ok(self
            .lookup_in_view(
                lk.prefixed_user_key(),
                lk.snapshot_seq(),
                &lk,
                Materialize::Value,
                &view,
            )?
            .and_then(|v| match v {
                PointValue::Value(value) => Some(value.into_vec()),
                PointValue::Length(_) => None,
            }))
    }

    /// Point lookup at a caller-pinned snapshot sequence, without copying
    /// the value out of the block or heap buffer it already lives in. The
    /// snapshot's registration in [`SnapshotRegistry`] is what keeps
    /// compaction from dropping the versions it needs, so the load order
    /// does not matter here; the view is still loaded once so every
    /// source the read walks agrees with every other.
    pub(crate) fn get_slice_at(
        &self,
        prefixed_key: &[u8],
        snapshot_seq: u64,
    ) -> std::io::Result<Option<DbSlice>> {
        let lk = LookupKey::from_prefixed(prefixed_key, snapshot_seq);
        self.get_slice(&lk)
    }

    /// Point lookup resolved against one already-loaded view.
    ///
    /// Walks sources newest→oldest (active memtable, frozen memtables
    /// newest first, L0 newest first, L1..Ln). At each source we check
    /// both the newest visible point entry and the newest visible
    /// covering range tombstone, carrying the largest RT seq forward so
    /// a range delete in a newer source can override a point entry in
    /// an older source. The first source yielding a decisive answer
    /// wins - a point entry with `seq > max_rt_so_far` gives its value;
    /// otherwise the range tombstone hides it.
    ///
    /// When a [`crate::MergeOperator`] is configured, the walk also
    /// collects any merge operands that sit on top of the terminator
    /// and calls the operator to collapse the chain into a final
    /// value at visibility time.
    #[cfg(test)]
    pub(crate) fn get(
        &self,
        prefixed_key: &[u8],
        snapshot_seq: u64,
    ) -> std::io::Result<Option<Vec<u8>>> {
        let lk = LookupKey::from_prefixed(prefixed_key, snapshot_seq);
        Ok(self.get_slice(&lk)?.map(DbSlice::into_vec))
    }

    /// Newest visible value for `key`, without copying it. The returned
    /// [`DbSlice`] borrows the block or heap buffer the value already
    /// lives in and keeps that owner alive.
    ///
    /// The read samples its sequence with the view it reads through
    /// already loaded. The order is load-bearing and it is the whole reason this is not
    /// `visible_seq.visible()` at the call site. Sampling the horizon
    /// first and loading the view afterwards leaves a window in
    /// between: `snapshot_seq()` registers nothing in the
    /// [`SnapshotRegistry`], so a compaction is free to drop the newest
    /// version at or below the sampled sequence, and the read then
    /// walks a view that no longer holds it and reports the key absent
    /// or an older value. That is a read travelling backwards.
    ///
    /// Loading the view first pins the sources, so every version the
    /// sampled horizon admits is still reachable through it.
    ///
    /// A caller reading at a *pinned* snapshot does not need this: the
    /// registration is what holds the versions, so the order does not
    /// matter there. See [`RegolithEngine::get_slice_at`].
    pub(crate) fn get_slice_latest(
        &self,
        cf_id: u32,
        key: &[u8],
    ) -> std::io::Result<Option<DbSlice>> {
        match self.lookup_latest_cf(cf_id, key, Materialize::Value)? {
            Some(PointValue::Value(value)) => Ok(Some(value)),
            Some(PointValue::Length(_)) => Err(std::io::Error::other(
                "point lookup produced a length where a value was requested",
            )),
            None => Ok(None),
        }
    }

    /// Length of the newest visible value for `key`, or `None`.
    pub(crate) fn get_size_latest(
        &self,
        cf_id: u32,
        key: &[u8],
        limit: Option<usize>,
    ) -> std::io::Result<Option<usize>> {
        Ok(self
            .lookup_latest_cf(cf_id, key, Materialize::LengthOnly(limit))?
            .map(|v| v.len()))
    }

    pub(crate) fn get_slice(&self, lk: &LookupKey) -> std::io::Result<Option<DbSlice>> {
        match self.lookup(lk, Materialize::Value)? {
            Some(PointValue::Value(value)) => Ok(Some(value)),
            Some(PointValue::Length(_)) => Err(std::io::Error::other(
                "point lookup produced a length where a value was requested",
            )),
            None => Ok(None),
        }
    }

    /// Length of the live value for `lk`, or `None` when there is none.
    ///
    /// Reads the same sources [`RegolithEngine::get_slice`] does and pays
    /// the same block reads, but never takes a reference on the block
    /// or buffer the value lives in.
    pub(crate) fn get_size(
        &self,
        lk: &LookupKey,
        limit: Option<usize>,
    ) -> std::io::Result<Option<usize>> {
        Ok(self
            .lookup(lk, Materialize::LengthOnly(limit))?
            .map(|v| v.len()))
    }

    /// Look one SSTable up, projecting the hit into whichever form the
    /// caller asked for. The two arms run the same bloom check, index
    /// search, block read and block scan; they differ only in whether
    /// the winning value is handed back as bytes or as a length.
    fn probe_file(
        &self,
        reader: &SsTableReader,
        lk: &LookupKey,
        materialize: Materialize,
    ) -> std::io::Result<LookupResult<PointValue>> {
        with_key_scratch(|buf| match materialize {
            Materialize::Value => Ok(reader
                .get(lk, buf, &self.cache)?
                .map_value(PointValue::Value)),
            Materialize::LengthOnly(limit) => Ok(reader
                .get_size(lk, buf, &self.cache, limit)?
                .map_value(PointValue::Length)),
        })
    }

    /// The single point-read source walk. `materialize` decides only
    /// what the winning entry is projected into, never which sources
    /// are consulted or how MVCC and range-tombstone precedence are
    /// resolved, so every `get`-shaped entry point agrees by
    /// construction.
    fn lookup(
        &self,
        lk: &LookupKey,
        materialize: Materialize,
    ) -> std::io::Result<Option<PointValue>> {
        self.ensure_open()?;
        let view = self.view.load();
        self.lookup_loaded(lk, materialize, &view)
    }

    /// [`Self::lookup`] against a view the caller already loaded, so a commit
    /// check reads a value the way a `get` does: a configured merge operator
    /// collapses the key's operands first.
    fn lookup_loaded(
        &self,
        lk: &LookupKey,
        materialize: Materialize,
        view: &ReadView,
    ) -> std::io::Result<Option<PointValue>> {
        if self.options.merge_operator.is_some() {
            Self::reject_guarded_merge(materialize)?;
            // A merge operator decides inside `full_merge` whether a
            // value exists at all, so a length-only request has to
            // collapse the chain exactly like a full read does.
            return Ok(self.get_with_merge(lk, view)?.map(PointValue::Value));
        }
        self.lookup_in_view(
            lk.prefixed_user_key(),
            lk.snapshot_seq(),
            lk,
            materialize,
            view,
        )
    }

    /// Read the newest visible value for `key`, sampling the horizon
    /// against the very view the read then walks.
    ///
    /// One view load, one sample, in that order. Sampling first and
    /// loading afterwards leaves a window where a compaction can drop
    /// the newest version at or below the sampled sequence, and the
    /// read reports the key absent or an older value: a read that
    /// travels backwards. Sampling under a *different* view load than
    /// the one the read walks has the same hole, only narrower, which
    /// is why the two happen here together rather than in a helper the
    /// caller composes.
    fn lookup_latest_cf(
        &self,
        cf_id: u32,
        key: &[u8],
        materialize: Materialize,
    ) -> std::io::Result<Option<PointValue>> {
        self.ensure_open()?;
        let view = self.view.load();
        // `LookupKey::new` writes the prefix into its own inline buffer,
        // so the read path stays allocation-free: prefixing into a
        // `Vec` here would put a malloc on every point read.
        let lk = LookupKey::new(cf_id, key, self.visible_seq.visible());
        let snapshot_seq = lk.snapshot_seq();
        if self.options.merge_operator.is_some() {
            Self::reject_guarded_merge(materialize)?;
            return Ok(self.get_with_merge(&lk, &view)?.map(PointValue::Value));
        }
        self.lookup_in_view(
            lk.prefixed_user_key(),
            snapshot_seq,
            &lk,
            materialize,
            &view,
        )
    }

    fn reject_guarded_merge(materialize: Materialize) -> std::io::Result<()> {
        if matches!(materialize, Materialize::LengthOnly(Some(_))) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "guarded size reads do not support a configured merge operator",
            ));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn lookup_in_view(
        &self,
        key: &[u8],
        snapshot_seq: u64,
        lk: &LookupKey,
        materialize: Materialize,
        view: &ReadView,
    ) -> std::io::Result<Option<PointValue>> {
        use crate::perf_context::{PerfTimer, PerfTimerField};

        // Memtable phase - timed via `PerfContext` at
        // `PerfLevel::EnableTime` so per-op breakdowns can
        // attribute time to "memtable vs SSTable". The first table the
        // walk reaches starts the SSTable phase: everything from there is
        // the "get_from_output_files" time.
        let mut timer = PerfTimer::new(PerfTimerField::GetFromMemtable);
        let mut in_tables = false;
        let walked = view.walk_newest_first(key, snapshot_seq, |source, max_rt_seq| {
            Ok(match source {
                Source::Memtable(mt) => match mt.get(lk) {
                    Some((seq, value)) => ControlFlow::Break(if seq > max_rt_seq {
                        value.map(|v| PointValue::of(v, materialize))
                    } else {
                        None
                    }),
                    None => ControlFlow::Continue(()),
                },
                // Readers are already open in the pinned `Version`, so no
                // filesystem access happens here - concurrent compaction
                // unlinking paths cannot break us.
                Source::Table(reader) => {
                    if !in_tables {
                        in_tables = true;
                        drop(std::mem::replace(
                            &mut timer,
                            PerfTimer::new(PerfTimerField::GetFromOutputFiles),
                        ));
                    }
                    match self.probe_file(reader, lk, materialize)? {
                        LookupResult::Found { seq, value } => {
                            ControlFlow::Break((seq > max_rt_seq).then_some(value))
                        }
                        LookupResult::FoundTombstone { .. } => ControlFlow::Break(None),
                        LookupResult::NotInTable => ControlFlow::Continue(()),
                    }
                }
            })
        })?;
        Ok(match walked {
            ControlFlow::Break(found) => found,
            ControlFlow::Continue(_) => None,
        })
    }

    /// Every visible entry for `lk`'s key in `view`, newest first, through
    /// the first terminator: a value, a deletion, or a covering range
    /// tombstone standing in as one.
    fn merge_chain_in_view(&self, lk: &LookupKey, view: &ReadView) -> std::io::Result<MergeChain> {
        use internal_key::VALUE_TYPE_DELETION;

        // `chain` records visible entries for `key` in newest-seq-
        // first order, stopping at (and including) the first
        // terminator (`VALUE` or `DELETION`). Range tombstones that
        // cover the key are treated as virtual deletion terminators.
        let mut chain = MergeChain::default();
        // The chain is the result; where the walk ended adds nothing to it.
        let _ = view.walk_newest_first(
            lk.prefixed_user_key(),
            lk.snapshot_seq(),
            |source, max_rt_seq| {
                let start = chain.len();
                let terminated = match source {
                    Source::Memtable(mt) => mt.collect_merge_chain(lk, &mut chain),
                    Source::Table(reader) => with_key_scratch(|buf| {
                        reader.collect_merge_chain(lk, buf, &self.cache, &mut chain)
                    })?,
                };
                // Entries are newest first, so a range tombstone hides the
                // first entry at or below it and every older one: it stands
                // in as the terminator.
                let hidden = chain
                    .iter()
                    .skip(start)
                    .position(|entry| entry.0 <= max_rt_seq);
                if let Some(hidden) = hidden {
                    chain.truncate(start + hidden);
                    chain.push((max_rt_seq, VALUE_TYPE_DELETION, DbSlice::empty()));
                    return Ok(ControlFlow::Break(()));
                }
                Ok(if terminated {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                })
            },
        )?;
        Ok(chain)
    }

    /// Merge-aware point lookup. Walks every source newest→oldest
    /// collecting merge operands and any base value / deletion into
    /// a single chain, honors range-tombstone coverage, and calls
    /// [`crate::MergeOperator::full_merge`] at the end to materialize
    /// the final value.
    ///
    /// Resolved against the caller's `view`, never one loaded here: the
    /// caller chose `lk`'s sequence against that view, and a later one
    /// may have had the version that sequence admits compacted away.
    ///
    /// Callers are responsible for having checked that
    /// `self.options.merge_operator.is_some()`; this helper asserts
    /// internally.
    fn get_with_merge(&self, lk: &LookupKey, view: &ReadView) -> std::io::Result<Option<DbSlice>> {
        use internal_key::{VALUE_TYPE_DELETION, VALUE_TYPE_MERGE, VALUE_TYPE_VALUE};

        let merge_op = self
            .options
            .merge_operator
            .as_ref()
            .expect("get_with_merge called without a merge operator");

        let key = lk.prefixed_user_key();
        let mut chain = self.merge_chain_in_view(lk, view)?;
        // Nearly every key's newest visible version is its value or its
        // deletion, with no operand above it: that version is the answer.
        if chain.len() <= 1
            && chain
                .last()
                .is_none_or(|version| version.1 != VALUE_TYPE_MERGE)
        {
            return Ok(match chain.pop() {
                Some((_, VALUE_TYPE_VALUE, value)) => Some(value),
                _ => None,
            });
        }
        // Materialize the chain. `chain` is newest-first; the last
        // entry (if any) is either a real VALUE / DELETION terminator
        // or (if !terminated) the oldest visible merge operand.
        let (base_slice, has_terminator) = match chain.last() {
            Some((_, VALUE_TYPE_VALUE, value)) => (Some(value.as_slice()), true),
            Some((_, VALUE_TYPE_DELETION, _)) => (None, true),
            _ => (None, false),
        };

        // Build operands in oldest-first order: walk the merge part
        // of the chain (everything except the terminator slot, if
        // one is present) in reverse.
        let merge_end = if has_terminator {
            chain.len() - 1
        } else {
            chain.len()
        };
        let mut operands_owned: Vec<&[u8]> = Vec::with_capacity(merge_end);
        operands_owned.extend(chain.iter().take(merge_end).map(|entry| {
            debug_assert_eq!(entry.1, VALUE_TYPE_MERGE);
            entry.2.as_slice()
        }));
        operands_owned.reverse();

        if operands_owned.is_empty() {
            // No merges at all - the chain is just a plain
            // Value / Deletion / nothing. Return the base directly.
            drop(operands_owned);
            return Ok(match chain.pop() {
                Some((_, vt, value)) if vt == VALUE_TYPE_VALUE => Some(value),
                _ => None,
            });
        }

        merge_call::apply_merge(merge_op.as_ref(), key, base_slice, &operands_owned)
            .map(|merged| Some(DbSlice::from_vec(merged)))
    }

    /// Batched point lookup at a given snapshot. Returns one `Option<Vec<u8>>`
    /// per input key in the same order as `keys`. Duplicate keys in the
    /// input produce duplicate results.
    ///
    /// The batch amortizes per-call overhead - one loaded view, one
    /// sequence, one logical walk of the source hierarchy - and
    /// short-circuits once every key has been resolved. All keys are read
    /// at that one sequence from that one view, with or without a merge
    /// operator, so a concurrent writer cannot make two keys disagree.
    pub(crate) fn multi_get_latest(&self, keys: &[&[u8]]) -> std::io::Result<Vec<Option<Vec<u8>>>> {
        self.ensure_open()?;
        let view = self.view.load();
        let snapshot_seq = self.visible_seq.visible();
        self.multi_get_in_view(keys, snapshot_seq, &view)
    }

    /// Batched point lookup at a caller-pinned snapshot sequence. See
    /// [`Self::get_slice_at`] for why the load order is free here.
    pub(crate) fn multi_get_at(
        &self,
        keys: &[&[u8]],
        snapshot_seq: u64,
    ) -> std::io::Result<Vec<Option<Vec<u8>>>> {
        self.ensure_open()?;
        let view = self.view.load();
        self.multi_get_in_view(keys, snapshot_seq, &view)
    }

    /// Batched point lookup resolved against one already-loaded view.
    fn multi_get_in_view(
        &self,
        keys: &[&[u8]],
        snapshot_seq: u64,
        view: &ReadView,
    ) -> std::io::Result<Vec<Option<Vec<u8>>>> {
        // When a merge operator is configured, fall back to per-key
        // resolution - the batched walk's short-circuiting logic
        // doesn't compose cleanly with merge-chain collection, and
        // merges are rare enough that the cost difference isn't
        // worth a specialized batched path.
        if self.options.merge_operator.is_some() {
            let mut out = Vec::with_capacity(keys.len());
            let mut lk = LookupKey::from_prefixed(&[], snapshot_seq);
            for key in keys {
                lk.reset_prefixed(key, snapshot_seq);
                out.push(self.get_with_merge(&lk, view)?.map(DbSlice::into_vec));
            }
            return Ok(out);
        }

        let mut results: Vec<Option<Vec<u8>>> = vec![None; keys.len()];
        if keys.is_empty() {
            return Ok(results);
        }
        let mut entries = grouped_multi_get_entries(keys);
        let mut unresolved = entries.len();
        // One encoder, re-pointed per key, instead of one per key per
        // source.
        let mut lk = LookupKey::from_prefixed(&[], snapshot_seq);

        // 1. Active memtable.
        {
            let mt = &view.active;
            for entry in &mut entries {
                if entry.resolved {
                    continue;
                }
                let rt = mt.covering_range_tombstone_seq(&entry.key, snapshot_seq);
                if rt > entry.max_rt {
                    entry.max_rt = rt;
                }
                lk.reset_prefixed(&entry.key, snapshot_seq);
                if let Some((pseq, popt)) = mt.get(&lk) {
                    let value = resolve_multi_get_value(pseq, popt, entry.max_rt);
                    set_multi_get_result(entry, &mut results, value);
                    unresolved -= 1;
                }
            }
        }
        if unresolved == 0 {
            return Ok(results);
        }

        // 2. Frozen memtables, newest first.
        {
            let frozen = &view.frozen;
            for mt in frozen.iter().rev() {
                for entry in &mut entries {
                    if entry.resolved {
                        continue;
                    }
                    let rt = mt.covering_range_tombstone_seq(&entry.key, snapshot_seq);
                    if rt > entry.max_rt {
                        entry.max_rt = rt;
                    }
                    lk.reset_prefixed(&entry.key, snapshot_seq);
                    if let Some((pseq, popt)) = mt.get(&lk) {
                        let value = resolve_multi_get_value(pseq, popt, entry.max_rt);
                        set_multi_get_result(entry, &mut results, value);
                        unresolved -= 1;
                    }
                }
                if unresolved == 0 {
                    return Ok(results);
                }
            }
        }

        let version = &view.version;

        // 3. L0 SSTables, newest first.
        for file in version.levels[0].iter().rev() {
            for entry in &mut entries {
                if entry.resolved || !file_covers_key(file, &entry.key) {
                    continue;
                }
                let rt = file
                    .reader
                    .covering_range_tombstone_seq(&entry.key, snapshot_seq);
                if rt > entry.max_rt {
                    entry.max_rt = rt;
                }
                lk.reset_prefixed(&entry.key, snapshot_seq);
                match with_key_scratch(|buf| file.reader.get(&lk, buf, &self.cache))? {
                    LookupResult::Found { seq, value } => {
                        let value = resolve_multi_get_value(seq, Some(value), entry.max_rt);
                        set_multi_get_result(entry, &mut results, value);
                        unresolved -= 1;
                    }
                    LookupResult::FoundTombstone { .. } => {
                        set_multi_get_result(entry, &mut results, None);
                        unresolved -= 1;
                    }
                    LookupResult::NotInTable => {}
                }
            }
            if unresolved == 0 {
                return Ok(results);
            }
        }

        // 4. L1..Ln: a level is one sorted run, so each key finds the few
        //    tables covering it by binary search rather than being matched
        //    against every table, and a table whose range covers the key
        //    but holds no point entry for it, as a tombstone-only table
        //    does, costs no more than any other. As a single-key read
        //    does, a key sees the tombstones of all its covering tables in
        //    a level before any of them is probed.
        for files in version.levels.iter().skip(1) {
            if files.is_empty() {
                continue;
            }
            for entry in &mut entries {
                if entry.resolved {
                    continue;
                }
                let run = manifest::covering(files, &entry.key);
                for file in run {
                    let rt = file
                        .reader
                        .covering_range_tombstone_seq(&entry.key, snapshot_seq);
                    if rt > entry.max_rt {
                        entry.max_rt = rt;
                    }
                }
                for file in run.iter().filter(|file| file.meta.num_entries > 0) {
                    lk.reset_prefixed(&entry.key, snapshot_seq);
                    match with_key_scratch(|buf| file.reader.get(&lk, buf, &self.cache))? {
                        LookupResult::Found { seq, value } => {
                            let value = resolve_multi_get_value(seq, Some(value), entry.max_rt);
                            set_multi_get_result(entry, &mut results, value);
                            unresolved -= 1;
                            break;
                        }
                        LookupResult::FoundTombstone { .. } => {
                            set_multi_get_result(entry, &mut results, None);
                            unresolved -= 1;
                            break;
                        }
                        LookupResult::NotInTable => {}
                    }
                }
                if unresolved == 0 {
                    return Ok(results);
                }
            }
        }

        Ok(results)
    }

    /// The version the last applied edit published.
    ///
    /// One load of the published view. It is the version the version set
    /// holds, because every section of the [`VersionStore`] publishes what
    /// it installed before it ends.
    fn published_version(&self) -> Arc<manifest::Version> {
        Arc::clone(&self.view.load().version)
    }

    /// Classify the current state against the configured stall
    /// thresholds; see [`stall_state::classify`].
    fn stall_state(&self) -> Option<(&'static str, bool)> {
        self.stall_signal.classify()
    }

    /// Refresh the cached stall level from the current L0 / memtable /
    /// pending-bytes state, and land the unit stopped writers wait on once
    /// writes may go on. Called after any event that changes those
    /// counters: a memtable rotation, a flush, a compaction pass, an ingest.
    pub(crate) fn refresh_stall_level(&self) {
        self.stall_signal.refresh();
    }

    /// The write stall writers wait on.
    #[cfg(test)]
    pub(crate) fn stall_signal(&self) -> &StallSignal {
        &self.stall_signal
    }

    /// Upper bound on background steps one stopped write runs on its own
    /// thread, with no worker, inline compaction on and no queue to leave
    /// them on, before it gives up with [`crate::Error::Busy`] (E16).
    ///
    /// The bound exists to cap write latency, not to cap iterations for
    /// their own sake: one L0 -> L1 pass normally drains the entire L0
    /// pool, so a caller that needs more than this many passes is not
    /// going to be rescued by another one on this thread.
    const MAX_INLINE_PASSES: usize = 32;

    /// How many times in a row a step may report nothing to do while a
    /// stop threshold is still active, before the stop counts as one no
    /// step relieves and the write fails with [`crate::Error::Busy`].
    ///
    /// Not zero, because another thread can relieve the stall between
    /// this thread's threshold check and its pick, and that races to
    /// an `Idle` that means the opposite of stuck. Small, because a
    /// picker that declines twice running while the threshold holds is
    /// declining for a structural reason (see
    /// [`crate::Options::level0_stop_writes_trigger`]) that another
    /// pass will not change.
    const MAX_IDLE_PASSES: usize = 2;

    /// Run at most one pending compaction job on the calling thread and
    /// report whether any work was done.
    ///
    /// Holds the engine-wide compaction gate shared, exactly as a
    /// background worker does, and uses the engine's claims, so a pass here
    /// and a worker never pick overlapping inputs.
    ///
    /// It never waits for another thread: when a `compact_range`, an
    /// ingest, a checkpoint, a `drop_all` or a backup holds the gate alone,
    /// or is owed it, this reports `Contended` at once. The thread asking
    /// may be the one that holds it, and a caller's call waits for no other
    /// thread (plan 4.10).
    pub(crate) fn run_one_compaction_pass(&self) -> std::io::Result<CompactionOutcome> {
        // A call that waits always gets an outcome.
        self.compaction_pass(commit::Help::Wait)
            .map(|outcome| outcome.unwrap_or(CompactionOutcome::Contended))
    }

    /// [`Self::run_one_compaction_pass`] for a caller that says what it does
    /// about waits. A pass hands sections to the version store's step (a
    /// file id per output, the install): a call that waits lets them wait
    /// for the step's work under way, and a queue's poll takes the step
    /// before the pass begins. `None` when the poll parked instead, with
    /// its waker registered on the step and nothing held.
    pub(crate) fn compaction_pass(
        &self,
        help: commit::Help<'_>,
    ) -> std::io::Result<Option<CompactionOutcome>> {
        self.ensure_writable()?;
        let outcome = {
            let Some(_pass) = self.compaction_gate.try_shared() else {
                return Ok(Some(CompactionOutcome::Contended));
            };
            let Some(kept) = self.keep(commit::Needs::VERSIONS, help) else {
                return Ok(None);
            };
            self.ensure_writable()?;
            let outcome = compaction::pick_and_run_compaction(
                &self.versions,
                &self.sst_dir,
                &self.cache,
                &self.options.to_compaction_options(),
                &self.snapshot_registry,
                &self.compaction_claims,
                // A caller's own pass: nothing a census step stops for.
                &|| false,
            );
            self.unkeep(kept);
            outcome.inspect_err(|err| self.compaction_failed(err))?
        };
        // As a worker does after each pass: writers a stop stopped are told
        // once the pass relieved it.
        self.refresh_stall_level();
        Ok(Some(outcome))
    }

    /// A compaction this thread ran failed with `err`: a step a write left
    /// owing, one relieving a stall, [`crate::Db::compact_step`] or a
    /// `compact_range`. Counted among the background errors and told to the
    /// listeners as a worker's failing pass is
    /// ([`compaction::tell_failure`]).
    ///
    /// Not recorded as the failure a stopped writer is told
    /// (`BackgroundHealth::record_failure`): with no worker, a writer told
    /// it would stop running the steps that are the only relief.
    fn compaction_failed(&self, err: &std::io::Error) {
        self.background_health.count_failure();
        // A listener's panic, contained in a background step, was logged
        // where it was caught: the caller gets the compaction's failure.
        let _told = compaction::tell_failure(&self.options.listeners, err);
    }

    /// Write every memtable currently held in memory out to level-0
    /// SSTables on the calling thread. A no-op when nothing is held.
    ///
    /// Shares [`Self::flush_all_memtables`] with the close path, so an
    /// explicit flush and a close write memtables out the same way.
    pub(crate) fn flush_active_memtable(&self) -> std::io::Result<()> {
        self.ensure_writable()?;
        let drained = self.drain_memtables(ActiveFlush::Always);
        // A failed drain too: a stopped writer its flush held off looks
        // again (`StallSignal`, relief handed back).
        self.refresh_stall_level();
        drained
    }

    /// The write-stall admission every write passes before it applies
    /// anything (D23). It never waits:
    ///
    /// - no stall: `Ok`;
    /// - a slowdown: `Ok`, or [`crate::Error::Busy`] under `no_slowdown`.
    ///   With no worker and inline compaction on, the write owes one step of
    ///   background work, run once it committed;
    /// - a stop: [`crate::Error::WouldBlock`] with a
    ///   [`crate::StallWait`] completed on this thread's queue when the
    ///   stall clears, or [`crate::Error::Busy`] under `no_slowdown`, or the
    ///   failure of the background work that would relieve it when that
    ///   work keeps failing, since waiting would not end. With no worker
    ///   and inline compaction on, the stop's step is left on this thread's
    ///   queue, or, with no queue, run inline, bounded (E16), and the write
    ///   then goes on once that relieved the stop.
    // Not closed and no cached stall is the overwhelming common case, so
    // this fast check stays inlined at every call site rather than a call
    // instruction: two byte loads and two branches over state the engine
    // already keeps resident (`close_state`, `cached_stall_level`), which
    // also skips the `stall_state()` call that loads the read view and
    // walks L0. Anything past that is cold and kept out of line instead of
    // bloating every caller that never stalls.
    #[inline]
    pub(crate) fn admit_write(&self, no_slowdown: bool) -> Result<(), crate::Error> {
        if !self.is_closed() && self.cached_stall_level.load(Ordering::Acquire) == 0 {
            return Ok(());
        }
        self.admit_write_slow(no_slowdown)
    }

    #[cold]
    #[inline(never)]
    fn admit_write_slow(&self, no_slowdown: bool) -> Result<(), crate::Error> {
        match self.stall_admission(no_slowdown)? {
            None => Ok(()),
            Some(reason) => self.stopped(reason),
        }
    }

    /// What the stall thresholds say to a write that arrives now: `None`
    /// when it goes on, `Some` with the reason when a stop holds it. Every
    /// writer is classified here, whatever it then does about a stop: a
    /// plain write takes the stall's wait ([`Self::stopped`]), a commit
    /// that blocks waits the stop out ([`Self::admit_commit`]), a
    /// `commit_nowait` commit is left behind the stall's unit
    /// ([`Self::admit_commit_nowait`]).
    fn stall_admission(&self, no_slowdown: bool) -> Result<Option<&'static str>, crate::Error> {
        if self.is_closed() {
            return Err(crate::Error::Closed);
        }
        // A latched engine takes no write and relieves no stall, so the
        // answer is the latch: what the write is told once it is admitted,
        // told here instead of a wait that would not end (E35).
        if self.is_latched() {
            return Err(self.latched_error().into());
        }
        match self.stall_state() {
            None => {
                // Stall cleared: update the cache so subsequent writers take
                // the fast path.
                self.cached_stall_level.store(0, Ordering::Release);
                self.stall_steps_idle.store(0, Ordering::Release);
                Ok(None)
            }
            Some((reason, false)) => {
                if no_slowdown {
                    return Err(crate::Error::Busy(reason));
                }
                self.stall_steps_idle.store(0, Ordering::Release);
                // No delay: a sleep relieves nothing. With no worker the
                // writer pays one step instead, which is what makes the write
                // rate track compaction on a host with no worker.
                if !self.has_worker && self.inline_compaction {
                    self.owe(background_step::OWED_STEP);
                }
                Ok(None)
            }
            Some((reason, true)) => {
                if no_slowdown {
                    return Err(crate::Error::Busy(reason));
                }
                Ok(Some(reason))
            }
        }
    }

    /// What says a stop for `reason` will not clear, so that nothing waits
    /// for it: the work that relieves it keeps failing (E36), or, where
    /// stopped writers relieve the stop themselves, their steps found
    /// nothing to do too many times in a row.
    fn stop_holds_for_good(&self, reason: &'static str) -> Result<(), crate::Error> {
        if let Some(failure) = self.failing_relief(reason) {
            return Err(failure);
        }
        if self.writers_relieve()
            && self.stall_steps_idle.load(Ordering::Acquire) > Self::MAX_IDLE_PASSES
        {
            return Err(crate::Error::Busy(reason));
        }
        Ok(())
    }

    /// Stopped writers relieve the stop themselves: no worker, and inline
    /// compaction on.
    fn writers_relieve(&self) -> bool {
        !self.has_worker && self.inline_compaction
    }

    /// A write a stop threshold stopped: the wait for the stop to clear, or
    /// the error that says waiting would not end.
    fn stopped(&self, reason: &'static str) -> Result<(), crate::Error> {
        self.stop_holds_for_good(reason)?;
        let queue = self.current_queue();
        if self.writers_relieve() {
            match &queue {
                None => return self.relieve_inline(Self::MAX_INLINE_PASSES),
                Some(queue) => self.leave_stall_step(queue),
            }
        }
        Err(self.stall_wait(queue, reason, None))
    }

    /// The write-stall admission of a commit that waits: a blocking
    /// `commit`, and a `commit_nowait` commit whose queue is gone. A
    /// transaction cannot be committed a second time, so its commit is never
    /// handed the stall's wait: this returns once no stop holds, having
    /// waited the stop out on this thread, or with what says the stop will
    /// not clear (the database closed, the engine latched, the relief keeps
    /// failing, no step relieves it). A waiting commit is told those as a
    /// write that arrives then is (E35, E36): whatever records them lands
    /// the stall's unit, and the commit, woken, is classified again.
    ///
    /// With a worker the commit sleeps until the stall's unit lands. With
    /// none it runs the steps that relieve the stop here, as many as the
    /// stop needs and no more, queue or no queue and whatever
    /// [`crate::Options::inline_compaction`] says: it chose to wait, it
    /// cannot be made again, and with no worker nobody else is certain to
    /// compact (on a target with one thread nobody can). The option still
    /// governs plain writes, whose caller holds the data.
    pub(crate) fn admit_commit(&self) -> Result<(), crate::Error> {
        if !self.is_closed() && self.cached_stall_level.load(Ordering::Acquire) == 0 {
            return Ok(());
        }
        while let Some(reason) = self.stall_admission(false)? {
            self.stop_holds_for_good(reason)?;
            let stopped = if !self.has_worker {
                // No bound on the passes: each makes progress, and a stop no
                // pass relieves ends this with `Busy`.
                match self.relieve_inline(usize::MAX) {
                    Ok(()) => continue,
                    Err(stopped) => stopped,
                }
            } else {
                self.stall_wait(None, reason, None)
            };
            match stopped {
                // The step found another thread's work in its way (a pass,
                // a flush, a job that holds the gate): the commit waits for
                // that work. A target with one thread has no such work, and
                // no wait to make.
                #[cfg(not(target_arch = "wasm32"))]
                crate::Error::WouldBlock(crate::WouldBlock::Stall(wait)) => wait.wait(),
                told => return Err(told),
            }
        }
        Ok(())
    }

    /// The write-stall admission of a `commit_nowait` commit on its queue's
    /// thread: `None` when it goes on, and under a stop the stall's unit,
    /// which the commit is left behind on that queue; the queue's owner
    /// drives the commit again when the unit lands (`commit::resume`), so
    /// the commit is classified again then and its ticket completes once
    /// the stop is relieved. Where stopped writers relieve the stop
    /// themselves, the step that does is left on the queue too. An error is
    /// what says the stop will not clear, as for [`Self::admit_commit`].
    pub(crate) fn admit_commit_nowait(&self) -> Result<Option<Arc<io::job::Job>>, crate::Error> {
        if !self.is_closed() && self.cached_stall_level.load(Ordering::Acquire) == 0 {
            return Ok(None);
        }
        let Some(reason) = self.stall_admission(false)? else {
            return Ok(None);
        };
        self.stop_holds_for_good(reason)?;
        if self.writers_relieve()
            && let Some(queue) = self.current_queue()
        {
            self.leave_stall_step(&queue);
        }
        commit::hook::at(commit::hook::Stage::Stall);
        let unit = self.stall_signal.unit();
        // Looked at again only now that the unit is there for a clearer to
        // land, as a wait is ([`StallSignal::wait`]): a stop that cleared,
        // an engine that latched or a relief that failed since the
        // classification above landed nothing of this commit's, so it is
        // landed here. The commit, left behind a unit that has landed, is
        // driven again at its queue's next poll.
        self.stall_signal.look_again();
        let _ = self.stop_will_not_clear();
        Ok(Some(unit))
    }

    /// The failure a write stopped for `reason` is told while the work that
    /// relieves that stop is failing: its last attempt failed and none has
    /// succeeded since ([`BackgroundHealth::failing`]). A wait would not end
    /// while it does, so the failure is the answer, OS error included. The
    /// one answer for a write that is stopped now and for one that was
    /// already waiting ([`Self::stall_wait`]).
    fn failing_relief(&self, reason: &'static str) -> Option<crate::Error> {
        let relief = if reason == STOP_TOO_MANY_MEMTABLES {
            Job::Flush
        } else {
            Job::Compaction
        };
        self.background_health
            .failing(relief)
            .map(|failure| failure.to_error())
    }

    /// What a stopped write returns: the wait for the stall `reason` to
    /// clear, or what says the wait would not end: the latch when the engine
    /// latched meanwhile (E35), the relief's failure when it failed
    /// meanwhile (E36). `queue` and `seen` are [`StallSignal::wait`]'s.
    fn stall_wait(
        &self,
        queue: Option<Arc<io::shared::QueueShared>>,
        reason: &'static str,
        seen: Option<usize>,
    ) -> crate::Error {
        let wait = self
            .stall_signal
            .wait(self.io(), queue, reason, seen)
            .on(self.me.clone());
        match self.stop_will_not_clear() {
            Some(told) => told,
            None => crate::Error::WouldBlock(crate::WouldBlock::Stall(wait)),
        }
    }

    /// Read once a wait, or a commit, is recorded on the stall's unit: what
    /// says the stop it waits for will not clear, with the unit landed for
    /// everyone recorded on it beside this one.
    fn stop_will_not_clear(&self) -> Option<crate::Error> {
        // Read only now that the wait is recorded. A latch publishes its
        // reason before it lands the unit, so either it found this wait
        // there or the reason is read here; the unit is landed again for the
        // writers that registered on it beside this one.
        if self.is_latched() {
            self.stall_signal.land();
            return Some(self.latched_error().into());
        }
        // The same for a relief that failed: the failure is stored before
        // the unit is landed (`BackgroundHealth::record_failure`). Read as
        // a write that arrives now reads it, only while a stop still holds:
        // one that cleared meanwhile landed the wait above, and the write,
        // run again, goes in.
        if let Some((reason, true)) = self.stall_state()
            && let Some(failure) = self.failing_relief(reason)
        {
            self.stall_signal.land();
            return Some(failure);
        }
        None
    }

    /// Relieve a stop on this thread, on a database with no worker and
    /// inline compaction on: steps until the stop clears, then the write
    /// goes on. A plain write from a thread with no queue runs at most
    /// `max_passes` of them (E16) and fails with [`crate::Error::Busy`]
    /// past that; a commit that waits runs as many as it takes. A step
    /// another thread holds the inputs of hands the writer a wait instead; a
    /// stop no step relieves fails the write with [`crate::Error::Busy`].
    /// The time spent is charged to [`crate::Ticker::WriteStallMicros`].
    fn relieve_inline(&self, max_passes: usize) -> Result<(), crate::Error> {
        let start = self.env.now_micros();
        let mut passes = 0usize;
        let mut idle = 0usize;
        let relieved = loop {
            if self.is_closed() {
                break Err(crate::Error::Closed);
            }
            let Some((reason, true)) = self.stall_state() else {
                break Ok(());
            };
            let seen = self.stall_signal.progress();
            // This thread has no queue and chose to wait: its step waits.
            match self
                .run_one_background_step(commit::Help::Wait)?
                .unwrap_or(CompactionOutcome::Contended)
            {
                CompactionOutcome::DidWork => {
                    idle = 0;
                    passes += 1;
                    if passes > max_passes {
                        break Err(crate::Error::Busy(reason));
                    }
                }
                // Another thread holds the inputs: the write waits until that
                // thread's work is done and looks again, as any stopped write
                // does (`StallSignal`, relief handed back).
                CompactionOutcome::Contended => {
                    break Err(self.stall_wait(None, reason, Some(seen)));
                }
                // Idle is ambiguous under concurrency: "nothing to compact
                // right now" is what a wedged engine looks like and also what
                // an engine another thread just relieved looks like, so the
                // thresholds are checked again from the top, a bounded number
                // of times.
                CompactionOutcome::Idle => {
                    idle += 1;
                    if idle > Self::MAX_IDLE_PASSES {
                        break Err(crate::Error::Busy(reason));
                    }
                }
            }
        };
        // No monotonic clock means nothing was measured: the ticker stays
        // untouched rather than gaining a fabricated zero.
        if let (Some(micros), Some(s)) = (self.elapsed_micros(start), self.statistics()) {
            s.add(crate::statistics::Ticker::WriteStallMicros, micros);
        }
        relieved
    }

    pub(crate) fn commit_with_conflict_check(
        &self,
        checks: ValidationSet,
        point_ops: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
        range_deletes: Vec<(Vec<u8>, Vec<u8>)>,
        merges: Vec<(Vec<u8>, Vec<u8>)>,
        appends: Vec<PendingAppend>,
        durability: DurabilityMode,
    ) -> std::io::Result<CommitOutcome> {
        self.commit_optimistic(
            checks,
            point_ops,
            range_deletes,
            merges,
            appends,
            durability,
        )
    }

    /// Whether this commit's write for `key` would store exactly what `key`
    /// already holds.
    ///
    /// Such a write is not a conflict. Take two transactions that both write
    /// `K = v`, with the first committing: under the serial order "first, then
    /// second" the second writes `K = v` and the final state is `K = v`, which
    /// is what letting both commit produces. The schedule has a serial
    /// equivalent, so refusing it rejects a correct history.
    ///
    /// The comparison is against the *committed* value, not against what the
    /// transaction observed, and byte equality is the whole test: "the key
    /// exists" is not enough. A delete is equal only to an absent key.
    ///
    /// Only ever called on a key that was about to abort the commit, so the
    /// read it costs is paid on the failure path and never on a clean commit.
    fn write_matches_committed(
        &self,
        key: &[u8],
        ops: &[WriteBatchOp],
        view: &ReadView,
        newest: WriteKind,
    ) -> std::io::Result<bool> {
        // Unresolved merge operands on top mean a point read replays them, so
        // what it returns is not what this key stores. Beneath a newest value
        // or deletion, any operands are already folded and the read is exact.
        if newest == WriteKind::Merge {
            return Ok(false);
        }

        let mut intended: Option<Option<&[u8]>> = None;
        for op in ops {
            match op {
                WriteBatchOp::Put { key: op_key, value } if op_key == key => {
                    intended = Some(Some(value.as_slice()));
                }
                WriteBatchOp::Delete { key: op_key } if op_key == key => {
                    intended = Some(None);
                }
                // A merge is not idempotent: two `+1` operands are not one
                // `+1`, so a merge on this key always conflicts.
                WriteBatchOp::Merge { key: op_key, .. } if op_key == key => return Ok(false),
                // A range delete makes the key's final value a function of the
                // range as well as the point op, which this comparison does
                // not model.
                WriteBatchOp::DeleteRange { start, end }
                    if start.as_slice() <= key && key < end.as_slice() =>
                {
                    return Ok(false);
                }
                _ => {}
            }
        }

        // No write for this key means it is here because the transaction read
        // it, and a read that no longer holds has to abort.
        let Some(intended) = intended else {
            return Ok(false);
        };

        self.committed_equals(key, intended, view)
    }

    /// [`Self::committed_equals`] for a key whose newest version is `newest`:
    /// an operand on top means a read replays it, so what the key stores is
    /// not what a lookup of its value returns, and the write never matches.
    fn committed_equals_after(
        &self,
        key: &[u8],
        intended: Option<&[u8]>,
        view: &ReadView,
        newest: WriteKind,
    ) -> std::io::Result<bool> {
        if newest == WriteKind::Merge {
            return Ok(false);
        }
        self.committed_equals(key, intended, view)
    }

    /// Whether `key` holds exactly `intended` (`None`: nothing) in `view`,
    /// whose newest version is a value or a deletion, never an operand.
    fn committed_equals(
        &self,
        key: &[u8],
        intended: Option<&[u8]>,
        view: &ReadView,
    ) -> std::io::Result<bool> {
        let snap = u64::MAX;
        let lk = LookupKey::from_prefixed(key, snap);
        let committed = self.lookup_in_view(key, snap, &lk, Materialize::Value, view)?;
        let committed = match committed {
            Some(PointValue::Value(slice)) => Some(slice),
            Some(PointValue::Length(_)) => return Ok(false),
            None => None,
        };

        Ok(match (intended, committed.as_deref()) {
            (Some(intended), Some(committed)) => intended == committed,
            // A delete of a key that is already absent stores the same thing.
            (None, None) => true,
            _ => false,
        })
    }

    /// Return the sequence number and kind of the newest write that
    /// touched `key` across every source, or `None` if nothing ever wrote
    /// it. A covering range tombstone that outranks every point entry
    /// reports as a range delete.
    /// Used by transaction commit to detect conflicts, so it counts
    /// every kind of write: point entries, point tombstones, merge
    /// operands, and range tombstones that cover the key. The caller
    /// only needs to know "was this key written to again?".
    ///
    /// Sources are visited newest-first and range-tombstone coverage
    /// is accumulated on the way down, mirroring the read path: a
    /// tombstone in a newer source outranks a point entry found in an
    /// older one.
    #[inline]
    fn latest_version_in_view(
        &self,
        key: &[u8],
        view: &ReadView,
    ) -> std::io::Result<Option<(u64, WriteKind)>> {
        self.latest_version_upto(key, u64::MAX, view)
    }

    /// [`Self::latest_version_in_view`] among the writes at or below `ceil`:
    /// what the key held for a reader at `ceil`, for a check that takes no
    /// slot of the commit ring (`commit/write_free.rs`).
    fn latest_version_upto(
        &self,
        key: &[u8],
        ceil: u64,
        view: &ReadView,
    ) -> std::io::Result<Option<(u64, WriteKind)>> {
        self.latest_version_bounded(key, ceil, None, view)
    }

    /// Only versions newer than `floor` can invalidate that observation.
    /// The table ceiling and the sources must come from the same view.
    fn latest_version_after(
        &self,
        key: &[u8],
        floor: u64,
        view: &ReadView,
    ) -> std::io::Result<Option<(u64, WriteKind)>> {
        self.latest_version_bounded(key, u64::MAX, Some(floor), view)
            .map(|newest| newest.filter(|(seq, _)| *seq > floor))
    }

    fn latest_version_bounded(
        &self,
        key: &[u8],
        ceil: u64,
        floor: Option<u64>,
        view: &ReadView,
    ) -> std::io::Result<Option<(u64, WriteKind)>> {
        let lk = LookupKey::from_prefixed(key, ceil);
        let walked = view.walk_newest_first(key, ceil, |source, max_rt_seq| {
            let version = match source {
                Source::Memtable(mt) => mt.latest_version(&lk),
                Source::Table(_) if floor.is_some_and(|floor| view.version.last_seq <= floor) => {
                    return Ok(ControlFlow::Continue(()));
                }
                Source::Table(reader) => {
                    with_key_scratch(|buf| reader.latest_version(&lk, buf, &self.cache))?
                }
            };
            // A covering range tombstone that outranks the point entry
            // stands in for it.
            Ok(match version {
                Some((point_seq, _)) if max_rt_seq > point_seq => {
                    ControlFlow::Break((max_rt_seq, WriteKind::RangeDelete))
                }
                Some((seq, value_type)) => {
                    ControlFlow::Break((seq, internal_key::write_kind(value_type)))
                }
                None => ControlFlow::Continue(()),
            })
        })?;
        Ok(match walked {
            ControlFlow::Break(version) => Some(version),
            ControlFlow::Continue(max_rt_seq) => {
                (max_rt_seq > 0).then_some((max_rt_seq, WriteKind::RangeDelete))
            }
        })
    }

    /// Move the active memtable to the frozen list behind a fresh one and
    /// a fresh WAL, in the ordered step, and return it.
    #[cfg(test)]
    pub(crate) fn seal_active(&self) -> std::io::Result<Arc<MemTable>> {
        self.ordered_needing(commit::Needs::VERSIONS, |_, ord| ord.seal(true))
    }

    /// Write the oldest frozen memtable out to an L0 SSTable and retire
    /// it, then wake the compaction worker, or, with none, owe the next
    /// write a bounded step. Returns `false` when there was
    /// nothing frozen to flush. See [`Flusher::flush_oldest`].
    fn flush_oldest_frozen(&self, flushing: &flush::Flushing<'_>) -> std::io::Result<bool> {
        let flushed = self.flusher.flush_oldest(flushing)?;
        if flushed {
            self.after_flush();
        }
        Ok(flushed)
    }

    /// The first part of a `compact_range`: the drain that brings the
    /// memtables to L0, so in-memory data that overlaps the range is there
    /// before the passes pick their inputs. `None` when nothing is in
    /// memory. "Data" includes range tombstones, and a log sealed under a
    /// key other than the current one is rotated away even when its
    /// memtable is empty, so a full compaction leaves no file naming a
    /// rotated key.
    ///
    /// The caller holds the compaction gate alone, before this drain, in
    /// the gate -> pipeline order an ingest and `drop_all` take too.
    pub(crate) fn compact_range_drain(&self) -> std::io::Result<Option<drain::Drain>> {
        self.ensure_writable()?;
        let needs_flush = |engine: &RegolithEngine, mt: &MemTable| {
            !mt.is_empty() || mt.has_range_tombstones() || engine.wal_key_is_stale()
        };
        Ok(needs_flush(self, &self.view.load().active).then(|| self.drain_active_if(needs_flush)))
    }

    /// The second part of a `compact_range`, after its drain: compact the
    /// tables overlapping the user-key range `[start, end)` down to the
    /// bottommost non-empty level, on this thread.
    ///
    /// The caller holds the compaction gate alone, so no compaction pass
    /// picks an overlapping input set meanwhile. Every pass hands sections
    /// to the version store's step (a file id per output, the install), so
    /// a queue's poll holds the step for the length of this call.
    ///
    /// Walks levels 0..MAX_LEVELS-1, picking range-overlapping files and
    /// merging them into the next level. A failure is told as any failing
    /// compaction is ([`Self::compaction_failed`]).
    pub(crate) fn compact_range_levels(
        &self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> std::io::Result<()> {
        self.ensure_writable()?;
        self.compact_range_passes(start, end)
            .inspect_err(|err| self.compaction_failed(err))
    }

    /// The passes of [`Self::compact_range_levels`].
    fn compact_range_passes(
        &self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> std::io::Result<()> {
        // Run the level-by-level push-down.
        let compaction_opts = self.options.to_compaction_options();
        // Under FIFO compaction there is no level push-down; a
        // synchronous compact_range just flushes the memtable and
        // runs the FIFO picker so any pending files over the cap
        // get dropped deterministically.
        match self.options.compaction_style {
            crate::options::CompactionStyle::Fifo => {
                let _ = compaction::run_fifo_pass(
                    &self.versions,
                    &self.sst_dir,
                    &self.cache,
                    &compaction_opts,
                )?;
            }
            // Under Universal compaction a manual compact_range folds
            // every L0 file into one run, matching the "force full
            // compaction" semantics a caller expects.
            crate::options::CompactionStyle::Universal => {
                compaction::run_universal_full_compaction(
                    &self.versions,
                    &self.sst_dir,
                    &self.cache,
                    &compaction_opts,
                    &self.snapshot_registry,
                )?;
            }
            _ => compaction::run_compact_range(
                &self.versions,
                &self.sst_dir,
                &self.cache,
                &compaction_opts,
                start,
                end,
                &self.snapshot_registry,
            )?,
        }

        // On an encrypted database a manual compaction also re-seals, over
        // the whole database whatever the range: every table not sealed under
        // the current key (written before encryption was turned on, or under
        // a key since rotated away, or left untouched by the passes above)
        // is rewritten, and so is a manifest holding a batch under another
        // key. With the log rotated above, no file names any other key after.
        if self.options.keyring.is_some() {
            compaction::reseal_tables(
                &self.versions,
                &self.sst_dir,
                &self.cache,
                &compaction_opts,
                &self.snapshot_registry,
            )?;
            self.versions.run(|versions| {
                if versions.has_stale_seals() {
                    versions.compact_manifest()
                } else {
                    Ok(())
                }
            })?;
        }
        // The passes emptied L0 toward the bottom: writers a stop stopped are
        // told once it no longer holds.
        self.refresh_stall_level();
        Ok(())
    }

    /// Whether, on an encrypted database, a log may name a key other than
    /// the current one: the active log is sealed under another key or not
    /// sealed at all, or a frozen memtable's log is still on disk (a
    /// rotation flushes it, and its log with it).
    fn wal_key_is_stale(&self) -> bool {
        let Some(keyring) = &self.options.keyring else {
            return false;
        };
        let current = keyring.current_id();
        !self.view.load().frozen.is_empty()
            || self
                .pipeline_state()
                .is_some_and(|tip| tip.after.log.seal_key() != Some(current))
    }

    /// What a checkpoint or a backup does before it takes the compaction
    /// gate: write every memtable out, so every write acknowledged so far is
    /// in an SSTable.
    ///
    /// Drain, do not merely seal. A capture names the SSTables of the
    /// current version and copies no WAL, so a memtable that is still
    /// waiting on a background flush would be missing from the result
    /// entirely.
    pub(crate) fn checkpoint_drain(&self) -> std::io::Result<()> {
        let mut drain = self.checkpoint_drain_start()?;
        self.drain_advance(&mut drain, commit::Help::Wait)
            .map(|_| ())
    }

    /// [`Self::checkpoint_drain`] as a drain its caller advances: a job a
    /// queue's poll runs, which may park in it.
    pub(crate) fn checkpoint_drain_start(&self) -> std::io::Result<drain::Drain> {
        self.ensure_writable()?;
        Ok(self.drain_all(ActiveFlush::Always))
    }

    /// [`Self::checkpoint_drain`] and [`Self::checkpoint_capture`] on this
    /// thread, waiting here for the gate: a call that chose to block (a
    /// backup).
    pub(crate) fn checkpoint_capture_here(&self) -> std::io::Result<CheckpointSnapshot> {
        self.checkpoint_drain()?;
        self.checkpoint_capture(self.compaction_gate.exclusive()?)
    }

    /// Atomically capture a consistent snapshot of the on-disk state
    /// for checkpoint / backup purposes, after
    /// [`Self::checkpoint_drain`].
    ///
    /// Compacts the manifest so the on-disk form is a single rewrite of
    /// the current version, and returns the captured `Arc<Version>` plus
    /// the paths of the files a caller needs to copy or hard-link.
    ///
    /// The returned [`CheckpointSnapshot`] holds `gate`, the compaction
    /// gate held alone, pinning every referenced file against concurrent
    /// unlink. Callers drop the snapshot as soon as their filesystem work
    /// is done: until then no compaction runs, and a `db.close()` that
    /// joins a compaction worker waiting for the gate waits too.
    pub(crate) fn checkpoint_capture(
        &self,
        gate: Exclusive,
    ) -> std::io::Result<CheckpointSnapshot> {
        self.ensure_writable()?;

        let (version, manifest_bytes) = self.versions.run(|versions| -> std::io::Result<_> {
            // Rewritten first, so what is captured is the canonical form
            // of exactly this version rather than a log of every edit
            // that ever reached it.
            versions.compact_manifest()?;
            let version = versions.current();
            // Read here, in the same section that produced `version`, so
            // the two cannot disagree. A concurrent flush runs its own
            // section but does not take the compaction gate held above, so
            // it is free to append `AddFile` records naming files this
            // checkpoint will not copy, and a rewrite can replace the file
            // entirely.
            let path = versions.manifest_path().to_path_buf();
            Ok((version, self.env.read(&path)?))
        })?;

        Ok(CheckpointSnapshot {
            version,
            manifest_bytes,
            sst_dir: self.sst_dir.clone(),
            _gate: gate,
        })
    }

    /// Approximate on-disk bytes whose user key falls in
    /// `[start, end)`. Fans out over every SSTable whose own
    /// user-key range overlaps the query range and sums their
    /// per-range estimates. Index-only - no data-block decompression
    /// happens, so the cost scales with `num_files * log(num_blocks)`.
    pub(crate) fn approximate_size_in_range(&self, start: &[u8], end: &[u8]) -> u64 {
        if start >= end {
            return 0;
        }
        let version = self.published_version();
        let mut total: u64 = 0;
        for level in &version.levels {
            for file in level {
                if file.meta.largest_key.as_slice() < start {
                    continue;
                }
                if file.meta.smallest_key.as_slice() >= end {
                    continue;
                }
                total += file
                    .reader
                    .approximate_size_in_range(start, end, &self.cache);
            }
        }
        total
    }

    /// Exact `(count, size)` for every entry in the active memtable
    /// whose user key falls in `[start, end)`. Frozen memtables are
    /// *not* included - a caller that wants "everything in memory"
    /// should call this and also walk the frozen memtables
    /// separately.
    pub(crate) fn approximate_memtable_stats(&self, start: &[u8], end: &[u8]) -> (u64, u64) {
        self.view
            .load()
            .active
            .approximate_stats_for_range(start, end)
    }

    // ── property helpers ───────────────────────────────────────────────
    //
    // These return raw values consumed by `Db::get_property` /
    // `Db::get_int_property`. Every method is cheap - no block
    // reads, no locks held beyond one load of the current version or
    // memtable read.

    /// Number of SSTable files at a specific level. Returns 0 for
    /// out-of-range levels rather than panicking, so
    /// `regolith.num-files-at-level<N>` for unknown levels reads
    /// cleanly as `Some(0)`.
    pub(crate) fn num_files_at_level(&self, level: usize) -> u64 {
        let version = self.published_version();
        version
            .levels
            .get(level)
            .map(|files| files.len() as u64)
            .unwrap_or(0)
    }

    /// Tables this open moved out of overlapping levels: zero once a
    /// read-write open has recorded the move.
    pub(crate) fn tables_demoted_at_open(&self) -> u64 {
        self.tables_demoted_at_open
    }

    /// Range tombstones this open copied in front of older keys an earlier
    /// release may have left them behind: zero once a read-write open has
    /// recorded the check.
    pub(crate) fn range_deletes_copied_at_open(&self) -> u64 {
        self.range_deletes_copied_at_open
    }

    /// Total size in bytes across every level of the current
    /// version - sum of every `LiveSst::meta.file_size`.
    pub(crate) fn total_sst_size(&self) -> u64 {
        let version = self.published_version();
        version
            .levels
            .iter()
            .flat_map(|level| level.iter())
            .map(|f| f.meta.file_size)
            .sum()
    }

    /// Total `num_entries` across every current SSTable. The
    /// manifest tracks this per file at ingest / flush /
    /// compaction time, so the sum is free to compute.
    pub(crate) fn total_sst_num_entries(&self) -> u64 {
        let version = self.published_version();
        version
            .levels
            .iter()
            .flat_map(|level| level.iter())
            .map(|f| f.meta.num_entries)
            .sum()
    }

    /// Approximate size of the active memtable (sum of every
    /// inserted internal key + value length seen so far; tracked
    /// by the memtable itself).
    pub(crate) fn active_memtable_size(&self) -> u64 {
        self.view.load().active.approximate_size() as u64
    }

    /// Bytes every in-memory memtable actually reserved from the global
    /// allocator: the sum of their arena chunk sizes plus the heap their
    /// range tombstones own.
    ///
    /// [`RegolithEngine::active_memtable_size`] is the payload figure that
    /// `write_buffer_size` bounds; this is what the process is really
    /// holding, so the gap between them is the arena's rounding waste.
    pub(crate) fn memtables_reserved_size(&self) -> u64 {
        let active = self.view.load().active.reserved_size() as u64;
        let frozen: u64 = self
            .view
            .load()
            .frozen
            .iter()
            .map(|mt| mt.reserved_size() as u64)
            .sum();
        active + frozen
    }

    /// Bytes parked in the memtable arena's recycling pool, waiting to
    /// back the next memtable instead of being returned to the global
    /// allocator. Bounded by
    /// `write_buffer_size * max_write_buffer_number`.
    pub(crate) fn arena_pool_size(&self) -> u64 {
        self.memtable_config.pool_bytes().0 as u64
    }

    /// Total approximate size of every frozen memtable.
    pub(crate) fn frozen_memtables_size(&self) -> u64 {
        self.view
            .load()
            .frozen
            .iter()
            .map(|mt| mt.approximate_size() as u64)
            .sum()
    }

    /// Number of live snapshots. Counts pins, not distinct seqs -
    /// two snapshots taken at the same seq contribute two.
    pub(crate) fn live_snapshot_count(&self) -> u64 {
        self.snapshot_registry.live_count()
    }

    /// Total bytes currently held by the block cache across all
    /// shards. Used by the `regolith.block-cache-usage` property.
    pub(crate) fn block_cache_usage(&self) -> usize {
        self.cache.usage()
    }

    /// Block cache capacity in bytes (the sum of every shard's
    /// budget). Used by the `regolith.block-cache-capacity`
    /// property.
    pub(crate) fn block_cache_capacity(&self) -> usize {
        self.cache.capacity()
    }

    pub(crate) fn background_error_count(&self) -> u64 {
        self.background_health.error_count()
    }

    /// Free and total space on the filesystem holding the database, read
    /// now. `None` where the environment cannot say.
    pub(crate) fn disk_space(&self) -> Option<crate::env::DiskSpace> {
        self.env.disk_space(&self.sst_dir).ok().flatten()
    }

    /// Bytes the currently-live SSTable readers hold *outside* the
    /// block cache budget: pinned indexes, pinned filter regions, any
    /// metadata block the cache refused, and range tombstones.
    ///
    /// This is the honest counterpart to `regolith.block-cache-usage`:
    /// together they account for every byte of SSTable metadata the
    /// engine is holding. With
    /// [`Options::cache_index_and_filter_blocks`] off this number grows
    /// with the number of open files; with it on, only the pinned
    /// top-level index of each partitioned file and its range
    /// tombstones remain here.
    ///
    /// [`Options::cache_index_and_filter_blocks`]: crate::Options::cache_index_and_filter_blocks
    pub(crate) fn pinned_metadata_bytes(&self) -> usize {
        let version = self.versions.current();
        version
            .levels
            .iter()
            .flat_map(|level| level.iter())
            .map(|file| file.reader.pinned_metadata_bytes())
            .sum()
    }

    /// Unix-seconds timestamp of the oldest live snapshot, or
    /// `None` when no snapshot is alive.
    pub(crate) fn oldest_snapshot_time_unix(&self) -> Option<u64> {
        self.snapshot_registry.oldest_snapshot_time_unix()
    }

    /// Borrow the current version so a caller can walk every
    /// SSTable's metadata - used by `regolith.sstables` formatter.
    pub(crate) fn current_version(&self) -> Arc<manifest::Version> {
        self.published_version()
    }

    /// Drop all data in the engine.
    pub(crate) fn drop_all(&self) -> std::io::Result<()> {
        let dropped = self.drop_all_excluded();
        // Once every exclusion the drop held is released, failed or not: a
        // stopped writer it held off looks again (`StallSignal`, relief
        // handed back). On success nothing is left for any threshold to
        // count.
        self.refresh_stall_level();
        dropped
    }

    /// [`Self::drop_all`] under its exclusions.
    fn drop_all_excluded(&self) -> std::io::Result<()> {
        self.ensure_writable()?;
        // Exclude background compaction for the whole drop, as
        // `compact_range`, an ingest's install and `checkpoint_capture`
        // do. Without it a worker holding the gate shared runs straight
        // through the `Reset` below and then applies its `AddFile`, which
        // `VersionSet::apply` lays over whatever version is current: a file
        // full of pre-drop rows lands in the freshly emptied version, and
        // `remove_obsolete_sst_files` cannot unlink it because the pre-drop
        // version never named it. The drop reports success and the data is
        // still there, across a reopen.
        //
        // Taken before the ordered step, in the gate -> pipeline order
        // every exclusive user of the gate takes. `drop_all` is a call that
        // does its own I/O and chose to block (plan 3.0), so it waits for
        // the gate on its own thread.
        let _gate = self.compaction_gate.exclusive()?;
        self.ordered_needing(commit::Needs::FLUSH, |engine, ord| {
            engine.drop_all_ordered(ord)
        })
    }

    /// [`Self::drop_all`] in the ordered step: every earlier commit is
    /// published and no later one is decided until this returns.
    fn drop_all_ordered(&self, ord: &mut commit::Ordered<'_>) -> std::io::Result<()> {
        // A flush the background is running took its memtable before the
        // drop and installs its table after it: held here, it finishes first
        // and the `Reset` below removes its table, instead of laying it over
        // the emptied version.
        let _flushing = self.flusher.flushing()?;
        self.ensure_writable()?;

        // Published before the version `Reset` below, so no reader can
        // see the pre-drop memtables against the post-drop version. The
        // ordered step excludes writers but not readers: a concurrent
        // reader may still briefly observe pre-drop SSTable data, exactly
        // as before this view existed.
        let fresh = Arc::new(MemTable::new(&self.memtable_config)?);
        self.view
            .update_memtables(|_, _| (Arc::clone(&fresh), Vec::new(), ()));

        let (old_version, wal_id, wal_path, new_log) =
            self.versions.run(|versions| -> std::io::Result<_> {
                let old_version = versions.current();
                let id = old_version.next_file_id;
                let wal_path = self.wal_dir.join(wal_filename(id));
                let new_log =
                    wal_log::Log::create_in(&self.env, &wal_path, self.options.keyring.as_deref())?;
                versions.apply(&[VersionEdit::Reset {
                    next_file_id: id + 1,
                    min_wal_id: id,
                }])?;
                Ok((old_version, id, wal_path, new_log))
            })?;

        self.cache.clear();
        // The sequence goes on from where it was: a commit published before
        // the drop may still be raising the read horizon to its own
        // sequence, which a horizon set back to zero would then jump to.
        ord.replace(
            fresh,
            Arc::new(new_log.with_early_writeback(self.early_writeback())),
        );
        self.wal_id.store(wal_id, Ordering::Release);

        self.versions
            .run(|versions| versions.compact_manifest())
            .map_err(|e| {
                std::io::Error::new(e.kind(), format!("drop_all rewriting manifest: {e}"))
            })?;

        remove_obsolete_sst_files(&*self.env, &self.sst_dir, &old_version).map_err(|e| {
            std::io::Error::new(e.kind(), format!("drop_all removing sstables: {e}"))
        })?;
        remove_obsolete_wal_files(&*self.env, &self.wal_dir, &wal_path)
            .map_err(|e| std::io::Error::new(e.kind(), format!("drop_all removing wals: {e}")))?;
        Ok(())
    }

    /// Test-only: whether the active memtable currently holds any
    /// entries. Used by `compact_range` tests to verify that the
    /// memtable was flushed to L0 as part of the range walk.
    #[cfg(test)]
    pub(crate) fn active_memtable_is_empty(&self) -> bool {
        let view = self.view.load();
        view.active.is_empty() && !view.active.has_range_tombstones()
    }

    /// Test-only: number of SSTable files at `level` in the current
    /// version.
    #[cfg(test)]
    pub(crate) fn level_file_count(&self, level: usize) -> usize {
        self.published_version().levels[level].len()
    }

    /// Test-only: total number of SSTable files across every level.
    #[cfg(test)]
    pub(crate) fn total_file_count(&self) -> usize {
        let v = self.published_version();
        v.levels.iter().map(|level| level.len()).sum()
    }

    /// Test-only: collect every raw `(seq, value_type)` version of
    /// `user_key` currently persisted across all SSTables. Used by the
    /// snapshot-pinning GC tests to check the post-compaction on-disk
    /// state - not just what reads see.
    #[cfg(test)]
    pub(crate) fn all_persisted_versions_of(
        &self,
        user_key: &[u8],
    ) -> std::io::Result<Vec<(u64, u8)>> {
        use internal_key::decode_internal_key;

        let version = self.published_version();
        let mut out = Vec::new();
        for level in &version.levels {
            for file in level {
                let mut entries = file.reader.iter_internal_stream(&self.cache)?;
                while let Some((ik, _v)) = entries.next_entry()? {
                    let (uk, seq, vt) = decode_internal_key(&ik);
                    if uk == user_key {
                        out.push((seq, vt));
                    }
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// Flush all data to disk and shut down background threads.
    ///
    /// The close contract (plan 3.0), in order:
    ///
    /// 1. Every commit ticket the final sync covers is resolved: the group a
    ///    nowait leader left owing its fsync lands here, so its members learn
    ///    they committed. No group is written after, since every write
    ///    checks the database open.
    /// 2. Every queue with a pending unit is told: a read's completion, after
    ///    which the read run again answers `Closed`; a stalled write's wait;
    ///    a job a queue owns, settled as its body settles a closed database
    ///    (a foreground job's ticket fails with `Closed`).
    /// 3. The `on_abort` callbacks of transactions still open run, on this
    ///    thread.
    /// 4. The memtables are flushed, the log closed, and regolith's threads
    ///    joined; a foreground job the workers had not run fails with
    ///    `Closed`.
    pub(crate) fn close(&self) -> std::io::Result<()> {
        // Close does its own I/O and chose to block (plan 3.0): a second
        // close waits here, on its own thread, for the first.
        let _close_guard = wait_here(self.close_lock.lock())?;
        match self.close_state.load(Ordering::Acquire) {
            CLOSE_STATE_CLOSED => return Ok(()),
            CLOSE_STATE_CLOSING => return Err(Self::closed_error()),
            _ => {}
        }

        self.close_state
            .store(CLOSE_STATE_CLOSING, Ordering::Release);
        // The tickets the final sync covers are resolved first: an ordered
        // unit runs with every commit before it published, and every commit
        // decided after this point finds the database closing.
        if self.pipeline.is_some() {
            let _ = self.ordered(|_, _| Ok(()));
        }
        // Every write a stall stopped is told now, and finds the database
        // closing when it runs again.
        self.stall_signal.land();
        // Every queue waiting on a read gets its completion now, and the read
        // run again sees the database closing.
        self.cache.close_io();
        // A transaction still open ends here, on this thread, before the final
        // sync; one already committing is left to finish through it.
        self.open_transactions.abort_all(self);

        match self.close_inner() {
            Ok(()) => {
                self.close_state
                    .store(CLOSE_STATE_CLOSED, Ordering::Release);
                Ok(())
            }
            Err(err) => {
                // Commits go on in a log of their own: one the close sealed
                // in place is synced and followed by a fresh one first. If
                // that fails too the database is latched, so no commit lands
                // in a log a flush removes.
                if self.pipeline.is_some()
                    && let Err(reopen) =
                        self.ordered(|engine, ord| ord.reopen_log(engine.is_latched()))
                {
                    tracing::error!(error = %reopen, "taking writes again after a failed close failed");
                    self.latch_wal_failure(&reopen);
                }
                // Reads through queues work again on the database the failed
                // close left open, and lock waits queue again.
                self.cache.reopen_io();
                self.open_transactions.reopen_locks();
                self.close_state.store(CLOSE_STATE_OPEN, Ordering::Release);
                self.refresh_stall_level();
                Err(err)
            }
        }
    }

    fn close_inner(&self) -> std::io::Result<()> {
        if self.is_read_only() {
            self.compaction.shutdown();
            return Ok(());
        }

        self.flush_memtables_for_close()?;

        // A latched log may end in bytes nobody can account for, and CLOSE
        // would vouch for them: replay would then refuse the crash it should
        // survive. Such a log is left for recovery to judge. Checked in the
        // ordered step, where a failing commit latches.
        self.ordered(|engine, ord| ord.close_log(engine.is_latched()))?;
        // Stop and join regolith's threads. A worker waiting for the
        // compaction gate exits once whoever holds the gate lets go: a job on
        // another worker when it ends, a queue's job that was handed the
        // gate while it was parked when the sweep above settled it. Then a
        // foreground job the workers never ran fails with `Closed`, and its
        // ticket is told. The log is closed, so the lane has nothing left to
        // hand over.
        self.compaction.shutdown();
        #[cfg(any(target_os = "linux", target_vendor = "apple"))]
        if let Some(lane) = &self.log_writeback {
            lane.shutdown();
        }

        Ok(())
    }

    fn flush_memtables_for_close(&self) -> std::io::Result<()> {
        self.drain_memtables(ActiveFlush::ForClose)
    }

    /// Rewrite the manifest now, the way growth does on its own.
    #[cfg(test)]
    pub(crate) fn force_manifest_rewrite(&self) -> std::io::Result<()> {
        self.versions.run(|versions| versions.compact_manifest())
    }

    /// Test-only: how many sealed memtables wait for a flush.
    #[cfg(test)]
    pub(crate) fn frozen_memtables(&self) -> usize {
        self.view.load().frozen.len()
    }

    /// Whether no memtable holds anything a flush would write out: the
    /// postcondition of a drain with [`ActiveFlush::Always`], and what a
    /// checkpoint depends on.
    #[cfg(test)]
    pub(crate) fn memtables_hold_no_data(&self) -> bool {
        let view = self.view.load();
        view.frozen.is_empty() && view.active.is_empty() && !view.active.has_range_tombstones()
    }
}

/// A consistent snapshot of on-disk state captured by
/// [`RegolithEngine::checkpoint_capture`]. Holds the engine's compaction
/// gate alone for its entire lifetime, so no compaction can unlink files
/// referenced by `version` while the snapshot is alive. Callers MUST drop
/// this snapshot as soon as they are done with the filesystem work: while
/// it lives no compaction runs, and a concurrent `db.close()` /
/// `drop(db)` that joins a compaction worker waiting for the gate waits
/// for the snapshot too.
pub(crate) struct CheckpointSnapshot {
    pub(crate) version: Arc<manifest::Version>,
    /// The manifest exactly as it described `version`, captured under
    /// the same lock.
    ///
    /// The bytes are held rather than a path and a length. A concurrent
    /// flush runs a section of the version store but not the compaction gate this
    /// snapshot holds, so it can append `AddFile` records naming files
    /// this checkpoint will not copy, and a manifest rewrite can replace
    /// the file wholesale. Re-reading the path later would then pick up
    /// a different manifest, and truncating it at a length measured
    /// against the old one can land mid-record. Copying what was read
    /// under the lock removes both.
    ///
    /// The manifest is rewritten once it grows past a multiple of its
    /// canonical size, so this is bounded by the live SSTable count.
    pub(crate) manifest_bytes: Vec<u8>,
    pub(crate) sst_dir: PathBuf,
    /// The compaction gate, held alone for the snapshot's lifetime.
    _gate: Exclusive,
}

impl CheckpointSnapshot {
    /// Format an SSTable filename for the given file id - exposes
    /// the engine's naming scheme to callers outside the `engine`
    /// module so they can stage files into checkpoint / backup
    /// directories.
    pub(crate) fn sst_filename(id: u64) -> String {
        sst_filename(id)
    }
}

fn remove_obsolete_sst_files(
    env: &dyn Env,
    sst_dir: &Path,
    version: &manifest::Version,
) -> std::io::Result<()> {
    let mut removed_any = false;
    for level in &version.levels {
        for file in level {
            let path = sst_dir.join(sst_filename(file.meta.file_id));
            removed_any |= remove_file_if_exists(env, &path);
        }
    }
    if removed_any {
        env.sync_dir(sst_dir)?;
    }
    Ok(())
}

fn remove_obsolete_wal_files(
    env: &dyn Env,
    wal_dir: &Path,
    keep_path: &Path,
) -> std::io::Result<()> {
    let mut removed_any = false;
    for path in wal_scan::walk(env, wal_dir)? {
        let path = path?;
        if path == keep_path {
            continue;
        }
        removed_any |= remove_file_if_exists(env, &path);
    }
    if removed_any {
        env.sync_dir(wal_dir)?;
    }
    Ok(())
}

/// Unlink an obsolete file, reporting whether anything was removed.
///
/// Best effort by design, and the two callers are the only ones: an
/// obsolete SSTable or WAL is already unreachable through the manifest,
/// so failing to unlink it costs disk until the next sweep and costs
/// correctness nothing. Propagating the failure instead would fail the
/// operation that happened to trigger the sweep, which is how a
/// `drop_all` or a compaction came to return "Access is denied".
///
/// Windows is why this is not merely defensive. A file unlinked while
/// something still holds it open stays delete-pending: the name is still
/// there, and both a second unlink and an open of it are refused with
/// `ACCESS_DENIED` rather than the `NotFound` unix answers with. A sweep
/// that races a reader therefore sees an error for a file that is
/// already on its way out.
fn remove_file_if_exists(env: &dyn Env, path: &Path) -> bool {
    match env.remove_file(path) {
        Ok(()) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => {
            tracing::debug!(
                path = %path.display(),
                error = %err,
                "could not unlink an obsolete file; leaving it for the next sweep"
            );
            false
        }
    }
}

fn wal_file_id(path: &Path) -> Option<u64> {
    let stem = path.file_stem()?.to_str()?;
    stem.strip_prefix("wal_")?.parse().ok()
}

fn should_replay_wal(path: &Path, min_wal_id: u64) -> bool {
    match wal_file_id(path) {
        Some(id) => id >= min_wal_id,
        // A temporary WAL name carries no id to compare against the
        // reset marker. Once a reset has committed, such a file must not
        // be allowed to resurrect data.
        None => min_wal_id == 0,
    }
}

fn next_wal_id(manifest_next_file_id: u64, wal_files: &[PathBuf]) -> u64 {
    wal_files
        .iter()
        .filter_map(|path| wal_file_id(path))
        .map(|id| id.saturating_add(1))
        .fold(manifest_next_file_id, u64::max)
}

fn apply_replayed_wal_entry(memtable: &MemTable, entry: WalEntry) -> u64 {
    match entry {
        WalEntry::Put { key, value, seq } => {
            memtable.put(&key, &value, seq);
            seq
        }
        WalEntry::Delete { key, seq } => {
            memtable.delete(&key, seq);
            seq
        }
        WalEntry::DeleteRange { start, end, seq } => {
            memtable.delete_range(&start, &end, seq);
            seq
        }
        WalEntry::Merge { key, operand, seq } => {
            memtable.merge(&key, &operand, seq);
            seq
        }
    }
}

#[cfg(test)]
mod recovery_tests;
