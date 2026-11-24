//! One builder method per [`Options`] field.
//!
//! Every method takes the options by value and returns them, so a chain reads as
//! `Options::default().durability(..).env(..)`. The fields are crate-private; this is
//! the only way a caller outside the crate sets one.

use super::*;

impl Options {
    /// Write buffer (memtable) size before flush. Must be greater
    /// than zero. Default: 64 MB.
    ///
    /// This bounds the memtable's arena bytes: the node header, tower,
    /// internal key and value of every entry, rounded to alignment. A
    /// memtable's arena reserves at most
    /// `write_buffer_size + max(arena_profile.max_chunk_size,
    /// largest single entry)` between writes, because the engine rotates
    /// as soon as the budget is reached. A single `WriteBatch` larger
    /// than the remaining budget overshoots by that batch's size, so
    /// batch size is the caller's to bound.
    #[must_use]
    pub fn write_buffer_size(mut self, write_buffer_size: usize) -> Self {
        self.write_buffer_size = write_buffer_size;
        self
    }

    /// Chunk sizing policy for the memtable arena. Default:
    /// [`ArenaProfile::SERVER`]; [`Options::embedded`] selects the
    /// small-footprint preset.
    #[must_use]
    pub fn arena_profile(mut self, arena_profile: ArenaProfile) -> Self {
        self.arena_profile = arena_profile;
        self
    }

    /// Data block size in SSTables. Must be greater than zero.
    /// Default: 16 KB.
    #[must_use]
    pub fn block_size(mut self, block_size: usize) -> Self {
        self.block_size = block_size;
        self
    }

    /// Block cache size in bytes for decompressed data blocks.
    /// `0` disables the block cache entirely: nothing is allocated
    /// for it, no block is retained, every read goes to the file,
    /// and the block-cache tickers stay at zero. Default: 512 MB.
    ///
    /// What the cache allocates tracks this budget, not the shard
    /// count: shard maps start empty and each shard caps its entry
    /// count at its share of the budget.
    #[must_use]
    pub fn block_cache_size(mut self, block_cache_size: usize) -> Self {
        self.block_cache_size = block_cache_size;
        self
    }

    /// Base-2 log of the block cache shard count. The block cache
    /// is split into `2^block_cache_num_shard_bits` shards keyed
    /// by `hash(file_id, offset)` so concurrent readers contend
    /// only with other readers that hash to the same shard.
    /// Must be <= [`MAX_BLOCK_CACHE_SHARD_BITS`]. Tiny cache budgets
    /// may use fewer effective shards so each shard has usable
    /// capacity, and a [`Options::block_cache_size`] of 0 makes this
    /// setting irrelevant because no shard is allocated.
    /// Default: 6 (64 shards).
    #[must_use]
    pub fn block_cache_num_shard_bits(mut self, block_cache_num_shard_bits: u32) -> Self {
        self.block_cache_num_shard_bits = block_cache_num_shard_bits;
        self
    }

    /// If `true`, the block cache refuses to admit a single entry
    /// that is larger than one shard's byte capacity; the caller
    /// uses the block directly without caching it. If `false`
    /// (default), an oversized entry evicts everything else in
    /// its shard and is admitted anyway.
    #[must_use]
    pub fn strict_capacity_limit(mut self, strict_capacity_limit: bool) -> Self {
        self.strict_capacity_limit = strict_capacity_limit;
        self
    }

    /// Bloom filter bits per key. Must be in
    /// `1..=MAX_BLOOM_BITS_PER_KEY`. Default: 10.
    #[must_use]
    pub fn bloom_bits_per_key(mut self, bloom_bits_per_key: usize) -> Self {
        self.bloom_bits_per_key = bloom_bits_per_key;
        self
    }

    /// Default block compression codec. Used at every level unless
    /// overridden by [`Options::compression_per_level`]. Default: LZ4.
    #[must_use]
    pub fn compression(mut self, compression: CompressionType) -> Self {
        self.compression = compression;
        self
    }

    /// Per-level compression override. When set, entry `i` selects the
    /// codec for level `i`. Levels beyond the vector's length fall
    /// back to [`Options::compression`]. `None` (default) means "use
    /// the default codec at every level".
    #[must_use]
    pub fn compression_per_level(
        mut self,
        compression_per_level: Option<Vec<CompressionType>>,
    ) -> Self {
        self.compression_per_level = compression_per_level;
        self
    }

    /// Number of L0 SSTables before triggering compaction. Must be
    /// greater than zero. Default: 4.
    #[must_use]
    pub fn l0_compaction_trigger(mut self, l0_compaction_trigger: usize) -> Self {
        self.l0_compaction_trigger = l0_compaction_trigger;
        self
    }

    /// Target size for level 1. Must be greater than zero.
    /// Default: 256 MB.
    #[must_use]
    pub fn level_base_bytes(mut self, level_base_bytes: u64) -> Self {
        self.level_base_bytes = level_base_bytes;
        self
    }

    /// Size multiplier between levels. Must be greater than zero.
    /// Default: 10.
    #[must_use]
    pub fn level_size_multiplier(mut self, level_size_multiplier: u64) -> Self {
        self.level_size_multiplier = level_size_multiplier;
        self
    }

    /// Target SSTable file size during compaction. Must be greater
    /// than zero. Default: 64 MB.
    #[must_use]
    pub fn target_file_size(mut self, target_file_size: u64) -> Self {
        self.target_file_size = target_file_size;
        self
    }

    /// Durability mode. Default: Eventual.
    #[must_use]
    pub fn durability(mut self, durability: DurabilityMode) -> Self {
        self.durability = durability;
        self
    }

    /// Optional user hook invoked during compaction for every point
    /// entry and range tombstone. See [`CompactionFilter`] for
    /// semantics and snapshot-isolation rules.
    #[must_use]
    pub fn compaction_filter(
        mut self,
        compaction_filter: Option<Arc<dyn CompactionFilter>>,
    ) -> Self {
        self.compaction_filter = compaction_filter;
        self
    }

    /// Optional prefix extractor. When set, SSTable writers build an
    /// additional prefix-keyed bloom filter that `Iter::seek_prefix`
    /// consults to skip files that cannot contain the scanned prefix.
    /// Point lookups are unaffected.
    #[must_use]
    pub fn prefix_extractor(mut self, prefix_extractor: Option<Arc<dyn PrefixExtractor>>) -> Self {
        self.prefix_extractor = prefix_extractor;
        self
    }

    /// Optional associative merge operator. When set, callers may
    /// emit merge operands via [`crate::Db::merge`] /
    /// [`crate::WriteBatch::merge`] instead of doing
    /// read-modify-write, and readers collapse the merge chain via
    /// [`MergeOperator::full_merge`] at visibility time. Without one, every
    /// merge write is refused with [`crate::Error::NoMergeOperator`].
    #[must_use]
    pub fn merge_operator(mut self, merge_operator: Option<Arc<dyn MergeOperator>>) -> Self {
        self.merge_operator = merge_operator;
        self
    }

    /// Opt-in flag accepted for parity with storage engines that
    /// require an explicit switch to get atomic multi-CF flushes.
    /// Regolith's column-family implementation is key-prefix based:
    /// every CF shares one memtable, one WAL, one manifest, and
    /// one flush path, so a multi-CF [`crate::WriteBatch`] is
    /// **always** atomic across CFs regardless of this flag's
    /// value. A flush either persists every participant's half of
    /// a batch or persists none of it.
    #[must_use]
    pub fn atomic_flush(mut self, atomic_flush: bool) -> Self {
        self.atomic_flush = atomic_flush;
        self
    }

    /// Event listeners subscribed to engine lifecycle events
    /// (flush, compaction, ingest, background errors). Dispatch
    /// is synchronous on the firing thread - listeners **must not
    /// block or re-enter the database**. See
    /// [`crate::EventListener`] for the full contract.
    ///
    /// Replaces every listener set before. To keep them, use
    /// [`Options::add_listener`].
    #[must_use]
    pub fn listeners(mut self, listeners: Vec<Arc<dyn crate::EventListener>>) -> Self {
        self.listeners = listeners;
        self
    }

    /// Add one event listener after the ones already set, keeping them:
    /// for a caller handed an [`Options`] that may carry listeners of its
    /// own. Listeners are told of an event in the order they were set and
    /// added. [`Options::listeners`] replaces the list instead.
    #[must_use]
    pub fn add_listener(mut self, listener: Arc<dyn crate::EventListener>) -> Self {
        self.listeners.push(listener);
        self
    }

    /// Database-wide transaction callbacks, run for every transaction of a
    /// [`crate::OptimisticTransactionDb`] or [`crate::TransactionDb`] opened
    /// with these options, at every isolation level. See
    /// [`crate::TransactionHooks`] for when each runs and what it may do.
    /// Unset by default, in which case a transaction pays nothing for them.
    #[must_use]
    pub fn transaction_hooks(mut self, hooks: Arc<dyn crate::TransactionHooks>) -> Self {
        self.transaction_hooks = Some(hooks);
        self
    }

    /// Encrypt the database at rest under keys from `provider`. Unset by
    /// default, in which case nothing is encrypted and nothing is paid.
    ///
    /// Every table block, write-ahead log record and manifest batch written
    /// with a provider set is sealed with AES-256-GCM-SIV under
    /// [`crate::KeyProvider::current`]; see [`crate::KeyProvider`] for the
    /// key rules. A database written without encryption opens with a
    /// provider and is sealed as its files are rewritten. An encrypted
    /// database must always be opened with a provider that still provides
    /// every key its files name: without one the open refuses with
    /// [`crate::Error::KeyProviderRequired`], and a missing key refuses it
    /// with [`crate::Error::UnknownKey`]. The provider's current key must be
    /// one it provides, or the open refuses with
    /// [`crate::Error::UnknownKey`].
    ///
    /// Also applies to [`crate::SstFileWriter`], which seals the tables it
    /// builds under the same key.
    #[must_use]
    pub fn key_provider(mut self, provider: Arc<dyn crate::KeyProvider>) -> Self {
        self.key_provider = Some(provider);
        self
    }

    /// Optional statistics sink. When set, every hot path in
    /// the engine updates the provided [`crate::Statistics`]
    /// object with tickers and histograms. The caller polls the
    /// same object to export metrics to their monitoring stack.
    /// `None` (default) short-circuits every instrumentation site
    /// at a branch, so disabled stats cost almost nothing.
    #[must_use]
    pub fn statistics(mut self, statistics: Option<Arc<crate::Statistics>>) -> Self {
        self.statistics = statistics;
        self
    }

    /// Optional rate limiter. When set, flush and compaction output
    /// writes are throttled via [`crate::RateLimiter::request`]
    /// before the engine moves on to the next job, capping the
    /// combined background-I/O rate at the limiter's configured
    /// bytes/second. Foreground (user) writes are not throttled.
    /// `None` (default) means background I/O is uncapped.
    #[must_use]
    pub fn rate_limiter(mut self, rate_limiter: Option<Arc<dyn crate::RateLimiter>>) -> Self {
        self.rate_limiter = rate_limiter;
        self
    }

    /// The number of L0 SSTables from which writes are slowed. No write is
    /// ever delayed. At or past this count two things change: a write
    /// that set [`crate::WriteOptions::no_slowdown`] is refused with
    /// [`crate::Error::Busy`], and on a database with no compaction worker
    /// and [`Options::inline_compaction`] on, each write runs one step of
    /// the flush or compaction that is due. `0` disables this
    /// trigger. If both L0 triggers are enabled, this must be <=
    /// [`Options::level0_stop_writes_trigger`]. Default: 20.
    ///
    /// Level-style, with the same caveat as
    /// [`Options::level0_stop_writes_trigger`]: under FIFO and
    /// universal compaction the L0 file count is not what the picker
    /// reduces, so the count can stay at or past this threshold for
    /// good.
    #[must_use]
    pub fn level0_slowdown_writes_trigger(mut self, level0_slowdown_writes_trigger: usize) -> Self {
        self.level0_slowdown_writes_trigger = level0_slowdown_writes_trigger;
        self
    }

    /// Stop foreground writes entirely when the number of L0
    /// SSTables reaches this threshold. A stopped write applies nothing
    /// and returns [`crate::Error::WouldBlock`] with a
    /// [`crate::StallWait`], which completes once compaction has brought
    /// the count back under the threshold. Plain writes, column-family
    /// writes (including creating and dropping a column family) and
    /// transactional commits that carry writes are stopped alike.
    /// `0` disables this trigger. Default: 36.
    ///
    /// # This is a level-style trigger
    ///
    /// Only [`CompactionStyle::Level`] reduces the L0 *file count*
    /// in response to this threshold. Under [`CompactionStyle::Fifo`]
    /// nothing ever merges L0 files (only the byte cap unlinks them),
    /// and under [`CompactionStyle::Universal`] the picker merges on
    /// its own size-ratio and amplification rules, which a healthy
    /// size tier can satisfy while sitting above this count. With
    /// either of those styles the threshold can therefore be reached
    /// and never relieved, and writes then fail with
    /// [`crate::Error::Busy`] until the configuration changes. The
    /// error message names the style and this field. Set this to `0`
    /// with those styles and bound memory with
    /// [`Options::max_write_buffer_number`] and
    /// [`Options::hard_pending_compaction_bytes_limit`] instead,
    /// which both apply to every style.
    #[must_use]
    pub fn level0_stop_writes_trigger(mut self, level0_stop_writes_trigger: usize) -> Self {
        self.level0_stop_writes_trigger = level0_stop_writes_trigger;
        self
    }

    /// The total bytes in L0 (regolith's approximation of "pending
    /// compaction bytes") from which writes are slowed, exactly as
    /// [`Options::level0_slowdown_writes_trigger`] slows them: no write
    /// is delayed, a [`crate::WriteOptions::no_slowdown`] write is
    /// refused, and with no worker and inline compaction on each write
    /// runs one step. `0` disables this trigger. If both pending-byte
    /// triggers are enabled, this must be <=
    /// [`Options::hard_pending_compaction_bytes_limit`].
    /// Default: 64 GB.
    #[must_use]
    pub fn soft_pending_compaction_bytes_limit(
        mut self,
        soft_pending_compaction_bytes_limit: u64,
    ) -> Self {
        self.soft_pending_compaction_bytes_limit = soft_pending_compaction_bytes_limit;
        self
    }

    /// Stop writes when total bytes in L0 reach this limit, as
    /// [`Options::level0_stop_writes_trigger`] stops them. `0`
    /// disables this trigger. Default: 256 GB.
    #[must_use]
    pub fn hard_pending_compaction_bytes_limit(
        mut self,
        hard_pending_compaction_bytes_limit: u64,
    ) -> Self {
        self.hard_pending_compaction_bytes_limit = hard_pending_compaction_bytes_limit;
        self
    }

    /// Cap on the number of in-memory memtables (active + frozen).
    ///
    /// A write that fills the active memtable seals it, and the
    /// background writes it out to a table: the compaction worker, or,
    /// with none, the write that sealed it once it has committed. When
    /// the flushes before it have not finished and one more sealed
    /// memtable would pass this count, the write that fills the active
    /// memtable writes the oldest out first, so a rotation never leaves
    /// more than this many. `1` writes every sealed memtable out in the
    /// write that sealed it. Memtables that cannot be written out (a
    /// failing flush) still count: past this count writes slow down, and
    /// at `2 * max_write_buffer_number` they stop. `0` disables the cap
    /// and both triggers. Default: 2.
    #[must_use]
    pub fn max_write_buffer_number(mut self, max_write_buffer_number: usize) -> Self {
        self.max_write_buffer_number = max_write_buffer_number;
        self
    }

    /// Compaction strategy. See [`CompactionStyle`] for the
    /// trade-offs. Default: [`CompactionStyle::Level`].
    #[must_use]
    pub fn compaction_style(mut self, compaction_style: CompactionStyle) -> Self {
        self.compaction_style = compaction_style;
        self
    }

    /// Tunables for [`CompactionStyle::Fifo`]. Ignored when the
    /// style is [`CompactionStyle::Level`].
    #[must_use]
    pub fn fifo_compaction_options(
        mut self,
        fifo_compaction_options: FifoCompactionOptions,
    ) -> Self {
        self.fifo_compaction_options = fifo_compaction_options;
        self
    }

    /// Tunables for [`CompactionStyle::Universal`]. Ignored when
    /// the style is not Universal.
    #[must_use]
    pub fn universal_compaction_options(
        mut self,
        universal_compaction_options: UniversalCompactionOptions,
    ) -> Self {
        self.universal_compaction_options = universal_compaction_options;
        self
    }

    /// Number of background threads available for compaction.
    ///
    /// `0` starts no background worker at all. Compaction then runs
    /// on whichever thread asks for it: unless
    /// [`Options::inline_compaction`] was turned off, a writer that
    /// reaches a write-stall threshold runs the compaction that relieves
    /// it itself, since nothing else would. This is the only mode that
    /// works on a single-threaded host such as `wasm32-wasip1`, where
    /// `std::thread::spawn` reports [`std::io::ErrorKind::Unsupported`],
    /// and it is what [`Options::embedded`] selects.
    ///
    /// When `> 1`, multiple non-overlapping compaction jobs can
    /// run concurrently (e.g. L1→L2 at key range `[a,m)` on one
    /// worker while L2→L3 at `[m,z)` runs on another). L0
    /// compactions are exclusive - only one L0 job runs at a
    /// time because L0 files can overlap arbitrarily.
    ///
    /// `1` keeps compaction single-threaded and matches
    /// pre-multi-worker behavior. It is the default off wasm; see
    /// [`DEFAULT_MAX_BACKGROUND_COMPACTIONS`] for why every wasm
    /// target defaults to `0` instead, so that [`crate::Db::open`]
    /// with [`Options::default`] works there too. On wasm any other
    /// value is rejected by [`Options::validate`].
    #[must_use]
    pub fn max_background_compactions(mut self, max_background_compactions: usize) -> Self {
        self.max_background_compactions = max_background_compactions;
        self
    }

    /// Hand the active write-ahead log to the host's writeback as it fills,
    /// on a thread of its own, so that the rotation that seals it has little
    /// left to sync. Off by default.
    ///
    /// A rotation makes the log it seals durable before the next log exists,
    /// inside the step every writer of the database waits on, so syncing a
    /// whole write buffer's worth of log stalls every writer. With this on,
    /// each time the log grows by an eighth of [`Options::write_buffer_size`]
    /// (at least 1 MiB) the thread starts the writeback of what was written,
    /// and the rotation syncs about that eighth. On Linux the writeback is
    /// started with `sync_file_range`. On Apple, where the rotation's sync is
    /// `F_FULLFSYNC` (the pages and the drive's cache), it is a plain `fsync`,
    /// which writes the pages without the cache flush; an error it gets is
    /// reported by the log's next sync. Measured on one Linux machine (NVMe,
    /// btrfs with compression, 32 MiB write buffer): the rotation's stall
    /// went from about 14 to 24 ms to about 2.5 ms. Not measured on Apple.
    /// Durability is unchanged: the writeback makes nothing durable, the
    /// sync at the rotation still does.
    ///
    /// It starts one extra thread, named `regolith-log-writeback`, through
    /// [`Options::env`], only on Linux and Apple targets, and only when a
    /// compaction worker runs ([`Options::max_background_compactions`] above
    /// `0`). The database's thread budget then grows by one. Elsewhere it is
    /// accepted and does nothing.
    #[must_use]
    pub fn log_writeback(mut self, log_writeback: bool) -> Self {
        self.log_writeback = log_writeback;
        self
    }

    /// With no compaction worker ([`Options::max_background_compactions`]
    /// `0`), whether a write a stall stopped runs the step that relieves it.
    /// Left unset, it is on for a database with no worker, which could not
    /// relieve its own stalls otherwise, and off for one that has a worker.
    ///
    /// A write stopped by a stall threshold never waits: it applies nothing
    /// and returns [`crate::Error::WouldBlock`] with a
    /// [`crate::StallWait`]. With a worker, the worker's flushes and
    /// compactions clear the stall. With none, nothing clears it unless a
    /// caller compacts, and this option says who:
    ///
    /// - **On.** The stopped write leaves the step (one flush or one
    ///   compaction pass) on its thread's [`crate::IoQueue`], run at that
    ///   queue's polls, one per poll, until the stall clears; a thread with
    ///   no queue runs the steps inline instead, bounded, and its write goes
    ///   on once they cleared the stall. A slowed write, under a slowdown
    ///   threshold, owes one step the same way.
    /// - **Off**, said explicitly: you drive compaction. A stopped write
    ///   returns its wait, which completes once you cleared the stall with
    ///   [`crate::Db::compact_step`] or [`crate::Db::flush`]. A transaction's
    ///   blocking commit is the one writer that still relieves a stop
    ///   itself, with as many steps as the stop needs and no more, because
    ///   it cannot be made again and has nobody to wait for; commit with
    ///   [`crate::Transaction::commit_nowait`] to keep the thread free.
    ///
    /// Ignored when a worker runs.
    #[must_use]
    pub fn inline_compaction(mut self, inline_compaction: bool) -> Self {
        self.inline_compaction = Some(inline_compaction);
        self
    }

    /// Accepted for compatibility with earlier releases.
    ///
    /// Compaction now streams a k-way merge with bounded memory
    /// and writes outputs on the compaction worker thread, so this
    /// knob does not change behavior.
    #[must_use]
    pub fn max_subcompactions(mut self, max_subcompactions: usize) -> Self {
        self.max_subcompactions = max_subcompactions;
        self
    }

    /// Hint the OS page cache to drop pages backing SSTables
    /// that are read or written by background compaction.
    ///
    /// Without the hint, gigabytes of sequentially-consumed
    /// compaction data pollute the page cache and evict hot
    /// foreground reads. With the hint, the kernel is told to
    /// discard those pages immediately after the compaction
    /// finishes with them, billing the page cache cost strictly
    /// to foreground data.
    ///
    /// Currently implemented as `posix_fadvise(DONTNEED)` on
    /// Linux; on other targets the flag is accepted but the
    /// hint is a no-op. `false` by default - callers who care
    /// about foreground latency stability on Linux should turn
    /// it on.
    #[must_use]
    pub fn evict_compaction_data_from_page_cache(
        mut self,
        evict_compaction_data_from_page_cache: bool,
    ) -> Self {
        self.evict_compaction_data_from_page_cache = evict_compaction_data_from_page_cache;
        self
    }

    /// Split the SSTable index into small leaf blocks on disk and keep
    /// only a compact top-level index in memory. Reduces resident
    /// memory when thousands of SSTables are open, at the cost of one
    /// extra disk read per point lookup (amortized by the OS page
    /// cache). Default: `false` (flat index loaded eagerly).
    #[must_use]
    pub fn partitioned_index(mut self, partitioned_index: bool) -> Self {
        self.partitioned_index = partitioned_index;
        self
    }

    /// Charge SSTable index and filter blocks to the block cache
    /// instead of pinning them in each open reader.
    ///
    /// With this off (the default), every open SSTable holds its whole
    /// index and its bloom filters resident for the reader's lifetime,
    /// outside [`Options::block_cache_size`]. With it on they are read
    /// through the cache and are evictable, so `block_cache_size`
    /// bounds index and filter bytes as well as data bytes. The cost is
    /// one cache lookup on the point-read path and a re-read from disk
    /// whenever an evicted filter is next consulted, which is why the
    /// default is off.
    ///
    /// Independent of [`Options::partitioned_index`]: the *leaves* of a
    /// partitioned index always go through the cache, and a partitioned
    /// file's top-level index is always pinned. This option decides
    /// where a flat file's whole index and every file's filter region
    /// live.
    ///
    /// `Db::get_int_property("regolith.pinned-metadata-bytes")` reports
    /// what the open files are holding outside the cache budget either
    /// way.
    ///
    /// Default: `false`.
    #[must_use]
    pub fn cache_index_and_filter_blocks(mut self, cache_index_and_filter_blocks: bool) -> Self {
        self.cache_index_and_filter_blocks = cache_index_and_filter_blocks;
        self
    }

    /// Target size for each index leaf block when
    /// [`Options::partitioned_index`] is enabled. Must be greater
    /// than zero. Ignored when partitioned indexing is off.
    /// Default: 4096.
    #[must_use]
    pub fn metadata_block_size(mut self, metadata_block_size: usize) -> Self {
        self.metadata_block_size = metadata_block_size;
        self
    }

    /// Open an existing database without creating files, rewriting
    /// recovered WALs, compacting, or allowing writes. Mutating APIs
    /// return [`crate::Error::ReadOnly`].
    ///
    /// Default: `false`.
    #[must_use]
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Maximum user-key length accepted by write APIs. Default: 8 MiB.
    /// A logged write is still refused if its framed record exceeds the
    /// write-ahead log's 1 GiB limit, whatever this allows; a write with
    /// `disable_wal` is exempt from that limit.
    #[must_use]
    pub fn max_key_size(mut self, max_key_size: usize) -> Self {
        self.max_key_size = max_key_size;
        self
    }

    /// Maximum value and merge-operand length accepted by write APIs.
    /// Default: 64 MiB. A logged write is still refused if its framed
    /// record exceeds the write-ahead log's 1 GiB limit, whatever this
    /// allows; a write with `disable_wal` is exempt from that limit.
    #[must_use]
    pub fn max_value_size(mut self, max_value_size: usize) -> Self {
        self.max_value_size = max_value_size;
        self
    }

    /// Keys one transaction buffers before it builds a hash index over
    /// them.
    ///
    /// A transaction keeps its own writes and reads in a linear buffer,
    /// which costs no table and answers a lookup by walking a handful of
    /// entries. Past this many keys the walk stops being the cheap option
    /// and the buffer indexes itself, which costs one table and one entry
    /// per key on top of the buffer itself.
    ///
    /// Set it to the number of keys the workload's transactions actually
    /// touch. Too low and a transaction pays for an index it did not need;
    /// too high and a large transaction walks further than it should. A
    /// value of `0` disables the count-based index, so an ordinary lookup
    /// always walks the list; that suits a workload of uniformly tiny
    /// transactions and is a poor choice for any other. It is not
    /// absolute: a pessimistic transaction that promotes a key through
    /// `get_for_update` and then scans still builds the index on first
    /// need, because a transactional scan cannot afford to walk it once
    /// per yielded key.
    ///
    /// Default: 32.
    #[must_use]
    pub fn transaction_keys_inline(mut self, transaction_keys_inline: usize) -> Self {
        self.transaction_keys_inline = transaction_keys_inline;
        self
    }

    /// The host platform this database runs on: its filesystem, its
    /// clock, and its threads.
    ///
    /// Defaults to [`crate::env::StdEnv`], which is `std::fs` +
    /// `std::time` + `std::thread` and behaves exactly as regolith did
    /// before this field existed. Replace it to run on a filesystem
    /// regolith does not know about, or on [`crate::env::MemEnv`] to keep
    /// a database entirely in memory.
    ///
    /// What the environment can actually do is reported by
    /// [`crate::env::Env::capabilities`] and handed back to callers
    /// through [`crate::Db::capabilities`].
    #[must_use]
    pub fn env(mut self, env: Arc<dyn crate::env::Env>) -> Self {
        self.env = env;
        self
    }

    /// Most SSTables that keep a file descriptor open at once. `0`, the
    /// default, keeps every live table's file open, one descriptor per
    /// table.
    ///
    /// A store needs a descriptor per live table, plus those a compaction
    /// is reading and writing, so a large store or a process with a low
    /// `RLIMIT_NOFILE` (256 under macOS launchd) runs out and compaction
    /// fails with EMFILE. With a limit, the least recently read tables
    /// have their descriptor closed and reopened on the next read; a
    /// read that misses the block cache on such a table pays an `open`.
    ///
    /// The limit is hard: SSTable descriptors never number more than
    /// `max_open_files`, and none is closed while a read uses it. A table
    /// a compaction deletes while a snapshot or iterator still reads it is
    /// renamed aside rather than unlinked, so its descriptor stays closable
    /// and reopens under the new name; the last reader unlinks it, and a
    /// crash leaves it for the next writable open to remove.
    ///
    /// When more threads read distinct tables at once than there are
    /// descriptors, what a read does depends on its
    /// [`ReadMode`](crate::ReadMode):
    ///
    /// - A `Blocking` read, which chose to block, may wait for the reads
    ///   running on one descriptor to finish, and then reopens its table
    ///   there.
    /// - A `CacheOnly` read never waits. It never reopens a table itself:
    ///   its miss goes to its I/O queue like any other device read. When the
    ///   queue's [`poll`](crate::IoQueue::poll) runs that read and finds
    ///   every descriptor in use by reads running on other threads, the read
    ///   stays pending on the queue and the poll returns. The first of those
    ///   reads to finish tells the queue, waking its owner if it registered
    ///   [`idle_waker`](crate::IoQueue::idle_waker), and the next poll runs
    ///   the read again. A pending read is retried each time a descriptor
    ///   frees, but `Blocking` reads that claim the freed descriptor first
    ///   can keep it pending for as long as they do.
    #[must_use]
    pub fn max_open_files(mut self, max_open_files: usize) -> Self {
        self.max_open_files = max_open_files;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rate_limiter::TokenBucketRateLimiter;

    struct KeepAll;

    impl CompactionFilter for KeepAll {
        fn filter(&self, _level: usize, _key: &[u8], _value: &[u8]) -> CompactionDecision {
            CompactionDecision::Keep
        }

        fn name(&self) -> &'static str {
            "keep-all"
        }
    }

    struct Concat;

    impl MergeOperator for Concat {
        fn full_merge(
            &self,
            _key: &[u8],
            _base: Option<&[u8]>,
            _operands: &[&[u8]],
        ) -> Option<Vec<u8>> {
            None
        }

        fn name(&self) -> &'static str {
            "concat"
        }
    }

    struct Quiet;

    impl crate::EventListener for Quiet {}

    /// Each builder must write its own field and no other: set one option to a
    /// value the defaults do not hold, check that field, then check the option
    /// set differs from the defaults in that field alone.
    macro_rules! each_builder_sets_its_own_field {
        ($($field:ident = $value:expr),* $(,)?) => {
            $({
                let defaults = Options::default();
                let built = Options::default().$field($value);
                assert_ne!(defaults.$field, built.$field, stringify!($field));
                assert_eq!(built.$field, $value, stringify!($field));
                let rest = Options { $field: defaults.$field.clone(), ..built };
                assert_eq!(
                    format!("{rest:?}"),
                    format!("{defaults:?}"),
                    "{} changed another option",
                    stringify!($field)
                );
            })*
        };
    }

    #[test]
    fn plain_value_builders_set_their_field() {
        each_builder_sets_its_own_field! {
            write_buffer_size = 4096,
            arena_profile = ArenaProfile::EMBEDDED,
            block_size = 1024,
            block_cache_size = 1024,
            block_cache_num_shard_bits = 2,
            strict_capacity_limit = true,
            bloom_bits_per_key = 3,
            compression = CompressionType::Snappy,
            compression_per_level = Some(vec![CompressionType::None]),
            l0_compaction_trigger = 9,
            level_base_bytes = 1 << 20,
            level_size_multiplier = 7,
            target_file_size = 1 << 20,
            durability = DurabilityMode::Immediate,
            atomic_flush = true,
            level0_slowdown_writes_trigger = 5,
            level0_stop_writes_trigger = 6,
            soft_pending_compaction_bytes_limit = 1 << 20,
            hard_pending_compaction_bytes_limit = 1 << 21,
            max_write_buffer_number = 3,
            compaction_style = CompactionStyle::Universal,
            fifo_compaction_options = FifoCompactionOptions { max_table_files_size: 1 << 20 },
            universal_compaction_options = UniversalCompactionOptions {
                size_ratio: 5,
                ..UniversalCompactionOptions::default()
            },
            max_background_compactions = 4,
            max_subcompactions = 2,
            evict_compaction_data_from_page_cache = true,
            partitioned_index = true,
            cache_index_and_filter_blocks = true,
            metadata_block_size = 512,
            read_only = true,
            max_key_size = 1024,
            max_value_size = 2048,
            transaction_keys_inline = 3,
            max_open_files = 64,
        }
    }

    #[test]
    fn shared_handle_builders_set_their_field() {
        let options = Options::default()
            .compaction_filter(Some(Arc::new(KeepAll)))
            .prefix_extractor(Some(Arc::new(FixedLengthPrefix(2))))
            .merge_operator(Some(Arc::new(Concat)))
            .listeners(vec![Arc::new(Quiet)])
            .statistics(Some(Arc::new(crate::Statistics::new())))
            .rate_limiter(Some(Arc::new(TokenBucketRateLimiter::new(
                1 << 20,
                1 << 20,
            ))));
        assert_eq!(options.compaction_filter.unwrap().name(), "keep-all");
        assert_eq!(
            options.prefix_extractor.unwrap().name(),
            "FixedLengthPrefix(2)"
        );
        assert_eq!(options.merge_operator.unwrap().name(), "concat");
        assert_eq!(options.listeners.len(), 1);
        assert!(options.statistics.is_some());
        assert!(options.rate_limiter.is_some());
    }

    #[test]
    fn the_env_builder_replaces_the_environment() {
        let env: Arc<dyn crate::env::Env> = Arc::new(crate::MemEnv::new());
        let options = Options::default().env(Arc::clone(&env));
        assert!(Arc::ptr_eq(&options.env, &env));
    }
}
