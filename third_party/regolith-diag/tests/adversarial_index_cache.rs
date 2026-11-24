//! Adversarial probes on cached SSTable metadata.
//!
//! With `cache_index_and_filter_blocks` on, an index or filter block is
//! evictable like any other. These tests keep the cache far too small to
//! hold the metadata, so every lookup has to re-read and re-decode it,
//! and check that reads still return the same answers as the pinned
//! configuration - including range scans, iterators and misses.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use regolith::{Db, Options};
use tempfile::TempDir;

const KEYS: usize = 6_000;

fn value_for(i: usize) -> Vec<u8> {
    let mut v = format!("value-{i:08}-").into_bytes();
    v.resize(160, (i % 251) as u8);
    v
}

fn key_for(i: usize) -> Vec<u8> {
    format!("key{i:08}").into_bytes()
}

fn base(partitioned: bool) -> Options {
    Options::default()
        .write_buffer_size(64 * 1024)
        .block_size(512)
        .metadata_block_size(512)
        .target_file_size(128 * 1024)
        .partitioned_index(partitioned)
        .block_cache_num_shard_bits(0)
}

/// Seed a database once and hand back its directory.
fn seeded(partitioned: bool) -> TempDir {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), base(partitioned)).unwrap();
    for i in 0..KEYS {
        regolith::through_stalls(|| db.put(&key_for(i), &value_for(i))).unwrap();
    }
    db.compact_range(None, None).wait().unwrap();
    db.close().unwrap();
    dir
}

fn verify_everything(db: &Db, label: &str) {
    for i in 0..KEYS {
        assert_eq!(
            db.get(&key_for(i)).unwrap().as_ref(),
            Some(&value_for(i)),
            "{label}: key {i} read back wrong"
        );
        assert!(db.has(&key_for(i)).unwrap(), "{label}: has() lost key {i}");
        assert_eq!(
            db.get_size(&key_for(i)).unwrap(),
            Some(value_for(i).len()),
            "{label}: get_size() wrong for key {i}"
        );
    }
    for i in 0..500 {
        let miss = format!("absent{i:08}").into_bytes();
        assert_eq!(db.get(&miss).unwrap(), None, "{label}: invented a miss");
        assert!(!db.has(&miss).unwrap(), "{label}: has() invented a miss");
    }
    let scanned = db.scan(None, None).unwrap();
    assert_eq!(scanned.len(), KEYS, "{label}: scan lost entries");
    for (i, (k, v)) in scanned.iter().enumerate() {
        assert_eq!(k, &key_for(i), "{label}: scan out of order at {i}");
        assert_eq!(v, &value_for(i), "{label}: scan value wrong at {i}");
    }
}

/// The metadata these tests evict is far larger than the cache they run
/// against, so the eviction pressure is real rather than assumed.
fn assert_cache_is_far_too_small(dir: &TempDir, partitioned: bool, cache_bytes: usize) {
    let pinned = Db::open(
        dir.path(),
        base(partitioned)
            .block_cache_size(32 * 1024 * 1024)
            .cache_index_and_filter_blocks(false),
    )
    .unwrap();
    let metadata = pinned
        .get_int_property("regolith.pinned-metadata-bytes")
        .expect("regolith.pinned-metadata-bytes is a known property");
    pinned.close().unwrap();
    assert!(
        metadata > (cache_bytes as u64) * 4,
        "partitioned={partitioned}: metadata is {metadata} B against a {cache_bytes} B cache, \
         which is not enough pressure to force eviction"
    );
}

/// A cache far smaller than the metadata it is asked to hold. Every
/// index and filter lookup evicts something, so the read path exercises
/// the re-read-and-re-decode branch on nearly every call.
#[test]
fn a_flat_index_stays_correct_when_the_cache_cannot_hold_it() {
    let dir = seeded(false);
    assert_cache_is_far_too_small(&dir, false, 8 * 1024);
    let opts = base(false)
        .block_cache_size(8 * 1024)
        .cache_index_and_filter_blocks(true);
    let db = Db::open(dir.path(), opts).unwrap();
    verify_everything(&db, "flat index, 8 KiB cache");
}

#[test]
fn a_partitioned_index_stays_correct_when_the_cache_cannot_hold_it() {
    let dir = seeded(true);
    assert_cache_is_far_too_small(&dir, true, 4 * 1024);
    let opts = base(true)
        .block_cache_size(4 * 1024)
        .cache_index_and_filter_blocks(true);
    let db = Db::open(dir.path(), opts).unwrap();
    verify_everything(&db, "partitioned index, 4 KiB cache");
}

/// `strict_capacity_limit` makes the cache refuse an oversized insert
/// outright rather than evicting for it. The reader must fall back to
/// pinning the block, not to a wrong answer or an error.
#[test]
fn a_strict_capacity_cache_that_refuses_the_index_still_reads_correctly() {
    let dir = seeded(false);
    let opts = base(false)
        .block_cache_size(4 * 1024)
        .strict_capacity_limit(true)
        .cache_index_and_filter_blocks(true);
    let db = Db::open(dir.path(), opts).unwrap();
    verify_everything(&db, "strict capacity, 4 KiB cache");
}

/// The evicting configuration and the pinned configuration must agree
/// key for key, byte for byte, over the same files on disk.
#[test]
fn evicted_metadata_answers_identically_to_pinned_metadata() {
    for partitioned in [false, true] {
        let dir = seeded(partitioned);

        let pinned = Db::open(
            dir.path(),
            base(partitioned)
                .block_cache_size(32 * 1024 * 1024)
                .cache_index_and_filter_blocks(false),
        )
        .unwrap();
        let pinned_scan = pinned.scan(None, None).unwrap();
        let pinned_reads: Vec<Option<Vec<u8>>> = (0..KEYS)
            .map(|i| pinned.get(&key_for(i)).unwrap())
            .collect();
        pinned.close().unwrap();
        drop(pinned);

        let evicting = Db::open(
            dir.path(),
            base(partitioned)
                .block_cache_size(8 * 1024)
                .cache_index_and_filter_blocks(true),
        )
        .unwrap();
        let evicting_scan = evicting.scan(None, None).unwrap();
        let evicting_reads: Vec<Option<Vec<u8>>> = (0..KEYS)
            .map(|i| evicting.get(&key_for(i)).unwrap())
            .collect();

        assert_eq!(
            pinned_scan, evicting_scan,
            "partitioned={partitioned}: scans differ between pinned and evicting metadata"
        );
        assert_eq!(
            pinned_reads, evicting_reads,
            "partitioned={partitioned}: point reads differ between pinned and evicting metadata"
        );
    }
}

/// Many readers hammering a cache too small for the metadata, so index
/// and filter entries are evicted out from under in-flight lookups.
#[test]
fn concurrent_readers_survive_metadata_eviction() {
    let dir = seeded(true);
    let db = Arc::new(
        Db::open(
            dir.path(),
            base(true)
                .block_cache_size(8 * 1024)
                .cache_index_and_filter_blocks(true),
        )
        .unwrap(),
    );

    let wrong = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for t in 0..8 {
        let db = Arc::clone(&db);
        let wrong = Arc::clone(&wrong);
        handles.push(thread::spawn(move || {
            for step in 0..2_000usize {
                let i = (step * 7 + t * 613) % KEYS;
                match db.get(&key_for(i)).unwrap() {
                    Some(v) if v == value_for(i) => {}
                    _ => {
                        wrong.fetch_add(1, Ordering::Relaxed);
                    }
                }
                if db.get_slice(&key_for(i)).unwrap().map(|s| s.to_vec()) != Some(value_for(i)) {
                    wrong.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(
        wrong.load(Ordering::Relaxed),
        0,
        "metadata eviction produced wrong reads under concurrency"
    );
}

/// Reverse iteration and seeks, which take a different index path from
/// point lookups, under the same eviction pressure.
#[test]
fn iteration_stays_correct_when_metadata_is_evicted() {
    let dir = seeded(true);
    let db = Db::open(
        dir.path(),
        base(true)
            .block_cache_size(8 * 1024)
            .cache_index_and_filter_blocks(true),
    )
    .unwrap();

    let mut it = db.iter();
    it.seek_to_last();
    let mut back = Vec::new();
    while it.valid() {
        back.push((it.key().unwrap().to_vec(), it.value().unwrap().to_vec()));
        it.prev();
    }
    it.status().unwrap();
    back.reverse();
    assert_eq!(back.len(), KEYS, "reverse iteration lost entries");
    for (i, (k, v)) in back.iter().enumerate() {
        assert_eq!(k, &key_for(i), "reverse iteration out of order at {i}");
        assert_eq!(v, &value_for(i), "reverse iteration value wrong at {i}");
    }

    for i in (0..KEYS).step_by(211) {
        let mut it = db.iter();
        it.seek(&key_for(i));
        assert!(it.valid(), "seek to key {i} landed nowhere");
        assert_eq!(it.key().unwrap(), key_for(i).as_slice());
        assert_eq!(it.value().unwrap(), value_for(i).as_slice());
        it.seek_for_prev(&key_for(i));
        assert!(it.valid());
        assert_eq!(it.key().unwrap(), key_for(i).as_slice());
    }
}
