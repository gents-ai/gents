//! Engine-wide counters and histograms for observability.
//!
//! A caller configures [`crate::Options::statistics`] with an
//! [`Arc<Statistics>`] and the engine increments tickers and
//! records histograms on every hot path it has instrumented. The
//! caller then polls the `Statistics` object (via
//! [`Statistics::get_ticker`], [`Statistics::get_histogram_snapshot`],
//! or [`Statistics::to_string`]) to export the values to their
//! monitoring stack of choice. A caller that kept only the database
//! reads the same object back with [`crate::Db::statistics`].
//!
//! # Metric names
//!
//! Every ticker and histogram exports a stable string name of the
//! form `regolith.<surface>.<metric>`, grouping related metrics
//! under a shared surface prefix. The surfaces are `write`
//! (memtable/batch writes), `read` (point lookups), `iter`
//! (iterator seek/next), `block_cache`, `bloom`, `compaction`,
//! `flush`, `wal`, `snapshot`, `commit` (transaction commit
//! outcomes), and `policy` ([`IsolationLevel::DefraLevel`]
//! relaxations).
//!
//! # Cost when disabled
//!
//! `Options::statistics = None` short-circuits every instrumentation
//! site behind an `Option::is_some` check plus an `Arc` clone at
//! open time. The hot-path overhead is a single branch. Reaching
//! for a non-`None` statistics object adds one relaxed `fetch_add` per
//! ticker update and a few atomic operations per histogram sample, all
//! on the calling thread's own shard. A [`StatsLevel`] below
//! [`StatsLevel::All`] drops the latency timers and the two clock reads
//! each takes, or every histogram, and keeps the tickers.
//!
//! # Per-thread shards (plan 4.6, D54)
//!
//! Every ticker and histogram is kept once per thread number (see
//! `per_thread`), each copy on cache lines of its own, so threads on
//! different cores never write the same line. A read sums the copies.
//! [`Statistics`] states what a read sees while updates run.
//!
//! # Histograms
//!
//! The initial histogram implementation tracks `count`, `sum`,
//! `min`, and `max` only. `HistogramSnapshot::average` gives you
//! `sum / count`. Percentiles / buckets are intentionally out of
//! scope for v1 - adding an HDR-style bucket array is a follow-up
//! that drops in behind the existing API without breaking
//! callers.

use kovan::CachePadded;

use crate::portability::{AtomicU64, Ordering};

/// Enumerated counters incremented by the engine. Every variant
/// is backed by one `AtomicU64` slot in [`Statistics`]; looking
/// up a ticker is `O(1)` and thread-safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
#[non_exhaustive]
pub enum Ticker {
    /// Key and value bytes of the writes that committed: the key and
    /// the value of each put and the key of each delete. Counts raw
    /// user bytes; does not include internal encoding overhead, the
    /// CF prefix, or WAL framing.
    ///
    /// This and the four counters of operations below count a write
    /// once it has committed, whichever call made it: a put, a write
    /// batch, a column family's call, a transaction's commit. A write
    /// that failed, was refused or lost a conflict adds nothing.
    BytesWritten = 0,
    /// Total value bytes returned from point lookups that found
    /// a live value.
    BytesRead = 1,
    /// Number of puts that committed.
    KeysWritten = 2,
    /// Number of `get` calls (including lookups that returned
    /// `None`).
    KeysRead = 3,
    /// Number of deletes that committed.
    KeysDeleted = 4,
    /// Number of range deletes that committed.
    RangeDeletesWritten = 5,
    /// Number of merge operands that committed.
    MergesWritten = 6,
    /// Block cache hits (the block was already resident).
    BlockCacheHit = 7,
    /// Block cache misses (the block had to be fetched from disk).
    BlockCacheMiss = 8,
    /// Block cache insertions (one per miss that actually
    /// populated the cache).
    BlockCacheAdd = 9,
    /// Bloom-filter "useful" hits - the filter correctly
    /// answered "not present" and spared a block read. Counts a point
    /// lookup a table's key filter ruled out, and a table a prefix seek's
    /// prefix filter ruled out.
    BloomFilterUseful = 10,
    /// Bloom-filter "full positive" hits - the filter said
    /// "maybe", and the key was actually present in the block.
    BloomFilterFullPositive = 11,
    /// Bytes read by compaction (sum of input file sizes).
    CompactionBytesRead = 12,
    /// Bytes written by compaction (sum of output file sizes).
    CompactionBytesWritten = 13,
    /// Number of compaction jobs that have run.
    CompactionCount = 14,
    /// Bytes written by the flush path (output SSTable size).
    FlushBytesWritten = 15,
    /// Number of memtable → L0 flushes.
    FlushCount = 16,
    /// Bytes appended to the WAL.
    WalBytesWritten = 17,
    /// Number of `Wal::sync` calls.
    WalSyncCount = 18,
    /// Number of `Iter::seek` / `Iter::seek_to_first` / etc.
    IterSeekCount = 19,
    /// Number of `Iter::next` calls that produced a key.
    IterNextCount = 20,
    /// Microseconds threads spent held by a write stall: relieving it
    /// inline, or blocked until it cleared (a commit that waits a stop out,
    /// [`crate::StallWait::wait`]).
    ///
    /// A wait that is polled or awaited on a queue holds no thread and is
    /// not counted, nor is inline relief that ends in a busy or closed
    /// error.
    WriteStallMicros = 21,
    /// Number of snapshots registered via `Db::snapshot`.
    SnapshotsRegistered = 22,
    /// Number of snapshots released (dropped or explicitly
    /// released).
    SnapshotsReleased = 23,
    /// Opens that discarded the end of the newest write-ahead log: the
    /// bytes from its first unusable record on, which no surviving record
    /// proves were made durable, as a crash leaves an unsynced tail. Each
    /// is also logged and passed to
    /// [`crate::EventListener::on_wal_tail_discarded`], with the file and
    /// the offset.
    WalTailDiscarded = 24,
    /// Optimistic or pessimistic transaction commits that returned
    /// `Ok`.
    CommitCount = 25,
    /// Transaction commits that returned
    /// [`crate::TransactionError::Conflict`].
    CommitConflicts = 26,
    /// The subset of [`Ticker::CommitConflicts`] where the
    /// conflicting key was a key the transaction read.
    CommitConflictsOnRead = 27,
    /// The subset of [`Ticker::CommitConflicts`] where the
    /// conflicting key was a key the transaction wrote or merged
    /// into.
    CommitConflictsOnWrite = 28,
    /// Written keys whose newer committed version was accepted
    /// because the write stored exactly what the key already holds.
    /// Counted only for commits that returned `Ok`.
    CommitWritesElided = 29,
    /// Blind merge-only keys accepted at
    /// [`crate::IsolationLevel::DefraLevel`] despite a newer merge
    /// operand, because operands commute. Counted only for commits
    /// that returned `Ok`.
    PolicyBlindMergesCommuted = 30,
    /// Scan stretches dropped because they stayed inside a
    /// [`crate::KeyClass::CommutativePrefix`], so the caller's
    /// policy declared them safe to leave unvalidated. Counted only
    /// for commits that returned `Ok`.
    PolicyScanRunsDropped = 31,
    /// Bytes discarded from the end of the newest write-ahead log at open,
    /// summed over the discards [`Ticker::WalTailDiscarded`] counts.
    WalTailDiscardedBytes = 32,
    /// Write-ahead logs that could not be removed once their writes were in
    /// tables. Each is logged, never replayed, and removed again by the
    /// next flush or open.
    WalRemoveFailed = 33,
    /// Opens that discarded the end of the MANIFEST: the bytes from its
    /// first batch that does not read back whole, which no later batch
    /// proves were made durable, as a crash leaves an unsynced tail. Each
    /// is also logged with the file and the offset.
    ManifestTailDiscarded = 34,
    /// Bytes discarded from the end of the MANIFEST at open, summed over the
    /// discards [`Ticker::ManifestTailDiscarded`] counts.
    ManifestTailDiscardedBytes = 35,
    /// Blocks the block cache refused to keep because every block its hand
    /// could take was held by a reader (or by another insert's hand), so
    /// keeping one more would pass the byte budget. The read that loaded the
    /// block still uses it, uncached. A steady count means the cache is too
    /// small for the blocks readers hold at once.
    BlockCacheAddRefusedHeld = 36,
    /// The subset of [`Ticker::CommitConflicts`] where the newer write
    /// that decided the race was a delete or a range delete: a key the
    /// transaction read or wrote was removed after its snapshot. It says
    /// what the newer write was, where [`Ticker::CommitConflictsOnRead`]
    /// and [`Ticker::CommitConflictsOnWrite`] say what the transaction
    /// did with the key, so one conflict can count here and there. A
    /// key deleted and then put again counts as a conflict with the put.
    CommitConflictsAfterDelete = 37,
    /// Tables an open moved to level 0 because an earlier release had left
    /// their level with overlapping key ranges, which a level below 0
    /// cannot hold. The data is unchanged, and the open is logged once. A
    /// read-write open records the move, so the next open counts nothing;
    /// a read-only open moves them in its own view, every time.
    ManifestTablesDemoted = 38,
    /// Compaction jobs run because more than half of the entries they read
    /// were deletions and the entries those delete. Each is also counted in
    /// [`Ticker::CompactionCount`]. Level and universal compaction only.
    CompactionDeletionTriggered = 39,
    /// Range deletes an open copied in front of every key because an
    /// earlier release may have left them behind older keys they delete,
    /// where a read would find the older key first. The data is unchanged,
    /// and the open is logged once. A read-write open records that it
    /// checked, so the next open counts nothing; a read-only open copies
    /// them in its own view, every time.
    ManifestRangeDeletesCopied = 40,
}

const NUM_TICKERS: usize = 41;

impl Ticker {
    /// Every defined ticker, in discriminant order. Adding a variant
    /// without appending it here would drop it from [`Statistics::dump`].
    pub const ALL: &'static [Ticker] = &[
        Ticker::BytesWritten,
        Ticker::BytesRead,
        Ticker::KeysWritten,
        Ticker::KeysRead,
        Ticker::KeysDeleted,
        Ticker::RangeDeletesWritten,
        Ticker::MergesWritten,
        Ticker::BlockCacheHit,
        Ticker::BlockCacheMiss,
        Ticker::BlockCacheAdd,
        Ticker::BloomFilterUseful,
        Ticker::BloomFilterFullPositive,
        Ticker::CompactionBytesRead,
        Ticker::CompactionBytesWritten,
        Ticker::CompactionCount,
        Ticker::FlushBytesWritten,
        Ticker::FlushCount,
        Ticker::WalBytesWritten,
        Ticker::WalSyncCount,
        Ticker::IterSeekCount,
        Ticker::IterNextCount,
        Ticker::WriteStallMicros,
        Ticker::SnapshotsRegistered,
        Ticker::SnapshotsReleased,
        Ticker::WalTailDiscarded,
        Ticker::CommitCount,
        Ticker::CommitConflicts,
        Ticker::CommitConflictsOnRead,
        Ticker::CommitConflictsOnWrite,
        Ticker::CommitWritesElided,
        Ticker::PolicyBlindMergesCommuted,
        Ticker::PolicyScanRunsDropped,
        Ticker::WalTailDiscardedBytes,
        Ticker::WalRemoveFailed,
        Ticker::ManifestTailDiscarded,
        Ticker::ManifestTailDiscardedBytes,
        Ticker::BlockCacheAddRefusedHeld,
        Ticker::CommitConflictsAfterDelete,
        Ticker::ManifestTablesDemoted,
        Ticker::CompactionDeletionTriggered,
        Ticker::ManifestRangeDeletesCopied,
    ];

    /// Stable string name for exporting to monitoring systems.
    pub fn name(&self) -> &'static str {
        match self {
            Ticker::BytesWritten => "regolith.write.bytes",
            Ticker::BytesRead => "regolith.read.bytes",
            Ticker::KeysWritten => "regolith.write.keys",
            Ticker::KeysRead => "regolith.read.keys",
            Ticker::KeysDeleted => "regolith.write.deletes",
            Ticker::RangeDeletesWritten => "regolith.write.range_deletes",
            Ticker::MergesWritten => "regolith.write.merges",
            Ticker::BlockCacheHit => "regolith.block_cache.hit",
            Ticker::BlockCacheMiss => "regolith.block_cache.miss",
            Ticker::BlockCacheAdd => "regolith.block_cache.add",
            Ticker::BloomFilterUseful => "regolith.bloom.useful",
            Ticker::BloomFilterFullPositive => "regolith.bloom.full_positive",
            Ticker::CompactionBytesRead => "regolith.compaction.bytes_read",
            Ticker::CompactionBytesWritten => "regolith.compaction.bytes_written",
            Ticker::CompactionCount => "regolith.compaction.count",
            Ticker::FlushBytesWritten => "regolith.flush.bytes_written",
            Ticker::FlushCount => "regolith.flush.count",
            Ticker::WalBytesWritten => "regolith.wal.bytes_written",
            Ticker::WalSyncCount => "regolith.wal.sync_count",
            Ticker::IterSeekCount => "regolith.iter.seek_count",
            Ticker::IterNextCount => "regolith.iter.next_count",
            Ticker::WriteStallMicros => "regolith.write.stall_micros",
            Ticker::SnapshotsRegistered => "regolith.snapshot.registered",
            Ticker::SnapshotsReleased => "regolith.snapshot.released",
            Ticker::WalTailDiscarded => "regolith.wal.tail_discarded",
            Ticker::CommitCount => "regolith.commit.count",
            Ticker::CommitConflicts => "regolith.commit.conflicts",
            Ticker::CommitConflictsOnRead => "regolith.commit.conflicts_on_read",
            Ticker::CommitConflictsOnWrite => "regolith.commit.conflicts_on_write",
            Ticker::CommitWritesElided => "regolith.commit.writes_elided",
            Ticker::PolicyBlindMergesCommuted => "regolith.policy.blind_merges_commuted",
            Ticker::PolicyScanRunsDropped => "regolith.policy.scan_runs_dropped",
            Ticker::WalTailDiscardedBytes => "regolith.wal.tail_discarded_bytes",
            Ticker::WalRemoveFailed => "regolith.wal.remove_failed",
            Ticker::ManifestTailDiscarded => "regolith.manifest.tail_discarded",
            Ticker::ManifestTailDiscardedBytes => "regolith.manifest.tail_discarded_bytes",
            Ticker::BlockCacheAddRefusedHeld => "regolith.block_cache.add_refused_held",
            Ticker::CommitConflictsAfterDelete => "regolith.commit.conflicts_after_delete",
            Ticker::ManifestTablesDemoted => "regolith.manifest.tables_demoted",
            Ticker::CompactionDeletionTriggered => "regolith.compaction.deletion_triggered",
            Ticker::ManifestRangeDeletesCopied => "regolith.manifest.range_deletes_copied",
        }
    }
}

/// Enumerated histograms recorded by the engine. Every variant
/// is backed by one histogram slot in [`Statistics`] updated with
/// lock-free atomics, so recording never blocks a thread and
/// histograms do not contend with each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
#[non_exhaustive]
pub enum Histogram {
    /// Wall-clock microseconds per `Db::get` call.
    DbGet = 0,
    /// Wall-clock microseconds per `Db::write` / `apply_batch`.
    DbWrite = 1,
    /// Wall-clock microseconds per `Iter::seek*` call.
    DbIterSeek = 2,
    /// Wall-clock microseconds per `Iter::next` call that
    /// produced a key.
    DbIterNext = 3,
    /// Wall-clock microseconds per compaction job.
    CompactionTime = 4,
    /// Wall-clock microseconds per flush.
    FlushTime = 5,
    /// Bytes returned per `Db::get` that found a live value.
    BytesPerRead = 6,
    /// Bytes per write that committed (a put, a batch, a commit):
    /// what that write added to [`Ticker::BytesWritten`].
    BytesPerWrite = 7,
    /// Wall-clock microseconds to append a batch to the WAL
    /// (including fsync when durability is Immediate).
    WalWriteTime = 8,
}

const NUM_HISTOGRAMS: usize = 9;

/// Every defined histogram, in discriminant order. Same pattern
/// as [`Ticker::ALL`].
const ALL_HISTOGRAMS: &[Histogram] = &[
    Histogram::DbGet,
    Histogram::DbWrite,
    Histogram::DbIterSeek,
    Histogram::DbIterNext,
    Histogram::CompactionTime,
    Histogram::FlushTime,
    Histogram::BytesPerRead,
    Histogram::BytesPerWrite,
    Histogram::WalWriteTime,
];

impl Histogram {
    /// Stable string name for exporting to monitoring systems.
    pub fn name(&self) -> &'static str {
        match self {
            Histogram::DbGet => "regolith.read.get_micros",
            Histogram::DbWrite => "regolith.write.batch_micros",
            Histogram::DbIterSeek => "regolith.iter.seek_micros",
            Histogram::DbIterNext => "regolith.iter.next_micros",
            Histogram::CompactionTime => "regolith.compaction.micros",
            Histogram::FlushTime => "regolith.flush.micros",
            Histogram::BytesPerRead => "regolith.read.bytes_per_get",
            Histogram::BytesPerWrite => "regolith.write.bytes_per_batch",
            Histogram::WalWriteTime => "regolith.wal.write_micros",
        }
    }
}

/// Immutable snapshot of a single histogram's state. Callers
/// read this to export to their metrics pipeline or assert in
/// tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HistogramSnapshot {
    /// Number of samples recorded.
    pub count: u64,
    /// Sum of all recorded values.
    pub sum: u64,
    /// Minimum recorded value (0 when `count == 0`).
    pub min: u64,
    /// Maximum recorded value (0 when `count == 0`).
    pub max: u64,
}

impl HistogramSnapshot {
    /// Arithmetic mean. Returns 0 when `count == 0`.
    pub fn average(&self) -> u64 {
        self.sum.checked_div(self.count).unwrap_or(0)
    }
}

/// One histogram's running count, sum, min and max.
///
/// Lock-free: `record` runs on hot read and write paths, and a mutex here
/// serialised every instrumented `get` across threads. Each field is
/// updated independently, so a snapshot taken while a sample is being
/// recorded may include part of that one sample (its count but not yet
/// its sum, say). That is the precision these observability counters
/// need; `reset` racing a recorder is equally approximate.
#[derive(Debug)]
struct HistogramData {
    count: AtomicU64,
    sum: AtomicU64,
    /// `u64::MAX` until the first sample, so `fetch_min` needs no branch.
    min: AtomicU64,
    max: AtomicU64,
}

impl Default for HistogramData {
    fn default() -> Self {
        Self {
            count: AtomicU64::new(0),
            sum: AtomicU64::new(0),
            min: AtomicU64::new(u64::MAX),
            max: AtomicU64::new(0),
        }
    }
}

impl HistogramData {
    /// Each read-modify-write after the count is skipped when it cannot
    /// change its field: adding zero, or a sample the loaded minimum and
    /// maximum already cover. Between resets the minimum only falls and the
    /// maximum only rises, so a bound that covers the sample when loaded
    /// covers it for good. A scope shorter than a microsecond records zero,
    /// so the common sample costs one atomic add, not four.
    fn record(&self, value: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        if value != 0 {
            let mut sum = self.sum.load(Ordering::Relaxed);
            while let Err(current) = self.sum.compare_exchange_weak(
                sum,
                sum.saturating_add(value),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                sum = current;
            }
        }
        if value < self.min.load(Ordering::Relaxed) {
            self.min.fetch_min(value, Ordering::Relaxed);
        }
        if value > self.max.load(Ordering::Relaxed) {
            self.max.fetch_max(value, Ordering::Relaxed);
        }
    }

    /// Folds this shard's state into `acc`: counts and sums add, the
    /// minimum and maximum widen. `acc.min` is `u64::MAX` until a shard
    /// with a sample is folded in.
    fn fold_into(&self, acc: &mut HistogramSnapshot) {
        acc.count = acc.count.wrapping_add(self.count.load(Ordering::Relaxed));
        acc.sum = acc.sum.saturating_add(self.sum.load(Ordering::Relaxed));
        acc.min = acc.min.min(self.min.load(Ordering::Relaxed));
        acc.max = acc.max.max(self.max.load(Ordering::Relaxed));
    }

    fn clear(&self) {
        self.count.store(0, Ordering::Relaxed);
        self.sum.store(0, Ordering::Relaxed);
        self.min.store(u64::MAX, Ordering::Relaxed);
        self.max.store(0, Ordering::Relaxed);
    }
}

/// One thread number's copy of every ticker and histogram.
#[derive(Debug)]
struct Shard {
    tickers: [AtomicU64; NUM_TICKERS],
    histograms: [HistogramData; NUM_HISTOGRAMS],
}

impl Shard {
    fn new() -> Self {
        // `AtomicU64` is not `Copy`, so `[x; N]` does not apply.
        Self {
            tickers: std::array::from_fn(|_| AtomicU64::new(0)),
            histograms: std::array::from_fn(|_| HistogramData::default()),
        }
    }
}

/// Engine-wide counters and histograms. Constructed by the
/// caller and passed to [`crate::Options::statistics`]. The
/// engine clones the `Arc` into the paths it wants to
/// instrument and updates it with lock-free atomics.
///
/// # Per-thread shards
///
/// Each ticker and histogram is kept once per thread number, each copy on
/// cache lines of its own: one per core, rounded up to a power of two and
/// capped, with threads past the cap sharing copies. An update is one
/// relaxed atomic add on the calling thread's copy (a histogram sample a
/// few), so cores never contend on a line. A read adds the copies up.
///
/// # What a read sees
///
/// - **Exact once quiescent.** With no update running, [`get_ticker`]
///   returns the sum of every amount added since the last [`reset`],
///   modulo 2^64, and a histogram snapshot counts every sample recorded.
///   Two threads sharing a copy lose nothing: every update is an atomic
///   read-modify-write.
/// - **Monotonic while updates run.** A ticker read returns a total
///   between the ticker's total when the read began and its total when the
///   read ended. It is never less than a total an earlier read of the same
///   ticker returned, where "earlier" means that read's result reached this
///   one's thread (the same thread, or through any synchronisation), unless
///   a [`reset`] ran in between.
/// - **Not a snapshot across metrics.** Two tickers read one after the
///   other may each include an update the other does not, and a histogram
///   sample being recorded may show in its count before its sum.
///
/// [`get_ticker`]: Statistics::get_ticker
/// [`reset`]: Statistics::reset
pub struct Statistics {
    /// One per thread number; the length is a power of two.
    shards: Box<[CachePadded<Shard>]>,
    level: StatsLevel,
}

/// What a [`Statistics`] measures, from least to most.
///
/// A timer reads the clock twice per instrumented call (each iterator step,
/// each `get`, each log write) and records a latency sample; a histogram
/// sample is a few atomic updates on each read and commit. A caller that
/// reads only what a lower level keeps pays for nothing above it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum StatsLevel {
    /// Every ticker; no histogram is recorded and no call reads the clock
    /// for one.
    ExceptHistogramOrTimers,
    /// Every ticker and the size histograms ([`Histogram::BytesPerRead`],
    /// [`Histogram::BytesPerWrite`]); no latency histogram is recorded and
    /// no call reads the clock for one.
    ExceptTimers,
    /// Every ticker and every histogram, latencies included.
    #[default]
    All,
}

impl std::fmt::Debug for Statistics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Statistics").finish_non_exhaustive()
    }
}

impl Default for Statistics {
    fn default() -> Self {
        Self::new()
    }
}

impl Statistics {
    /// Construct a fresh `Statistics` with every ticker and
    /// histogram at zero, with one shard per thread number.
    pub fn new() -> Self {
        Self::with_level(StatsLevel::All)
    }

    /// [`Statistics::new`] measuring only what `level` names.
    ///
    /// ```
    /// use std::sync::Arc;
    ///
    /// use regolith::{Db, Histogram, Options, Statistics, StatsLevel, Ticker};
    ///
    /// let stats = Arc::new(Statistics::with_level(StatsLevel::ExceptTimers));
    /// let db = Db::open("counted", Options::memory().statistics(Some(Arc::clone(&stats))))?;
    /// db.put(b"k", b"v")?;
    /// db.get(b"k")?;
    /// assert_eq!(stats.get_ticker(Ticker::KeysRead), 1);
    /// assert_eq!(stats.get_histogram_snapshot(Histogram::DbGet).count, 0);
    /// # Ok::<(), regolith::Error>(())
    /// ```
    pub fn with_level(level: StatsLevel) -> Self {
        Self::with_shards(crate::per_thread::width(), level)
    }

    /// What this measures.
    pub fn level(&self) -> StatsLevel {
        self.level
    }

    /// Whether latency histograms are recorded, and so whether a timed call
    /// reads the clock at all.
    #[inline]
    pub(crate) fn timed(&self) -> bool {
        self.level >= StatsLevel::All
    }

    /// A `Statistics` with `shards` copies (rounded up to a power of two).
    fn with_shards(shards: usize, level: StatsLevel) -> Self {
        let shards = shards.max(1).next_power_of_two();
        Self {
            shards: (0..shards)
                .map(|_| CachePadded::new(Shard::new()))
                .collect(),
            level,
        }
    }

    /// The calling thread's shard.
    #[inline]
    fn shard(&self) -> &Shard {
        &self.shards[crate::per_thread::index() & (self.shards.len() - 1)]
    }

    /// Read the current total of a ticker: the sum of every shard. See
    /// [`Statistics`] for what a read concurrent with updates returns.
    pub fn get_ticker(&self, ticker: Ticker) -> u64 {
        self.shards
            .iter()
            .map(|shard| shard.tickers[ticker as usize].load(Ordering::Relaxed))
            .fold(0, u64::wrapping_add)
    }

    /// Return an immutable snapshot of a histogram's state, merged across
    /// every shard.
    pub fn get_histogram_snapshot(&self, hist: Histogram) -> HistogramSnapshot {
        let mut merged = HistogramSnapshot {
            count: 0,
            sum: 0,
            min: u64::MAX,
            max: 0,
        };
        for shard in self.shards.iter() {
            shard.histograms[hist as usize].fold_into(&mut merged);
        }
        if merged.count == 0 {
            return HistogramSnapshot::default();
        }
        if merged.min == u64::MAX {
            // A sample counted before its minimum landed.
            merged.min = 0;
        }
        merged
    }

    /// Zero every ticker and clear every histogram.
    pub fn reset(&self) {
        for shard in self.shards.iter() {
            for t in &shard.tickers {
                t.store(0, Ordering::Relaxed);
            }
            for h in &shard.histograms {
                h.clear();
            }
        }
    }

    /// Human-readable dump of every ticker and histogram. Meant
    /// for debug output, not machine parsing.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        out.push_str("-- tickers --\n");
        for ticker in Ticker::ALL {
            out.push_str(&format!(
                "{:40} {}\n",
                ticker.name(),
                self.get_ticker(*ticker)
            ));
        }
        out.push_str("-- histograms --\n");
        for hist in ALL_HISTOGRAMS {
            let snap = self.get_histogram_snapshot(*hist);
            out.push_str(&format!(
                "{:40} count={} sum={} min={} max={} avg={}\n",
                hist.name(),
                snap.count,
                snap.sum,
                snap.min,
                snap.max,
                snap.average(),
            ));
        }
        out
    }

    /// Add `amount` to `ticker`: one relaxed `fetch_add` on the calling
    /// thread's shard.
    #[inline]
    pub(crate) fn add(&self, ticker: Ticker, amount: u64) {
        self.shard().tickers[ticker as usize].fetch_add(amount, Ordering::Relaxed);
    }

    /// Record a single sample into `hist`, in the calling thread's shard,
    /// unless this level records no histogram.
    #[inline]
    pub(crate) fn record(&self, hist: Histogram, value: u64) {
        if self.level > StatsLevel::ExceptHistogramOrTimers {
            self.shard().histograms[hist as usize].record(value);
        }
    }

    /// [`Self::add`] into shard `shard` (taken modulo the shard count).
    #[cfg(test)]
    fn add_to(&self, shard: usize, ticker: Ticker, amount: u64) {
        self.shards[shard & (self.shards.len() - 1)].tickers[ticker as usize]
            .fetch_add(amount, Ordering::Relaxed);
    }
}

/// Convenience RAII helper: creates a timer on construction and
/// records the elapsed wall-clock microseconds into `hist` on
/// `Drop`. With no statistics handle, or one at
/// [`StatsLevel::ExceptTimers`], neither reads the clock.
pub(crate) struct TimeScope<'a> {
    /// Start reading in microseconds. `None` when no statistics
    /// handle is installed, and also `None` on a platform with no
    /// monotonic clock: the histogram then takes no sample at all
    /// rather than a zero that reads like a measurement.
    start: Option<u64>,
    stats: Option<&'a Statistics>,
    hist: Histogram,
}

impl<'a> TimeScope<'a> {
    pub(crate) fn new(stats: Option<&'a Statistics>, hist: Histogram) -> Self {
        let stats = stats.filter(|stats| stats.timed());
        Self {
            start: stats.and_then(|_| crate::env::platform_micros()),
            stats,
            hist,
        }
    }
}

impl Drop for TimeScope<'_> {
    fn drop(&mut self) {
        // The clock is read only for a scope that took a start reading.
        let (Some(start), Some(stats)) = (self.start, self.stats) else {
            return;
        };
        if let Some(now) = crate::env::platform_micros() {
            stats.record(self.hist, now.saturating_sub(start));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn concurrent_records_are_all_counted() {
        let stats = std::sync::Arc::new(Statistics::new());
        let threads: Vec<_> = (0..8u64)
            .map(|t| {
                let stats = std::sync::Arc::clone(&stats);
                std::thread::spawn(move || {
                    for i in 0..10_000u64 {
                        stats.record(Histogram::DbGet, t * 10_000 + i + 1);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let snap = stats.get_histogram_snapshot(Histogram::DbGet);
        assert_eq!(snap.count, 80_000);
        assert_eq!(snap.sum, (1..=80_000u64).sum::<u64>());
        assert_eq!(snap.min, 1);
        assert_eq!(snap.max, 80_000);
    }

    /// More threads than shards, so shards are shared: once every writer
    /// has finished, each ticker's total is exactly the sum of its adds.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn ticker_totals_are_exact_once_writers_are_quiescent() {
        const ADDS: u64 = 50_000;
        let stats = std::sync::Arc::new(Statistics::with_shards(4, StatsLevel::All));
        let start = std::sync::Arc::new(std::sync::Barrier::new(16));
        let threads: Vec<_> = (0..16u64)
            .map(|t| {
                let (stats, start) = (std::sync::Arc::clone(&stats), std::sync::Arc::clone(&start));
                std::thread::spawn(move || {
                    start.wait();
                    for i in 0..ADDS {
                        stats.add(Ticker::KeysRead, 1);
                        stats.add(Ticker::BytesRead, t + i);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(stats.get_ticker(Ticker::KeysRead), 16 * ADDS);
        let expected: u64 = (0..16u64)
            .map(|t| (0..ADDS).map(|i| t + i).sum::<u64>())
            .sum();
        assert_eq!(stats.get_ticker(Ticker::BytesRead), expected);
        assert_eq!(
            stats.get_ticker(Ticker::KeysWritten),
            0,
            "untouched tickers stay zero"
        );
    }

    #[test]
    fn a_read_sums_every_shard() {
        let stats = Statistics::with_shards(8, StatsLevel::All);
        for shard in 0..8 {
            stats.add_to(shard, Ticker::WalSyncCount, 1 << shard);
        }
        assert_eq!(stats.get_ticker(Ticker::WalSyncCount), 0xFF);
        stats.reset();
        assert_eq!(
            stats.get_ticker(Ticker::WalSyncCount),
            0,
            "reset clears every shard"
        );
    }

    /// While writers run, no read of a ticker returns less than an earlier
    /// read of it did.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn concurrent_reads_never_go_backwards() {
        let stats = std::sync::Arc::new(Statistics::with_shards(4, StatsLevel::All));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writers: Vec<_> = (0..6)
            .map(|_| {
                let (stats, stop) = (std::sync::Arc::clone(&stats), std::sync::Arc::clone(&stop));
                std::thread::spawn(move || {
                    let mut added = 0u64;
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) || added < 10_000 {
                        stats.add(Ticker::IterNextCount, 3);
                        added += 1;
                    }
                    added
                })
            })
            .collect();
        let mut last = 0u64;
        for _ in 0..20_000 {
            let now = stats.get_ticker(Ticker::IterNextCount);
            assert!(now >= last, "read {now} after reading {last}");
            assert_eq!(now % 3, 0, "a read sums whole adds");
            last = now;
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let total: u64 = writers.into_iter().map(|w| w.join().unwrap()).sum();
        assert!(last <= total * 3);
        assert_eq!(stats.get_ticker(Ticker::IterNextCount), total * 3);
    }

    #[test]
    fn a_histogram_merges_its_shards() {
        let stats = Statistics::with_shards(4, StatsLevel::All);
        for (shard, value) in [(0usize, 7u64), (1, 2), (1, 30), (3, 11)] {
            stats.shards[shard].histograms[Histogram::DbGet as usize].record(value);
        }
        let snap = stats.get_histogram_snapshot(Histogram::DbGet);
        assert_eq!((snap.count, snap.sum, snap.min, snap.max), (4, 50, 2, 30));
        stats.reset();
        assert_eq!(
            stats.get_histogram_snapshot(Histogram::DbGet),
            HistogramSnapshot::default()
        );
    }

    #[test]
    fn an_empty_histogram_snapshots_as_zero_and_reset_empties_it() {
        let stats = Statistics::new();
        let empty = stats.get_histogram_snapshot(Histogram::DbGet);
        assert_eq!((empty.count, empty.sum, empty.min, empty.max), (0, 0, 0, 0));
        stats.record(Histogram::DbGet, 7);
        stats.reset();
        let reset = stats.get_histogram_snapshot(Histogram::DbGet);
        assert_eq!((reset.count, reset.sum, reset.min, reset.max), (0, 0, 0, 0));
        stats.record(Histogram::DbGet, 9);
        let one = stats.get_histogram_snapshot(Histogram::DbGet);
        assert_eq!((one.count, one.sum, one.min, one.max), (1, 9, 9, 9));
    }

    #[test]
    fn ticker_add_and_read() {
        let s = Statistics::new();
        assert_eq!(s.get_ticker(Ticker::BytesWritten), 0);
        s.add(Ticker::BytesWritten, 100);
        s.add(Ticker::BytesWritten, 50);
        assert_eq!(s.get_ticker(Ticker::BytesWritten), 150);
    }

    #[test]
    fn histogram_records_min_max_sum_count() {
        let s = Statistics::new();
        s.record(Histogram::DbGet, 10);
        s.record(Histogram::DbGet, 20);
        s.record(Histogram::DbGet, 5);
        let snap = s.get_histogram_snapshot(Histogram::DbGet);
        assert_eq!(snap.count, 3);
        assert_eq!(snap.sum, 35);
        assert_eq!(snap.min, 5);
        assert_eq!(snap.max, 20);
        assert_eq!(snap.average(), 11);
    }

    #[test]
    fn reset_zeroes_everything() {
        let s = Statistics::new();
        s.add(Ticker::BytesRead, 42);
        s.record(Histogram::FlushTime, 99);
        s.reset();
        assert_eq!(s.get_ticker(Ticker::BytesRead), 0);
        let snap = s.get_histogram_snapshot(Histogram::FlushTime);
        assert_eq!(snap, HistogramSnapshot::default());
    }

    #[test]
    fn ticker_all_lists_every_ticker_in_discriminant_order() {
        assert_eq!(Ticker::ALL.len(), NUM_TICKERS);
        for (slot, ticker) in Ticker::ALL.iter().enumerate() {
            assert_eq!(*ticker as usize, slot, "{} is out of order", ticker.name());
        }
    }

    #[test]
    fn dump_contains_every_ticker_and_histogram_name() {
        let s = Statistics::new();
        let out = s.dump();
        assert!(out.contains("regolith.write.bytes"));
        assert!(out.contains("regolith.block_cache.hit"));
        assert!(out.contains("regolith.read.get_micros"));
        assert!(out.contains("regolith.flush.micros"));
    }

    #[test]
    fn every_metric_name_is_unique_and_surface_prefixed() {
        const SURFACES: [&str; 12] = [
            "write",
            "read",
            "iter",
            "block_cache",
            "bloom",
            "compaction",
            "flush",
            "wal",
            "manifest",
            "snapshot",
            "commit",
            "policy",
        ];
        let mut names: Vec<&str> = Ticker::ALL
            .iter()
            .map(|t| t.name())
            .chain(ALL_HISTOGRAMS.iter().map(|h| h.name()))
            .collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "metric names must be unique");
        for name in names {
            let rest = name
                .strip_prefix("regolith.")
                .unwrap_or_else(|| panic!("{name} must start with `regolith.`"));
            let surface = rest.split('.').next().unwrap();
            assert!(
                SURFACES.contains(&surface),
                "{name} uses unknown surface `{surface}`"
            );
        }
    }

    #[test]
    fn histogram_empty_snapshot_is_default() {
        let s = Statistics::new();
        assert_eq!(
            s.get_histogram_snapshot(Histogram::DbGet),
            HistogramSnapshot::default()
        );
    }

    #[test]
    fn time_scope_records_on_drop() {
        let s = Statistics::new();
        {
            let _t = TimeScope::new(Some(&s), Histogram::DbGet);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let snap = s.get_histogram_snapshot(Histogram::DbGet);
        assert_eq!(snap.count, 1);
        assert!(snap.sum > 0, "scope should have recorded non-zero micros");
    }

    #[test]
    fn time_scope_disabled_is_noop() {
        let _t = TimeScope::new(None, Histogram::DbGet);
        // Nothing to assert - the test is that Drop runs without
        // panicking when the stats handle is absent.
    }

    /// Without timers a scope records no latency, and the tickers and size
    /// histograms still count.
    #[test]
    fn a_level_without_timers_records_no_latency_and_still_counts() {
        let s = Statistics::with_level(StatsLevel::ExceptTimers);
        assert_eq!(s.level(), StatsLevel::ExceptTimers);
        {
            let t = TimeScope::new(Some(&s), Histogram::DbGet);
            assert!(
                t.start.is_none(),
                "no clock read for a scope without timers"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        s.add(Ticker::KeysRead, 1);
        s.record(Histogram::BytesPerRead, 7);
        assert_eq!(s.get_histogram_snapshot(Histogram::DbGet).count, 0);
        assert_eq!(s.get_ticker(Ticker::KeysRead), 1);
        assert_eq!(s.get_histogram_snapshot(Histogram::BytesPerRead).sum, 7);
        assert_eq!(Statistics::new().level(), StatsLevel::All);
    }

    /// The lowest level keeps tickers and no histogram, sizes included.
    #[test]
    fn a_level_without_histograms_keeps_only_tickers() {
        let s = Statistics::with_level(StatsLevel::ExceptHistogramOrTimers);
        {
            let t = TimeScope::new(Some(&s), Histogram::DbGet);
            assert!(t.start.is_none(), "no clock read without histograms");
        }
        s.add(Ticker::KeysRead, 2);
        s.record(Histogram::BytesPerRead, 7);
        assert_eq!(s.get_ticker(Ticker::KeysRead), 2);
        for hist in [Histogram::DbGet, Histogram::BytesPerRead] {
            assert_eq!(s.get_histogram_snapshot(hist).count, 0, "{hist:?}");
        }
    }

    /// A sample skips the updates it cannot change and still lands exactly:
    /// zeros leave the sum alone, and a bound already past the sample is
    /// kept.
    #[test]
    fn samples_inside_the_bounds_keep_count_sum_min_and_max_exact() {
        let s = Statistics::with_shards(1, StatsLevel::All);
        for value in [5, 0, 9, 0, 5, 3, 9] {
            s.record(Histogram::DbGet, value);
        }
        let snap = s.get_histogram_snapshot(Histogram::DbGet);
        assert_eq!((snap.count, snap.sum, snap.min, snap.max), (7, 31, 0, 9));
    }
}
