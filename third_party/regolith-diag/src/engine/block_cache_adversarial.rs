//! Adversarial probes for the CLOCK block cache.
//!
//! Every test here starts from "this is broken" and tries to prove it:
//! a hand that never stops, a map and a ring that disagree, a byte
//! total that drifts, a block returned under the wrong key, a budget
//! that grows with the shard count. Each one asserts a structural
//! invariant rather than an outcome, so a future policy change that
//! breaks the contract fails here rather than in production.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::*;
use crate::engine::block::{BlockBuilder, RESTART_INTERVAL};

fn block_with(tag: u8, size: usize) -> Arc<Block> {
    let mut builder = BlockBuilder::new(RESTART_INTERVAL);
    builder.add(b"k", &vec![tag; size]);
    Arc::new(Block::decode(builder.finish()).expect("decode"))
}

fn dummy_block(size: usize) -> Arc<Block> {
    block_with(0, size)
}

/// Tag of a block built by [`block_with`]. The single entry's value is
/// a run of `tag` bytes and it is the last thing in the entry region,
/// so this identifies which payload a lookup actually returned.
fn tag_of(block: &Block) -> u8 {
    block
        .entry_data()
        .last()
        .copied()
        .expect("a block built by block_with has one entry")
}

/// Run `body` on a worker and fail the test if it has not finished
/// within `limit`. A CLOCK hand that spins therefore fails rather than
/// wedging the run.
fn within<F>(limit: Duration, what: &str, body: F)
where
    F: FnOnce() + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        body();
        let _ = tx.send(());
    });
    if rx.recv_timeout(limit).is_err() {
        panic!("{what} did not finish within {limit:?}");
    }
}

/// Whether two entries are the same cached block.
fn same_entry(a: &CacheEntry, b: &CacheEntry) -> bool {
    match (a, b) {
        (CacheEntry::Data(x), CacheEntry::Data(y)) => Arc::ptr_eq(x, y),
        (CacheEntry::Index(x), CacheEntry::Index(y)) => Arc::ptr_eq(x, y),
        (CacheEntry::Filter(x), CacheEntry::Filter(y)) => Arc::ptr_eq(x, y),
        _ => false,
    }
}

/// The structural contract of a shard, checked once every thread that
/// touched the cache has finished:
///
/// * no slot is left busy, and no doomed entry is left in the ring,
/// * every live ring slot holds the entry the map names for its key, by
///   slot and generation, and the map's handle pins that very block,
/// * the map holds nothing the ring does not,
/// * the shard's `used` is exactly the sum of the live entries' charges,
/// * the free stack names only empty slots, once each, and every slot the
///   ring ever handed out is either live or free,
/// * the ring stayed within its slot bound.
fn assert_shard_invariants(cache: &BlockCache, context: &str) {
    for (idx, shard) in cache.shards.iter().enumerate() {
        let Some(state) = shard.state() else {
            assert_eq!(
                shard.used.load(Ordering::Acquire),
                0,
                "{context}: shard {idx} charges bytes without ever having stored"
            );
            continue;
        };
        let range = state.ring.high_water();
        let mut live: Vec<(usize, u32, CacheKey, usize, CacheEntry)> = Vec::new();
        for slot in 0..range {
            assert_eq!(
                state.ring.flags(slot) & ring::BUSY,
                0,
                "{context}: shard {idx} slot {slot} was left busy"
            );
            if let Some(mut claim) = state.ring.claim_any(slot) {
                let generation = claim.generation();
                let node = claim.node();
                let entry = node
                    .entry
                    .as_ref()
                    .map(CacheEntry::clone_ref)
                    .unwrap_or_else(|| panic!("{context}: shard {idx} slot {slot} lost its block"));
                live.push((slot, generation, node.key, node.charge, entry));
                assert!(
                    claim.release().is_none(),
                    "{context}: shard {idx} slot {slot} was doomed and left in the ring"
                );
            }
        }

        let charged: usize = live.iter().map(|(_, _, _, charge, _)| charge).sum();
        let used = shard.used.load(Ordering::Acquire);
        assert_eq!(
            charged, used,
            "{context}: shard {idx} used={used} but live slots charge {charged}"
        );
        assert_eq!(
            state.map.len(),
            live.len(),
            "{context}: shard {idx} map holds {} entries, ring holds {}",
            state.map.len(),
            live.len()
        );
        for (slot, generation, key, _, entry) in &live {
            let found = state
                .map
                .get(key)
                .unwrap_or_else(|| panic!("{context}: shard {idx} slot {slot} is not in the map"));
            assert!(
                found.names(*slot, *generation),
                "{context}: shard {idx} slot {slot} and the map disagree on the entry"
            );
            let pinned = found.entry.upgrade().unwrap_or_else(|| {
                panic!("{context}: shard {idx} slot {slot}: the map's handle no longer pins it")
            });
            assert!(
                same_entry(&pinned, entry),
                "{context}: shard {idx} slot {slot}: the map pins another block"
            );
        }

        let free = state.ring.free_indices();
        let mut seen = std::collections::HashSet::new();
        for &slot in &free {
            assert!(slot < range, "{context}: free slot out of range");
            assert_eq!(
                state.ring.flags(slot) & ring::OCCUPIED,
                0,
                "{context}: shard {idx} free-lists a live slot"
            );
            assert!(
                seen.insert(slot),
                "{context}: shard {idx} free-lists a slot twice"
            );
        }
        assert_eq!(
            live.len() + free.len(),
            range,
            "{context}: shard {idx} lost track of a slot"
        );
        assert!(
            range <= shard.max_slots,
            "{context}: shard {idx} ring grew past its bound"
        );
    }
    assert_eq!(
        cache.usage(),
        cache.true_usage(),
        "{context}: usage() drifted from the shard totals"
    );
    assert!(
        cache.true_usage() <= cache.capacity(),
        "{context}: {} bytes held against a {}-byte budget",
        cache.true_usage(),
        cache.capacity()
    );
}

/// Attack: set every reference bit, then insert. The classic CLOCK
/// livelock is a hand that finds nothing evictable and never returns.
#[test]
fn the_hand_terminates_when_every_entry_is_referenced() {
    within(Duration::from_secs(20), "a fully referenced sweep", || {
        let cache = BlockCache::with_config(256 * 1024, 0, false);
        let mut stored = 0u64;
        for i in 0..64u64 {
            cache.insert(1, i * 4096, dummy_block(1024));
            if cache.get(1, i * 4096).is_some() {
                stored = i + 1;
            }
        }
        for i in 0..stored {
            let _ = cache.get(1, i * 4096);
        }
        for i in 0..stored {
            assert!(
                cache.get(1, i * 4096).is_some(),
                "setup: every live entry must be referenced"
            );
        }
        cache.insert(2, 0, dummy_block(1024));
        assert!(cache.usage() <= cache.capacity());
        assert_shard_invariants(&cache, "fully referenced sweep");
    });
}

/// Attack: readers re-set reference bits as fast as the hand clears
/// them, which is the only case the forced second revolution exists
/// for. The writer must keep making progress.
#[test]
fn readers_re_referencing_cannot_stall_an_insert() {
    let cache = Arc::new(BlockCache::with_config(256 * 1024, 0, false));
    for i in 0..64u64 {
        cache.insert(1, i * 4096, dummy_block(1024));
    }
    let stop = Arc::new(AtomicBool::new(false));
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let cache = Arc::clone(&cache);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    for i in 0..64u64 {
                        let _ = cache.get(1, i * 4096);
                    }
                }
            })
        })
        .collect();

    let deadline = Instant::now() + Duration::from_secs(20);
    for i in 0..5_000u64 {
        cache.insert(2, i * 4096, dummy_block(1024));
        assert!(
            Instant::now() < deadline,
            "inserts stalled behind re-referencing readers at iteration {i}"
        );
    }
    stop.store(true, Ordering::Relaxed);
    for r in readers {
        r.join().expect("reader");
    }
    assert_shard_invariants(&cache, "re-referencing readers");
}

/// Attack: a budget that cannot hold one block. Nothing may be stored,
/// at any shard count, strict or not.
#[test]
fn a_budget_below_one_block_stores_nothing() {
    for strict in [false, true] {
        for bits in [0u32, 4, 8] {
            let cache = BlockCache::with_config(4096, bits, strict);
            cache.insert(1, 0, dummy_block(64 * 1024));
            assert!(
                cache.get(1, 0).is_none(),
                "strict={strict} bits={bits}: a block larger than the budget was cached"
            );
            assert_eq!(cache.usage(), 0);
            assert_shard_invariants(&cache, "budget below one block");
        }
    }
}

/// Attack: the maximum shard count against a budget far below the
/// per-shard floor. Shards must collapse instead of leaving the cache
/// unable to hold anything.
#[test]
fn a_huge_shard_count_with_a_tiny_budget_collapses() {
    let cache = BlockCache::with_config(64 * 1024, MAX_SHARD_BITS, false);
    assert_eq!(cache.num_shards(), 1, "shards did not collapse");
    cache.insert(1, 0, dummy_block(1024));
    assert!(cache.get(1, 0).is_some());
    assert!(cache.usage() <= cache.capacity());
    assert_shard_invariants(&cache, "huge shard count, tiny budget");
}

/// Attack: a disabled cache must allocate nothing and answer nothing,
/// including through `evict_file` and `clear`.
#[test]
fn a_zero_budget_cache_holds_no_shard_state() {
    let cache = BlockCache::with_config(0, MAX_SHARD_BITS, false);
    assert!(cache.shards.is_empty());
    for i in 0..1000u64 {
        cache.insert(i, i, dummy_block(1024));
        assert!(cache.get(i, i).is_none());
    }
    cache.evict_file(3);
    cache.clear();
    assert_eq!((cache.usage(), cache.capacity()), (0, 0));
}

/// Attack: return the wrong block. Distinct payloads under keys that
/// share a shard, share an offset across files, and share a file
/// across offsets.
#[test]
fn a_lookup_never_returns_another_keys_block() {
    let cache = BlockCache::with_config(64 * 1024 * 1024, 6, false);
    let mut expected = Vec::new();
    let mut tag = 1u8;
    for file_id in 0..40u64 {
        for offset in 0..8u64 {
            cache.insert(file_id, offset * 4096, block_with(tag, 512));
            expected.push((file_id, offset * 4096, tag));
            tag = tag.wrapping_add(1).max(1);
        }
    }
    for (file_id, offset, tag) in &expected {
        let got = cache
            .get(*file_id, *offset)
            .unwrap_or_else(|| panic!("({file_id},{offset}) fell out of a cache that fits it"));
        assert_eq!(
            tag_of(&got),
            *tag,
            "({file_id},{offset}) returned another key's block"
        );
    }

    // Keys that hash to the same shard must still be distinguished.
    let mut by_shard: std::collections::BTreeMap<usize, Vec<(u64, u64, u8)>> = Default::default();
    for (file_id, offset, tag) in &expected {
        let idx = cache.shard_index(&CacheKey {
            file_id: *file_id,
            offset: *offset,
        });
        by_shard
            .entry(idx)
            .or_default()
            .push((*file_id, *offset, *tag));
    }
    let collided = by_shard.values().filter(|v| v.len() > 1).count();
    assert!(collided > 0, "setup: no two keys shared a shard");
    for group in by_shard.values() {
        for (file_id, offset, tag) in group {
            assert_eq!(
                tag_of(&cache.get(*file_id, *offset).expect("present")),
                *tag
            );
        }
    }
    assert_shard_invariants(&cache, "wrong-key lookup");
}

/// Attack: re-insert the same key with different content. The reader
/// must see the new block, and the bytes must be counted once.
#[test]
fn reinserting_a_key_replaces_the_block_and_the_charge() {
    let cache = BlockCache::with_config(1024 * 1024, 0, false);
    cache.insert(9, 128, block_with(1, 512));
    let after_first = cache.usage();
    for tag in 2..=64u8 {
        cache.insert(9, 128, block_with(tag, 512));
        assert_eq!(
            tag_of(&cache.get(9, 128).expect("present")),
            tag,
            "a re-insert left the old block visible"
        );
        assert_eq!(cache.usage(), after_first, "a re-insert double-counted");
        assert_eq!(cache.entry_count(), 1);
    }
    assert_shard_invariants(&cache, "re-insert");
}

/// Attack: a re-insert whose replacement is a different size must move
/// the byte total by exactly the difference.
#[test]
fn reinserting_a_key_with_a_different_size_reprices_it() {
    let cache = BlockCache::with_config(4 * 1024 * 1024, 0, false);
    let small = block_with(1, 256);
    let large = block_with(2, 64 * 1024);
    cache.insert(4, 0, Arc::clone(&small));
    assert_eq!(
        cache.usage(),
        entry_charge(&CacheEntry::Data(Arc::clone(&small)))
    );
    cache.insert(4, 0, Arc::clone(&large));
    assert_eq!(
        cache.usage(),
        entry_charge(&CacheEntry::Data(Arc::clone(&large)))
    );
    cache.insert(4, 0, Arc::clone(&small));
    assert_eq!(
        cache.usage(),
        entry_charge(&CacheEntry::Data(Arc::clone(&small)))
    );
    assert_eq!(cache.entry_count(), 1);
    assert_shard_invariants(&cache, "re-insert repricing");
}

/// Attack: `evict_file` racing reads of the same file, which is what a
/// compaction unlinking a file actually does. No reader may see a block
/// under the wrong key and the accounting must land exact.
#[test]
fn evict_file_racing_readers_of_that_file() {
    let cache = Arc::new(BlockCache::with_config(16 * 1024 * 1024, 4, false));
    let stop = Arc::new(AtomicBool::new(false));
    const OFFSETS: u64 = 96;

    let readers: Vec<_> = (0..4)
        .map(|t| {
            let cache = Arc::clone(&cache);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    for off in 0..OFFSETS {
                        for file_id in 1..=3u64 {
                            if let Some(b) = cache.get(file_id, off * 4096) {
                                assert_eq!(
                                    tag_of(&b),
                                    (file_id * 100 + off) as u8,
                                    "reader {t} saw the wrong block for ({file_id},{off})"
                                );
                            }
                        }
                    }
                }
            })
        })
        .collect();

    let writer = {
        let cache = Arc::clone(&cache);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                for file_id in 1..=3u64 {
                    for off in 0..OFFSETS {
                        cache.insert(
                            file_id,
                            off * 4096,
                            block_with((file_id * 100 + off) as u8, 1024),
                        );
                    }
                }
            }
        })
    };

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut rounds = 0u64;
    while Instant::now() < deadline {
        cache.evict_file(2);
        rounds += 1;
    }
    stop.store(true, Ordering::Relaxed);
    writer.join().expect("writer");
    for r in readers {
        r.join().expect("reader");
    }
    assert!(rounds > 0);
    assert_shard_invariants(&cache, "evict_file racing readers");

    cache.evict_file(1);
    cache.evict_file(2);
    cache.evict_file(3);
    assert_eq!(cache.usage(), 0, "evict_file left bytes behind");
    assert_eq!(cache.entry_count(), 0);
    assert_shard_invariants(&cache, "after evicting every file");
}

/// Attack: everything at once against a budget small enough that the
/// hand runs constantly. The byte total must be exact when it settles.
#[test]
fn a_mixed_storm_keeps_the_accounting_exact() {
    let cache = Arc::new(BlockCache::with_config(2 * 1024 * 1024, 3, false));
    let stop = Arc::new(AtomicBool::new(false));

    let mut handles = Vec::new();
    for t in 0..6u64 {
        let cache = Arc::clone(&cache);
        handles.push(std::thread::spawn(move || {
            for i in 0..20_000u64 {
                let file_id = (t * 7 + i) % 11;
                cache.insert(file_id, (i % 512) * 4096, dummy_block(1024));
                let _ = cache.get(file_id, ((i * 3) % 512) * 4096);
                if i % 512 == 0 {
                    cache.evict_file(file_id);
                }
            }
        }));
    }
    // `usage()` is the one word every insert reserves against before it
    // touches a shard and every removal releases last, so it is the exact
    // bound at every instant and never reads below what the shards hold.
    // A running sum of the shards read one after another is not an
    // instant at all once no lock freezes them, so the monitor samples the
    // word, and the shards are checked against it when the storm settles.
    let monitor = {
        let cache = Arc::clone(&cache);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut worst = 0usize;
            while !stop.load(Ordering::Relaxed) {
                worst = worst.max(cache.usage());
            }
            worst
        })
    };
    for h in handles {
        h.join().expect("worker");
    }
    stop.store(true, Ordering::Relaxed);
    let worst = monitor.join().expect("monitor");
    assert!(
        worst <= cache.capacity(),
        "the cache peaked at {worst} bytes against a {}-byte budget",
        cache.capacity()
    );
    assert_shard_invariants(&cache, "mixed storm");
}

/// Attack: oversized entries, which are the only ones allowed past a
/// shard's own share, racing normal inserts. The cache-wide reservation
/// reads a total that other shards' in-flight inserts have not
/// published yet, so this is where an over-budget cache would show up.
#[test]
fn oversized_inserts_racing_normal_inserts_stay_in_budget() {
    for _ in 0..8 {
        let cache = Arc::new(BlockCache::with_config(8 * 64 * 1024, 3, false));
        let capacity = cache.capacity();
        let mut handles = Vec::new();
        for t in 0..4u64 {
            let cache = Arc::clone(&cache);
            handles.push(std::thread::spawn(move || {
                for i in 0..3_000u64 {
                    cache.insert(t * 1000 + i, 0, dummy_block(96 * 1024));
                }
            }));
        }
        for t in 0..4u64 {
            let cache = Arc::clone(&cache);
            handles.push(std::thread::spawn(move || {
                for i in 0..3_000u64 {
                    cache.insert(500_000 + t, i * 4096, dummy_block(16 * 1024));
                }
            }));
        }
        for h in handles {
            h.join().expect("worker");
        }
        assert!(
            cache.true_usage() <= capacity,
            "oversized race left {} bytes against a {capacity}-byte budget",
            cache.true_usage()
        );
        assert_shard_invariants(&cache, "oversized race");
    }
}

/// Samples the budget monitor must take before it may stop, so the
/// observation is dense enough to catch a transient overshoot without
/// the test depending on how the scheduler treated it.
const MONITOR_SAMPLE_FLOOR: u64 = 1_000;

/// Attack: an oversized entry is admitted against the cache-wide budget
/// while normal inserts race it on every other shard. Every insert, of
/// either kind, reserves against the cache-wide total by CAS before it
/// touches a shard, so if any path skipped or reordered that the total
/// would go over budget for real. A monitor samples the total, which is
/// the exact figure at every instant, while the race runs rather than
/// only after it, and the shards are checked against it when it ends.
#[test]
fn the_oversized_reservation_cannot_be_raced_over_budget() {
    let cache = Arc::new(BlockCache::with_config(8 * 64 * 1024, 3, false));
    let capacity = cache.capacity();
    let stop = Arc::new(AtomicBool::new(false));
    let monitor = {
        let cache = Arc::clone(&cache);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut worst = 0usize;
            let mut samples = 0u64;
            // Sample until the workers are done AND a floor is reached.
            // Asserting afterwards that the floor happened to be hit
            // makes the test fail on a loaded machine for a reason that
            // has nothing to do with the budget being respected.
            while !stop.load(Ordering::Relaxed) || samples < MONITOR_SAMPLE_FLOOR {
                worst = worst.max(cache.usage());
                samples += 1;
            }
            (worst, samples)
        })
    };

    let mut handles = Vec::new();
    for t in 0..4u64 {
        let cache = Arc::clone(&cache);
        handles.push(std::thread::spawn(move || {
            for i in 0..8_000u64 {
                cache.insert(t * 100_000 + i, 0, dummy_block(96 * 1024));
            }
        }));
    }
    for t in 0..4u64 {
        let cache = Arc::clone(&cache);
        handles.push(std::thread::spawn(move || {
            for i in 0..8_000u64 {
                cache.insert(900_000 + t, i * 4096, dummy_block(48 * 1024));
            }
        }));
    }
    for h in handles {
        h.join().expect("worker");
    }
    stop.store(true, Ordering::Relaxed);
    let (worst, samples) = monitor.join().expect("monitor");
    println!("ADVPEAK oversized race peaked at {worst} of {capacity} bytes over {samples} samples");
    assert!(
        samples >= MONITOR_SAMPLE_FLOOR,
        "the monitor did not reach its sample floor: {samples}"
    );
    assert!(
        worst <= capacity,
        "the oversized reservation was raced to {worst} bytes against a {capacity}-byte budget"
    );
    assert_shard_invariants(&cache, "oversized reservation race");
}

/// Attack: `clear` racing inserts and reads, then the invariants.
#[test]
fn clear_racing_a_storm_leaves_a_consistent_shard() {
    for _ in 0..20 {
        let cache = Arc::new(BlockCache::with_config(4 * 1024 * 1024, 4, false));
        let stop = Arc::new(AtomicBool::new(false));
        let workers: Vec<_> = (0..4u64)
            .map(|t| {
                let cache = Arc::clone(&cache);
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    let mut i = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        cache.insert(t, i % 4096, dummy_block(512));
                        let _ = cache.get(t, (i / 2) % 4096);
                        i += 1;
                    }
                })
            })
            .collect();
        for _ in 0..200 {
            cache.clear();
        }
        stop.store(true, Ordering::Relaxed);
        for w in workers {
            w.join().expect("worker");
        }
        assert_shard_invariants(&cache, "clear racing a storm");
    }
}

/// Attack: many small entries against a small budget, to check the ring
/// reuses slots instead of growing without bound. `slots.len()` is the
/// peak live entry count, so it must stay near what the budget can hold
/// rather than tracking the number of inserts.
#[test]
fn the_ring_reuses_slots_instead_of_growing_with_the_insert_count() {
    let cache = BlockCache::with_config(256 * 1024, 0, false);
    for i in 0..200_000u64 {
        cache.insert(1, i * 64, dummy_block(0));
    }
    let slots: usize = cache
        .shards
        .iter()
        .map(|s| s.state().map_or(0, |state| state.ring.high_water()))
        .sum();
    let live = cache.entry_count();
    assert!(
        slots <= live * 2 + 8,
        "ring grew to {slots} slots for {live} live entries after 200k inserts"
    );
    assert!(
        slots <= cache.capacity() / ENTRY_OVERHEAD + 8,
        "ring slots {slots} exceed what the byte budget can hold"
    );
    assert_shard_invariants(&cache, "slot reuse");
}

/// Attack: an oversized entry re-inserted at the same key. The
/// cache-wide reservation subtracts the shard's current bytes, so a
/// mistake here compounds one insert at a time.
#[test]
fn repeated_oversized_inserts_at_one_key_do_not_compound() {
    let cache = BlockCache::with_config(8 * 64 * 1024, 3, false);
    for tag in 1..=200u8 {
        cache.insert(77, 0, block_with(tag, 96 * 1024));
        assert!(
            cache.usage() <= cache.capacity(),
            "oversized re-inserts compounded to {} bytes",
            cache.usage()
        );
    }
    assert_eq!(tag_of(&cache.get(77, 0).expect("present")), 200);
    assert_shard_invariants(&cache, "repeated oversized insert");
}

/// Attack: strict mode must refuse rather than admit over budget, and
/// must leave the byte total untouched when it refuses.
#[test]
fn strict_mode_refuses_without_disturbing_the_accounting() {
    let cache = BlockCache::with_config(8 * 64 * 1024, 3, true);
    cache.insert(1, 0, dummy_block(16 * 1024));
    let baseline = cache.usage();
    assert!(baseline > 0);
    for i in 0..100u64 {
        cache.insert(2, i, dummy_block(96 * 1024));
    }
    assert_eq!(cache.usage(), baseline, "a strict refusal moved the total");
    assert!(cache.get(2, 0).is_none());
    assert!(cache.get(1, 0).is_some());
    assert_shard_invariants(&cache, "strict refusal");
}

/// Attack: an entry that exactly fills a shard, then another. The
/// budget is a hard bound at the boundary, not just below it.
#[test]
fn an_entry_that_exactly_fills_a_shard_is_admitted_once() {
    let probe = dummy_block(4096);
    let exact = entry_charge(&CacheEntry::Data(Arc::clone(&probe)));
    let cache = BlockCache::with_config(exact, 0, false);
    assert_eq!(cache.capacity(), exact);
    cache.insert(1, 0, dummy_block(4096));
    assert_eq!(cache.usage(), exact);
    cache.insert(1, 4096, dummy_block(4096));
    assert_eq!(cache.usage(), exact, "two entries fit a one-entry budget");
    assert_eq!(cache.entry_count(), 1);
    assert_shard_invariants(&cache, "exact fill");
}

/// Exact LRU with the shipped cache's sharding, per-shard byte budget
/// and per-entry charge, so a replay of one trace through both differs
/// only by the replacement policy.
///
/// Recency is a monotonic tick in a `BTreeMap`, which is exact rather
/// than approximate: this is the policy CLOCK has to be measured
/// against, not another approximation of it.
struct LruShard {
    capacity: usize,
    used: usize,
    tick: u64,
    entries: std::collections::HashMap<CacheKey, (usize, u64)>,
    order: std::collections::BTreeMap<u64, CacheKey>,
}

impl LruShard {
    fn touch(&mut self, key: CacheKey) -> bool {
        let Some((_, at)) = self.entries.get_mut(&key) else {
            return false;
        };
        let old = *at;
        self.tick += 1;
        *at = self.tick;
        self.order.remove(&old);
        self.order.insert(self.tick, key);
        true
    }

    fn insert(&mut self, key: CacheKey, charge: usize) {
        if charge > self.capacity {
            return;
        }
        if let Some((old_charge, at)) = self.entries.remove(&key) {
            self.order.remove(&at);
            self.used -= old_charge;
        }
        while self.used + charge > self.capacity {
            let Some((&at, &victim)) = self.order.iter().next() else {
                break;
            };
            self.order.remove(&at);
            if let Some((c, _)) = self.entries.remove(&victim) {
                self.used -= c;
            }
        }
        self.tick += 1;
        self.entries.insert(key, (charge, self.tick));
        self.order.insert(self.tick, key);
        self.used += charge;
    }
}

struct ShardedLru {
    shards: Vec<LruShard>,
    mask: u64,
}

impl ShardedLru {
    fn new(capacity: usize, num_shards: usize) -> Self {
        let per_shard = capacity / num_shards;
        Self {
            shards: (0..num_shards)
                .map(|_| LruShard {
                    capacity: per_shard,
                    used: 0,
                    tick: 0,
                    entries: Default::default(),
                    order: Default::default(),
                })
                .collect(),
            mask: (num_shards - 1) as u64,
        }
    }

    /// The shipped shard hash, byte for byte, so the two caches split
    /// the same trace the same way.
    fn shard(&self, key: &CacheKey) -> usize {
        let mut buf = [0u8; 16];
        buf[..8].copy_from_slice(&key.file_id.to_le_bytes());
        buf[8..].copy_from_slice(&key.offset.to_le_bytes());
        (xxh3_64(&buf) & self.mask) as usize
    }

    fn access(&mut self, key: CacheKey, charge: usize) -> bool {
        let idx = self.shard(&key);
        if self.shards[idx].touch(key) {
            return true;
        }
        self.shards[idx].insert(key, charge);
        false
    }
}

/// splitmix64: a deterministic generator so a trace is reproducible
/// from its seed on any machine, with no dependency added.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Zipfian sampler over `n` keys, by inverse CDF.
struct Zipf {
    cdf: Vec<f64>,
}

impl Zipf {
    fn new(n: usize, theta: f64) -> Self {
        let mut cdf = Vec::with_capacity(n);
        let mut total = 0.0;
        for i in 0..n {
            total += 1.0 / ((i + 1) as f64).powf(theta);
            cdf.push(total);
        }
        for v in cdf.iter_mut() {
            *v /= total;
        }
        Self { cdf }
    }
    fn sample(&self, rng: &mut Rng) -> u64 {
        let u = rng.next_f64();
        match self
            .cdf
            .binary_search_by(|p| p.partial_cmp(&u).unwrap_or(std::cmp::Ordering::Less))
        {
            Ok(i) | Err(i) => i.min(self.cdf.len() - 1) as u64,
        }
    }
}

fn trace_key(block: u64) -> CacheKey {
    CacheKey {
        file_id: block / 512,
        offset: (block % 512) * 4096,
    }
}

/// Deterministic access traces. Each returns block indices.
fn build_trace(name: &str, ops: usize, universe: usize) -> Vec<u64> {
    let mut rng = Rng(0x5EED_1234_ABCD_0001);
    let mut trace = Vec::with_capacity(ops);
    match name {
        "zipf0.99" => {
            let z = Zipf::new(universe, 0.99);
            for _ in 0..ops {
                trace.push(z.sample(&mut rng));
            }
        }
        "zipf+sweep" => {
            // Point reads punctuated by a compaction-style sweep of a
            // contiguous run that is never re-read.
            let z = Zipf::new(universe, 0.99);
            let mut sweep = universe as u64;
            while trace.len() < ops {
                for _ in 0..20_000 {
                    trace.push(z.sample(&mut rng));
                }
                for i in 0..4_000u64 {
                    trace.push(sweep + i);
                }
                sweep += 4_000;
            }
            trace.truncate(ops);
        }
        "loop" => {
            // A cyclic sweep of a working set larger than the cache:
            // the pathology neither policy fixes.
            for i in 0..ops {
                trace.push((i % universe) as u64);
            }
        }
        "recency-ladder" => {
            // Adversarial for CLOCK: a strictly recency-ordered walk of
            // a working set just past the budget, where exact LRU keeps
            // the newest entries and an approximation can drop one that
            // was touched more recently than the entry it spares.
            let mut i = 0u64;
            while trace.len() < ops {
                for step in 0..universe as u64 {
                    trace.push((i + step) % universe as u64);
                    if step % 8 == 0 {
                        trace.push((i + step) % universe as u64);
                    }
                }
                i += 3;
            }
            trace.truncate(ops);
        }
        "hot+scan" => {
            // A hot set half the budget, plus uniform noise ten times
            // the budget: the mix a real LSM read path sees.
            let hot = universe / 8;
            for _ in 0..ops {
                if rng.next_f64() < 0.6 {
                    trace.push(rng.next_u64() % hot as u64);
                } else {
                    trace.push(rng.next_u64() % universe as u64);
                }
            }
        }
        "stack-depth" => {
            // The canonical LRU-optimal shape: accesses drawn by stack
            // distance, so exact recency ranking is worth the most it
            // can ever be worth. If CLOCK loses anywhere, here.
            let mut stack: Vec<u64> = (0..3_000u64).collect();
            for _ in 0..ops {
                let d = (rng.next_u64() % stack.len() as u64) as usize;
                let block = stack.remove(d);
                stack.insert(0, block);
                trace.push(block);
            }
        }
        "reread-at-distance" => {
            // Directly attacks insert-ref = 0: every block is read once
            // on admission and once again exactly `D` accesses later.
            // Exact LRU keeps it whenever D is inside the budget; a
            // fresh CLOCK entry has no reference bit to protect it.
            const D: u64 = 400;
            let mut i = 0u64;
            while trace.len() < ops {
                trace.push(i);
                if i >= D {
                    trace.push(i - D);
                }
                i += 1;
            }
            trace.truncate(ops);
        }
        other => panic!("unknown trace {other}"),
    }
    trace
}

/// The number that decides the policy change: hits on a fixed trace,
/// the shipped CLOCK cache against exact LRU at the same byte budget.
///
/// This is a count, not a benchmark: it does not depend on machine
/// load and it reproduces exactly from the seed above.
#[test]
fn clock_hit_rate_against_exact_lru_on_fixed_traces() {
    const OPS: usize = 300_000;
    const UNIVERSE: usize = 20_000;
    // Every miss caches a block of its own, as every block the engine
    // reads from a table is its own allocation: one `Arc` cached under
    // every key would pin every entry (see `BlockCache::insert`).
    let charge = entry_charge(&CacheEntry::Data(dummy_block(1024)));

    let mut worst: Option<(String, usize, f64)> = None;
    println!("\ntrace                entries    LRU hits   LRU %   CLOCK hits CLOCK %   delta");
    for name in [
        "zipf0.99",
        "zipf+sweep",
        "loop",
        "recency-ladder",
        "hot+scan",
        "stack-depth",
        "reread-at-distance",
    ] {
        let trace = build_trace(name, OPS, UNIVERSE);
        for entries in [256usize, 1024, 4096] {
            let capacity = entries * charge;
            let cache = BlockCache::with_config(capacity, 0, false);
            assert_eq!(cache.num_shards(), 1);
            let mut lru = ShardedLru::new(cache.capacity(), 1);

            let mut clock_hits = 0usize;
            let mut lru_hits = 0usize;
            for &b in &trace {
                let key = trace_key(b);
                if cache.get(key.file_id, key.offset).is_some() {
                    clock_hits += 1;
                } else {
                    cache.insert(key.file_id, key.offset, dummy_block(1024));
                }
                if lru.access(key, charge) {
                    lru_hits += 1;
                }
                assert!(cache.usage() <= cache.capacity());
            }
            let pct = |h: usize| h as f64 * 100.0 / OPS as f64;
            let delta = pct(clock_hits) - pct(lru_hits);
            println!(
                "{name:<20} {entries:>7} {lru_hits:>11} {:>7.2} {clock_hits:>11} {:>7.2} {delta:>+7.2}",
                pct(lru_hits),
                pct(clock_hits)
            );
            if worst.as_ref().is_none_or(|w| delta < w.2) {
                worst = Some((name.to_string(), entries, delta));
            }
        }
    }
    let (name, entries, delta) = worst.expect("at least one configuration");
    println!("worst configuration for CLOCK: {name} at {entries} entries, {delta:+.2} points\n");
    assert!(
        delta > -2.0,
        "CLOCK loses {delta:.2} points against exact LRU on {name} at {entries} entries: \
         a policy change that costs this much hit rate is worse than the dependency it removed"
    );
}

/// Attack: `evict_file` is a range walk over an ordered map, so the
/// ends of the key space and adjacent file ids are where it would run
/// off or stop early.
#[test]
fn evict_file_walks_only_its_own_file_at_the_key_space_edges() {
    let cache = BlockCache::with_config(64 * 1024 * 1024, 6, false);
    let files = [0u64, 1, 2, u64::MAX - 1, u64::MAX];
    let offsets = [0u64, 1, 4096, u64::MAX / 2, u64::MAX];
    let mut tag = 1u8;
    let mut placed = Vec::new();
    for f in files {
        for o in offsets {
            cache.insert(f, o, block_with(tag, 256));
            placed.push((f, o, tag));
            tag = tag.wrapping_add(1).max(1);
        }
    }
    let mut evicted: Vec<u64> = Vec::new();
    for f in files {
        cache.evict_file(f);
        evicted.push(f);
        for (pf, po, ptag) in &placed {
            let got = cache.get(*pf, *po);
            if evicted.contains(pf) {
                assert!(got.is_none(), "file {pf} offset {po} survived its eviction");
            } else {
                let block = got.unwrap_or_else(|| {
                    panic!("evicting {f} dropped ({pf},{po}) from a cache that fits it")
                });
                assert_eq!(tag_of(&block), *ptag, "evicting {f} corrupted ({pf},{po})");
            }
        }
        assert_shard_invariants(&cache, "evict_file at the edges");
    }
    assert_eq!(cache.usage(), 0);
    assert_eq!(cache.entry_count(), 0);
}

/// Attack: `evict_file` must not stop at the first offset gap, and must
/// not walk into the next file id.
#[test]
fn evict_file_covers_a_sparse_offset_range_and_stops_at_the_next_file() {
    let cache = BlockCache::with_config(64 * 1024 * 1024, 6, false);
    for f in 5..=7u64 {
        for o in [0u64, 7, 1 << 20, 1 << 40, u64::MAX] {
            cache.insert(f, o, block_with((f * 10) as u8, 256));
        }
    }
    let before = cache.usage();
    cache.evict_file(6);
    assert_eq!(cache.entry_count(), 10, "evict_file took the wrong range");
    assert!(cache.usage() < before);
    for o in [0u64, 7, 1 << 20, 1 << 40, u64::MAX] {
        assert!(cache.get(6, o).is_none());
        assert!(cache.get(5, o).is_some());
        assert!(cache.get(7, o).is_some());
    }
    assert_shard_invariants(&cache, "sparse evict_file");
}

/// The ring's segments stay allocated after the entries they held are
/// gone, so `usage()` can read zero while the shard still holds slot
/// memory. That retention has to stay bounded by the byte budget rather
/// than by how many inserts the cache has ever seen, after `evict_file`
/// and after `clear` alike: a lock-free ring cannot free a segment another
/// thread may be indexing, so `clear` empties it without shrinking it.
#[test]
fn an_emptied_cache_retains_only_budget_bounded_ring_capacity() {
    let budget = 1024 * 1024;
    let cache = BlockCache::with_config(budget, 0, false);
    for i in 0..300_000u64 {
        cache.insert(1, i * 64, dummy_block(0));
    }
    let ring_bytes = |cache: &BlockCache| -> usize {
        cache
            .shards
            .iter()
            .map(|s| s.state().map_or(0, |state| state.ring.allocated_bytes()))
            .sum()
    };
    let full = ring_bytes(&cache);
    cache.evict_file(1);
    assert_eq!(cache.usage(), 0);
    let retained = ring_bytes(&cache);
    println!(
        "ADVRETAIN emptied cache still holds {retained} ring bytes against a {budget}-byte budget"
    );
    assert_eq!(retained, full, "evicting entries never grows the ring");
    assert!(
        retained <= budget / 4,
        "an emptied cache retains {retained} ring bytes against a {budget}-byte budget"
    );
    assert_shard_invariants(&cache, "emptied ring retention");

    // Refilling reuses the retained slots rather than growing past them.
    for i in 0..300_000u64 {
        cache.insert(2, i * 64, dummy_block(0));
    }
    cache.clear();
    assert_eq!(cache.usage(), 0);
    assert_eq!(cache.entry_count(), 0);
    assert_eq!(
        ring_bytes(&cache),
        retained,
        "a refill after the eviction grew the ring instead of reusing it"
    );
    assert_shard_invariants(&cache, "cleared ring retention");
}

/// Probe: `regolith.block-cache-add` counts only inserts that stored a
/// block.
///
/// The ticker documents itself as one per miss that populated the
/// cache. It used to be incremented after the shard lock was released,
/// on a path every refusal also reached, so a strict-mode refusal that
/// stored nothing was counted as an add while the cache-wide
/// reservation refusal returned early and was not: the two refusal
/// paths disagreed with each other and with the doc. This pins all
/// three paths plus a positive control.
#[test]
fn the_add_ticker_counts_only_inserts_that_stored_a_block() {
    let stats = Arc::new(Statistics::new());
    let cache =
        BlockCache::with_config(8 * 64 * 1024, 3, true).with_stats(Some(Arc::clone(&stats)));
    for _ in 0..10 {
        cache.insert(1, 0, dummy_block(96 * 1024));
    }
    assert_eq!(cache.usage(), 0, "strict mode stored an oversized entry");
    assert_eq!(
        stats.get_ticker(Ticker::BlockCacheAdd),
        0,
        "a strict-mode refusal stored nothing but was counted as an add"
    );

    // A block larger than the whole budget in non-strict mode is the
    // second refusal path, and it stores nothing either.
    let stats = Arc::new(Statistics::new());
    let cache = BlockCache::with_config(64 * 1024, 0, false).with_stats(Some(Arc::clone(&stats)));
    for _ in 0..10 {
        cache.insert(1, 0, dummy_block(256 * 1024));
    }
    assert_eq!(cache.usage(), 0);
    assert_eq!(
        stats.get_ticker(Ticker::BlockCacheAdd),
        0,
        "a block larger than the whole budget was counted as an add"
    );

    // The third: an oversized entry that fits the cache-wide budget in
    // principle but not against the current total. Some calls win the
    // reservation and store, the rest refuse, so the ticker lands
    // strictly between the two.
    let stats = Arc::new(Statistics::new());
    let cache =
        BlockCache::with_config(8 * 64 * 1024, 3, false).with_stats(Some(Arc::clone(&stats)));
    let oversized = dummy_block(100 * 1024);
    assert!(
        entry_charge(&CacheEntry::Data(Arc::clone(&oversized))) > cache.capacity() / 8
            && entry_charge(&CacheEntry::Data(Arc::clone(&oversized))) < cache.capacity(),
        "setup: the block must be oversized for a shard but not for the cache"
    );
    const CALLS: u64 = 200;
    for i in 0..CALLS {
        cache.insert(i, 0, Arc::clone(&oversized));
    }
    let adds = stats.get_ticker(Ticker::BlockCacheAdd);
    assert!(
        adds > 0 && adds < CALLS,
        "expected some of {CALLS} oversized inserts to store and some to refuse, got {adds}"
    );
    assert!(cache.usage() <= cache.capacity());

    // Positive control: an insert that stores is counted exactly once.
    let stats = Arc::new(Statistics::new());
    let cache =
        BlockCache::with_config(8 * 1024 * 1024, 3, false).with_stats(Some(Arc::clone(&stats)));
    for i in 0..25u64 {
        cache.insert(1, i * 4096, dummy_block(512));
    }
    assert_eq!(
        stats.get_ticker(Ticker::BlockCacheAdd),
        25,
        "an insert that stored a block was not counted"
    );
    assert_eq!(cache.entry_count(), 25);
}
