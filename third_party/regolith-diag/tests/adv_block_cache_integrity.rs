//! End to end: a cache small enough that the CLOCK hand runs on every
//! read, while compaction unlinks files under it.
//!
//! `evict_file` is called by `delete_old_files` the moment a compaction
//! drops an input SSTable, so a reader can be inside a lookup for a
//! file that is being evicted. If the cache ever returned a block under
//! the wrong key, or dropped one it still owed, the values read here
//! would not match what was written.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};

use regolith::{Db, Options, Statistics, Ticker};
use tempfile::TempDir;

const KEYS: u32 = 40_000;

fn key(i: u32) -> Vec<u8> {
    format!("k{i:07}").into_bytes()
}

fn value(i: u32) -> Vec<u8> {
    let mut v = format!("v{i:07}:").into_bytes();
    v.extend(std::iter::repeat_n((i % 251) as u8, 64 + (i % 97) as usize));
    v
}

#[test]
fn readers_never_see_a_wrong_block_while_compaction_evicts_files() {
    let dir = TempDir::new().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = Arc::new(
        Db::open(
            dir.path(),
            Options::default()
                // Far smaller than the data, so the hand never stops.
                .block_cache_size(256 * 1024)
                .block_cache_num_shard_bits(4)
                .block_size(1024)
                .write_buffer_size(256 * 1024)
                .statistics(Some(Arc::clone(&stats))),
        )
        .unwrap(),
    );

    for i in 0..KEYS {
        regolith::through_stalls(|| db.put(&key(i), &value(i))).unwrap();
    }
    db.compact_range(None, None).wait().unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(Barrier::new(5));
    let readers: Vec<_> = (0..4u32)
        .map(|t| {
            let db = Arc::clone(&db);
            let stop = Arc::clone(&stop);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                let mut reads = 0u64;
                let mut i = t;
                while !stop.load(Ordering::Relaxed) {
                    let k = i % KEYS;
                    assert_eq!(
                        db.get(&key(k)).unwrap(),
                        Some(value(k)),
                        "reader {t} read the wrong value for key {k}"
                    );
                    reads += 1;
                    i = i.wrapping_add(7919);
                }
                reads
            })
        })
        .collect();

    barrier.wait();
    // Rewrite and recompact so `delete_old_files` keeps calling
    // `evict_file` under the readers.
    for round in 0..6u32 {
        for i in (round..KEYS).step_by(97) {
            regolith::through_stalls(|| db.put(&key(i), &value(i))).unwrap();
        }
        db.compact_range(None, None).wait().unwrap();
    }
    stop.store(true, Ordering::Relaxed);

    let total: u64 = readers.into_iter().map(|r| r.join().expect("reader")).sum();
    assert!(total > 10_000, "the readers barely ran: {total} reads");

    for i in 0..KEYS {
        assert_eq!(db.get(&key(i)).unwrap(), Some(value(i)), "key {i} diverged");
    }

    let usage = db.get_int_property("regolith.block-cache-usage").unwrap();
    let capacity = db
        .get_int_property("regolith.block-cache-capacity")
        .unwrap();
    let hits = stats.get_ticker(Ticker::BlockCacheHit);
    let misses = stats.get_ticker(Ticker::BlockCacheMiss);
    println!(
        "ADVINTEGRITY reads={total} usage={usage} capacity={capacity} hits={hits} misses={misses}"
    );
    assert!(
        usage <= capacity,
        "cache holds {usage} bytes against a {capacity}-byte budget after the storm"
    );
    assert!(hits > 0, "the cache never served a hit");
}

/// The same storm with the cache switched off: reads must still be
/// correct and the cache must stay at zero, allocating nothing.
#[test]
fn a_disabled_cache_serves_the_same_storm() {
    let dir = TempDir::new().unwrap();
    let db = Arc::new(
        Db::open(
            dir.path(),
            Options::default()
                .block_cache_size(0)
                .block_cache_num_shard_bits(8)
                .block_size(1024)
                .write_buffer_size(256 * 1024),
        )
        .unwrap(),
    );
    for i in 0..KEYS {
        regolith::through_stalls(|| db.put(&key(i), &value(i))).unwrap();
    }
    db.compact_range(None, None).wait().unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let readers: Vec<_> = (0..4u32)
        .map(|t| {
            let db = Arc::clone(&db);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut i = t;
                while !stop.load(Ordering::Relaxed) {
                    let k = i % KEYS;
                    assert_eq!(db.get(&key(k)).unwrap(), Some(value(k)));
                    i = i.wrapping_add(7919);
                }
            })
        })
        .collect();
    for _ in 0..3 {
        for i in (0..KEYS).step_by(211) {
            regolith::through_stalls(|| db.put(&key(i), &value(i))).unwrap();
        }
        db.compact_range(None, None).wait().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    for r in readers {
        r.join().expect("reader");
    }
    assert_eq!(
        (
            db.get_int_property("regolith.block-cache-usage").unwrap(),
            db.get_int_property("regolith.block-cache-capacity")
                .unwrap()
        ),
        (0, 0)
    );
}
