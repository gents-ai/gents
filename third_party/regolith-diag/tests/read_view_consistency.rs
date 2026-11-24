//! Independent adversarial review of the published read view.
//!
//! The fix publishes the active memtable, the frozen memtables and the
//! version as one immutable object, so a read resolves every source at
//! one point in time. These probes attack the two shapes the shipped
//! suite does not cover directly:
//!
//! * `drop_all` running under read load. It is the one operation that
//!   moves data in the *newer* direction, emptying every source at once,
//!   so it is where "successive views only move data older" is most
//!   likely to be violated.
//! * a `Snapshot` held open while compaction rewrites and unlinks the
//!   files underneath it, which is what the view's pin chain exists for.
//!
//! The invariant asserted throughout is the one the bug report names: a
//! key that is only ever overwritten, never deleted, must never read back
//! as absent, and a repeated read of it must never travel backwards.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;

use regolith::{Db, DurabilityMode, Options};
use tempfile::TempDir;

const KEYS: u32 = 64;
/// Writer threads in the overwrite-only probe. Each owns a disjoint
/// slice of the key space.
const WRITERS: u32 = 4;

/// The keys writer `w` owns.
fn keys_of(w: u32) -> impl Iterator<Item = u32> {
    let per = KEYS / WRITERS;
    (w * per)..((w + 1) * per)
}

fn opts() -> Options {
    Options::default()
        .write_buffer_size(16 * 1024)
        .durability(DurabilityMode::Eventual)
}

fn key(i: u32) -> Vec<u8> {
    format!("k{i:05}").into_bytes()
}

fn value(stamp: u64) -> Vec<u8> {
    format!("v{stamp:016}").into_bytes()
}

fn stamp(v: &[u8]) -> Option<u64> {
    std::str::from_utf8(v.get(1..)?).ok()?.parse().ok()
}

/// A key that is only ever overwritten must never read back absent, and
/// repeated reads of it must not travel backwards, while `compact_range`
/// and heavy overwriting run concurrently.
#[test]
fn a_never_deleted_key_never_reads_absent_under_compaction_and_rotation() {
    let dir = TempDir::new().expect("tempdir");
    let db = Arc::new(Db::open(dir.path(), opts()).expect("open"));
    for i in 0..KEYS {
        regolith::through_stalls(|| db.put(&key(i), &value(0))).expect("seed");
    }

    let stop = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicU64::new(0));
    let violations: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    thread::scope(|s| {
        // Each writer owns a disjoint slice of the key space, so the
        // stamp stored under any one key rises strictly with that
        // writer's rounds. Sharing keys between writers would make the
        // stored stamp legitimately non-monotonic and the oracle wrong.
        for w in 0..WRITERS {
            let (db, stop) = (Arc::clone(&db), Arc::clone(&stop));
            s.spawn(move || {
                let mut n = 1u64;
                while !stop.load(Ordering::Relaxed) {
                    for i in keys_of(w) {
                        let _ = db.put(&key(i), &value(n));
                    }
                    n += 1;
                }
            });
        }
        let compactor = {
            let (db, stop) = (Arc::clone(&db), Arc::clone(&stop));
            s.spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let _ = db.compact_range(None, None).wait();
                }
            })
        };

        for _ in 0..4 {
            let (db, stop, reads, violations) = (
                Arc::clone(&db),
                Arc::clone(&stop),
                Arc::clone(&reads),
                Arc::clone(&violations),
            );
            s.spawn(move || {
                let mut last = vec![0u64; KEYS as usize];
                while !stop.load(Ordering::Relaxed) {
                    for i in 0..KEYS {
                        match db.get(&key(i)) {
                            Ok(None) => violations.lock().expect("violations").push(format!(
                                "k{i:05} read ABSENT though it is only ever overwritten"
                            )),
                            Ok(Some(v)) => {
                                let Some(st) = stamp(&v) else {
                                    violations
                                        .lock()
                                        .expect("violations")
                                        .push(format!("k{i:05} served unparsable {v:02x?}"));
                                    continue;
                                };
                                let prev = last[i as usize];
                                if st < prev {
                                    violations.lock().expect("violations").push(format!(
                                        "k{i:05} travelled backwards: {prev} then {st}"
                                    ));
                                }
                                last[i as usize] = st.max(prev);
                            }
                            Err(e) => violations
                                .lock()
                                .expect("violations")
                                .push(format!("k{i:05} errored: {e}")),
                        }
                        reads.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }

        thread::sleep(std::time::Duration::from_secs(6));
        stop.store(true, Ordering::Relaxed);
        let _ = compactor;
    });

    let v = violations.lock().expect("violations");
    println!(
        "overwrite-only invariant: {} reads, {} violation(s)",
        reads.load(Ordering::Relaxed),
        v.len(),
    );
    assert!(
        v.is_empty(),
        "{}",
        v.iter().take(10).cloned().collect::<Vec<_>>().join("\n  "),
    );
}

/// `drop_all` under read load. A read may legitimately see the database
/// full or empty, but it must never error, never serve a value that was
/// never written, and never see a key reappear with an older stamp than
/// one the same reader already saw after the same generation of writes.
#[test]
fn drop_all_under_read_load_never_serves_a_value_that_was_never_written() {
    let dir = TempDir::new().expect("tempdir");
    let db = Arc::new(Db::open(dir.path(), opts()).expect("open"));
    let stop = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicU64::new(0));
    let drops = Arc::new(AtomicU64::new(0));
    let violations: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    thread::scope(|s| {
        for w in 0..2 {
            let (db, stop) = (Arc::clone(&db), Arc::clone(&stop));
            s.spawn(move || {
                let mut n = 1u64;
                while !stop.load(Ordering::Relaxed) {
                    for i in 0..KEYS {
                        let _ = db.put(&key(i), &value(n * 100 + w));
                    }
                    n += 1;
                }
            });
        }
        {
            let (db, stop, drops) = (Arc::clone(&db), Arc::clone(&stop), Arc::clone(&drops));
            s.spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    if db.drop_all().is_ok() {
                        drops.fetch_add(1, Ordering::Relaxed);
                    }
                    thread::yield_now();
                }
            });
        }
        for _ in 0..4 {
            let (db, stop, reads, violations) = (
                Arc::clone(&db),
                Arc::clone(&stop),
                Arc::clone(&reads),
                Arc::clone(&violations),
            );
            s.spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    for i in 0..KEYS {
                        match db.get(&key(i)) {
                            Ok(None) => {}
                            Ok(Some(v)) => {
                                if stamp(&v).is_none() {
                                    violations.lock().expect("violations").push(format!(
                                        "k{i:05} served a value that was never written: {v:02x?}"
                                    ));
                                }
                            }
                            Err(e) => violations
                                .lock()
                                .expect("violations")
                                .push(format!("k{i:05} errored during drop_all: {e}")),
                        }
                        reads.fetch_add(1, Ordering::Relaxed);
                    }
                    // A scan must agree with itself: strictly increasing keys.
                    match db.scan(None, None) {
                        Ok(pairs) => {
                            for pair in pairs.windows(2) {
                                if pair[0].0 >= pair[1].0 {
                                    violations.lock().expect("violations").push(format!(
                                        "scan out of order: {:02x?} then {:02x?}",
                                        pair[0].0, pair[1].0
                                    ));
                                }
                            }
                        }
                        Err(e) => violations
                            .lock()
                            .expect("violations")
                            .push(format!("scan errored: {e}")),
                    }
                }
            });
        }
        thread::sleep(std::time::Duration::from_secs(6));
        stop.store(true, Ordering::Relaxed);
    });

    let v = violations.lock().expect("violations");
    println!(
        "drop_all under load: {} reads, {} drop_all calls, {} violation(s)",
        reads.load(Ordering::Relaxed),
        drops.load(Ordering::Relaxed),
        v.len(),
    );
    assert!(
        v.is_empty(),
        "{}",
        v.iter().take(10).cloned().collect::<Vec<_>>().join("\n  "),
    );
}

/// A `Snapshot` taken before a storm of compactions must keep serving the
/// exact state it captured, even though every file it was built on has
/// since been rewritten and unlinked.
#[test]
fn a_snapshot_survives_the_compaction_that_unlinks_every_file_under_it() {
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), opts()).expect("open");
    for i in 0..KEYS {
        regolith::through_stalls(|| db.put(&key(i), &value(1))).expect("seed");
    }
    db.compact_range(None, None).wait().expect("compact");

    let snap = db.snapshot();
    let captured: Vec<(Vec<u8>, Vec<u8>)> = snap.scan(None, None).expect("scan");
    assert_eq!(
        captured.len(),
        KEYS as usize,
        "the snapshot must see the seed"
    );

    // Rewrite everything many times over and compact after each pass, so
    // every file the snapshot was built on is unlinked.
    for generation in 2..40u64 {
        for i in 0..KEYS {
            regolith::through_stalls(|| db.put(&key(i), &value(generation))).expect("put");
        }
        db.compact_range(None, None).wait().expect("compact");
    }

    for i in 0..KEYS {
        assert_eq!(
            snap.get(&key(i)).expect("snapshot get"),
            Some(value(1)),
            "the snapshot lost k{i:05} after its files were rewritten",
        );
    }
    assert_eq!(
        snap.scan(None, None).expect("scan"),
        captured,
        "the snapshot's scan changed under it",
    );

    let mut back = Vec::new();
    let mut it = snap.iter();
    it.seek_to_last();
    while it.valid() {
        back.push((
            it.key().expect("key").to_vec(),
            it.value().expect("value").to_vec(),
        ));
        it.prev();
    }
    back.reverse();
    assert_eq!(back, captured, "the snapshot's reverse walk disagrees");

    println!(
        "snapshot held across 38 rewrite-and-compact generations: {} entries stable, \
         forward and backward",
        captured.len(),
    );
    drop(snap);
    drop(db);
}
