//! The block cache's byte budget is a hard bound.
//!
//! Every test drives the cache through the public API and checks the
//! `regolith.block-cache-usage` property against `regolith.block-cache-capacity`.
//! Each one reproduced an over-budget cache or a silently shrunken one
//! before the accounting rework.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads, the filesystem or proptest, none of which exist there. The browser
// suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use regolith::{Db, Options, Statistics, Ticker};
use tempfile::TempDir;

fn usage_and_capacity(db: &Db) -> (u64, u64) {
    (
        db.get_int_property("regolith.block-cache-usage").unwrap(),
        db.get_int_property("regolith.block-cache-capacity")
            .unwrap(),
    )
}

#[test]
fn values_larger_than_one_shard_stay_inside_the_budget() {
    const BUDGET: usize = 16 * 1024 * 1024;
    const VALUE_LEN: usize = 256 * 1024;
    const KEYS: u32 = 300;

    for shard_bits in [0u32, 4, 8] {
        let dir = TempDir::new().unwrap();
        let db = Db::open(
            dir.path(),
            Options::default()
                .block_cache_size(BUDGET)
                .block_cache_num_shard_bits(shard_bits)
                .write_buffer_size(8 * 1024 * 1024),
        )
        .unwrap();

        let value = vec![7u8; VALUE_LEN];
        for i in 0..KEYS {
            regolith::through_stalls(|| db.put(format!("k{i:05}").as_bytes(), &value)).unwrap();
        }
        db.compact_range(None, None).wait().unwrap();
        for i in 0..KEYS {
            assert!(db.get(format!("k{i:05}").as_bytes()).unwrap().is_some());
        }

        let (usage, capacity) = usage_and_capacity(&db);
        assert!(
            usage <= capacity,
            "shard_bits {shard_bits}: cache holds {usage} bytes against a {capacity}-byte budget"
        );
    }
}

#[test]
fn a_budget_below_one_block_holds_nothing_it_cannot_afford() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(
        dir.path(),
        Options::default()
            .block_cache_size(1024)
            .block_size(16 * 1024)
            .write_buffer_size(16 * 1024),
    )
    .unwrap();
    for i in 0..2000u32 {
        regolith::through_stalls(|| db.put(format!("k{i:06}").as_bytes(), &[9u8; 128])).unwrap();
    }
    db.compact_range(None, None).wait().unwrap();
    for i in 0..2000u32 {
        assert!(db.get(format!("k{i:06}").as_bytes()).unwrap().is_some());
    }
    let (usage, capacity) = usage_and_capacity(&db);
    assert!(
        usage <= capacity,
        "a {capacity}-byte cache holds {usage} bytes"
    );
}

#[test]
fn strict_capacity_limit_holds_the_budget() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(
        dir.path(),
        Options::default()
            .block_cache_size(16 * 1024 * 1024)
            .block_cache_num_shard_bits(8)
            .strict_capacity_limit(true)
            .write_buffer_size(8 * 1024 * 1024),
    )
    .unwrap();
    let value = vec![3u8; 256 * 1024];
    for i in 0..200u32 {
        regolith::through_stalls(|| db.put(format!("k{i:05}").as_bytes(), &value)).unwrap();
    }
    db.compact_range(None, None).wait().unwrap();
    for i in 0..200u32 {
        assert!(db.get(format!("k{i:05}").as_bytes()).unwrap().is_some());
    }
    let (usage, capacity) = usage_and_capacity(&db);
    assert!(usage <= capacity, "usage {usage} over capacity {capacity}");
}

#[test]
fn a_zero_budget_serves_a_real_read_workload() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(
        dir.path(),
        Options::default()
            .block_cache_size(0)
            .write_buffer_size(16 * 1024)
            .block_size(1024),
    )
    .unwrap();
    for i in 0..5000u32 {
        regolith::through_stalls(|| {
            db.put(format!("k{i:06}").as_bytes(), format!("v{i:06}").as_bytes())
        })
        .unwrap();
    }
    db.compact_range(None, None).wait().unwrap();

    for i in 0..5000u32 {
        assert_eq!(
            db.get(format!("k{i:06}").as_bytes()).unwrap(),
            Some(format!("v{i:06}").into_bytes())
        );
    }
    assert!(db.get(b"absent").unwrap().is_none());

    let mut scanned = 0;
    let mut it = db.iter();
    it.seek_to_first();
    while it.valid() {
        scanned += 1;
        it.next();
    }
    it.status().unwrap();
    assert_eq!(scanned, 5000);

    let (usage, capacity) = usage_and_capacity(&db);
    assert_eq!((usage, capacity), (0, 0));
}

#[test]
fn the_configured_block_size_does_not_shrink_the_cache() {
    // Regression: an entry-count cap derived from the `block_size` in
    // the `Options` used to open evicted entries that fit comfortably
    // inside the byte budget, so reopening the same files with a
    // larger configured block size doubled the miss count with nothing
    // reported anywhere.
    const KEYS: u32 = 20_000;
    let dir = TempDir::new().unwrap();
    {
        let db = Db::open(
            dir.path(),
            Options::default()
                .block_size(1024)
                .write_buffer_size(64 * 1024),
        )
        .unwrap();
        for i in 0..KEYS {
            regolith::through_stalls(|| db.put(format!("k{i:06}").as_bytes(), &[i as u8; 64]))
                .unwrap();
        }
        db.compact_range(None, None).wait().unwrap();
        db.close().unwrap();
    }

    let mut retained = Vec::new();
    let mut misses = Vec::new();
    for block_size in [1024usize, 1024 * 1024] {
        let stats = Arc::new(Statistics::new());
        let db = Db::open(
            dir.path(),
            Options::default()
                .block_size(block_size)
                .block_cache_size(64 * 1024 * 1024)
                .statistics(Some(Arc::clone(&stats))),
        )
        .unwrap();
        for _ in 0..2 {
            for i in 0..KEYS {
                assert!(db.get(format!("k{i:06}").as_bytes()).unwrap().is_some());
            }
        }
        misses.push(stats.get_ticker(Ticker::BlockCacheMiss));
        retained.push(usage_and_capacity(&db).0);
    }

    assert!(
        misses[1] <= misses[0] * 2,
        "reopening with a larger configured block size cost hit rate: {misses:?}"
    );
    assert!(
        retained[1] * 2 >= retained[0],
        "reopening with a larger configured block size shrank the cache: {retained:?}"
    );
}
