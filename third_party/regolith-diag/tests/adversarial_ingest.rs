//! Adversarial probe on external-file ingest under concurrent writes.
//!
//! Ingest allocates its sequence number under the commit pipeline's
//! lock and publishes the read horizon outside it, so this drives it
//! against a stream of ordinary writers and a reader that demands the
//! ingested file be visible the moment `ingest_external_files` returns.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

use regolith::{Db, IngestOptions, Options, SstFileWriter, WriteBatch};
use tempfile::TempDir;

fn build_sst(path: &std::path::Path, batch: usize, opts: &Options) {
    let mut writer = SstFileWriter::create(path, opts).unwrap();
    for i in 0..64 {
        writer
            .put(
                format!("ing_{batch:04}_{i:03}").as_bytes(),
                format!("v{batch}").as_bytes(),
            )
            .unwrap();
    }
    writer.finish().unwrap();
}

/// Everything an ingest published must be readable from another thread
/// the instant the call returns, and every concurrent ordinary write
/// must still be there afterwards.
#[test]
fn ingest_under_concurrent_writers_stays_visible_and_loses_nothing() {
    let dir = TempDir::new().unwrap();
    let staging = TempDir::new().unwrap();
    let opts = Options::default().write_buffer_size(32 * 1024);
    let db = Arc::new(Db::open(dir.path(), opts.clone()).unwrap());

    let stop = Arc::new(AtomicBool::new(false));
    let written = Arc::new(AtomicUsize::new(0));
    let mut writers = Vec::new();
    for w in 0..6usize {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        let written = Arc::clone(&written);
        writers.push(thread::spawn(move || {
            let mut round = 0usize;
            while !stop.load(Ordering::Relaxed) {
                let mut batch = WriteBatch::new();
                for k in 0..8 {
                    batch.put(format!("w{w}_{round:06}_{k}").as_bytes(), b"v");
                }
                regolith::through_stalls(|| db.write(batch.clone())).unwrap();
                written.fetch_add(1, Ordering::Relaxed);
                round += 1;
            }
            round
        }));
    }

    let batches = 40usize;
    for batch in 0..batches {
        let path = staging.path().join(format!("ing_{batch}.sst"));
        // `snapshot_consistency` defaults to rejecting an ingest while a
        // snapshot is pinned, which is not what this probe is about.
        build_sst(&path, batch, &opts);
        db.ingest_external_files(
            std::slice::from_ref(&path),
            IngestOptions {
                snapshot_consistency: false,
                ..IngestOptions::default()
            },
        )
        .wait()
        .unwrap();

        // Visible immediately, from this thread and from a fresh snapshot.
        for i in 0..64 {
            let k = format!("ing_{batch:04}_{i:03}");
            assert_eq!(
                db.get(k.as_bytes()).unwrap(),
                Some(format!("v{batch}").into_bytes()),
                "an ingested key was not visible when ingest returned"
            );
        }
        let snap = db.snapshot();
        for i in (0..64).step_by(7) {
            let k = format!("ing_{batch:04}_{i:03}");
            assert_eq!(
                snap.get(k.as_bytes()).unwrap(),
                Some(format!("v{batch}").into_bytes()),
                "a snapshot taken after the ingest could not see it"
            );
        }
    }

    stop.store(true, Ordering::Relaxed);
    let rounds: Vec<usize> = writers.into_iter().map(|w| w.join().unwrap()).collect();

    // Every ingested key and every concurrent write must survive.
    for batch in 0..batches {
        for i in 0..64 {
            assert_eq!(
                db.get(format!("ing_{batch:04}_{i:03}").as_bytes()).unwrap(),
                Some(format!("v{batch}").into_bytes()),
                "an ingested key was lost"
            );
        }
    }
    for (w, rounds) in rounds.iter().enumerate() {
        for round in 0..*rounds {
            for k in 0..8 {
                assert_eq!(
                    db.get(format!("w{w}_{round:06}_{k}").as_bytes()).unwrap(),
                    Some(b"v".to_vec()),
                    "a concurrent write was lost across an ingest"
                );
            }
        }
    }
    assert!(
        written.load(Ordering::Relaxed) > 100,
        "the writers barely ran"
    );

    // And all of it must survive a reopen.
    db.close().unwrap();
    drop(db);
    let reopened = Db::open(dir.path(), Options::default()).unwrap();
    for batch in 0..batches {
        for i in (0..64).step_by(11) {
            assert_eq!(
                reopened
                    .get(format!("ing_{batch:04}_{i:03}").as_bytes())
                    .unwrap(),
                Some(format!("v{batch}").into_bytes()),
                "an ingested key did not survive a reopen"
            );
        }
    }
    for (w, rounds) in rounds.iter().enumerate() {
        for round in (0..*rounds).step_by(7) {
            assert_eq!(
                reopened
                    .get(format!("w{w}_{round:06}_0").as_bytes())
                    .unwrap(),
                Some(b"v".to_vec()),
                "a concurrent write did not survive a reopen"
            );
        }
    }
}

/// A `disable_wal` writer riding in a group with WAL-enabled writers:
/// its ops consume sequence numbers that never reach the log. Recovery
/// must cope with the resulting hole rather than mis-order anything.
#[test]
fn wal_disabled_writers_in_a_group_do_not_break_recovery() {
    let dir = TempDir::new().unwrap();
    let db = Arc::new(
        Db::open(
            dir.path(),
            Options::default().write_buffer_size(8 * 1024 * 1024),
        )
        .unwrap(),
    );

    let mut handles = Vec::new();
    for w in 0..8usize {
        let db = Arc::clone(&db);
        handles.push(thread::spawn(move || {
            let wo = regolith::WriteOptions {
                disable_wal: w % 2 == 0,
                ..regolith::WriteOptions::default()
            };
            for i in 0..500usize {
                regolith::through_stalls(|| {
                    db.put_opt(&wo, format!("d{w}_{i:04}").as_bytes(), b"v")
                })
                .unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    // Everything is readable before any flush.
    for w in 0..8usize {
        for i in 0..500usize {
            assert_eq!(
                db.get(format!("d{w}_{i:04}").as_bytes()).unwrap(),
                Some(b"v".to_vec())
            );
        }
    }

    // Drop without closing, so nothing is flushed and only the WAL is
    // left to recover from.
    drop(db);
    let reopened = Db::open(dir.path(), Options::default()).unwrap();
    for w in (1..8usize).step_by(2) {
        for i in 0..500usize {
            assert_eq!(
                reopened.get(format!("d{w}_{i:04}").as_bytes()).unwrap(),
                Some(b"v".to_vec()),
                "a WAL-backed write from writer {w} was lost even though its group logged it"
            );
        }
    }
    // Writes that opted out of the WAL are allowed to be gone; what must
    // not happen is a wrong value or a resurrected different key.
    for (k, v) in reopened.scan(None, None).unwrap() {
        assert_eq!(v, b"v".to_vec(), "recovery invented a value for {k:?}");
    }
}
