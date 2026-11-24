//! A checkpoint checked against the history that produced it.
//!
//! The Elle harness checks whether a *live* history is consistent. It
//! says nothing about a database derived from one, because its fault
//! injection only damages WAL tails. A checkpoint that silently dropped
//! an acknowledged write would pass every Elle model in the tree.
//!
//! The same method catches it though: record what committed and when,
//! then hold the derived database to that record. `Db::write_sequenced`
//! makes the boundary exact rather than approximate - each write reports
//! the sequence it committed at, and the captured version reports the
//! sequence it captured, so "acknowledged before the capture" is a
//! comparison rather than a guess about thread timing.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use regolith::{Checkpoint, Db, Options, WriteBatch};
use tempfile::TempDir;

fn key(i: u64) -> Vec<u8> {
    format!("k{i:06}").into_bytes()
}

/// Every write acknowledged at or before the sequence a checkpoint
/// captured must be in that checkpoint.
///
/// This is the property a checkpoint exists to provide. Losing one is
/// silent: the checkpoint opens, reads fine, and is simply missing rows.
#[test]
fn a_checkpoint_holds_every_write_acknowledged_before_its_capture() {
    let src = TempDir::new().expect("tempdir");
    let db = Db::open(src.path(), Options::default()).expect("open");

    // A record of (key, committed sequence) for everything acknowledged.
    let mut acked: Vec<(u64, u64)> = Vec::new();
    for i in 0..600u64 {
        let mut batch = WriteBatch::new();
        batch.put(&key(i), &[b'v'; 128]);
        let seq = db.write_sequenced(batch).expect("write");
        acked.push((i, seq));
    }

    let tgt = TempDir::new().expect("tempdir");
    let boundary = db.latest_sequence();
    Checkpoint::new(&db)
        .expect("checkpoint")
        .create(tgt.path())
        .wait()
        .expect("create");

    // Writes after the capture. None of these may appear.
    for i in 600..900u64 {
        let mut batch = WriteBatch::new();
        batch.put(&key(i), b"after");
        db.write_sequenced(batch).expect("write");
    }

    let restored = Db::open(tgt.path(), Options::default()).expect("reopen checkpoint");

    let mut missing = Vec::new();
    for (i, seq) in &acked {
        if *seq > boundary {
            continue;
        }
        if restored.get(&key(*i)).expect("get").is_none() {
            missing.push((*i, *seq));
        }
    }
    assert!(
        missing.is_empty(),
        "{} write(s) acknowledged at or before sequence {boundary} are absent from the \
         checkpoint; first few: {:?}",
        missing.len(),
        &missing[..missing.len().min(5)]
    );

    for i in 600..900u64 {
        assert_eq!(
            restored.get(&key(i)).expect("get"),
            None,
            "k{i:06} was written after the capture but appears in the checkpoint"
        );
    }
}

/// The same, with writers running throughout so the capture lands in the
/// middle of live traffic rather than on a quiet database.
///
/// Only writes whose acknowledged sequence is at or below the captured
/// one are required, which is what makes this deterministic despite the
/// concurrency: a write racing the capture is permitted to be in or out,
/// and its sequence says which.
#[test]
fn a_checkpoint_under_concurrent_writers_holds_its_acknowledged_prefix() {
    let src = TempDir::new().expect("tempdir");
    let db = Arc::new(Db::open(src.path(), Options::default()).expect("open"));

    let stop = Arc::new(AtomicBool::new(false));
    let acked: Arc<std::sync::Mutex<Vec<(u64, u64)>>> = Arc::new(std::sync::Mutex::new(Vec::new()));

    let mut writers = Vec::new();
    for w in 0..4u64 {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        let acked = Arc::clone(&acked);
        writers.push(thread::spawn(move || {
            let mut i = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let k = w * 1_000_000 + i;
                let mut batch = WriteBatch::new();
                batch.put(&key(k), &[b'v'; 96]);
                match db.write_sequenced(batch) {
                    Ok(seq) => acked.lock().expect("lock").push((k, seq)),
                    Err(e) => panic!("write failed: {e}"),
                }
                i += 1;
            }
        }));
    }

    // Let the writers get ahead, then capture mid-flight.
    while acked.lock().expect("lock").len() < 500 {
        thread::yield_now();
    }
    let tgt = TempDir::new().expect("tempdir");
    Checkpoint::new(&db)
        .expect("checkpoint")
        .create(tgt.path())
        .wait()
        .expect("create");
    let boundary = {
        let restored = Db::open(tgt.path(), Options::default()).expect("reopen");
        let b = restored.latest_sequence();
        drop(restored);
        b
    };

    stop.store(true, Ordering::Relaxed);
    for w in writers {
        w.join().expect("writer");
    }

    let restored = Db::open(tgt.path(), Options::default()).expect("reopen checkpoint");
    let acked = acked.lock().expect("lock");
    let mut required = 0usize;
    let mut missing = Vec::new();
    for (k, seq) in acked.iter() {
        if *seq > boundary {
            continue;
        }
        required += 1;
        if restored.get(&key(*k)).expect("get").is_none() {
            missing.push((*k, *seq));
        }
    }
    assert!(
        required > 0,
        "no write was acknowledged at or below the captured sequence {boundary}, so this \
         checked nothing"
    );
    assert!(
        missing.is_empty(),
        "{} of {required} write(s) acknowledged at or before sequence {boundary} are absent \
         from the checkpoint; first few: {:?}",
        missing.len(),
        &missing[..missing.len().min(5)]
    );
}
