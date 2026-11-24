//! `drop_all` publishes a view that drops the last reference to every
//! memtable and then calls `kovan::flush()` while readers may still be
//! holding guards on the view it just replaced. If a pinned view could
//! ever be freed under a reader, this is the shape that finds it.
//!
//! The assertion is not "the key is present" - `drop_all` legitimately
//! removes it - but "whatever comes back is a value this workload ever
//! wrote". A freed view reads as garbage or crashes; it does not read
//! as a well-formed older value.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

use regolith::{Db, Options};
use tempfile::TempDir;

const KEYS: usize = 64;
const READERS: usize = 8;

fn key(i: usize) -> Vec<u8> {
    format!("k{i:04}").into_bytes()
}

fn value(stamp: u64) -> Vec<u8> {
    let mut v = format!("v{stamp:016}").into_bytes();
    v.resize(128, b'#');
    v
}

fn well_formed(v: &[u8]) -> bool {
    v.len() == 128
        && v[0] == b'v'
        && v[1..17].iter().all(|c| c.is_ascii_digit())
        && v[17..].iter().all(|c| *c == b'#')
}

#[test]
fn readers_never_observe_a_freed_view_across_drop_all() {
    let rounds: u64 = std::env::var("REGOLITH_DROPALL_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(400);

    let dir = TempDir::new().expect("tempdir");
    let db = Arc::new(
        Db::open(dir.path(), Options::default().write_buffer_size(8 * 1024)).expect("open"),
    );

    let bad: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicU64::new(0));
    let gate = Arc::new(Barrier::new(READERS + 1 + 1));
    let mut handles = Vec::new();

    for r in 0..READERS {
        let (db, bad, gate, done, reads) = (
            Arc::clone(&db),
            Arc::clone(&bad),
            Arc::clone(&gate),
            Arc::clone(&done),
            Arc::clone(&reads),
        );
        handles.push(thread::spawn(move || {
            gate.wait();
            while !done.load(Ordering::Acquire) {
                for i in 0..KEYS {
                    let got = if r % 2 == 0 {
                        db.get(&key(i)).expect("get")
                    } else {
                        let snap = db.snapshot();
                        snap.get(&key(i)).expect("snap get")
                    };
                    reads.fetch_add(1, Ordering::Relaxed);
                    if let Some(v) = got
                        && !well_formed(&v)
                    {
                        bad.lock().unwrap().push(format!(
                            "reader {r} key {i}: malformed value len={} head={:?}",
                            v.len(),
                            &v[..v.len().min(20)]
                        ));
                    }
                }
                let mut it = db.iter();
                it.seek_to_first();
                while it.valid() {
                    if let Some(v) = it.value()
                        && !well_formed(v)
                    {
                        bad.lock()
                            .unwrap()
                            .push(format!("reader {r} iter: malformed value len={}", v.len()));
                    }
                    reads.fetch_add(1, Ordering::Relaxed);
                    it.next();
                }
            }
        }));
    }

    {
        let (db, gate, done) = (Arc::clone(&db), Arc::clone(&gate), Arc::clone(&done));
        handles.push(thread::spawn(move || {
            gate.wait();
            for round in 1..=rounds {
                for i in 0..KEYS {
                    regolith::through_stalls(|| db.put(&key(i), &value(round))).expect("put");
                }
                if round.is_multiple_of(7) {
                    db.drop_all().expect("drop_all");
                }
                if round.is_multiple_of(11) {
                    db.compact_range(None, None).wait().expect("compact");
                }
            }
            done.store(true, Ordering::Release);
        }));
    }

    gate.wait();
    for h in handles {
        h.join().expect("join");
    }

    let bad = bad.lock().unwrap();
    eprintln!(
        "drop_all race: {rounds} rounds, {} reads, {} malformed values",
        reads.load(Ordering::Relaxed),
        bad.len()
    );
    assert!(bad.is_empty(), "first 5: {:#?}", &bad[..bad.len().min(5)]);
}
