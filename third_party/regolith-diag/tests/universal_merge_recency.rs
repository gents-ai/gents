//! A universal L0 merge must not overtake a flush that landed while it ran.
//!
//! Recency inside L0 is position, not sequence number: a lookup walks the
//! level in reverse and takes the first match without comparing sequence
//! numbers across files. A merge that appends its output therefore claims the
//! newest slot. Nothing excludes a flush while a merge runs, so a memtable
//! flushed mid-merge installs a genuinely newer file first, and an appended
//! merge output then sits in front of it. The newer write reads as lost, with
//! no error, until some later merge happens to fold it in.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads and the filesystem, neither of which exists there.
#![cfg(not(target_arch = "wasm32"))]

use regolith::{
    CompactionOutcome, CompactionStyle, Db, Options, UniversalCompactionOptions, WriteBatch,
};

/// Universal compaction with tiny files, so a handful of writes produces
/// several L0 runs and the merge has something to fold.
fn universal_options(workers: usize) -> Options {
    Options::default()
        .compaction_style(CompactionStyle::Universal)
        .universal_compaction_options(UniversalCompactionOptions::default())
        .write_buffer_size(32 * 1024)
        .block_size(4 * 1024)
        .block_cache_size(0)
        .target_file_size(64 * 1024)
        .l0_compaction_trigger(2)
        .level0_slowdown_writes_trigger(0)
        .level0_stop_writes_trigger(0)
        .max_background_compactions(workers)
}

/// Fill enough to leave several L0 files behind.
fn seed(db: &Db, rounds: u64) {
    let filler = [b'f'; 256];
    for r in 0..rounds {
        let mut batch = WriteBatch::new();
        for i in 0..200u64 {
            batch.put(&(r * 1000 + i).to_be_bytes(), &filler);
        }
        regolith::through_stalls(|| db.write(batch.clone())).unwrap();
        db.flush().unwrap();
    }
}

const KEY: &[u8] = b"the-contended-key";

#[test]
fn a_write_that_lands_during_a_universal_merge_is_not_overtaken() {
    // Repeated because the interleaving that loses the write needs the flush
    // to land while a merge is in flight. One round can miss it; a run of
    // them does not, and every round asserts the same invariant, so a
    // regression shows up as a failure rather than as flakiness.
    for round in 0..12 {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path(), universal_options(4)).unwrap();

        regolith::through_stalls(|| db.put(KEY, b"old")).unwrap();
        seed(&db, 6);

        // Newer value, then a flush to push it into its own L0 file while the
        // background workers are merging what came before.
        regolith::through_stalls(|| db.put(KEY, b"new")).unwrap();
        db.flush().unwrap();

        // Give the merge time to land its output.
        for _ in 0..50 {
            if db.compact_step().unwrap() == CompactionOutcome::DidWork {
                continue;
            }
            break;
        }

        assert_eq!(
            db.get(KEY).unwrap().as_deref(),
            Some(&b"new"[..]),
            "round {round}: the later write must win, whatever compaction did \
             with the files underneath it"
        );

        // And it must survive a reopen, because the manifest records the same
        // order the in-memory version had.
        db.close().unwrap();
        drop(db);
        let db = Db::open(dir.path(), universal_options(1)).unwrap();
        assert_eq!(
            db.get(KEY).unwrap().as_deref(),
            Some(&b"new"[..]),
            "round {round}: the order must be recorded, not just held in memory"
        );
    }
}

#[test]
fn a_universal_merge_keeps_every_key_it_folded() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), universal_options(4)).unwrap();

    const N: u64 = 4_000;
    let value = [b'v'; 64];
    let mut batch = WriteBatch::new();
    for i in 0..N {
        batch.put(&i.to_be_bytes(), &value);
        if batch.buffered_bytes() >= 16 * 1024 {
            let full = std::mem::take(&mut batch);
            regolith::through_stalls(|| db.write(full.clone())).unwrap();
        }
    }
    regolith::through_stalls(|| db.write(batch.clone())).unwrap();
    db.flush().unwrap();
    while db.compact_step().unwrap() == CompactionOutcome::DidWork {}

    let seen = db
        .scan_stream(None, None)
        .unwrap()
        .try_fold(0u64, |seen, entry| entry.map(|_| seen + 1))
        .unwrap();
    assert_eq!(seen, N, "a merge must not drop or duplicate keys");

    for i in (0..N).step_by(97) {
        assert_eq!(
            db.get(&i.to_be_bytes()).unwrap().as_deref(),
            Some(&value[..]),
            "key {i} must still be readable after the merge"
        );
    }
}
