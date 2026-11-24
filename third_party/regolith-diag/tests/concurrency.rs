//! Concurrency scenarios - multiple threads driving reads and
//! writes against a single `Db` handle.
//!
//! Scenarios ported from `db_test.cc` multi-threaded tests and
//! from the black-box core of RocksDB's `db_stress`. The long-running
//! soak runs in the gate too, sized so that `cargo test` stays fast.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads, the filesystem or proptest, none of which exist there. The browser
// suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

mod common;

use common::{fill_sequential, force_compaction, open};

// ── short, always-on concurrency tests ──────────────────────────

#[test]
fn parallel_writers_produce_durable_writes() {
    // db_test.cc::MultiThreadedReadersWriters (writer half) - every
    // write committed from any thread must be visible after a
    // `join()` barrier, regardless of which thread made it.
    let dir = TempDir::new().unwrap();
    let db = Arc::new(open(&dir));

    let writer_count = 4usize;
    let writes_per_writer = 250usize;
    let mut handles = Vec::new();
    for t in 0..writer_count {
        let db = Arc::clone(&db);
        handles.push(thread::spawn(move || {
            for i in 0..writes_per_writer {
                let key = format!("t{}_k{:05}", t, i);
                regolith::through_stalls(|| db.put(key.as_bytes(), key.as_bytes())).unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    for t in 0..writer_count {
        for i in [0usize, writes_per_writer / 2, writes_per_writer - 1] {
            let key = format!("t{}_k{:05}", t, i);
            assert_eq!(db.get(key.as_bytes()).unwrap(), Some(key.into_bytes()));
        }
    }
}

#[test]
fn concurrent_readers_during_flush_see_consistent_values() {
    // db_test.cc::ReadsDuringFlush - reads must not observe a
    // key that "disappears" because of a concurrent flush. The
    // shape of the test: seed a key with a value, spawn readers
    // that keep asserting "value is exactly X" while the main
    // thread forces a flush.
    let dir = TempDir::new().unwrap();
    let db = Arc::new(open(&dir));
    regolith::through_stalls(|| db.put(b"pinned", b"stable")).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let reader_handles: Vec<_> = (0..3)
        .map(|_| {
            let db = Arc::clone(&db);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    assert_eq!(
                        db.get(b"pinned").unwrap(),
                        Some(b"stable".to_vec()),
                        "reader saw inconsistent value during flush"
                    );
                }
            })
        })
        .collect();

    // Drive enough writes to trigger flushes.
    fill_sequential(&db, 300);
    force_compaction(&db);

    stop.store(true, Ordering::Relaxed);
    for h in reader_handles {
        h.join().unwrap();
    }
}

#[test]
fn snapshot_held_by_reader_outlives_writer_compaction() {
    // db_test.cc::SnapshotHoldsBackGC - a snapshot captured before
    // writer overwrites the key must remain observable through the
    // writer's updates plus a compaction.
    let dir = TempDir::new().unwrap();
    let db = Arc::new(open(&dir));
    regolith::through_stalls(|| db.put(b"k", b"v0")).unwrap();
    let snap = db.snapshot();

    let db_writer = Arc::clone(&db);
    let writer = thread::spawn(move || {
        for i in 1..=100 {
            let v = format!("v{i}");
            regolith::through_stalls(|| db_writer.put(b"k", v.as_bytes())).unwrap();
        }
        force_compaction(&db_writer);
    });

    // While the writer runs, keep asserting the snapshot's view.
    for _ in 0..50 {
        assert_eq!(snap.get(b"k").unwrap(), Some(b"v0".to_vec()));
    }

    writer.join().unwrap();
    // Post-join: snapshot still observes the old value even after
    // the writer's compaction.
    assert_eq!(snap.get(b"k").unwrap(), Some(b"v0".to_vec()));
    assert_eq!(db.get(b"k").unwrap(), Some(b"v100".to_vec()));
}

#[test]
fn concurrent_batch_writers_are_atomic() {
    // db_test.cc::ConcurrentBatchWriters - each thread writes its
    // own batch; every batch's contents must be either fully
    // visible or fully absent when observed from another thread.
    use regolith::WriteBatch;

    let dir = TempDir::new().unwrap();
    let db = Arc::new(open(&dir));
    let writer_count = 4usize;
    let batches_per_writer = 50usize;

    let mut handles = Vec::new();
    for t in 0..writer_count {
        let db = Arc::clone(&db);
        handles.push(thread::spawn(move || {
            for i in 0..batches_per_writer {
                let mut batch = WriteBatch::new();
                // All three keys share a prefix so an observer
                // checking "t{t}_{i}_{a,b,c}" can easily test
                // partial-visibility.
                batch.put(format!("t{}_{}_a", t, i).as_bytes(), b"1");
                batch.put(format!("t{}_{}_b", t, i).as_bytes(), b"2");
                batch.put(format!("t{}_{}_c", t, i).as_bytes(), b"3");
                regolith::through_stalls(|| db.write(batch.clone())).unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    // Every batch must have landed atomically - all three keys present.
    for t in 0..writer_count {
        for i in 0..batches_per_writer {
            let present = b"abc"
                .iter()
                .filter(|&&c| {
                    db.get(format!("t{}_{}_{}", t, i, c as char).as_bytes())
                        .unwrap()
                        .is_some()
                })
                .count();
            assert_eq!(present, 3, "batch t{}_{} partially visible", t, i);
        }
    }
}

#[test]
fn snapshot_never_observes_a_torn_batch() {
    // A snapshot taken while a batch commit is in flight must see the
    // batch's keys all-or-nothing. The engine publishes the read horizon
    // only after the whole batch is applied, so a snapshot at that horizon
    // cannot catch the memtable mid-batch. Before that fix the sequence was
    // advanced up front, and a reader could observe a prefix of the batch.
    use regolith::WriteBatch;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as O};

    let dir = TempDir::new().unwrap();
    let db = Arc::new(open(&dir));
    let batch_width = 12usize;
    let total_batches = 6_000usize;

    let stop = Arc::new(AtomicBool::new(false));
    // The batch the writer is about to commit, so the reader can aim its
    // snapshots at the in-flight batch instead of hunting for it.
    let frontier = Arc::new(AtomicUsize::new(0));

    let writer = {
        let db = Arc::clone(&db);
        let frontier = Arc::clone(&frontier);
        thread::spawn(move || {
            for i in 0..total_batches {
                frontier.store(i, O::Release);
                let mut batch = WriteBatch::new();
                for k in 0..batch_width {
                    batch.put(format!("b{i}_{k}").as_bytes(), b"v");
                }
                regolith::through_stalls(|| db.write(batch.clone())).unwrap();
            }
        })
    };

    let reader = {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        let frontier = Arc::clone(&frontier);
        thread::spawn(move || {
            while !stop.load(O::Relaxed) {
                let at = frontier.load(O::Acquire);
                let snap = db.snapshot();
                // Check the in-flight batch and its neighbour: whichever the
                // snapshot's horizon includes must be whole, never partial.
                for i in [at, at + 1] {
                    if i >= total_batches {
                        continue;
                    }
                    let present = (0..batch_width)
                        .filter(|k| snap.get(format!("b{i}_{k}").as_bytes()).unwrap().is_some())
                        .count();
                    assert!(
                        present == 0 || present == batch_width,
                        "snapshot saw a torn batch b{i}: {present}/{batch_width} keys",
                    );
                }
            }
        })
    };

    writer.join().unwrap();
    stop.store(true, O::Relaxed);
    reader.join().unwrap();
}

#[test]
fn reads_during_compaction_return_correct_values() {
    // db_test.cc::ReadsDuringCompaction - reads issued in parallel
    // with a compaction that moves files between levels must keep
    // returning the correct current-version value for every key.
    let dir = TempDir::new().unwrap();
    let db = Arc::new(open(&dir));
    fill_sequential(&db, 500);

    let stop = Arc::new(AtomicBool::new(false));
    let err_flag = Arc::new(AtomicBool::new(false));
    let reader = {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        let err_flag = Arc::clone(&err_flag);
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                for i in [0usize, 250, 499] {
                    let k = format!("key_{:06}", i);
                    let expected = format!("val_{:06}", i).into_bytes();
                    match db.get(k.as_bytes()) {
                        Ok(Some(got)) if got == expected => {}
                        _ => {
                            err_flag.store(true, Ordering::Relaxed);
                            return;
                        }
                    }
                }
            }
        })
    };

    force_compaction(&db);
    stop.store(true, Ordering::Relaxed);
    reader.join().unwrap();
    assert!(!err_flag.load(Ordering::Relaxed));
}

// ── long soak ──────────────────────────────────────────────────

#[test]
fn writer_compactor_contention_soak() {
    // db_stress-style mixed workload soak: N threads each do a
    // random mix of put/delete/get over a 10-second window while
    // the main thread triggers periodic compactions. Passes if
    // no assertion fires and no panic occurs.
    let dir = TempDir::new().unwrap();
    let db = Arc::new(open(&dir));
    let deadline = Instant::now() + Duration::from_secs(10);
    let ops = Arc::new(AtomicUsize::new(0));

    let mut workers = Vec::new();
    for t in 0..4u64 {
        let db = Arc::clone(&db);
        let ops = Arc::clone(&ops);
        workers.push(thread::spawn(move || {
            let mut seed = t.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            while Instant::now() < deadline {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let key = format!("k_{:06}", seed % 1024);
                match seed % 3 {
                    0 => {
                        regolith::through_stalls(|| db.put(key.as_bytes(), b"v")).unwrap();
                    }
                    1 => {
                        regolith::through_stalls(|| db.delete(key.as_bytes())).unwrap();
                    }
                    _ => {
                        let _ = db.get(key.as_bytes()).unwrap();
                    }
                }
                ops.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    let compactor = {
        let db = Arc::clone(&db);
        thread::spawn(move || {
            while Instant::now() < deadline {
                force_compaction(&db);
                thread::sleep(Duration::from_millis(300));
            }
        })
    };

    for w in workers {
        w.join().unwrap();
    }
    compactor.join().unwrap();

    // Sanity: the workload actually did work.
    assert!(ops.load(Ordering::Relaxed) > 1000, "soak ran too slowly");
}
