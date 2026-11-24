//! Read-your-own-writes and lock-order probes for the published read
//! view (the published read view).
//!
//! Publication is a compare-exchange, so there is no publish mutex for
//! a version edit to nest under the version-set mutex and no lock order
//! left to invert. This still drives every publisher at once (rotation,
//! foreground flush, background compaction, `compact_range`,
//! `drop_all`) with a watchdog that fails instead of hanging: the
//! liveness question survives the mutex that prompted it, because a
//! compare-exchange retry loop has its own way to make no progress.
//!
//! Read-your-own-writes is the other half: a thread that writes a key
//! and immediately reads it back must see at least what it just wrote,
//! whatever rotation or flush lands between the two calls.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use regolith::{Db, Options, WriteBatch};
use tempfile::TempDir;

fn env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// Every writer reads back what it just wrote, through `get`,
/// `multi_get` and `iter`, while rotation and compaction churn.
#[test]
fn a_writer_always_reads_back_at_least_its_own_write() {
    let threads = env("REGOLITH_RYW_THREADS", 12) as usize;
    let ops = env("REGOLITH_RYW_OPS", 4000);

    let dir = TempDir::new().expect("tempdir");
    let db = Arc::new(
        Db::open(dir.path(), Options::default().write_buffer_size(8 * 1024)).expect("open"),
    );

    let bad: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new(Barrier::new(threads));
    let mut handles = Vec::new();
    for t in 0..threads {
        let (db, bad, gate) = (Arc::clone(&db), Arc::clone(&bad), Arc::clone(&gate));
        handles.push(thread::spawn(move || {
            gate.wait();
            for i in 0..ops {
                let key = format!("t{t:03}_k{:04}", i % 64).into_bytes();
                let value = format!("v{i:016}").into_bytes();
                if i % 3 == 0 {
                    let mut b = WriteBatch::new();
                    b.put(&key, &value);
                    regolith::through_stalls(|| db.write(b.clone())).expect("write");
                } else {
                    regolith::through_stalls(|| db.put(&key, &value)).expect("put");
                }

                let got = db.get(&key).expect("get");
                if got.as_deref() != Some(value.as_slice()) {
                    bad.lock().expect("lock").push(format!(
                        "thread {t} op {i}: get did not read back its own write ({:?})",
                        got.map(|v| String::from_utf8_lossy(&v).into_owned()),
                    ));
                }
                let batched = db.multi_get(&[key.as_slice()]).expect("multi_get");
                if batched[0].as_deref() != Some(value.as_slice()) {
                    bad.lock().expect("lock").push(format!(
                        "thread {t} op {i}: multi_get did not read back its own write",
                    ));
                }
                if i % 97 == 0 {
                    let mut it = db.iter();
                    it.seek(&key);
                    let seen = it.valid() && it.key() == Some(key.as_slice());
                    it.status().expect("iter status");
                    if !seen {
                        bad.lock().expect("lock").push(format!(
                            "thread {t} op {i}: iter did not find the key it just wrote",
                        ));
                    }
                }
            }
        }));
    }
    for h in handles {
        h.join().expect("worker panicked");
    }
    let recorded = std::mem::take(&mut *bad.lock().expect("lock"));
    println!(
        "read-your-writes: {threads} threads x {ops} ops, {} violation(s)",
        recorded.len()
    );
    assert!(
        recorded.is_empty(),
        "{}",
        recorded
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

/// Drive every publisher of the read view at once. The assertion is
/// liveness: the run must finish, and every reader must keep making
/// progress. Publication is a compare-exchange retry loop, which is
/// lock-free but not wait-free, so a publisher that could not converge
/// under maximum contention would stall here rather than fail a value
/// check; a stalled run is the failure.
#[test]
fn every_publisher_of_the_read_view_running_at_once_stays_live() {
    let secs = env("REGOLITH_LIVE_SECS", 8);
    let dir = TempDir::new().expect("tempdir");
    let db = Arc::new(
        Db::open(dir.path(), Options::default().write_buffer_size(4 * 1024)).expect("open"),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(AtomicU64::new(0));
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut handles = Vec::new();

    for t in 0..8usize {
        let (db, stop) = (Arc::clone(&db), Arc::clone(&stop));
        handles.push(thread::spawn(move || {
            let mut i = 0u64;
            while !stop.load(Ordering::Relaxed) {
                regolith::through_stalls(|| {
                    db.put(format!("p{t}_{:05}", i % 4096).as_bytes(), &[b'x'; 96])
                })
                .expect("put");
                i += 1;
            }
        }));
    }
    {
        let (db, stop) = (Arc::clone(&db), Arc::clone(&stop));
        handles.push(thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                db.compact_range(None, None).wait().expect("compact_range");
            }
        }));
    }
    {
        let (db, stop) = (Arc::clone(&db), Arc::clone(&stop));
        handles.push(thread::spawn(move || {
            let mut n = 0u64;
            while !stop.load(Ordering::Relaxed) {
                n += 1;
                if n.is_multiple_of(64) {
                    db.drop_all().expect("drop_all");
                }
                regolith::through_stalls(|| db.put(b"churn", &n.to_be_bytes())).expect("put");
            }
        }));
    }
    for _ in 0..4usize {
        let (db, stop, progress) = (Arc::clone(&db), Arc::clone(&stop), Arc::clone(&progress));
        handles.push(thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let _ = db.get(b"p0_00001").expect("get");
                let mut it = db.iter();
                it.seek_to_last();
                let _ = it.valid();
                it.status().expect("iter status");
                progress.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    // The stall threshold has to fit inside the window it is watching.
    // It was a flat 60 seconds against an 8 second run, so
    // `stalled.elapsed()` could never reach it, `wedged` was always
    // `None`, and the assertion below was `assert!(true)`: the wedge
    // detector this test exists for had never run.
    let stall_limit = Duration::from_secs(secs / 4).max(Duration::from_secs(1));
    let mut last = 0u64;
    let mut stalled = Instant::now();
    let mut wedged = None;
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
        let now = progress.load(Ordering::Relaxed);
        if now != last {
            last = now;
            stalled = Instant::now();
        } else if stalled.elapsed() > stall_limit {
            wedged = Some(now);
            break;
        }
    }
    stop.store(true, Ordering::Relaxed);

    // Asserted before the join: a genuinely wedged reader never returns
    // to check `stop`, so joining first would hang and bury the reason.
    let rounds = progress.load(Ordering::Relaxed);
    assert!(
        wedged.is_none(),
        "the read view wedged: no reader progressed for {stall_limit:?} at {rounds} rounds",
    );
    // And a run where the readers never got going proves nothing either
    // way, so it is a failure rather than a quiet pass.
    assert!(
        rounds > 0,
        "no reader completed a round in {secs}s, so nothing was observed"
    );
    for h in handles {
        h.join().expect("worker panicked");
    }
    println!(
        "liveness: {} reader rounds over {secs}s with rotation, compaction, drop_all and \
         background compaction all publishing",
        progress.load(Ordering::Relaxed),
    );
}
