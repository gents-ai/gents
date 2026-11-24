//! SSTable index and filter blocks under the block-cache budget.
//!
//! These are the public-API checks that the bound is real: with
//! `cache_index_and_filter_blocks` on, what the open files hold outside
//! `block_cache_size` collapses to the pinned top-level index of each
//! partitioned file (and to nothing at all for a flat file), and reads
//! keep answering identically either way.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use common::force_compaction;
use regolith::{Db, Options};
use tempfile::TempDir;

/// Small files and small blocks, so a modest fill produces several
/// SSTables with non-trivial indexes.
fn opts(partitioned: bool, cache_metadata: bool) -> Options {
    Options::default()
        .write_buffer_size(128 * 1024)
        .block_size(1024)
        .target_file_size(256 * 1024)
        .partitioned_index(partitioned)
        .metadata_block_size(1024)
        .cache_index_and_filter_blocks(cache_metadata)
}

const KEYS: usize = 8_000;

fn fill(db: &Db) {
    let value = vec![b'v'; 128];
    for i in 0..KEYS {
        regolith::through_stalls(|| db.put(format!("key{i:08}").as_bytes(), &value)).unwrap();
    }
}

fn verify(db: &Db) {
    let value = vec![b'v'; 128];
    for i in (0..KEYS).step_by(13) {
        assert_eq!(
            db.get(format!("key{i:08}").as_bytes()).unwrap().as_ref(),
            Some(&value),
            "key{i:08} must read back"
        );
    }
    for i in 0..200 {
        assert_eq!(db.get(format!("absent{i:08}").as_bytes()).unwrap(), None);
    }
}

fn pinned_metadata(db: &Db) -> u64 {
    db.get_int_property("regolith.pinned-metadata-bytes")
        .expect("regolith.pinned-metadata-bytes is a known property")
}

/// Build a database on disk, then reopen it under `cache_metadata` and
/// hand the reopened handle to `check`.
fn with_reopened(partitioned: bool, cache_metadata: bool, check: impl FnOnce(&Db)) {
    let dir = TempDir::new().unwrap();
    {
        let db = Db::open(dir.path(), opts(partitioned, false)).unwrap();
        fill(&db);
        force_compaction(&db);
        db.close().unwrap();
    }
    let db = Db::open(dir.path(), opts(partitioned, cache_metadata)).unwrap();
    check(&db);
    db.close().unwrap();
}

#[test]
fn reads_agree_under_both_metadata_policies() {
    for partitioned in [false, true] {
        for cache_metadata in [false, true] {
            with_reopened(partitioned, cache_metadata, verify);
        }
    }
}

#[test]
fn caching_metadata_removes_the_flat_index_from_the_pinned_total() {
    with_reopened(false, false, |db| {
        verify(db);
        assert!(
            pinned_metadata(db) > 0,
            "a pinned flat index must be reported"
        );
    });
    with_reopened(false, true, |db| {
        verify(db);
        assert_eq!(
            pinned_metadata(db),
            0,
            "with metadata cached, a flat file pins nothing"
        );
    });
}

#[test]
fn a_partitioned_file_still_pins_only_its_top_level_index() {
    let mut pinned_default = 0;
    with_reopened(true, false, |db| {
        verify(db);
        pinned_default = pinned_metadata(db);
        assert!(pinned_default > 0);
    });

    with_reopened(true, true, |db| {
        verify(db);
        let pinned_cached = pinned_metadata(db);
        assert!(
            pinned_cached * 2 < pinned_default,
            "top-level indexes alone ({pinned_cached}) must be well under \
             the pinned default ({pinned_default})"
        );
    });
}

#[test]
fn cached_metadata_is_charged_to_the_block_cache() {
    with_reopened(false, true, |db| {
        let before = db.get_int_property("regolith.block-cache-usage").unwrap();
        verify(db);
        let after = db.get_int_property("regolith.block-cache-usage").unwrap();
        assert!(
            after > before,
            "index and filter bytes must land in the cache ({before} -> {after})"
        );
        assert_eq!(
            db.get_int_property("regolith.pinned-metadata-bytes")
                .unwrap(),
            0,
            "and must not also be pinned"
        );
    });
}

#[test]
fn a_tiny_cache_still_answers_correctly_with_metadata_cached() {
    let dir = TempDir::new().unwrap();
    {
        let db = Db::open(dir.path(), opts(true, false)).unwrap();
        fill(&db);
        force_compaction(&db);
        db.close().unwrap();
    }

    // One shard, 64 KiB: index leaves and filters are evicted
    // constantly. Correctness must not depend on them staying resident.
    let tiny = opts(true, true)
        .block_cache_size(64 * 1024)
        .block_cache_num_shard_bits(0);
    let db = Db::open(dir.path(), tiny).unwrap();
    verify(&db);
    db.close().unwrap();
}

#[test]
fn iteration_agrees_under_both_metadata_policies() {
    let dir = TempDir::new().unwrap();
    {
        let db = Db::open(dir.path(), opts(true, false)).unwrap();
        fill(&db);
        force_compaction(&db);
        db.close().unwrap();
    }

    let mut runs = Vec::new();
    for cache_metadata in [false, true] {
        let db = Db::open(dir.path(), opts(true, cache_metadata)).unwrap();
        let mut keys = Vec::new();
        let mut iter = db.iter();
        iter.seek_to_first();
        while iter.valid() {
            keys.push(iter.key().unwrap().to_vec());
            iter.next();
        }
        db.close().unwrap();
        runs.push(keys);
    }
    assert_eq!(runs[0].len(), KEYS);
    assert_eq!(
        runs[0], runs[1],
        "iteration order must not depend on where the index lives"
    );
}
