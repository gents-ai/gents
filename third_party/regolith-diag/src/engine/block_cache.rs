use std::sync::{Arc, OnceLock, Weak};

// Through the portability shim: a 32-bit target without a 64-bit atomic
// instruction gets the fallback implementation rather than failing to build.
use crate::portability::{AtomicPtr, AtomicUsize, Ordering};

use kovan_map::HopscotchMap;

use xxhash_rust::xxh3::xxh3_64;

use super::block::Block;
use super::filter_block::FilterBlock;
use super::index_block::IndexBlock;
use super::io::IoRuntime;
use crate::options::MAX_BLOCK_CACHE_SHARD_BITS;
use crate::statistics::{Statistics, Ticker};

mod ring;

use ring::{Claim, Hand, Node, Ring};

/// Cache key: (file_id, block_offset).
#[derive(Hash, Eq, PartialEq, Clone, Copy, Debug)]
struct CacheKey {
    file_id: u64,
    offset: u64,
}

/// Hard upper bound on the number of shards the cache will ever
/// create. A 32-bit shard-bit config of 8 → 256 shards is plenty
/// for a single-process embedded store.
const MAX_SHARD_BITS: u32 = MAX_BLOCK_CACHE_SHARD_BITS;

/// Minimum per-shard capacity. Tiny caches with many shards
/// would otherwise produce shards with 0 bytes of capacity, which
/// is almost certainly a misconfiguration: fall back to a single
/// shard in that case.
const MIN_SHARD_CAPACITY: usize = 64 * 1024;

/// Bookkeeping bytes an entry costs beyond [`Block::charge`]: the map
/// node with its embedded reclamation header and the weak reference it
/// holds, the ring's [`Node`] allocation, and the ring slot.
///
/// Every live entry is charged at least this, which is also what bounds
/// the ring: a shard never has more slots in use than its bytes allow.
/// `tests/adv_block_cache_overhead.rs` re-measures the real bookkeeping
/// with a counting allocator on every run and fails if it drifts more
/// than 16 bytes past this charge, so the constant cannot rot quietly.
const ENTRY_OVERHEAD: usize = 160;

/// Bytes per entry assumed when sizing a shard's bucket array: the
/// default `block_size` plus [`ENTRY_OVERHEAD`]. Only a starting
/// estimate; the map grows past a 0.75 load factor on its own and never
/// shrinks below what it was built with.
const ESTIMATED_ENTRY_BYTES: usize = 4 * 1024 + ENTRY_OVERHEAD;

/// The most hand steps one revolution of an insert's search takes. A shard
/// with more entries than this sweeps it per insert rather than the whole
/// ring, so an insert into a large shard that readers have mostly pinned
/// costs at most `2 * SWEEP + 1` steps before it is refused.
const SWEEP: usize = 4096;

/// Floor on a shard's bucket count. A cache small enough to estimate
/// fewer entries than this still gets a table worth hashing into.
const MIN_MAP_BUCKETS: usize = 64;

/// Ceiling on a shard's bucket count, so a very large budget with very
/// small blocks cannot make the bucket array itself the dominant cost.
/// At 8 bytes a bucket this is 512 KiB per shard; past it the map grows
/// on demand instead.
const MAX_MAP_BUCKETS: usize = 64 * 1024;

/// What a cache slot holds: the cache's own strong reference to a block.
///
/// Data blocks, index blocks and filter blocks share one key space
/// because they occupy disjoint byte ranges of the same file: the
/// SSTable layout writes data blocks, then the range-tombstone block,
/// then the filter region, then the index, so no two of them can start
/// at the same offset.
enum CacheEntry {
    Data(Arc<Block>),
    Index(Arc<IndexBlock>),
    Filter(Arc<FilterBlock>),
}

/// The map's handle on an entry: weak, so a reader that finds it takes
/// its pin by upgrading, and an eviction that wins the pin check leaves
/// nothing for a later upgrade to reach.
#[derive(Clone)]
enum WeakEntry {
    Data(Weak<Block>),
    Index(Weak<IndexBlock>),
    Filter(Weak<FilterBlock>),
}

impl CacheEntry {
    fn payload_charge(&self) -> usize {
        match self {
            Self::Data(block) => block.charge(),
            Self::Index(block) => block.charge(),
            Self::Filter(block) => block.charge(),
        }
    }

    fn clone_ref(&self) -> Self {
        match self {
            Self::Data(b) => Self::Data(Arc::clone(b)),
            Self::Index(b) => Self::Index(Arc::clone(b)),
            Self::Filter(b) => Self::Filter(Arc::clone(b)),
        }
    }

    fn downgrade(&self) -> WeakEntry {
        match self {
            Self::Data(b) => WeakEntry::Data(Arc::downgrade(b)),
            Self::Index(b) => WeakEntry::Index(Arc::downgrade(b)),
            Self::Filter(b) => WeakEntry::Filter(Arc::downgrade(b)),
        }
    }

    /// Give up the cache's reference, but only when no reader holds the
    /// block: the pin check and the retraction are one compare-and-swap of
    /// the block's strong count from one (the cache's) to zero, after which
    /// no reader can take a new pin through the map. `Err` hands the
    /// reference back when a reader holds one.
    fn release_if_unpinned(self) -> Result<(), Self> {
        match self {
            Self::Data(b) => Arc::try_unwrap(b).map(drop).map_err(Self::Data),
            Self::Index(b) => Arc::try_unwrap(b).map(drop).map_err(Self::Index),
            Self::Filter(b) => Arc::try_unwrap(b).map(drop).map_err(Self::Filter),
        }
    }
}

impl WeakEntry {
    /// Pin the entry for a reader, if the cache still holds it.
    fn upgrade(&self) -> Option<CacheEntry> {
        match self {
            Self::Data(b) => b.upgrade().map(CacheEntry::Data),
            Self::Index(b) => b.upgrade().map(CacheEntry::Index),
            Self::Filter(b) => b.upgrade().map(CacheEntry::Filter),
        }
    }
}

fn entry_charge(entry: &CacheEntry) -> usize {
    entry.payload_charge() + ENTRY_OVERHEAD
}

/// The map's value for a key: the weak handle readers pin through, and
/// the `(slot, generation)` that names the ring entry holding the strong
/// reference.
#[derive(Clone)]
struct Indexed {
    entry: WeakEntry,
    slot: u32,
    generation: u32,
}

impl Indexed {
    fn names(&self, slot: usize, generation: u32) -> bool {
        self.slot as usize == slot && self.generation == generation
    }
}

/// A shard's structures, built on its first insert.
struct ShardState {
    map: HopscotchMap<CacheKey, Indexed>,
    ring: Ring,
}

/// One shard: its byte budget, the bytes charged against it, and its
/// lazily built map and ring.
struct CacheShard {
    /// Byte budget for this shard: total capacity / num_shards.
    capacity: usize,
    /// Bytes charged by this shard's entries and by inserts that have
    /// reserved but not yet published. Reserved by compare-and-swap, so
    /// it never passes `capacity` except by an oversized entry the
    /// cache-wide budget admitted.
    used: AtomicUsize,
    /// Built on the shard's first insert and freed with the shard. An
    /// empty shard costs this pointer and the two words above, so the
    /// cache's footprint follows its budget, not its shard count.
    state: AtomicPtr<ShardState>,
    /// The most ring slots the shard may use: what the whole cache's
    /// budget allows at [`ENTRY_OVERHEAD`] an entry.
    max_slots: usize,
}

impl CacheShard {
    fn new(capacity: usize, max_slots: usize) -> Self {
        Self {
            capacity,
            used: AtomicUsize::new(0),
            state: AtomicPtr::new(std::ptr::null_mut()),
            max_slots,
        }
    }

    #[allow(unsafe_code)]
    fn state(&self) -> Option<&ShardState> {
        let state = self.state.load(Ordering::Acquire);
        // SAFETY: a non-null pointer is the `Box<ShardState>` installed by
        // `state_or_init`, freed only by `Drop` with `&mut self`.
        (!state.is_null()).then(|| unsafe { &*state })
    }

    /// This shard's structures, built on first use. Lock-free: racing
    /// builders each make one and all but the first drop theirs.
    ///
    /// The map is sized from the shard's own byte budget rather than left
    /// at the map's default 524,288 buckets, which at 8 bytes a bucket
    /// would cost 4 MiB per shard before holding a single block.
    #[allow(unsafe_code)]
    fn state_or_init(&self) -> &ShardState {
        if let Some(state) = self.state() {
            return state;
        }
        let estimate =
            (self.capacity / ESTIMATED_ENTRY_BYTES).clamp(MIN_MAP_BUCKETS, MAX_MAP_BUCKETS);
        let built = Box::into_raw(Box::new(ShardState {
            map: HopscotchMap::with_capacity(estimate),
            ring: Ring::new(self.max_slots),
        }));
        match self.state.compare_exchange(
            std::ptr::null_mut(),
            built,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            // SAFETY: just installed; freed only by `Drop`.
            Ok(_) => unsafe { &*built },
            Err(winner) => {
                // SAFETY: `built` was never published, so this is its only
                // owner; `winner` is the installed state, as in `state`.
                drop(unsafe { Box::from_raw(built) });
                unsafe { &*winner }
            }
        }
    }

    /// Look up `key` and pin its block, giving the entry a second chance
    /// against the hand.
    ///
    /// Lock-free and wait-free past the map: one map read under the map's
    /// own reclamation guard, one upgrade of the weak handle (the pin), and
    /// one load of the slot word, plus one CAS the first time the entry is
    /// read after the hand passed it.
    fn get(&self, key: &CacheKey) -> Option<CacheEntry> {
        let state = self.state()?;
        let found = state.map.get(key)?;
        let entry = found.entry.upgrade()?;
        state.ring.touch(found.slot as usize, found.generation);
        Some(entry)
    }
}

impl Drop for CacheShard {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        let state = self.state.load(Ordering::Acquire);
        if !state.is_null() {
            // SAFETY: installed by `state_or_init` from `Box::into_raw`,
            // and `&mut self` excludes every other user.
            drop(unsafe { Box::from_raw(state) });
        }
    }
}

/// Reserve `size` against `counter` unless that would take it past
/// `limit`. One compare-and-swap per attempt; retried only when another
/// thread changed the counter.
fn bounded_add(counter: &AtomicUsize, size: usize, limit: usize) -> bool {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let Some(after) = current.checked_add(size).filter(|after| *after <= limit) else {
            return false;
        };
        match counter.compare_exchange_weak(current, after, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(actual) => current = actual,
        }
    }
}

/// Sharded, lock-free CLOCK block cache for decompressed SSTable blocks.
///
/// The cache is split into `2^shard_bits` independent shards keyed by
/// `xxh3(file_id, offset)`. Each shard holds a lock-free hash map from key
/// to entry and a lock-free CLOCK ring of the entries themselves
/// (`block_cache/ring.rs`). No path takes a lock: a hit, an insert, an
/// eviction, `evict_file` and `clear` are compare-and-swaps on the map, the
/// ring's slot words and two byte counters.
///
/// # Reads
///
/// A hit is one map read, one upgrade of the map's weak handle to the
/// block, which is the reader's pin, and one load of the entry's slot word,
/// plus one CAS to set the reference bit the first time the entry is read
/// after the hand passed it. Readers never wait for an insert or an
/// eviction.
///
/// CLOCK approximates LRU: the reference bit ranks entries into "touched
/// since the hand last passed" or not, where LRU ranks them exactly. On the
/// traces replayed for this cache (zipfian point reads, zipfian plus a
/// compaction sweep, and an LSM level-shaped mix, at four budgets each) it
/// lands within a point of LRU on every one of them. It does not fix the
/// cyclic-sweep pathology: on a working set 1.5x the budget both policies
/// score zero.
///
/// # Eviction and pins
///
/// An insert that does not fit runs its shard's hand: one step inspects one
/// slot, claimed by one CAS, so concurrent inserters inspect different
/// slots. The first revolution clears reference bits; from the second the
/// hand takes whatever it lands on, so a reader that keeps re-setting bits
/// costs at most one extra miss and never stalls an insert.
///
/// **A block a reader holds is never evicted.** The cache holds the only
/// strong reference to a block it caches; a reader's pin is a clone of it.
/// The hand evicts by one compare-and-swap of that strong count from one to
/// zero, which fails while any reader holds the block and leaves the entry
/// in place, and after which no reader can pin it. An entry the hand cannot
/// evict is passed over. An insert that finds nothing it may evict within
/// two revolutions is refused, and the caller uses its block uncached.
///
/// Three removals are explicit rather than CLOCK's and do not wait for
/// readers, because the entry they drop is obsolete: a re-insert of the
/// same key replaces it, `evict_file` drops a deleted table's blocks and
/// `clear` drops everything. A reader holding such a block keeps its own
/// reference; the cache stops counting it.
///
/// # Capacity
///
/// `Options::block_cache_size` is the total byte budget, and it is a hard
/// bound on [`BlockCache::usage`], the figure the cache accounts and the
/// `regolith.block-cache-usage` property publishes: every insert reserves
/// its charge against the cache-wide total by CAS before it reserves
/// against its shard, so neither can be raced past its bound, and the sum
/// of the shards never exceeds the total. Pinned blocks stay charged until
/// their entries go, so the budget bounds every cached block a reader is
/// using too. The budget is split evenly across shards; each shard runs its
/// own hand as inserts would push it over its share.
///
/// An entry larger than one shard's share is handled by
/// [`Options::strict_capacity_limit`]:
///
/// * `false` (default): the per-shard split is a soft target. The entry is
///   reserved against the cache-wide budget and the shard is emptied of
///   every entry no reader holds, so no number of shards can add up past
///   `block_cache_size`. An entry larger than the whole budget is never
///   cached.
/// * `true`: the shard refuses the insert and leaves the caller to use the
///   block directly; nothing is cached.
///
/// A budget of 0 disables the cache: no shard is allocated, every `get`
/// misses, every `insert` is dropped, and the block-cache tickers stay at
/// zero.
///
/// # Allocation
///
/// Everything the cache allocates is driven by the byte budget, never by
/// the shard count: a shard's map and ring are not created until the
/// shard's first insert, the map's bucket array is then sized from that
/// shard's share of the budget, the ring grows in doubling segments as
/// entries arrive, and each entry is charged [`Block::charge`] plus
/// [`ENTRY_OVERHEAD`] for its bookkeeping. An empty shard costs four words.
///
/// The ring's segments stay allocated once grown, `clear` included: a
/// lock-free ring cannot free a segment another thread may be indexing.
/// They hold at most twice the peak number of live entries at 16 bytes a
/// slot, which the budget bounds.
///
/// The map defers reclaiming a removed entry's node until no reader can
/// still be traversing it. The node holds the key and a weak handle, so
/// what it defers is a few dozen bytes and never the block: the block's
/// bytes are freed at the eviction, by the compare-and-swap that wins the
/// pin check.
pub(crate) struct BlockCache {
    shards: Box<[CacheShard]>,
    /// Total capacity across all shards, in bytes. Kept
    /// separately so `usage_and_capacity` can answer quickly
    /// without summing per-shard.
    capacity: usize,
    /// Shard mask = `num_shards - 1`. `num_shards` is always a
    /// power of two so `hash & mask` picks the shard.
    shard_mask: u64,
    /// Number of shards (always `shard_mask + 1`). Only referenced
    /// by tests that want to confirm the configured shard count;
    /// production paths go through `shard_mask` directly.
    #[cfg(test)]
    num_shards: usize,
    /// Bytes reserved across all shards. Every reservation is made here
    /// first and every release taken from here last, so this is never
    /// below the sum of the shards and never above `capacity`.
    total_used: AtomicUsize,
    /// Whether strict capacity is enforced. See struct doc.
    strict: bool,
    /// Optional statistics sink. When set, every `get` and
    /// `insert` call increments the corresponding tickers.
    stats: Option<Arc<Statistics>>,
    /// The reads in flight for `CacheOnly` handles and the queues they
    /// complete on: the cache's misses that are not read yet. Made by the
    /// first `Db::io_queue`, so a database that never opens one pays nothing.
    io: OnceLock<IoRuntime>,
}

impl BlockCache {
    /// Create a new block cache with the given capacity in bytes
    /// and default sharding and strictness.
    #[cfg(test)]
    pub(crate) fn new(capacity_bytes: usize) -> Self {
        Self::with_config(capacity_bytes, 6, false)
    }

    /// Create a new block cache with an explicit byte budget,
    /// shard-bits, and strictness configuration. `shard_bits` is
    /// clamped to `[0, MAX_SHARD_BITS]`.
    ///
    /// A `capacity_bytes` of 0 builds a disabled cache: no shard is
    /// allocated, nothing is stored, and `get` always misses.
    pub(crate) fn with_config(
        capacity_bytes: usize,
        shard_bits: u32,
        strict_capacity_limit: bool,
    ) -> Self {
        if capacity_bytes == 0 {
            return Self {
                shards: Vec::new().into_boxed_slice(),
                capacity: 0,
                shard_mask: 0,
                #[cfg(test)]
                num_shards: 0,
                total_used: AtomicUsize::new(0),
                strict: strict_capacity_limit,
                stats: None,
                io: OnceLock::new(),
            };
        }
        let shard_bits = shard_bits.min(MAX_SHARD_BITS);
        let mut num_shards: usize = 1usize << shard_bits;
        // Fall back to fewer shards if splitting would leave
        // every shard below the minimum useful capacity.
        while num_shards > 1 && capacity_bytes / num_shards < MIN_SHARD_CAPACITY {
            num_shards /= 2;
        }
        let per_shard = capacity_bytes / num_shards;
        let capacity = per_shard * num_shards;
        // A shard holds at most what the whole budget can pay for: its own
        // share, or more only through oversized entries the cache-wide
        // reservation admitted.
        let max_slots = capacity / ENTRY_OVERHEAD + 2;
        let shards: Box<[CacheShard]> = (0..num_shards)
            .map(|_| CacheShard::new(per_shard, max_slots))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            shards,
            capacity,
            shard_mask: (num_shards - 1) as u64,
            #[cfg(test)]
            num_shards,
            total_used: AtomicUsize::new(0),
            strict: strict_capacity_limit,
            stats: None,
            io: OnceLock::new(),
        }
    }

    /// The table of reads in flight for `CacheOnly` handles, made on first
    /// use.
    pub(crate) fn io(&self) -> &IoRuntime {
        self.io.get_or_init(IoRuntime::new)
    }

    /// The table of reads in flight, if anything made it: a database that
    /// never opened a queue or read through one has none.
    pub(crate) fn opened_io(&self) -> Option<&IoRuntime> {
        self.io.get()
    }

    /// `close`: hand every queue waiting on a read its completion now. A
    /// database that never opened a queue has nothing to do.
    pub(crate) fn close_io(&self) {
        if let Some(io) = self.io.get() {
            io.close();
        }
    }

    /// A close that failed: reads through queues make units again.
    pub(crate) fn reopen_io(&self) {
        if let Some(io) = self.io.get() {
            io.reopen();
        }
    }

    /// Attach an optional statistics sink. Called once at engine
    /// open after the cache has been constructed; subsequent
    /// `get` / `insert` calls will update the provided tickers.
    pub(crate) fn with_stats(mut self, stats: Option<Arc<Statistics>>) -> Self {
        self.stats = stats;
        self
    }

    /// Hash a cache key down to a shard index.
    fn shard_index(&self, key: &CacheKey) -> usize {
        let mut buf = [0u8; 16];
        buf[..8].copy_from_slice(&key.file_id.to_le_bytes());
        buf[8..].copy_from_slice(&key.offset.to_le_bytes());
        (xxh3_64(&buf) & self.shard_mask) as usize
    }

    /// Look one slot up and project out the requested payload kind. A
    /// disabled cache (`block_cache_size` of 0) always misses and records
    /// nothing: there was no cache lookup to count.
    ///
    /// A slot holding another kind reads as a miss and is left in place:
    /// the key space is disjoint by construction, so a mismatch means a
    /// corrupt or aliased offset, not a stale entry worth evicting. It is
    /// counted as a miss too, because the caller got nothing back.
    fn lookup<T>(
        &self,
        file_id: u64,
        offset: u64,
        project: fn(CacheEntry) -> Option<Arc<T>>,
    ) -> Option<Arc<T>> {
        if self.shards.is_empty() {
            return None;
        }
        let key = CacheKey { file_id, offset };
        let idx = self.shard_index(&key);
        let hit = self.shards[idx].get(&key).and_then(project);
        if let Some(s) = self.stats.as_deref() {
            if hit.is_some() {
                s.add(Ticker::BlockCacheHit, 1);
            } else {
                s.add(Ticker::BlockCacheMiss, 1);
            }
        }
        crate::perf_context::record_block_cache_lookup(hit.is_some());
        hit
    }

    /// Try to get a data block. The returned `Arc` pins the block: the
    /// cache will not evict it while it is held.
    pub(crate) fn get(&self, file_id: u64, offset: u64) -> Option<Arc<Block>> {
        self.lookup(file_id, offset, |entry| match entry {
            CacheEntry::Data(block) => Some(block),
            _ => None,
        })
    }

    /// Try to get an SSTable index block, pinned as [`Self::get`] pins.
    pub(crate) fn get_index(&self, file_id: u64, offset: u64) -> Option<Arc<IndexBlock>> {
        self.lookup(file_id, offset, |entry| match entry {
            CacheEntry::Index(block) => Some(block),
            _ => None,
        })
    }

    /// Try to get an SSTable filter block, pinned as [`Self::get`] pins.
    pub(crate) fn get_filter(&self, file_id: u64, offset: u64) -> Option<Arc<FilterBlock>> {
        self.lookup(file_id, offset, |entry| match entry {
            CacheEntry::Filter(block) => Some(block),
            _ => None,
        })
    }

    pub(crate) fn insert_index(&self, file_id: u64, offset: u64, block: Arc<IndexBlock>) -> bool {
        self.store(file_id, offset, CacheEntry::Index(block))
    }

    pub(crate) fn insert_filter(&self, file_id: u64, offset: u64, block: Arc<FilterBlock>) -> bool {
        self.store(file_id, offset, CacheEntry::Filter(block))
    }

    /// Insert a block into the cache. The block may be evicted
    /// before it is next read, especially under memory pressure.
    /// The function signature deliberately takes ownership of the
    /// `Arc`: the caller's clone is the one they continue to
    /// use, and the cache's copy is managed internally. A disabled
    /// cache (`block_cache_size` of 0) drops the block.
    ///
    /// Every strong reference to the block beyond the cache's own counts as
    /// a reader's pin, so the caller's clone pins the entry for as long as
    /// the caller keeps it, and one `Arc` cached under two keys pins both
    /// entries until one of them is removed. Each block read from a table
    /// is its own allocation, so the engine never does the latter.
    pub(crate) fn insert(&self, file_id: u64, offset: u64, block: Arc<Block>) {
        self.store(file_id, offset, CacheEntry::Data(block));
    }

    /// Admit one entry of any kind, honouring the byte budget and
    /// `strict_capacity_limit`. Returns whether it was cached.
    fn store(&self, file_id: u64, offset: u64, entry: CacheEntry) -> bool {
        if self.shards.is_empty() {
            return false;
        }
        let key = CacheKey { file_id, offset };
        let size = entry_charge(&entry);
        let shard = &self.shards[self.shard_index(&key)];
        let reserved = if size <= shard.capacity {
            self.admit(shard, size)
        } else if self.strict || size > self.capacity {
            // Too big for one shard, and either strict mode or too big for
            // the whole cache. Nothing was reserved or touched.
            false
        } else {
            self.admit_oversized(shard, size)
        };
        let stored = reserved && self.install(shard, key, entry, size);
        // Counted only when the block was actually cached: the ticker
        // documents itself as one per miss that populated the cache, and
        // a refusal populates nothing.
        if stored && let Some(s) = self.stats.as_deref() {
            s.add(Ticker::BlockCacheAdd, 1);
        }
        stored
    }

    /// Reserve `size` for an entry that fits its shard's share, running the
    /// shard's hand until it fits. `false` when two sweeps found nothing
    /// the hand may evict: every entry it reached pinned by a reader, or
    /// held by another inserter's hand.
    ///
    /// A sweep is one revolution of the ring, capped at [`SWEEP`] steps: the
    /// first honours reference bits, the second takes whatever is not
    /// pinned. The cap bounds what one insert pays when most of a large
    /// shard is pinned; the hand is shared, so the inserts that follow
    /// carry on round the ring from where this one stopped.
    fn admit(&self, shard: &CacheShard, size: usize) -> bool {
        let state = shard.state_or_init();
        let mut steps = 0usize;
        loop {
            if self.reserve(shard, size) {
                return true;
            }
            let sweep = state.ring.high_water().min(SWEEP);
            if sweep == 0 || steps > 2 * sweep {
                self.refused_held();
                return false;
            }
            self.hand_step(shard, state, steps >= sweep);
            steps += 1;
        }
    }

    /// Reserve `size` for an entry larger than its shard's share: against
    /// the cache-wide budget only, emptying the shard of every entry no
    /// reader holds. `false` when even an emptied shard would not make room.
    fn admit_oversized(&self, shard: &CacheShard, size: usize) -> bool {
        let state = shard.state_or_init();
        if !bounded_add(&self.total_used, size, self.capacity) {
            // Only worth emptying the shard when what the other shards hold
            // leaves room; an estimate, the reservation below decides.
            let others = self
                .total_used
                .load(Ordering::Acquire)
                .saturating_sub(shard.used.load(Ordering::Acquire));
            if others.saturating_add(size) > self.capacity {
                return false;
            }
            self.evict_unpinned(shard, state);
            if !bounded_add(&self.total_used, size, self.capacity) {
                // What the emptied shard still holds is held by readers.
                self.refused_held();
                return false;
            }
        } else {
            self.evict_unpinned(shard, state);
        }
        // The cache-wide reservation came first, so the shards never sum
        // past the total.
        shard.used.fetch_add(size, Ordering::AcqRel);
        true
    }

    /// Count an insert refused because what the hand could evict was held
    /// (plan D58): the read goes on with its block uncached.
    fn refused_held(&self) {
        if let Some(s) = self.stats.as_deref() {
            s.add(Ticker::BlockCacheAddRefusedHeld, 1);
        }
    }

    /// Reserve `size` against the cache-wide budget, then the shard's.
    fn reserve(&self, shard: &CacheShard, size: usize) -> bool {
        if !bounded_add(&self.total_used, size, self.capacity) {
            return false;
        }
        if bounded_add(&shard.used, size, shard.capacity) {
            return true;
        }
        self.total_used.fetch_sub(size, Ordering::AcqRel);
        false
    }

    /// Return `size` an entry or a refused reservation held: the shard
    /// first, the total last, the reverse of [`Self::reserve`].
    fn release(&self, shard: &CacheShard, size: usize) {
        shard.used.fetch_sub(size, Ordering::AcqRel);
        self.total_used.fetch_sub(size, Ordering::AcqRel);
    }

    /// Publish a reserved entry into its shard's ring and map. Returns
    /// `false`, with the reservation returned, only when the ring is at its
    /// slot bound, which the reservation makes unreachable.
    fn install(&self, shard: &CacheShard, key: CacheKey, entry: CacheEntry, size: usize) -> bool {
        let state = shard.state_or_init();
        let Some(slot) = state.ring.take() else {
            self.release(shard, size);
            return false;
        };
        // Pinned by this insert until the map names it, so the hand cannot
        // evict an entry the map has not filed yet and leave the map naming
        // a dead one.
        let pin = entry.clone_ref();
        let handle = entry.downgrade();
        let node = Box::new(Node {
            entry: Some(entry),
            key,
            charge: size,
        });
        let Some(generation) = state.ring.publish(slot, node) else {
            state.ring.give(slot);
            self.release(shard, size);
            return false;
        };
        let indexed = Indexed {
            entry: handle,
            slot: slot as u32,
            generation,
        };
        if let Some(replaced) = state.map.insert(key, indexed)
            && let Some(claim) = state.ring.doom(replaced.slot as usize, replaced.generation)
        {
            self.remove(shard, state, claim);
        }
        // A `clear` may have removed this entry between its publication and
        // the map insert above, finding nothing in the map to unlink. One
        // side or the other sees the other's write (both fence), so the map
        // never keeps naming an entry the ring dropped.
        std::sync::atomic::fence(Ordering::SeqCst);
        if !state.ring.is_live(slot, generation) {
            state
                .map
                .remove_if(&key, |found| found.names(slot, generation));
        }
        drop(pin);
        true
    }

    /// One step of `shard`'s hand: evict what it lands on if no reader
    /// holds it, finish a removal another thread asked for, or pass.
    fn hand_step(&self, shard: &CacheShard, state: &ShardState, forced: bool) {
        if let Hand::Claimed(claim) = state.ring.step(forced) {
            self.evict_or_keep(shard, state, claim);
        }
    }

    /// Evict the claimed entry unless a reader holds it; a doomed entry
    /// goes whatever its pins.
    fn evict_or_keep(&self, shard: &CacheShard, state: &ShardState, mut claim: Claim<'_>) {
        if claim.doomed() {
            self.remove(shard, state, claim);
            return;
        }
        let Some(entry) = claim.node().entry.take() else {
            self.remove(shard, state, claim);
            return;
        };
        match entry.release_if_unpinned() {
            Ok(()) => self.remove(shard, state, claim),
            Err(pinned) => {
                claim.node().entry = Some(pinned);
                if let Some(doomed) = claim.release() {
                    self.remove(shard, state, doomed);
                }
            }
        }
    }

    /// Take the claimed entry out of the ring and the map, give its slot
    /// back and return its charge. Drops the cache's reference if the entry
    /// still holds one (an explicit removal).
    fn remove(&self, shard: &CacheShard, state: &ShardState, claim: Claim<'_>) {
        let (slot, generation) = (claim.index(), claim.generation());
        let node = claim.remove();
        // Pairs with the fence in `install`: see there.
        std::sync::atomic::fence(Ordering::SeqCst);
        state
            .map
            .remove_if(&node.key, |found| found.names(slot, generation));
        // The slot goes back before the bytes do, so a slot in use always
        // carries a charge and the ring never outgrows the budget.
        state.ring.give(slot);
        self.release(shard, node.charge);
    }

    /// Evict every entry of `shard` no reader holds, reference bits
    /// ignored.
    fn evict_unpinned(&self, shard: &CacheShard, state: &ShardState) {
        for slot in 0..state.ring.high_water() {
            if let Some(claim) = state.ring.claim_any(slot) {
                self.evict_or_keep(shard, state, claim);
            }
        }
    }

    /// Record a "useful" bloom-filter hit - the filter correctly
    /// returned "not present" and spared a block read. Called
    /// from SSTable reader paths that have already consulted the
    /// cache and know they're about to short-circuit the lookup.
    pub(crate) fn record_bloom_useful(&self) {
        if let Some(s) = self.stats.as_deref() {
            s.add(Ticker::BloomFilterUseful, 1);
        }
        crate::perf_context::record_bloom_check(true);
    }

    /// Record a "full positive" bloom-filter hit - the filter
    /// said "maybe", the reader went to the block, and the key
    /// was actually present.
    pub(crate) fn record_bloom_full_positive(&self) {
        if let Some(s) = self.stats.as_deref() {
            s.add(Ticker::BloomFilterFullPositive, 1);
        }
        crate::perf_context::record_bloom_check(false);
    }

    /// Evict all blocks belonging to a specific file.
    ///
    /// Driven off each shard's map, which names every filed entry with its
    /// `(slot, generation)`: the entry is unlinked from the map and its ring
    /// slot doomed, so a slot another thread holds right now is removed by
    /// that thread. An insert of the same file racing this call may land
    /// after the walk passed its key; its block stays charged until the
    /// hand evicts it, which costs memory and never a wrong read, since a
    /// file id is never reused.
    pub(crate) fn evict_file(&self, file_id: u64) {
        for shard in self.shards.iter() {
            let Some(state) = shard.state() else {
                continue;
            };
            for (key, found) in state.map.iter() {
                if key.file_id != file_id {
                    continue;
                }
                if state
                    .map
                    .remove_if(&key, |now| now.names(found.slot as usize, found.generation))
                    .is_some()
                    && let Some(claim) = state.ring.doom(found.slot as usize, found.generation)
                {
                    self.remove(shard, state, claim);
                }
            }
        }
    }

    /// Clear the entire cache.
    ///
    /// Every published entry is doomed and removed, each returning exactly
    /// the bytes it charged, so the running total stays exact under racing
    /// inserts. An insert racing the walk may land behind it and stay.
    pub(crate) fn clear(&self) {
        for shard in self.shards.iter() {
            let Some(state) = shard.state() else {
                continue;
            };
            for slot in 0..state.ring.high_water() {
                if let Some(generation) = state.ring.live_generation(slot)
                    && let Some(claim) = state.ring.doom(slot, generation)
                {
                    self.remove(shard, state, claim);
                }
            }
        }
    }

    /// Total bytes currently held across every shard, counting each
    /// entry's [`Block::charge`] plus [`ENTRY_OVERHEAD`]. Used by the
    /// `regolith.block-cache-usage` property and by unit tests to verify
    /// eviction. Lock-free; it includes an insert's reservation from the
    /// moment it is made, so it never reads below what the shards hold.
    pub(crate) fn usage(&self) -> usize {
        self.total_used.load(Ordering::Acquire)
    }

    /// Total byte capacity: the sum of every shard's budget.
    /// This may be slightly smaller than the
    /// `Options::block_cache_size` the user requested because the
    /// total is rounded down to an integer multiple of the shard
    /// count.
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }
}

#[cfg(test)]
impl BlockCache {
    /// Number of shards in this cache. Exposed for tests that
    /// want to verify multi-shard distribution.
    pub(crate) fn num_shards(&self) -> usize {
        self.num_shards
    }

    /// Number of shards currently holding at least one entry.
    /// Used by tests to confirm sharding actually distributes
    /// inserts across the shard array.
    pub(crate) fn populated_shards(&self) -> usize {
        self.shards
            .iter()
            .filter(|s| s.used.load(Ordering::Acquire) > 0)
            .count()
    }

    /// Bytes the shards hold, summed. Never above [`Self::usage`], which
    /// takes every reservation first and every release last.
    pub(crate) fn true_usage(&self) -> usize {
        self.shards
            .iter()
            .map(|s| s.used.load(Ordering::Acquire))
            .sum()
    }

    /// Entries currently filed in the shards' maps.
    pub(crate) fn entry_count(&self) -> usize {
        self.shards
            .iter()
            .map(|s| s.state().map_or(0, |state| state.map.len()))
            .sum()
    }
}

#[cfg(test)]
#[path = "block_cache/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "block_cache_adversarial.rs"]
mod adversarial;
