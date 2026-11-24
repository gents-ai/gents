//! Four things the rest of the API already promises, held on the paths that
//! were not keeping them.
//!
//! 1. A handle that is not live is an error, not an empty column family.
//! 2. `drop_all` drops everything, including what a compaction was midway
//!    through writing.
//! 3. `close` returns, even when an ingest holds the compaction gate.
//! 4. A drop that returns is a drop every later batch sees, whatever else
//!    is writing.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads and the filesystem, neither of which exists there.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use regolith::{CompactionOutcome, Db, Error, Options, WriteBatch};

fn small() -> Options {
    Options::default()
        .write_buffer_size(64 * 1024)
        .block_size(4 * 1024)
        .block_cache_size(0)
        .target_file_size(128 * 1024)
        .l0_compaction_trigger(2)
}

#[test]
fn a_dropped_cf_handle_is_an_error_on_the_iterator_paths_too() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), small()).unwrap();

    let cf = db.create_column_family("things").unwrap();
    regolith::through_stalls(|| db.put_cf(&cf, b"a", b"1")).unwrap();
    db.drop_column_family(cf.clone()).unwrap();

    // The rest of the CF surface already reports this.
    assert!(db.scan_cf(&cf, None, None).is_err());
    assert!(db.get_cf(&cf, b"a").is_err());

    // The iterators must too. Before, each of these read as an empty column
    // family that succeeded, which is the worst of both: no rows and no
    // reason.
    let mut it = db.iter_cf(&cf);
    it.seek_to_first();
    assert!(!it.valid());
    assert!(
        it.status().is_err(),
        "iter_cf over a dropped handle must say why it is empty"
    );

    let mut tail = db.iter_tailing_cf(&cf);
    tail.seek_to_first();
    assert!(
        tail.status().is_err(),
        "iter_tailing_cf over a dropped handle must say why it is empty"
    );

    let snap = db.snapshot();
    let mut sit = snap.iter_cf(&cf);
    sit.seek_to_first();
    assert!(
        sit.status().is_err(),
        "Snapshot::iter_cf over a dropped handle must say why it is empty"
    );

    // A live handle is unaffected, and reports clean.
    let live = db.create_column_family("live").unwrap();
    regolith::through_stalls(|| db.put_cf(&live, b"k", b"v")).unwrap();
    let mut ok = db.iter_cf(&live);
    ok.seek_to_first();
    assert!(ok.valid());
    ok.status().expect("a live handle must report clean");
}

#[test]
fn drop_all_is_not_undone_by_a_compaction_that_was_already_running() {
    // Enough data, with workers running, that a compaction is very likely in
    // flight when the drop lands. Repeated so the interleaving is met rather
    // than hoped for.
    for round in 0..8 {
        let dir = tempfile::tempdir().unwrap();
        let opts = small().max_background_compactions(4);
        let db = Db::open(dir.path(), opts.clone()).unwrap();

        let value = [b'v'; 128];
        let mut batch = WriteBatch::new();
        for i in 0..20_000u64 {
            batch.put(&i.to_be_bytes(), &value);
            if batch.buffered_bytes() >= 64 * 1024 {
                let full = std::mem::take(&mut batch);
                regolith::through_stalls(|| db.write(full.clone())).unwrap();
            }
        }
        regolith::through_stalls(|| db.write(batch.clone())).unwrap();

        db.drop_all().unwrap();

        // Give any compaction that was mid-flight time to try to apply its
        // edits over the reset version.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let n = db.scan_stream(None, None).unwrap().count();
            assert_eq!(
                n, 0,
                "round {round}: drop_all reported success, so nothing may come back"
            );
            if db.compact_step().unwrap() != CompactionOutcome::DidWork {
                break;
            }
        }
        assert_eq!(db.scan_stream(None, None).unwrap().count(), 0);

        // And it must stay dropped across a reopen, because a compaction that
        // landed after the reset would be in the manifest.
        db.close().unwrap();
        drop(db);
        let db = Db::open(dir.path(), opts).unwrap();
        assert_eq!(
            db.scan_stream(None, None).unwrap().count(),
            0,
            "round {round}: the drop must be recorded, not just held in memory"
        );
    }
}

#[test]
fn close_returns_while_an_ingest_holds_the_compaction_gate() {
    // close() used to join the compaction workers while holding the mutex
    // that ingest_external_files wants, and a worker cannot exit while ingest
    // holds the compaction gate. That is a cycle, and a cycle hangs. The
    // assertion is simply that this returns.
    for round in 0..6 {
        let dir = tempfile::tempdir().unwrap();
        let opts = small().max_background_compactions(4);
        let db = Arc::new(Db::open(dir.path(), opts).unwrap());

        let value = [b'v'; 128];
        let mut batch = WriteBatch::new();
        for i in 0..8_000u64 {
            batch.put(&i.to_be_bytes(), &value);
            if batch.buffered_bytes() >= 32 * 1024 {
                let full = std::mem::take(&mut batch);
                regolith::through_stalls(|| db.write(full.clone())).unwrap();
            }
        }
        regolith::through_stalls(|| db.write(batch.clone())).unwrap();

        let writer = {
            let db = Arc::clone(&db);
            std::thread::spawn(move || {
                // Keep the engine busy so workers are live and contending
                // when the close lands.
                for i in 0..4_000u64 {
                    if regolith::through_stalls(|| db.put(&(1_000_000 + i).to_be_bytes(), b"x"))
                        .is_err()
                    {
                        break;
                    }
                }
            })
        };

        let start = Instant::now();
        db.close().unwrap();
        let elapsed = start.elapsed();
        writer.join().unwrap();

        assert!(
            elapsed < Duration::from_secs(30),
            "round {round}: close took {elapsed:?}, which means it was waiting on \
             something that was waiting on it"
        );
    }
}

#[test]
fn a_drop_that_returned_is_seen_by_every_batch_that_starts_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open(dir.path(), small()).unwrap());
    let hot = db.create_column_family("hot").unwrap();

    let dropped = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<()>();

    let writer = {
        let db = Arc::clone(&db);
        let dropped = Arc::clone(&dropped);
        let hot = hot.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut results = Vec::new();
            let mut ok_count = 0u64;
            let mut after_true_count = 0u64;
            let mut sent = false;
            let mut i: u64 = 0;
            loop {
                let after = dropped.load(Ordering::Acquire);
                let mut batch = WriteBatch::new();
                batch.put(format!("d{i}").as_bytes(), b"v");
                batch.put_cf(&hot, format!("h{i}").as_bytes(), b"v");
                batch.put(format!("e{i}").as_bytes(), b"v");
                let r = regolith::through_stalls(|| db.write(batch.clone()));
                if r.is_ok() {
                    ok_count += 1;
                    if ok_count == 64 && !sent {
                        let _ = tx.send(());
                        sent = true;
                    }
                }
                if after {
                    after_true_count += 1;
                }
                results.push((i, after, r));
                i += 1;
                if after_true_count >= 64 || Instant::now() > deadline {
                    break;
                }
            }
            results
        })
    };

    rx.recv_timeout(Duration::from_secs(30))
        .expect("writer never reached 64 ok batches");
    db.drop_column_family(hot.clone()).unwrap();
    dropped.store(true, Ordering::Release);

    let results = writer.join().unwrap();

    // G1: a drop that returned before a batch's liveness check must be seen
    // by that batch, no matter what else is contending for the registry.
    let after_true: Vec<_> = results.iter().filter(|(_, after, _)| *after).collect();
    assert!(
        after_true.len() >= 64,
        "writer must have observed the drop for at least 64 batches, got {}",
        after_true.len()
    );
    for (i, _, r) in &after_true {
        assert!(
            matches!(r, Err(Error::InvalidColumnFamily(_))),
            "batch {i} observed the drop before writing but was not rejected: {r:?}"
        );
    }

    // G2: every batch is applied as a whole or not at all, drop race or not.
    for (i, _after, r) in &results {
        match r {
            Ok(()) => {
                assert_eq!(
                    db.get(format!("d{i}").as_bytes()).unwrap(),
                    Some(b"v".to_vec()),
                    "batch {i} reported success, so its keys must be readable"
                );
                assert_eq!(
                    db.get(format!("e{i}").as_bytes()).unwrap(),
                    Some(b"v".to_vec())
                );
            }
            Err(Error::InvalidColumnFamily(_)) => {
                assert_eq!(db.get(format!("d{i}").as_bytes()).unwrap(), None);
                assert_eq!(db.get(format!("e{i}").as_bytes()).unwrap(), None);
            }
            Err(other) => panic!("batch {i} failed with an unexpected error: {other:?}"),
        }
    }

    assert!(db.column_family("hot").is_none());
}
