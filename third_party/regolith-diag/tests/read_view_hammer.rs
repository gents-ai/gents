//! Scaled-up hammer against the kovan-backed read view.
//!
//! Two invariants, driven far harder than the existing read-view regressions
//! and with every publisher of the view running at once:
//!
//! * **Monotonic reads.** A key that is only ever overwritten must
//!   never read back absent, and the stamp a single reader thread
//!   observes for one key must never travel backwards.
//! * **Batch atomicity.** A writer sets its whole key group to one
//!   stamp in a single `WriteBatch`. A reader that samples the group
//!   must see one stamp across the group, never a mixture of two.
//!
//! Bounded by operation counts, not by wall time, so it is not a soak.
//!
//! Scale is `REGOLITH_HAMMER_VERSIONS` (default 120) and
//! `REGOLITH_HAMMER_COMPACTIONS` (default 24). The defaults are the
//! smallest shape that still has every publisher running at once; the
//! soak shape is 900 and 400, which is hours of CPU and tens of GB of
//! scratch space, not a gate.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

use regolith::{Db, Options, WriteBatch};
use tempfile::TempDir;

const WRITERS: usize = 6;
const READERS: usize = 6;
const KEYS_PER_WRITER: usize = 24;
/// Column-family create/drop cycles the churn thread performs. Bounded
/// for the reason given at its spawn site.
const CF_CHURN_PASSES: u64 = 256;

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn key_of(w: usize, i: usize) -> Vec<u8> {
    format!("w{w:03}k{i:04}").into_bytes()
}

fn value_of(stamp: u64) -> Vec<u8> {
    let mut v = format!("v{stamp:016}").into_bytes();
    v.resize(192, b'.');
    v
}

fn stamp_of(v: &[u8]) -> u64 {
    std::str::from_utf8(&v[1..17])
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(u64::MAX)
}

fn open(dir: &TempDir) -> Arc<Db> {
    Arc::new(Db::open(dir.path(), Options::default().write_buffer_size(8 * 1024)).expect("open"))
}

#[test]
fn overwritten_keys_never_vanish_and_never_travel_backwards() {
    // Gate-shaped by default, crankable for a soak. Every publisher is
    // already running at this size; more versions buy a wider window
    // for a rare interleaving, not a different shape. The full soak is
    // `REGOLITH_HAMMER_VERSIONS=900 REGOLITH_HAMMER_COMPACTIONS=400`, which
    // costs hours of CPU and tens of GB of scratch and is not a gate.
    let versions = env("REGOLITH_HAMMER_VERSIONS", 120) as u64;
    let compaction_passes = env("REGOLITH_HAMMER_COMPACTIONS", 24) as u32;
    let dir = TempDir::new().expect("tempdir");
    let db = open(&dir);

    let keys: Vec<Vec<u8>> = (0..WRITERS)
        .flat_map(|w| (0..KEYS_PER_WRITER).map(move |i| key_of(w, i)))
        .collect();
    for k in &keys {
        regolith::through_stalls(|| db.put(k, &value_of(0))).expect("seed");
    }

    let bad: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let live = Arc::new(AtomicU64::new(WRITERS as u64));
    let stop = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicU64::new(0));
    // Writers, readers, two chaos threads, and this thread.
    let gate = Arc::new(Barrier::new(WRITERS + READERS + 2 + 1));
    let mut handles = Vec::new();

    for w in 0..WRITERS {
        let (db, live, gate) = (Arc::clone(&db), Arc::clone(&live), Arc::clone(&gate));
        handles.push(thread::spawn(move || {
            gate.wait();
            for v in 1..=versions {
                // Whole group to one stamp in one batch: the unit the
                // batch-atomicity half of this test checks.
                let mut b = WriteBatch::new();
                for i in 0..KEYS_PER_WRITER {
                    b.put(&key_of(w, i), &value_of(v));
                }
                regolith::through_stalls(|| db.write(b.clone())).expect("write batch");
            }
            live.fetch_sub(1, Ordering::AcqRel);
        }));
    }

    for r in 0..READERS {
        let (db, bad, gate, live, reads) = (
            Arc::clone(&db),
            Arc::clone(&bad),
            Arc::clone(&gate),
            Arc::clone(&live),
            Arc::clone(&reads),
        );
        handles.push(thread::spawn(move || {
            let mut high = vec![0u64; WRITERS * KEYS_PER_WRITER];
            gate.wait();
            while live.load(Ordering::Acquire) > 0 {
                for w in 0..WRITERS {
                    let group: Vec<Vec<u8>> = (0..KEYS_PER_WRITER).map(|i| key_of(w, i)).collect();
                    let refs: Vec<&[u8]> = group.iter().map(|k| k.as_slice()).collect();
                    // Surface rotates so every read entry point is hit.
                    let observed: Vec<Option<Vec<u8>>> = match (r + w) % 3 {
                        0 => db.multi_get(&refs).expect("multi_get"),
                        1 => refs.iter().map(|k| db.get(k).expect("get")).collect(),
                        _ => {
                            let snap = db.snapshot();
                            refs.iter()
                                .map(|k| snap.get(k).expect("snap get"))
                                .collect()
                        }
                    };
                    reads.fetch_add(observed.len() as u64, Ordering::Relaxed);

                    let mut seen_stamps = Vec::new();
                    for (i, slot) in observed.iter().enumerate() {
                        let idx = w * KEYS_PER_WRITER + i;
                        match slot {
                            None => bad.lock().unwrap().push(format!(
                                "reader {r}: key w{w:03}k{i:04} read back ABSENT after \
                                 having been seen at stamp {}",
                                high[idx]
                            )),
                            Some(v) => {
                                let s = stamp_of(v);
                                seen_stamps.push(s);
                                if s < high[idx] {
                                    bad.lock().unwrap().push(format!(
                                        "reader {r}: key w{w:03}k{i:04} went BACKWARDS \
                                         {} -> {s}",
                                        high[idx]
                                    ));
                                }
                                high[idx] = high[idx].max(s);
                            }
                        }
                    }
                    // multi_get and the snapshot read resolve against one
                    // view and one horizon, so a batch must be all or
                    // nothing across the group.
                    if (r + w) % 3 != 1 && seen_stamps.len() == KEYS_PER_WRITER {
                        let lo = seen_stamps.iter().copied().min().unwrap_or(0);
                        let hi = seen_stamps.iter().copied().max().unwrap_or(0);
                        if lo != hi {
                            bad.lock().unwrap().push(format!(
                                "reader {r}: writer {w}'s batch observed TORN, \
                                 stamps span {lo}..={hi}"
                            ));
                        }
                    }
                }
            }
        }));
    }

    // Chaos 1: foreground compaction on a user thread.
    {
        let (db, gate, live) = (Arc::clone(&db), Arc::clone(&gate), Arc::clone(&live));
        handles.push(thread::spawn(move || {
            gate.wait();
            // Bounded: `compact_range` is O(dataset) per pass, so an
            // unbounded loop turns the run quadratic in the dataset
            // rather than pushing publication churn any harder.
            let mut passes = 0u32;
            while live.load(Ordering::Acquire) > 0 && passes < compaction_passes {
                db.compact_range(None, None).wait().expect("compact_range");
                passes += 1;
            }
        }));
    }

    // Chaos 2: column families created and dropped under the readers,
    // each one a version edit that republishes the view.
    {
        let (db, gate, live, stop) = (
            Arc::clone(&db),
            Arc::clone(&gate),
            Arc::clone(&live),
            Arc::clone(&stop),
        );
        handles.push(thread::spawn(move || {
            gate.wait();
            // Bounded like the compaction thread beside it. Each pass
            // creates and drops a column family, which edits the
            // manifest, so an unbounded spinner makes the run cost grow
            // with how long the writers happen to take rather than with
            // the work they do: this test timed out at 300s on a loaded
            // machine while finishing in seconds on an idle one.
            let mut n = 0u64;
            while live.load(Ordering::Acquire) > 0 && n < CF_CHURN_PASSES {
                let name = format!("chaos_cf_{n}");
                if let Ok(cf) = db.create_column_family(&name) {
                    let _ = regolith::through_stalls(|| db.put_cf(&cf, b"x", b"y"));
                    let _ = db.drop_column_family(cf);
                }
                n += 1;
            }
            stop.store(true, Ordering::Release);
        }));
    }

    gate.wait();
    for h in handles {
        h.join().expect("join");
    }

    let bad = bad.lock().unwrap();
    eprintln!(
        "hammer: {} writers x {versions} batched versions x {KEYS_PER_WRITER} keys, \
         {READERS} readers, {} key-reads, {} violations",
        WRITERS,
        reads.load(Ordering::Relaxed),
        bad.len()
    );
    assert!(
        bad.is_empty(),
        "{} read-view violations, first 10: {:#?}",
        bad.len(),
        &bad[..bad.len().min(10)]
    );
}
