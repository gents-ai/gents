//! Commits that share a sync of the log, through the public API.
//!
//! The three properties these tests defend, in priority order:
//!
//! 1. A snapshot never observes a torn batch. The read horizon passes a
//!    commit only once the whole of it is durable and applied.
//! 2. Every writer that returns `Ok` really did commit, and every writer
//!    whose commit a failed sync discarded learns it did not.
//! 3. N concurrent durable writers cost far fewer than N fsyncs: one sync
//!    covers every record written when it starts. Threads started together
//!    overlap only by luck, so that test holds the sync round until every
//!    writer's write is decided and waits behind it, which needs the
//!    engine's internals: it lives beside them, in
//!    `src/engine/commit/overlap_tests.rs`, and still writes through `Db`.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use regolith::{Db, DurabilityMode, Options, Statistics, Ticker, WriteBatch, WriteOptions};
use tempfile::TempDir;

fn durable_opts(stats: Option<Arc<Statistics>>) -> Options {
    Options::default()
        .durability(DurabilityMode::Immediate)
        .statistics(stats)
}

#[test]
fn a_serial_durable_writer_still_syncs_once_per_write() {
    // The other side of the amortisation: with nothing to batch with, a
    // durable write is still a durable write.
    let dir = TempDir::new().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = Db::open(dir.path(), durable_opts(Some(Arc::clone(&stats)))).unwrap();

    for i in 0..16 {
        regolith::through_stalls(|| db.put(format!("k{i:03}").as_bytes(), b"v")).unwrap();
    }
    assert_eq!(stats.get_ticker(Ticker::WalSyncCount), 16);
}

#[test]
fn a_snapshot_never_observes_a_torn_batch() {
    // A batch is all-or-nothing to every reader, however many commits
    // share its sync.
    const BATCH: usize = 32;
    const ROUNDS: usize = 400;

    let dir = TempDir::new().unwrap();
    let db = Arc::new(Db::open(dir.path(), Options::default()).unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicUsize::new(0));

    let reader = {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        let reads = Arc::clone(&reads);
        thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let snap = db.snapshot();
                for round in 0..ROUNDS {
                    let first = snap.get(format!("b{round:04}_00").as_bytes()).unwrap();
                    let last = snap
                        .get(format!("b{round:04}_{:02}", BATCH - 1).as_bytes())
                        .unwrap();
                    assert_eq!(
                        first.is_some(),
                        last.is_some(),
                        "round {round} was observed torn: first={:?} last={:?}",
                        first.is_some(),
                        last.is_some()
                    );
                    if first.is_some() {
                        for i in 0..BATCH {
                            assert!(
                                snap.get(format!("b{round:04}_{i:02}").as_bytes())
                                    .unwrap()
                                    .is_some(),
                                "round {round} key {i} missing from a batch whose ends are both present"
                            );
                        }
                    }
                    reads.fetch_add(1, Ordering::Relaxed);
                }
            }
        })
    };

    let mut writers = Vec::new();
    for lane in 0..4 {
        let db = Arc::clone(&db);
        writers.push(thread::spawn(move || {
            for round in (lane..ROUNDS).step_by(4) {
                let mut batch = WriteBatch::new();
                for i in 0..BATCH {
                    batch.put(format!("b{round:04}_{i:02}").as_bytes(), b"v");
                }
                regolith::through_stalls(|| db.write(batch.clone())).unwrap();
            }
        }));
    }
    for handle in writers {
        handle.join().unwrap();
    }
    stop.store(true, Ordering::Release);
    reader.join().unwrap();

    assert!(reads.load(Ordering::Relaxed) > 0, "the reader never ran");
    for round in 0..ROUNDS {
        for i in 0..BATCH {
            assert_eq!(
                db.get(format!("b{round:04}_{i:02}").as_bytes()).unwrap(),
                Some(b"v".to_vec())
            );
        }
    }
}

#[test]
fn every_writer_reads_its_own_write_immediately() {
    // The read horizon is published before any follower is released, so a
    // writer that returns from `put` and snapshots right away sees itself.
    const WRITERS: usize = 8;
    const PER_WRITER: usize = 500;

    let dir = TempDir::new().unwrap();
    let db = Arc::new(Db::open(dir.path(), Options::default()).unwrap());

    let mut handles = Vec::with_capacity(WRITERS);
    for w in 0..WRITERS {
        let db = Arc::clone(&db);
        handles.push(thread::spawn(move || {
            for i in 0..PER_WRITER {
                let key = format!("ryw{w}_{i:05}");
                regolith::through_stalls(|| db.put(key.as_bytes(), b"mine")).unwrap();
                let snap = db.snapshot();
                assert_eq!(
                    snap.get(key.as_bytes()).unwrap(),
                    Some(b"mine".to_vec()),
                    "writer {w} could not see its own committed write"
                );
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn every_committed_write_survives_a_reopen() {
    const WRITERS: usize = 6;
    const PER_WRITER: usize = 200;

    let dir = TempDir::new().unwrap();
    {
        let db = Arc::new(Db::open(dir.path(), durable_opts(None)).unwrap());
        let mut handles = Vec::with_capacity(WRITERS);
        for w in 0..WRITERS {
            let db = Arc::clone(&db);
            handles.push(thread::spawn(move || {
                for i in 0..PER_WRITER {
                    regolith::through_stalls(|| {
                        db.put(format!("d{w}_{i:04}").as_bytes(), b"durable")
                    })
                    .unwrap();
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        db.close().unwrap();
    }

    let db = Db::open(dir.path(), Options::default()).unwrap();
    for w in 0..WRITERS {
        for i in 0..PER_WRITER {
            assert_eq!(
                db.get(format!("d{w}_{i:04}").as_bytes()).unwrap(),
                Some(b"durable".to_vec()),
                "a durable write from thread {w} did not survive the reopen"
            );
        }
    }
}

#[test]
fn every_synced_write_replays_from_the_wal_without_a_close() {
    // No `close()` and a write buffer large enough that nothing flushes,
    // so recovery has to come out of the WAL records alone.
    const WRITERS: usize = 4;
    const PER_WRITER: usize = 200;

    let dir = TempDir::new().unwrap();
    {
        let db = Arc::new(
            Db::open(
                dir.path(),
                durable_opts(None).write_buffer_size(64 * 1024 * 1024),
            )
            .unwrap(),
        );
        let mut handles = Vec::with_capacity(WRITERS);
        for w in 0..WRITERS {
            let db = Arc::clone(&db);
            handles.push(thread::spawn(move || {
                for i in 0..PER_WRITER {
                    regolith::through_stalls(|| db.put(format!("wal{w}_{i:04}").as_bytes(), b"v"))
                        .unwrap();
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
    }

    let db = Db::open(dir.path(), Options::default()).unwrap();
    for w in 0..WRITERS {
        for i in 0..PER_WRITER {
            assert_eq!(
                db.get(format!("wal{w}_{i:04}").as_bytes()).unwrap(),
                Some(b"v".to_vec()),
                "a write that returned Ok under Immediate durability did not replay"
            );
        }
    }
}

#[test]
fn mixed_durability_writers_all_commit() {
    const PER_LANE: usize = 200;

    let dir = TempDir::new().unwrap();
    let db = Arc::new(Db::open(dir.path(), Options::default()).unwrap());

    let lanes: Vec<WriteOptions> = vec![
        WriteOptions {
            sync: true,
            ..WriteOptions::default()
        },
        WriteOptions::default(),
        WriteOptions {
            disable_wal: true,
            ..WriteOptions::default()
        },
    ];

    let mut handles = Vec::new();
    for (lane, opts) in lanes.into_iter().enumerate() {
        let db = Arc::clone(&db);
        handles.push(thread::spawn(move || {
            for i in 0..PER_LANE {
                regolith::through_stalls(|| {
                    db.put_opt(&opts, format!("m{lane}_{i:04}").as_bytes(), b"v")
                })
                .unwrap();
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }

    for lane in 0..3 {
        for i in 0..PER_LANE {
            assert_eq!(
                db.get(format!("m{lane}_{i:04}").as_bytes()).unwrap(),
                Some(b"v".to_vec())
            );
        }
    }
}

#[test]
fn administrative_operations_still_exclude_writers() {
    // `compact_range` and `close` run their steps as ordered units of the
    // commit ring, in ring order with the writes, so they cannot interleave
    // with a commit.
    let dir = TempDir::new().unwrap();
    let db =
        Arc::new(Db::open(dir.path(), Options::default().write_buffer_size(8 * 1024)).unwrap());

    let stop = Arc::new(AtomicBool::new(false));
    let mut writers = Vec::new();
    for w in 0..4 {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        writers.push(thread::spawn(move || {
            let mut i = 0usize;
            while !stop.load(Ordering::Acquire) {
                regolith::through_stalls(|| {
                    db.put(format!("a{w}_{i:05}").as_bytes(), &[b'x'; 128])
                })
                .unwrap();
                i += 1;
            }
            i
        }));
    }

    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        db.compact_range(None, None).wait().unwrap();
    }
    stop.store(true, Ordering::Release);

    let counts: Vec<usize> = writers.into_iter().map(|h| h.join().unwrap()).collect();
    for (w, count) in counts.iter().enumerate() {
        assert!(*count > 0, "writer {w} made no progress against compaction");
        for i in 0..*count {
            assert_eq!(
                db.get(format!("a{w}_{i:05}").as_bytes()).unwrap(),
                Some(vec![b'x'; 128]),
                "writer {w} lost key {i} across a concurrent compaction"
            );
        }
    }
}

#[test]
fn a_batch_of_many_operations_gets_one_sync() {
    let dir = TempDir::new().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = Db::open(dir.path(), durable_opts(Some(Arc::clone(&stats)))).unwrap();

    let mut batch = WriteBatch::new();
    for i in 0..256 {
        batch.put(format!("batch{i:04}").as_bytes(), b"v");
    }
    regolith::through_stalls(|| db.write(batch.clone())).unwrap();

    assert_eq!(
        stats.get_ticker(Ticker::WalSyncCount),
        1,
        "a 256-operation batch is one commit and therefore one fdatasync"
    );
    for i in 0..256 {
        assert_eq!(
            db.get(format!("batch{i:04}").as_bytes()).unwrap(),
            Some(b"v".to_vec())
        );
    }
}
