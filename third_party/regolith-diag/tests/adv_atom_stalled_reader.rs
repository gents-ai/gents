//! Stalled-reader probes: a reader that captures its sources and then
//! reads long after several publications must still see exactly the
//! data it captured, with the files it needs kept alive through the
//! `Arc<Version>` -> `Arc<LiveSst>` pin chain that the read view now
//! sits on top of.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use regolith::{Db, Options};
use tempfile::TempDir;

fn open(dir: &TempDir) -> Arc<Db> {
    Arc::new(Db::open(dir.path(), Options::default().write_buffer_size(8 * 1024)).expect("open"))
}

const KEYS: usize = 512;

fn key(i: usize) -> Vec<u8> {
    format!("k{i:06}").into_bytes()
}

/// A snapshot taken before heavy churn, read after it. Every key must
/// still resolve to the value it had when the snapshot was taken, even
/// though compaction has since rewritten and unlinked every file the
/// snapshot needs.
#[test]
fn a_snapshot_read_after_many_publications_still_sees_its_own_instant() {
    let dir = TempDir::new().expect("tempdir");
    let db = open(&dir);

    for i in 0..KEYS {
        regolith::through_stalls(|| db.put(&key(i), format!("stamp{:04}", 0).as_bytes()))
            .expect("seed");
    }
    let snap = db.snapshot();

    let stop = Arc::new(AtomicBool::new(false));
    let churn = {
        let (db, stop) = (Arc::clone(&db), Arc::clone(&stop));
        thread::spawn(move || {
            let mut v = 1u32;
            while !stop.load(Ordering::Acquire) && v <= 60 {
                for i in 0..KEYS {
                    regolith::through_stalls(|| db.put(&key(i), format!("stamp{v:04}").as_bytes()))
                        .expect("put");
                }
                db.compact_range(None, None).wait().expect("compact");
                v += 1;
            }
        })
    };

    // Read the snapshot repeatedly while the churn runs, then once more
    // after it has finished, so the last pass reads through a view that
    // is many publications behind.
    let mut passes = 0u32;
    while !churn.is_finished() {
        for i in (0..KEYS).step_by(7) {
            let got = snap.get(&key(i)).expect("snap get");
            assert_eq!(
                got.as_deref(),
                Some(b"stamp0000".as_ref()),
                "snapshot key {i} drifted on pass {passes}"
            );
        }
        passes += 1;
    }
    stop.store(true, Ordering::Release);
    churn.join().expect("join");

    for i in 0..KEYS {
        assert_eq!(
            snap.get(&key(i)).expect("snap get").as_deref(),
            Some(b"stamp0000".as_ref()),
            "snapshot key {i} drifted after the churn finished"
        );
    }
    eprintln!("stalled snapshot survived {passes} concurrent read passes");
}

/// An iterator captured before a compaction keeps reading its own
/// sources after the compaction has unlinked every file underneath it.
#[test]
fn an_iterator_outlives_the_compaction_that_unlinks_its_files() {
    let dir = TempDir::new().expect("tempdir");
    let db = open(&dir);
    for i in 0..KEYS {
        regolith::through_stalls(|| db.put(&key(i), b"v0")).expect("seed");
    }
    db.compact_range(None, None).wait().expect("compact");

    let mut iter = db.iter();
    iter.seek_to_first();
    let mut seen = 0usize;
    // Consume one entry so the iterator has really opened its sources.
    assert!(iter.valid());
    assert_eq!(iter.value(), Some(b"v0".as_ref()));
    seen += 1;
    iter.next();

    for v in 1..=20u32 {
        for i in 0..KEYS {
            regolith::through_stalls(|| db.put(&key(i), format!("v{v}").as_bytes())).expect("put");
        }
        db.compact_range(None, None).wait().expect("compact");
    }

    while iter.valid() {
        assert_eq!(
            iter.value(),
            Some(b"v0".as_ref()),
            "the iterator's captured version drifted"
        );
        seen += 1;
        iter.next();
    }
    assert_eq!(seen, KEYS, "iterator lost entries across the compactions");
}
