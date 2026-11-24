//! Unit tests of the block cache's public behaviour: budget, sharding,
//! replacement, eviction, pins and the accounting `usage()` reports.

use super::*;

/// The per-entry footprint is charged against the byte budget, so
/// growing it silently would let the cache hold more heap than the
/// budget admits. This pins the parts of it a code change could move
/// without anyone noticing: the ring's node and slot.
#[test]
#[cfg(target_pointer_width = "64")]
fn a_cache_entry_does_not_outgrow_its_charge() {
    assert_eq!(
        std::mem::size_of::<ring::Node>(),
        40,
        "the ring's node grew: either shrink it again or raise ENTRY_OVERHEAD \
         and re-measure the hit rate, because admission changes with it"
    );
    assert_eq!(
        std::mem::size_of::<ring::Slot>(),
        16,
        "a ring slot grew, and an emptied ring retains its slots"
    );
}

use crate::engine::block::{BlockBuilder, RESTART_INTERVAL};

fn dummy_block(size: usize) -> Arc<Block> {
    let mut builder = BlockBuilder::new(RESTART_INTERVAL);
    let value = vec![0u8; size];
    builder.add(b"k", &value);
    Arc::new(Block::decode(builder.finish()).expect("decode"))
}

#[test]
fn single_insert_then_get() {
    let cache = BlockCache::new(1024 * 1024);
    let blk = dummy_block(256);
    cache.insert(1, 0, blk.clone());
    assert!(cache.get(1, 0).is_some());
    assert!(cache.usage() >= 256);
}

#[test]
fn eviction_bounds_total_usage() {
    // 4 KB capacity, 1 shard (MIN_SHARD_CAPACITY fallback
    // collapses to 1 since 4 KB / 64 = 64 bytes per shard).
    let cache = BlockCache::with_config(4 * 1024, 6, false);
    // Insert many 1 KB blocks. Only a handful should survive.
    for i in 0..32u64 {
        cache.insert(1, i * 100, dummy_block(1024));
    }
    let usage = cache.usage();
    assert!(
        usage <= cache.capacity(),
        "usage {usage} exceeded capacity {}",
        cache.capacity()
    );
    // Oldest entry should have been evicted.
    assert!(cache.get(1, 0).is_none());
}

#[test]
fn strict_capacity_rejects_oversized_entry() {
    let cache = BlockCache::with_config(64 * 1024, 0, true);
    // Single shard, 64 KB capacity. A 128 KB block won't fit.
    let big = dummy_block(128 * 1024);
    cache.insert(1, 0, big);
    assert!(
        cache.get(1, 0).is_none(),
        "strict cache must reject oversized entries"
    );
    assert_eq!(cache.usage(), 0);
}

#[test]
fn non_strict_cache_admits_an_entry_bigger_than_one_shard() {
    // 8 shards of 64 KiB. A 128 KiB block does not fit its own
    // shard but fits the 512 KiB cache-wide budget, so the
    // non-strict cache empties the shard and takes it.
    let cache = BlockCache::with_config(512 * 1024, 3, false);
    assert_eq!(cache.num_shards(), 8);
    cache.insert(1, 0, dummy_block(128 * 1024));
    assert!(
        cache.get(1, 0).is_some(),
        "non-strict cache should admit an entry larger than one shard"
    );
    assert!(cache.usage() <= cache.capacity());
}

#[test]
fn non_strict_cache_refuses_an_entry_bigger_than_the_whole_budget() {
    let cache = BlockCache::with_config(64 * 1024, 0, false);
    cache.insert(1, 0, dummy_block(128 * 1024));
    assert!(
        cache.get(1, 0).is_none(),
        "a block larger than the entire budget must not be cached"
    );
    assert_eq!(cache.usage(), 0);
}

#[test]
fn oversized_admissions_stay_inside_the_budget_at_every_shard_count() {
    // One oversized entry per shard used to be admitted with no
    // cache-wide check, so resident bytes scaled with the shard
    // count instead of the budget.
    let budget = 256 * 64 * 1024;
    let mut usages = Vec::new();
    for bits in [0u32, 4, 8] {
        let cache = BlockCache::with_config(budget, bits, false);
        for file_id in 0..4096u64 {
            cache.insert(file_id, 0, dummy_block(256 * 1024));
        }
        assert!(
            cache.usage() <= cache.capacity(),
            "shard_bits {bits}: usage {} over capacity {}",
            cache.usage(),
            cache.capacity()
        );
        usages.push(cache.usage());
    }
    assert_eq!(
        usages[0], usages[2],
        "resident bytes still track the shard count"
    );
}

#[test]
fn sharding_distributes_inserts_across_shards() {
    // 64 MB so we get the full 64-shard default. Insert 1024
    // entries across many different (file_id, offset) pairs
    // and verify more than one shard ends up populated.
    let cache = BlockCache::with_config(64 * 1024 * 1024, 6, false);
    for i in 0..1024u64 {
        cache.insert(i, i * 4096, dummy_block(1024));
    }
    let populated = cache.populated_shards();
    assert_eq!(cache.num_shards(), 64);
    assert!(
        populated > 16,
        "expected inserts to fan out across shards, populated = {populated}"
    );
}

#[test]
fn evict_file_removes_only_that_files_blocks() {
    let cache = BlockCache::with_config(64 * 1024 * 1024, 6, false);
    for off in 0..16u64 {
        cache.insert(1, off * 4096, dummy_block(1024));
        cache.insert(2, off * 4096, dummy_block(1024));
    }
    cache.evict_file(1);
    // File 1 is gone.
    for off in 0..16u64 {
        assert!(cache.get(1, off * 4096).is_none());
        assert!(cache.get(2, off * 4096).is_some());
    }
}

#[test]
fn clear_zeroes_usage() {
    let cache = BlockCache::with_config(64 * 1024 * 1024, 6, false);
    for i in 0..64u64 {
        cache.insert(1, i * 4096, dummy_block(1024));
    }
    assert!(cache.usage() > 0);
    cache.clear();
    assert_eq!(cache.usage(), 0);
    assert!(cache.get(1, 0).is_none());
}

#[test]
fn repeated_insert_at_same_key_does_not_double_count() {
    let cache = BlockCache::with_config(64 * 1024, 0, false);
    cache.insert(1, 0, dummy_block(1024));
    let first_usage = cache.usage();
    cache.insert(1, 0, dummy_block(1024));
    cache.insert(1, 0, dummy_block(1024));
    // Re-inserting the same key replaces rather than accumulating.
    let final_usage = cache.usage();
    assert_eq!(first_usage, final_usage);
}

#[test]
fn miss_on_absent_key_returns_none() {
    let cache = BlockCache::with_config(64 * 1024, 0, false);
    assert!(cache.get(99, 999).is_none());
}

#[test]
fn capacity_reflects_rounded_budget() {
    // 100 KB / 64 shards would drop below MIN_SHARD_CAPACITY, so
    // the constructor collapses to fewer shards. Capacity is the
    // actual rounded budget after collapse, not the request.
    let cache = BlockCache::with_config(100_000, 6, false);
    assert!(cache.capacity() <= 100_000);
    assert!(cache.capacity() > 0);
}

#[test]
fn resident_bytes_track_the_byte_budget_not_the_shard_count() {
    // The defect this guards: every shard used to preallocate a
    // fixed 1,000,000-entry map, so the cache's own footprint
    // scaled with the shard count and ignored the byte budget.
    // Nothing is allocated up front now, and the budget is the
    // only bound at any shard count.
    let budget = 8 * 1024 * 1024;
    let mut usages = Vec::new();
    for bits in [0u32, 2, 4, 6] {
        let cache = BlockCache::with_config(budget, bits, false);
        assert_eq!(cache.usage(), 0, "a fresh cache holds nothing");
        for i in 0..8192u64 {
            cache.insert(1, i * 4096, dummy_block(4096));
        }
        assert!(
            cache.usage() <= cache.capacity(),
            "shard_bits {bits}: usage {} over capacity {}",
            cache.usage(),
            cache.capacity()
        );
        usages.push(cache.usage());
    }
    // Every configuration converges on the same budget, within one
    // entry per shard of rounding.
    let spread =
        usages.iter().max().copied().unwrap_or(0) - usages.iter().min().copied().unwrap_or(0);
    assert!(
        spread <= budget / 16,
        "resident bytes moved with shard_bits: {usages:?}"
    );
}

#[test]
fn per_entry_overhead_is_charged_against_the_budget() {
    // A budget filled with tiny blocks is bounded by the entry
    // overhead, not just by payload bytes: without charging it, a
    // 1 MiB budget would hold millions of 64-byte blocks.
    let cache = BlockCache::with_config(1024 * 1024, 0, false);
    for i in 0..100_000u64 {
        cache.insert(1, i * 64, dummy_block(0));
    }
    assert!(cache.usage() <= cache.capacity());
    assert!(
        cache.entry_count() <= cache.capacity() / ENTRY_OVERHEAD,
        "held {} entries against a {}-byte budget",
        cache.entry_count(),
        cache.capacity()
    );
}

#[test]
fn a_working_set_that_fits_the_budget_is_kept_whole() {
    // The regression this guards: an entry-count cap derived from
    // the configured `block_size` evicted entries that fit inside
    // the byte budget, silently shrinking the cache.
    let cache = BlockCache::with_config(8 * 1024 * 1024, 0, false);
    let mut offered = 0usize;
    for i in 0..3500u64 {
        let blk = dummy_block(1024);
        offered += entry_charge(&CacheEntry::Data(Arc::clone(&blk)));
        cache.insert(1, i * 4096, blk);
    }
    assert!(
        offered <= cache.capacity(),
        "test setup: the working set must fit the byte budget"
    );
    assert_eq!(
        cache.entry_count(),
        3500,
        "the cache evicted entries that fit inside its byte budget"
    );
    assert_eq!(cache.usage(), offered);
}

#[test]
fn zero_budget_disables_the_cache() {
    let cache = BlockCache::with_config(0, 6, false);
    assert_eq!(cache.num_shards(), 0);
    assert_eq!(cache.capacity(), 0);
    cache.insert(1, 0, dummy_block(256));
    assert!(cache.get(1, 0).is_none());
    assert_eq!(cache.usage(), 0);
    cache.evict_file(1);
    cache.clear();
    assert_eq!(cache.usage(), 0);
}

#[test]
fn zero_budget_strict_cache_is_also_disabled() {
    let cache = BlockCache::with_config(0, 0, true);
    cache.insert(1, 0, dummy_block(256));
    assert!(cache.get(1, 0).is_none());
    assert_eq!(cache.usage(), 0);
}

#[test]
fn tiny_budget_still_admits_a_block_that_fits() {
    let cache = BlockCache::with_config(4096, 6, false);
    cache.insert(1, 0, dummy_block(128));
    assert!(cache.get(1, 0).is_some());
    assert!(cache.usage() <= cache.capacity());
}

/// Byte accounting is exact: `usage()` is the sum of every live
/// entry's charge, which backs the `regolith.block-cache-usage`
/// property.
#[test]
fn byte_accounting_is_exact() {
    let cache = BlockCache::with_config(64 * 1024 * 1024, 0, false);
    let mut expected = 0usize;
    for i in 0..64u64 {
        let blk = dummy_block(512);
        expected += entry_charge(&CacheEntry::Data(Arc::clone(&blk)));
        cache.insert(1, i * 4096, blk);
    }
    assert_eq!(cache.usage(), expected);
}

/// `clear()` used to store a flat zero into the running total
/// outside the shard locks, so a concurrent `insert` could add its
/// delta afterwards and leave `usage()` reporting bytes the cache
/// does not hold, permanently.
#[test]
fn usage_does_not_drift_when_clear_races_insert() {
    use std::sync::atomic::AtomicBool;
    for _ in 0..50 {
        let cache = Arc::new(BlockCache::with_config(64 * 1024 * 1024, 6, false));
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let cache = Arc::clone(&cache);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut i = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    cache.insert(i % 97, i * 4096, dummy_block(256));
                    i += 1;
                }
            })
        };
        for _ in 0..300 {
            cache.clear();
        }
        stop.store(true, Ordering::Relaxed);
        writer.join().expect("writer");
        assert_eq!(
            cache.usage(),
            cache.true_usage(),
            "usage() drifted away from the real byte total"
        );
    }
}

/// Concurrent readers and writers racing eviction: the byte budget
/// holds under contention.
#[test]
fn concurrent_inserts_respect_the_budget() {
    let cache = Arc::new(BlockCache::with_config(1024 * 1024, 2, false));
    let mut handles = Vec::new();
    for t in 0..8u64 {
        let cache = Arc::clone(&cache);
        handles.push(std::thread::spawn(move || {
            for i in 0..4000u64 {
                cache.insert(t, i * 64, dummy_block(64));
                let _ = cache.get(t, (i / 2) * 64);
            }
        }));
    }
    for h in handles {
        h.join().expect("worker");
    }
    assert!(
        cache.true_usage() <= cache.capacity(),
        "usage {} over capacity {}",
        cache.true_usage(),
        cache.capacity()
    );
}

#[test]
fn evict_file_does_not_touch_other_files() {
    let cache = BlockCache::with_config(64 * 1024 * 1024, 6, false);
    cache.insert(7, 0, dummy_block(1024));
    cache.insert(8, 0, dummy_block(1024));
    let before = cache.usage();
    cache.evict_file(99); // a file id that was never inserted
    assert_eq!(cache.usage(), before);
    assert!(cache.get(7, 0).is_some());
    assert!(cache.get(8, 0).is_some());
}

/// The block a reader holds stays cached, however many inserts follow:
/// the hand passes over it on every revolution.
#[test]
fn a_block_a_reader_holds_is_never_evicted() {
    let cache = BlockCache::with_config(64 * 1024, 0, false);
    cache.insert(1, 0, dummy_block(1024));
    let held = cache.get(1, 0).expect("cached");
    for i in 1..2_000u64 {
        cache.insert(2, i * 4096, dummy_block(1024));
        assert!(cache.usage() <= cache.capacity());
    }
    let again = cache.get(1, 0).expect("a pinned block was evicted");
    assert!(Arc::ptr_eq(&held, &again), "the pinned entry was replaced");
    drop((held, again));
    // Unpinned, it is an ordinary entry and goes on the next revolutions.
    for i in 2_000..4_000u64 {
        cache.insert(2, i * 4096, dummy_block(1024));
    }
    assert!(
        cache.get(1, 0).is_none(),
        "an unpinned entry is evicted again"
    );
}

/// With every entry pinned the cache refuses an insert rather than
/// evicting one or growing past its budget, and admits again once a pin
/// is released.
#[test]
fn a_shard_full_of_pinned_blocks_refuses_inserts_and_holds_its_budget() {
    let cache = BlockCache::with_config(16 * 1024, 0, false);
    let mut held = Vec::new();
    let mut i = 0u64;
    loop {
        cache.insert(1, i * 4096, dummy_block(1024));
        match cache.get(1, i * 4096) {
            Some(block) => held.push(block),
            None => break,
        }
        i += 1;
    }
    assert!(held.len() > 4, "setup: the budget holds several blocks");
    let before = cache.usage();
    for j in 0..64u64 {
        cache.insert(2, j * 4096, dummy_block(1024));
        assert!(
            cache.get(2, j * 4096).is_none(),
            "an insert evicted a pinned block"
        );
    }
    assert_eq!(cache.usage(), before, "a refused insert moved the total");
    for (k, block) in held.iter().enumerate() {
        let again = cache
            .get(1, k as u64 * 4096)
            .expect("a pinned block was evicted");
        assert!(Arc::ptr_eq(block, &again));
    }
    held.pop();
    cache.insert(3, 0, dummy_block(1024));
    assert!(
        cache.get(3, 0).is_some(),
        "an insert was refused with an unpinned entry to evict"
    );
}

/// Every insert a shard full of held blocks refuses is counted (plan D58),
/// and only those: an insert that found a block to evict, and one that
/// fitted, count nothing.
#[test]
fn a_refused_insert_is_counted_once_and_an_admitted_one_never() {
    let stats = Arc::new(Statistics::new());
    let cache = BlockCache::with_config(16 * 1024, 0, false).with_stats(Some(Arc::clone(&stats)));
    let refused = || stats.get_ticker(Ticker::BlockCacheAddRefusedHeld);
    let mut held = Vec::new();
    let mut i = 0u64;
    loop {
        cache.insert(1, i * 4096, dummy_block(1024));
        match cache.get(1, i * 4096) {
            Some(block) => held.push(block),
            None => break,
        }
        i += 1;
    }
    assert_eq!(refused(), 1, "the insert that ended the fill was refused");
    let added = stats.get_ticker(Ticker::BlockCacheAdd);
    for j in 0..64u64 {
        cache.insert(2, j * 4096, dummy_block(1024));
    }
    assert_eq!(refused(), 65, "each refused insert counts once");
    assert_eq!(
        stats.get_ticker(Ticker::BlockCacheAdd),
        added,
        "a refused insert counted as an add"
    );
    held.pop();
    cache.insert(3, 0, dummy_block(1024));
    assert!(cache.get(3, 0).is_some());
    assert_eq!(refused(), 65, "an insert that evicted counted as refused");
}

/// An entry larger than its shard's share is refused, and counted, when the
/// blocks its shard still holds after it empties the shard leave no room.
/// Refused because the other shards are full, it is not counted: those
/// blocks were never offered to its hand.
#[test]
fn an_oversized_insert_refused_for_held_blocks_is_counted() {
    let stats = Arc::new(Statistics::new());
    // Four shards of 64 KiB.
    let cache = BlockCache::with_config(256 * 1024, 2, false).with_stats(Some(Arc::clone(&stats)));
    let shard_of = |file_id: u64, offset: u64| cache.shard_index(&CacheKey { file_id, offset });
    let target = shard_of(9, 0);
    let big = || dummy_block(40 * 1024);
    assert!(
        entry_charge(&CacheEntry::Data(big())) > cache.shards[target].capacity,
        "setup: the entry is larger than its shard's share"
    );
    // Fill the target shard with blocks a reader holds.
    let mut held = Vec::new();
    let mut offset = 0u64;
    while cache.shards[target].used.load(Ordering::Acquire) < 48 * 1024 {
        offset += 4096;
        if shard_of(1, offset) != target {
            continue;
        }
        cache.insert(1, offset, dummy_block(8 * 1024));
        held.push(cache.get(1, offset).expect("setup: the shard admits it"));
    }
    // Other shards hold unpinned blocks, few enough that the entry would
    // fit beside them were the target shard empty.
    let in_target = cache.shards[target].used.load(Ordering::Acquire);
    let mut other = 0u64;
    while cache.total_used.load(Ordering::Acquire) - in_target < 136 * 1024 {
        other += 4096;
        if shard_of(2, other) == target {
            continue;
        }
        cache.insert(2, other, dummy_block(8 * 1024));
    }
    let before = stats.get_ticker(Ticker::BlockCacheAddRefusedHeld);
    cache.insert(9, 0, big());
    assert!(
        cache.get(9, 0).is_none(),
        "setup: the held blocks leave no room"
    );
    assert_eq!(
        stats.get_ticker(Ticker::BlockCacheAddRefusedHeld),
        before + 1,
        "an oversized insert refused for held blocks went uncounted"
    );
    // With the target's blocks released, the same insert fits.
    held.clear();
    cache.insert(9, 0, big());
    assert!(cache.get(9, 0).is_some(), "an insert was refused with room");
    assert_eq!(
        stats.get_ticker(Ticker::BlockCacheAddRefusedHeld),
        before + 1
    );
}

/// Explicit removals do not wait for readers: a re-insert, `evict_file`
/// and `clear` drop the cache's reference, and the reader keeps its own.
#[test]
fn explicit_removals_drop_pinned_entries_and_leave_the_reader_its_block() {
    let cache = BlockCache::with_config(1024 * 1024, 0, false);
    cache.insert(1, 0, dummy_block(512));
    let held = cache.get(1, 0).expect("cached");
    cache.insert(1, 0, dummy_block(512));
    let replaced = cache.get(1, 0).expect("re-inserted");
    assert!(
        !Arc::ptr_eq(&held, &replaced),
        "the re-insert did not replace"
    );
    cache.evict_file(1);
    assert!(cache.get(1, 0).is_none());
    cache.insert(2, 0, dummy_block(512));
    let pinned = cache.get(2, 0).expect("cached");
    cache.clear();
    assert!(cache.get(2, 0).is_none());
    assert_eq!(cache.usage(), 0);
    assert!(!held.entry_data().is_empty() && !pinned.entry_data().is_empty());
}

/// Readers pin and release blocks while inserters churn far past the
/// budget: a block a reader holds is always the one the cache returns for
/// its key, and the budget holds at every sample.
#[test]
fn pins_hold_and_the_budget_holds_under_readers_and_inserters() {
    use std::sync::atomic::{AtomicBool, AtomicUsize as StdAtomicUsize};
    const KEYS: u64 = 256;
    let cache = Arc::new(BlockCache::with_config(256 * 1024, 2, false));
    let stop = Arc::new(AtomicBool::new(false));
    let pinned_checks = Arc::new(StdAtomicUsize::new(0));
    std::thread::scope(|scope| {
        for t in 0..4u64 {
            let (cache, stop, checks) = (
                Arc::clone(&cache),
                Arc::clone(&stop),
                Arc::clone(&pinned_checks),
            );
            scope.spawn(move || {
                let mut held: Vec<(u64, Arc<Block>)> = Vec::new();
                let mut i = t;
                while !stop.load(Ordering::Acquire) {
                    let key = (i * 7919) % KEYS;
                    if let Some(block) = cache.get(1, key * 4096) {
                        held.push((key, block));
                    }
                    if held.len() > 8 {
                        let (key, block) = held.remove(0);
                        let again = cache
                            .get(1, key * 4096)
                            .expect("a block a reader holds was evicted");
                        assert!(Arc::ptr_eq(&block, &again), "a pinned entry was replaced");
                        checks.fetch_add(1, Ordering::Relaxed);
                    }
                    i += 4;
                }
            });
        }
        // Each key of file 1 has one inserter, which re-inserts it only
        // after the hand took it: a re-insert over a live entry is an
        // explicit replacement, which does not wait for readers.
        //
        // An inserter churns 20,000 times, and on until a reader has
        // re-checked a block it held: left to the scheduler, inserters on
        // one core can finish before a reader thread has held nine blocks.
        let inserters: Vec<_> = (0..3u64)
            .map(|t| {
                let (cache, checks) = (Arc::clone(&cache), Arc::clone(&pinned_checks));
                scope.spawn(move || {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
                    let mut i = 0u64;
                    while i < 20_000 || checks.load(Ordering::Relaxed) == 0 {
                        assert!(
                            i < 20_000 || std::time::Instant::now() < deadline,
                            "no reader ever re-checked a block it held"
                        );
                        let key = (i * 3 + t) % KEYS;
                        if key % 3 == t && cache.get(1, key * 4096).is_none() {
                            cache.insert(1, key * 4096, dummy_block(2048));
                        }
                        cache.insert(2, (i * 3 + t) * 4096, dummy_block(2048));
                        assert!(cache.usage() <= cache.capacity());
                        i += 1;
                    }
                })
            })
            .collect();
        let monitor_cache = Arc::clone(&cache);
        let monitor_stop = Arc::clone(&stop);
        scope.spawn(move || {
            while !monitor_stop.load(Ordering::Acquire) {
                assert!(monitor_cache.usage() <= monitor_cache.capacity());
            }
        });
        // The inserters are the ones that finish; then the readers and the
        // monitor stop.
        for inserter in inserters {
            inserter.join().expect("inserter");
        }
        stop.store(true, Ordering::Release);
    });
    assert!(
        pinned_checks.load(Ordering::Relaxed) > 0,
        "no reader ever re-checked a block it held"
    );
    assert_eq!(cache.usage(), cache.true_usage());
    assert!(cache.true_usage() <= cache.capacity());
}

mod model {
    //! The cache against a sequential model of what it may hold.
    //!
    //! CLOCK chooses its victims itself, so the model does not predict
    //! which entries survive; it bounds what may be there and what must:
    //! every entry the cache returns is the last block inserted under its
    //! key, a block a reader holds is still the cache's entry for its key
    //! unless an explicit removal took it, and the accounted bytes are
    //! exactly the charges of what the cache returns, within the budget.
    //!
    //! The model holds strong references only where a reader would: in the
    //! explicit pins. What it knows of the cache's other entries is held
    //! weakly, which keeps no block from the hand, so the sequences exercise
    //! ordinary eviction and unpinning; and the run must show it did, by an
    //! unpinned entry the hand evicted under pressure.

    use std::cell::Cell;
    use std::collections::HashMap;

    use proptest::prelude::*;
    use proptest::test_runner::{Config, TestRunner};

    use super::*;

    #[derive(Debug, Clone)]
    enum Op {
        Insert { file: u64, block: u64, size: usize },
        Get { file: u64, block: u64 },
        Pin { file: u64, block: u64 },
        Unpin,
        EvictFile { file: u64 },
        Clear,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            6 => (0..3u64, 0..24u64, 0..3000usize)
                .prop_map(|(file, block, size)| Op::Insert { file, block, size }),
            4 => (0..3u64, 0..24u64).prop_map(|(file, block)| Op::Get { file, block }),
            2 => (0..3u64, 0..24u64).prop_map(|(file, block)| Op::Pin { file, block }),
            2 => Just(Op::Unpin),
            1 => (0..3u64).prop_map(|file| Op::EvictFile { file }),
            1 => Just(Op::Clear),
        ]
    }

    /// What the model keeps of the last block inserted under a key: a weak
    /// reference, which leaves the block's count to the cache and the
    /// readers, and keeps its allocation, so no later block can take its
    /// address while the model compares by it.
    type Seen = std::sync::Weak<Block>;

    fn seen(block: &Arc<Block>) -> Seen {
        Arc::downgrade(block)
    }

    /// Whether `block` is the one `seen` was taken of.
    fn is(seen: &Seen, block: &Arc<Block>) -> bool {
        std::ptr::eq(seen.as_ptr(), Arc::as_ptr(block))
    }

    /// One generated sequence against the cache. Counts in `evicted` each
    /// entry the hand took under pressure while no reader held it.
    fn check(ops: Vec<Op>, shard_bits: u32, evicted: &Cell<u64>) -> Result<(), TestCaseError> {
        // A third of what the 72 keys can hold at once, so inserts meet a
        // full cache and the hand runs.
        let cache = BlockCache::with_config(32 * 1024, shard_bits, false);
        // The last block inserted under each key that the cache still
        // holds: the only one it may return for that key.
        let mut latest: HashMap<(u64, u64), Seen> = HashMap::new();
        // Blocks a reader holds, with the key they were read under.
        let mut pins: Vec<((u64, u64), Arc<Block>)> = Vec::new();
        for op in ops {
            match op {
                Op::Insert { file, block, size } => {
                    let b = dummy_block(size);
                    cache.insert(file, block * 4096, Arc::clone(&b));
                    // An insert either replaces the key's entry or, refused,
                    // leaves the cache as its search left it: the older
                    // block if the hand did not take it.
                    match cache.get(file, block * 4096) {
                        Some(got) if Arc::ptr_eq(&got, &b) => {
                            latest.insert((file, block), seen(&b));
                        }
                        Some(got) => prop_assert!(
                            latest.get(&(file, block)).is_some_and(|old| is(old, &got)),
                            "a refused insert left a block that was never the latest"
                        ),
                        None => {
                            latest.remove(&(file, block));
                        }
                    }
                }
                Op::Get { file, block } => {
                    let _ = cache.get(file, block * 4096);
                }
                Op::Pin { file, block } => {
                    if let Some(b) = cache.get(file, block * 4096) {
                        pins.push(((file, block), b));
                    }
                }
                Op::Unpin => {
                    if !pins.is_empty() {
                        pins.remove(0);
                    }
                }
                Op::EvictFile { file } => {
                    cache.evict_file(file);
                    latest.retain(|(f, _), _| *f != file);
                    pins.retain(|((f, _), _)| *f != file);
                }
                Op::Clear => {
                    cache.clear();
                    latest.clear();
                    pins.clear();
                }
            }
            prop_assert!(cache.usage() <= cache.capacity());
            let mut charged = 0usize;
            let mut gone = Vec::new();
            for (&(file, block), want) in &latest {
                match cache.get(file, block * 4096) {
                    Some(got) => {
                        prop_assert!(is(want, &got), "a stale block for ({}, {})", file, block);
                        charged += entry_charge(&CacheEntry::Data(got));
                    }
                    // No explicit removal took it: the hand did, under the
                    // pressure of an insert.
                    None => {
                        prop_assert!(
                            !pins
                                .iter()
                                .any(|(key, held)| *key == (file, block) && is(want, held)),
                            "a block a reader holds was evicted"
                        );
                        gone.push((file, block));
                    }
                }
            }
            prop_assert_eq!(
                cache.usage(),
                charged,
                "the total is not what the cache holds"
            );
            for key in gone {
                latest.remove(&key);
                evicted.set(evicted.get() + 1);
            }
        }
        Ok(())
    }

    #[test]
    fn the_cache_matches_its_sequential_model() {
        let mut runner = TestRunner::new(Config {
            cases: 128,
            failure_persistence: None,
            ..Config::default()
        });
        let evicted = Cell::new(0u64);
        let input = (proptest::collection::vec(op(), 1..200), 0u32..2);
        runner
            .run(&input, |(ops, shard_bits)| check(ops, shard_bits, &evicted))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            evicted.get() > 0,
            "no sequence saw the hand evict an entry no reader held: the model never \
             exercised eviction"
        );
    }
}
