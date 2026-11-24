//! the published read view on the scan entry points, including the column-family ones.
//!
//! `Db::get`, `Db::multi_get` and `Db::iter` load the published read
//! view **before** sampling the read horizon. `Db::scan`,
//! `Db::scan_page`, `Db::scan_cf`, `Db::scan_page_cf` and the
//! CF-registry load once did the opposite:
//!
//! ```text
//! let seq = self.engine.snapshot_seq();          // horizon first
//! collect_range(&self.engine, .., seq)?          // sources second
//! ```
//!
//! Between those two statements a compaction that no snapshot pins is
//! free to drop the newest version at or below `seq`, and the scan then
//! finds only versions it must filter out. All five now build their
//! iterator with `new_iter_latest()`, which loads the view and samples
//! the horizon in that order; the collectors take the iterator rather
//! than a sequence, so there is no longer a way to spell the inverted
//! order at a call site.
//!
//! Every key here has exactly one writer and is only ever overwritten,
//! so "absent" and "went backwards" are both violations of the read
//! path and never of the workload.
//!
//! Scale comes from `REGOLITH_SCAN_ORDER_INSTANCES` / `REGOLITH_SCAN_ORDER_ROUNDS`; the
//! defaults are the smallest shape that reproduced on a 36-core box.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

use regolith::{ColumnFamilyHandle, Db, Options};
use tempfile::TempDir;

#[derive(Clone, Copy, Debug)]
enum Surface {
    Scan,
    ScanPage,
    ScanCf,
    ScanPageCf,
}

const WRITERS: usize = 4;
const READERS: usize = 3;

fn key_of(w: usize, i: usize) -> Vec<u8> {
    format!("w{w:03}k{i:04}").into_bytes()
}

fn value_of(stamp: u64) -> Vec<u8> {
    format!("v{stamp:016}").into_bytes()
}

fn stamp_of(v: &[u8]) -> u64 {
    std::str::from_utf8(&v[1..])
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn sample(
    db: &Db,
    cf: Option<&ColumnFamilyHandle>,
    surface: Surface,
    keys: &[Vec<u8>],
) -> Vec<Option<u64>> {
    let pairs: Vec<(Vec<u8>, Vec<u8>)> = match surface {
        Surface::Scan => db.scan(None, None).expect("scan"),
        Surface::ScanCf => db.scan_cf(cf.expect("cf"), None, None).expect("scan_cf"),
        Surface::ScanPage => {
            let mut out = Vec::new();
            let mut start: Option<Vec<u8>> = None;
            loop {
                let page = db.scan_page(start.as_deref(), None, 8).expect("scan_page");
                out.extend(page.entries);
                match page.next_start {
                    Some(n) => start = Some(n),
                    None => break,
                }
            }
            out
        }
        Surface::ScanPageCf => {
            let mut out = Vec::new();
            let mut start: Option<Vec<u8>> = None;
            loop {
                let page = db
                    .scan_page_cf(cf.expect("cf"), start.as_deref(), None, 8)
                    .expect("scan_page_cf");
                out.extend(page.entries);
                match page.next_start {
                    Some(n) => start = Some(n),
                    None => break,
                }
            }
            out
        }
    };
    keys.iter()
        .map(|k| {
            pairs
                .iter()
                .find(|(pk, _)| pk == k)
                .map(|(_, v)| stamp_of(v))
        })
        .collect()
}

fn run(
    surface: Surface,
    keys_per_writer: usize,
    versions: u64,
    min_rounds: usize,
) -> (u64, Vec<String>) {
    let dir = TempDir::new().expect("tempdir");
    let db = Arc::new(
        Db::open(dir.path(), Options::default().write_buffer_size(16 * 1024)).expect("open"),
    );
    let cf = match surface {
        Surface::ScanCf | Surface::ScanPageCf => Some(Arc::new(
            db.create_column_family("attack").expect("create cf"),
        )),
        _ => None,
    };

    let keys: Vec<Vec<u8>> = (0..WRITERS)
        .flat_map(|w| (0..keys_per_writer).map(move |i| key_of(w, i)))
        .collect();
    for k in &keys {
        match &cf {
            Some(h) => regolith::through_stalls(|| db.put_cf(h, k, &value_of(0))).expect("seed"),
            None => regolith::through_stalls(|| db.put(k, &value_of(0))).expect("seed"),
        }
    }

    let live = Arc::new(AtomicU64::new(WRITERS as u64));
    let stop = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicU64::new(0));
    let bad: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new(Barrier::new(WRITERS + READERS + 2));

    let mut handles = Vec::new();
    for w in 0..WRITERS {
        let (db, cf, live, gate) = (
            Arc::clone(&db),
            cf.clone(),
            Arc::clone(&live),
            Arc::clone(&gate),
        );
        handles.push(thread::spawn(move || {
            gate.wait();
            for v in 1..=versions {
                for i in 0..keys_per_writer {
                    match &cf {
                        Some(h) => {
                            regolith::through_stalls(|| db.put_cf(h, &key_of(w, i), &value_of(v)))
                                .expect("put_cf")
                        }
                        None => regolith::through_stalls(|| db.put(&key_of(w, i), &value_of(v)))
                            .expect("put"),
                    }
                }
            }
            live.fetch_sub(1, Ordering::AcqRel);
        }));
    }

    {
        let (db, live, gate, stop) = (
            Arc::clone(&db),
            Arc::clone(&live),
            Arc::clone(&gate),
            Arc::clone(&stop),
        );
        handles.push(thread::spawn(move || {
            gate.wait();
            let mut n = 0u64;
            loop {
                db.compact_range(None, None).wait().expect("compact_range");
                n += 1;
                if n >= 3 && live.load(Ordering::Acquire) == 0 && stop.load(Ordering::Acquire) {
                    break;
                }
            }
        }));
    }

    for r in 0..READERS {
        let (db, cf, live, gate, reads, bad, keys) = (
            Arc::clone(&db),
            cf.clone(),
            Arc::clone(&live),
            Arc::clone(&gate),
            Arc::clone(&reads),
            Arc::clone(&bad),
            keys.clone(),
        );
        handles.push(thread::spawn(move || {
            let mut seen = vec![0u64; keys.len()];
            gate.wait();
            let mut round = 0usize;
            let mut local = 0u64;
            loop {
                let got = sample(&db, cf.as_deref(), surface, &keys);
                for (idx, obs) in got.iter().enumerate() {
                    match obs {
                        None => bad.lock().expect("lock").push(format!(
                            "{surface:?} reader {r} round {round}: {} read back ABSENT (last seen stamp {})",
                            String::from_utf8_lossy(&keys[idx]),
                            seen[idx],
                        )),
                        Some(s) => {
                            if *s < seen[idx] {
                                bad.lock().expect("lock").push(format!(
                                    "{surface:?} reader {r} round {round}: {} went BACKWARDS {} -> {s}",
                                    String::from_utf8_lossy(&keys[idx]),
                                    seen[idx],
                                ));
                            }
                            seen[idx] = seen[idx].max(*s);
                        }
                    }
                    local += 1;
                }
                round += 1;
                if round >= min_rounds && live.load(Ordering::Acquire) == 0 {
                    break;
                }
            }
            reads.fetch_add(local, Ordering::Relaxed);
        }));
    }

    gate.wait();
    for h in handles.drain(..) {
        if !h.is_finished() {
            stop.store(true, Ordering::Release);
        }
        h.join().expect("worker panicked");
    }
    let recorded = std::mem::take(&mut *bad.lock().expect("lock"));
    (reads.load(Ordering::Relaxed), recorded)
}

fn drive(surface: Surface) {
    // Sized for the smallest machine that runs the gate, not the
    // largest. Each instance runs four writers, three readers and a
    // compactor, so the defaults below already put 32 threads in flight;
    // the previous 16 instances x 2000 versions needed 128 concurrent
    // threads and did not finish inside five minutes on a two-core CI
    // runner, while finishing in seconds on a 32-core desktop. What this
    // hunts is the ordering between loading the view and sampling the
    // horizon, and that window is per read, so the number of reads is
    // what matters rather than the number of databases: these defaults
    // still drive over a million of them.
    let instances = env("REGOLITH_SCAN_ORDER_INSTANCES", 4);
    let rounds = env("REGOLITH_SCAN_ORDER_ROUNDS", 2);
    let keys = env("REGOLITH_SCAN_ORDER_KEYS", 16);
    let versions = env("REGOLITH_SCAN_ORDER_VERSIONS", 500) as u64;
    let min_rounds = env("REGOLITH_SCAN_ORDER_MIN_ROUNDS", 40);

    let mut reads = 0u64;
    let mut all = Vec::new();
    let mut dirty = 0usize;
    let mut total = 0usize;
    for _ in 0..rounds {
        let outs: Vec<(u64, Vec<String>)> = thread::scope(|s| {
            let hs: Vec<_> = (0..instances)
                .map(|_| s.spawn(move || run(surface, keys, versions, min_rounds)))
                .collect();
            hs.into_iter()
                .map(|h| h.join().expect("instance"))
                .collect()
        });
        for (n, v) in outs {
            total += 1;
            reads += n;
            if !v.is_empty() {
                dirty += 1;
                all.extend(v);
            }
        }
    }
    println!(
        "{surface:?}: {reads} reads over {total} instances, {dirty} dirty, {} violation(s)",
        all.len()
    );
    assert!(
        all.is_empty(),
        "{surface:?}: {} violation(s) over {reads} reads in {dirty}/{total} instances:\n  {}",
        all.len(),
        all.iter()
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n  "),
    );
}

#[test]
fn db_scan_never_travels_backwards() {
    drive(Surface::Scan);
}

#[test]
fn db_scan_page_never_travels_backwards() {
    drive(Surface::ScanPage);
}

#[test]
fn db_scan_cf_never_travels_backwards() {
    drive(Surface::ScanCf);
}

#[test]
fn db_scan_page_cf_never_travels_backwards() {
    drive(Surface::ScanPageCf);
}
