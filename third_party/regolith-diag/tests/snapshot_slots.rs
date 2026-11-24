//! The snapshot registry's per-thread slots and the statistics shards,
//! through the public API (plan 4.6, D54).
//!
//! A `Snapshot` or a `Transaction` is `Send`: it may be created on one
//! thread and dropped on another. Its pin lives in the slot of the thread
//! that created it and must be released exactly there, whatever thread
//! drops it; until then compaction must keep what it reads. Statistics are
//! counted per thread and summed on read, so totals must be exact once the
//! counting threads are done.

use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::thread;

use regolith::{Db, OptimisticTransactionDb, Options, Statistics, Ticker, TxnOptions};
use tempfile::TempDir;

fn options() -> Options {
    Options::default().max_background_compactions(0)
}

fn live_snapshots(db: &Db) -> u64 {
    db.get_int_property("regolith.num-snapshots")
        .expect("the property is always reported")
}

/// Sixteen threads take snapshots and hand half of them to the next thread
/// to drop. While all are held the count is exact; once all are dropped,
/// on whichever thread, nothing is left pinned.
#[test]
fn snapshots_dropped_on_other_threads_are_released_exactly() {
    const THREADS: usize = 16;
    const EACH: usize = 40;
    let dir = TempDir::new().unwrap();
    let db = Arc::new(Db::open(dir.path(), options()).unwrap());
    let held = Arc::new(Barrier::new(THREADS + 1));
    let release = Arc::new(Barrier::new(THREADS + 1));
    let (senders, receivers): (Vec<_>, Vec<_>) = (0..THREADS)
        .map(|_| mpsc::channel::<regolith::Snapshot>())
        .unzip();
    let workers: Vec<_> = receivers
        .into_iter()
        .enumerate()
        .map(|(t, inbox)| {
            let db = Arc::clone(&db);
            let next = senders[(t + 1) % THREADS].clone();
            let (held, release) = (Arc::clone(&held), Arc::clone(&release));
            thread::spawn(move || {
                let mut mine = Vec::new();
                for i in 0..EACH {
                    db.put(format!("k{t}-{i}").as_bytes(), b"v").unwrap();
                    let snapshot = db.snapshot();
                    if i % 2 == 0 {
                        next.send(snapshot).unwrap();
                    } else {
                        mine.push(snapshot);
                    }
                }
                drop(next);
                held.wait();
                release.wait();
                // Drop the ones handed over by the previous thread, then our own.
                let received: Vec<_> = inbox.try_iter().collect();
                assert_eq!(received.len(), EACH / 2);
                drop(received);
                drop(mine);
            })
        })
        .collect();
    drop(senders);
    held.wait();
    assert_eq!(live_snapshots(&db), (THREADS * EACH) as u64);
    release.wait();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(live_snapshots(&db), 0);
    let readers = db.wait_for_snapshots();
    assert_eq!(readers.wait(std::time::Duration::from_secs(5)), 0);
}

/// A snapshot taken on one thread, moved to another, keeps its version
/// through a full compaction until it is dropped there.
#[test]
fn a_moved_snapshot_keeps_its_version_through_compaction() {
    let dir = TempDir::new().unwrap();
    let db = Arc::new(Db::open(dir.path(), options()).unwrap());
    db.put(b"k", b"old").unwrap();
    db.flush().unwrap();
    let snapshot = thread::spawn({
        let db = Arc::clone(&db);
        move || db.snapshot()
    })
    .join()
    .unwrap();
    db.put(b"k", b"new").unwrap();
    db.flush().unwrap();
    db.compact_range(None, None).wait().unwrap();
    let seen = thread::spawn(move || {
        let value = snapshot.get(b"k").unwrap();
        drop(snapshot);
        value
    })
    .join()
    .unwrap();
    assert_eq!(seen.as_deref(), Some(&b"old"[..]));
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"new"[..]));
    assert_eq!(live_snapshots(&db), 0);
}

/// Transactions begun on one thread and ended on another (committed or
/// dropped) leave nothing pinned.
#[test]
fn transactions_ended_on_other_threads_release_their_pins() {
    let dir = TempDir::new().unwrap();
    let db = Arc::new(OptimisticTransactionDb::open(dir.path(), options()).unwrap());
    let (to_end, ended) = mpsc::channel();
    let begun: Vec<_> = (0..8)
        .map(|t| {
            let (db, to_end) = (Arc::clone(&db), to_end.clone());
            thread::spawn(move || {
                for i in 0..25 {
                    let txn = db.begin(&TxnOptions::default());
                    txn.put(format!("t{t}-{i}").as_bytes(), b"v").unwrap();
                    to_end.send((i % 2 == 0, txn)).unwrap();
                }
            })
        })
        .collect();
    drop(to_end);
    for t in begun {
        t.join().unwrap();
    }
    let enders: Vec<_> = (0..4)
        .map(|_| {
            let batch: Vec<_> = ended.try_iter().take(50).collect();
            thread::spawn(move || {
                for (commit, txn) in batch {
                    if commit {
                        txn.commit().unwrap();
                    } else {
                        drop(txn);
                    }
                }
            })
        })
        .collect();
    for t in enders {
        t.join().unwrap();
    }
    assert_eq!(live_snapshots(db.db()), 0);
}

/// Statistics counted on many threads, more threads than cores, sum to
/// exactly what was counted once they are done.
#[test]
fn statistics_counted_on_many_threads_sum_exactly() {
    const THREADS: usize = 48;
    const READS: usize = 200;
    let stats = Arc::new(Statistics::new());
    let dir = TempDir::new().unwrap();
    let db =
        Arc::new(Db::open(dir.path(), options().statistics(Some(Arc::clone(&stats)))).unwrap());
    db.put(b"k", b"value").unwrap();
    stats.reset();
    let start = Arc::new(Barrier::new(THREADS));
    let readers: Vec<_> = (0..THREADS)
        .map(|_| {
            let (db, start) = (Arc::clone(&db), Arc::clone(&start));
            thread::spawn(move || {
                start.wait();
                for _ in 0..READS {
                    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"value"[..]));
                    drop(db.snapshot());
                }
            })
        })
        .collect();
    for r in readers {
        r.join().unwrap();
    }
    let total = (THREADS * READS) as u64;
    assert_eq!(stats.get_ticker(Ticker::KeysRead), total);
    assert_eq!(stats.get_ticker(Ticker::BytesRead), total * 5);
    assert_eq!(stats.get_ticker(Ticker::SnapshotsRegistered), total);
    assert_eq!(stats.get_ticker(Ticker::SnapshotsReleased), total);
}
