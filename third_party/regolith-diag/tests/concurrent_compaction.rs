//! Compaction workers must never claim the same input files twice.
//!
//! A level below L0 is read as a single sorted run: the iterator walks its
//! files in key order and expects each key to be greater than the last. Two
//! workers that pick the same inputs produce two output sets covering the
//! same key range, both of which land in the target level. The level is
//! then not sorted, and a scan across it either reports that iteration went
//! backwards or silently stops at the first key that does not advance.
//!
//! The invariant is checked the way a reader would meet it: scan everything
//! and require that the keys strictly increase and that every key written
//! comes back exactly once.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads and the filesystem, neither of which exists there.
#![cfg(not(target_arch = "wasm32"))]

use regolith::{CompactionOutcome, Db, Options, WriteBatch};

/// Small enough that a few megabytes reach L1 through many separate
/// compaction jobs, which is what gives the workers something to race over.
fn contended_options(workers: usize) -> Options {
    Options::default()
        // Deliberately tiny. Small files and a trigger of one turn a few
        // tens of megabytes into hundreds of separate compaction jobs, and
        // it is the number of independent picks, not the volume of data,
        // that decides whether two workers ever choose the same input.
        .write_buffer_size(128 * 1024)
        .block_size(4 * 1024)
        .block_cache_size(256 * 1024)
        .target_file_size(64 * 1024)
        .level_base_bytes(128 * 1024)
        .l0_compaction_trigger(1)
        .level0_slowdown_writes_trigger(0)
        .level0_stop_writes_trigger(0)
        .max_background_compactions(workers)
        .max_subcompactions(workers)
}

fn write_dense_keys(db: &Db, count: u64) {
    let value = [b'v'; 256];
    let mut batch = WriteBatch::new();
    for i in 0..count {
        batch.put(&i.to_be_bytes(), &value);
        if batch.buffered_bytes() >= 128 * 1024 {
            let full = std::mem::take(&mut batch);
            regolith::through_stalls(|| db.write(full.clone())).unwrap();
        }
    }
    regolith::through_stalls(|| db.write(batch.clone())).unwrap();
}

/// Read the whole database back and hold the reader's own invariant to it.
fn assert_single_sorted_run(db: &Db, expected: u64) {
    let mut seen: u64 = 0;
    let mut last: Option<Vec<u8>> = None;
    for entry in db.scan_stream(None, None).unwrap() {
        let (key, _value) = entry.unwrap();
        if let Some(prev) = &last {
            assert!(
                key.as_slice() > prev.as_slice(),
                "scan went backwards at entry {seen}: {:?} did not follow {:?}",
                key,
                prev
            );
        }
        last = Some(key);
        seen += 1;
    }
    // A level holding two overlapping runs also truncates the scan, because
    // the iterator stops at the key that fails to advance. Counting catches
    // that even when the ordering assertion above does not fire first.
    assert_eq!(
        seen, expected,
        "scan returned {seen} keys, expected {expected}"
    );
}

#[test]
fn many_workers_keep_every_level_a_single_sorted_run() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), contended_options(16)).unwrap();

    const KEYS: u64 = 300_000;
    write_dense_keys(&db, KEYS);
    db.flush().unwrap();
    while db.compact_step().unwrap() == CompactionOutcome::DidWork {}

    assert_single_sorted_run(&db, KEYS);
}

#[test]
fn a_reopened_database_reads_back_what_the_workers_wrote() {
    let dir = tempfile::tempdir().unwrap();

    const KEYS: u64 = 200_000;
    {
        let db = Db::open(dir.path(), contended_options(16)).unwrap();
        write_dense_keys(&db, KEYS);
        db.flush().unwrap();
        while db.compact_step().unwrap() == CompactionOutcome::DidWork {}
        db.close().unwrap();
    }

    // Duplicated outputs survive in the manifest, so a reopen is where a
    // corrupt level structure shows up even if the writing process saw
    // consistent reads from its own cache.
    let db = Db::open(dir.path(), contended_options(2)).unwrap();
    assert_single_sorted_run(&db, KEYS);
}
