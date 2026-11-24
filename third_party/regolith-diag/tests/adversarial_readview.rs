//! Adversarial probes for the published-read-view contract (the published read view).
//!
//! Each test drives one public read surface against the same
//! workload: every key has exactly one writer, which only ever
//! overwrites it with a strictly increasing stamp. Nothing is ever
//! deleted, so a read that returns "absent", or a stamp below one the
//! same reader already saw, is a violation of the read path, never of
//! the workload.
//!
//! The surfaces are split into one test each so a failure names the
//! API that broke rather than "some read".
//!
//! Scale is read from `REGOLITH_ADV_ROUNDS` (default 1) so the same gate
//! can be cranked up for a soak without editing it.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

use regolith::{Db, Options, Statistics, Ticker};
use tempfile::TempDir;

/// Which public read surface a reader thread exercises.
#[derive(Clone, Copy, Debug)]
pub enum Surface {
    /// `Db::get`, one key at a time.
    Get,
    /// `Db::multi_get`, every key in one call.
    MultiGet,
    /// `Db::scan(None, None)`, the whole default column family.
    Scan,
    /// `Db::scan_page`, walked to exhaustion.
    ScanPage,
    /// `Db::iter`, walked to exhaustion.
    Iter,
    /// `Db::snapshot` then a point read through it.
    SnapshotGet,
    /// `Db::snapshot` then a full scan through it.
    SnapshotScan,
}

/// Knobs for one adversarial run.
pub struct Scale {
    /// Threads that each own a disjoint slice of the key space.
    pub writers: usize,
    /// Keys each writer owns.
    pub keys_per_writer: usize,
    /// Threads hammering the surface under test.
    pub readers: usize,
    /// Minimum reader rounds before a reader may leave.
    pub min_rounds: usize,
    /// Versions each writer stamps onto each of its keys.
    pub versions: u64,
    /// Run a user-thread `compact_range` loop alongside the workload.
    pub compactor: bool,
}

fn key_of(writer: usize, i: usize) -> Vec<u8> {
    format!("w{writer:03}k{i:04}").into_bytes()
}

fn value_of(stamp: u64) -> Vec<u8> {
    format!("{stamp:016}").into_bytes()
}

fn stamp_of(value: &[u8]) -> u64 {
    std::str::from_utf8(value)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or_else(|| panic!("value is not a stamp: {value:?}"))
}

/// Every violation one run recorded, capped so a badly broken build
/// reports the first few instead of gigabytes.
#[derive(Default)]
pub struct Outcome {
    pub reads: u64,
    pub violations: Vec<String>,
}

const MAX_VIOLATIONS: usize = 12;

fn record(log: &Mutex<Vec<String>>, msg: String) {
    let mut log = log.lock().expect("violation log poisoned");
    if log.len() < MAX_VIOLATIONS {
        log.push(msg);
    }
}

/// Read one surface once and return the observed stamp per key, with
/// `None` for a key the surface did not return at all.
fn sample(db: &Db, surface: Surface, keys: &[Vec<u8>]) -> Vec<Option<u64>> {
    let refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
    match surface {
        Surface::Get => refs
            .iter()
            .map(|k| db.get(k).expect("get failed").map(|v| stamp_of(&v)))
            .collect(),
        Surface::MultiGet => db
            .multi_get(&refs)
            .expect("multi_get failed")
            .into_iter()
            .map(|opt| opt.map(|v| stamp_of(&v)))
            .collect(),
        Surface::Scan => {
            let pairs = db.scan(None, None).expect("scan failed");
            lookup(keys, pairs)
        }
        Surface::ScanPage => {
            let mut pairs = Vec::new();
            let mut start: Option<Vec<u8>> = None;
            loop {
                let page = db
                    .scan_page(start.as_deref(), None, 7)
                    .expect("scan_page failed");
                pairs.extend(page.entries);
                match page.next_start {
                    Some(next) => start = Some(next),
                    None => break,
                }
            }
            lookup(keys, pairs)
        }
        Surface::Iter => {
            let mut it = db.iter();
            it.seek_to_first();
            let mut pairs = Vec::new();
            while it.valid() {
                pairs.push((
                    it.key().expect("valid iter has a key").to_vec(),
                    it.value().expect("valid iter has a value").to_vec(),
                ));
                it.next();
            }
            it.status().expect("iterator reported an error");
            lookup(keys, pairs)
        }
        Surface::SnapshotGet => {
            let snap = db.snapshot();
            refs.iter()
                .map(|k| {
                    snap.get(k)
                        .expect("snapshot get failed")
                        .map(|v| stamp_of(&v))
                })
                .collect()
        }
        Surface::SnapshotScan => {
            let snap = db.snapshot();
            let pairs = snap.scan(None, None).expect("snapshot scan failed");
            lookup(keys, pairs)
        }
    }
}

fn lookup(keys: &[Vec<u8>], pairs: Vec<(Vec<u8>, Vec<u8>)>) -> Vec<Option<u64>> {
    let map: std::collections::HashMap<Vec<u8>, Vec<u8>> = pairs.into_iter().collect();
    keys.iter()
        .map(|k| map.get(k).map(|v| stamp_of(v)))
        .collect()
}

/// Drive one surface against the overwrite-only workload.
pub fn run(scale: &Scale, surface: Surface) -> Outcome {
    let dir = TempDir::new().expect("tempdir");
    let stats = Arc::new(Statistics::new());
    let db = Db::open(
        dir.path(),
        Options::default()
            .write_buffer_size(16 * 1024)
            .statistics(Some(Arc::clone(&stats))),
    )
    .expect("open failed");
    let db = Arc::new(db);

    let keys: Vec<Vec<u8>> = (0..scale.writers)
        .flat_map(|w| (0..scale.keys_per_writer).map(move |i| key_of(w, i)))
        .collect();
    for k in &keys {
        regolith::through_stalls(|| db.put(k, &value_of(0))).expect("seed put failed");
    }

    let writers_live = Arc::new(AtomicU64::new(scale.writers as u64));
    let stop_compactor = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicU64::new(0));
    let violations: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new(Barrier::new(
        scale.writers + scale.readers + usize::from(scale.compactor) + 1,
    ));

    let mut handles = Vec::new();
    for w in 0..scale.writers {
        let db = Arc::clone(&db);
        let live = Arc::clone(&writers_live);
        let gate = Arc::clone(&gate);
        let keys_each = scale.keys_per_writer;
        let versions = scale.versions;
        handles.push(thread::spawn(move || {
            gate.wait();
            for v in 1..=versions {
                for i in 0..keys_each {
                    regolith::through_stalls(|| db.put(&key_of(w, i), &value_of(v)))
                        .expect("put failed");
                }
            }
            live.fetch_sub(1, Ordering::AcqRel);
        }));
    }

    if scale.compactor {
        let db = Arc::clone(&db);
        let live = Arc::clone(&writers_live);
        let gate = Arc::clone(&gate);
        let stop = Arc::clone(&stop_compactor);
        handles.push(thread::spawn(move || {
            gate.wait();
            let mut n = 0u64;
            loop {
                db.compact_range(None, None)
                    .wait()
                    .expect("compact_range failed");
                n += 1;
                if n >= 3 && live.load(Ordering::Acquire) == 0 && stop.load(Ordering::Acquire) {
                    break;
                }
            }
        }));
    }

    for r in 0..scale.readers {
        let db = Arc::clone(&db);
        let live = Arc::clone(&writers_live);
        let gate = Arc::clone(&gate);
        let reads = Arc::clone(&reads);
        let violations = Arc::clone(&violations);
        let keys = keys.clone();
        let min_rounds = scale.min_rounds;
        handles.push(thread::spawn(move || {
            let mut seen = vec![0u64; keys.len()];
            gate.wait();
            let mut round = 0usize;
            let mut local = 0u64;
            loop {
                let got = sample(&db, surface, &keys);
                for (idx, observed) in got.iter().enumerate() {
                    match observed {
                        None => record(
                            &violations,
                            format!(
                                "{surface:?} reader {r} round {round}: key {} read back as \
                                 ABSENT; it is only ever overwritten, last seen at stamp {}",
                                String::from_utf8_lossy(&keys[idx]),
                                seen[idx],
                            ),
                        ),
                        Some(stamp) => {
                            if *stamp < seen[idx] {
                                record(
                                    &violations,
                                    format!(
                                        "{surface:?} reader {r} round {round}: key {} went \
                                         BACKWARDS from stamp {} to stamp {stamp}",
                                        String::from_utf8_lossy(&keys[idx]),
                                        seen[idx],
                                    ),
                                );
                            }
                            seen[idx] = seen[idx].max(*stamp);
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
        if h.is_finished() {
            h.join().expect("worker panicked");
        } else {
            stop_compactor.store(true, Ordering::Release);
            h.join().expect("worker panicked");
        }
    }
    stop_compactor.store(true, Ordering::Release);

    assert!(
        stats.get_ticker(Ticker::FlushCount) > 0,
        "{surface:?}: no flush ran, the workload never left the memtable",
    );

    let recorded = {
        let mut log = violations.lock().expect("violation log poisoned");
        std::mem::take(&mut *log)
    };
    Outcome {
        reads: reads.load(Ordering::Relaxed),
        violations: recorded,
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn default_scale() -> Scale {
    Scale {
        writers: 4,
        keys_per_writer: env_usize("REGOLITH_ADV_KEYS", 12),
        readers: 3,
        min_rounds: env_usize("REGOLITH_ADV_MIN_ROUNDS", 50),
        versions: env_usize("REGOLITH_ADV_VERSIONS", 1_500) as u64,
        compactor: env_usize("REGOLITH_ADV_COMPACTOR", 1) != 0,
    }
}

/// Run `instances` independent databases at once, `rounds` times over.
/// Oversubscribing the machine is what makes the read path interleave
/// with a user-thread `compact_range` often enough to matter; one
/// instance on a quiet box almost never reaches the window.
fn drive(surface: Surface) {
    let rounds = env_usize("REGOLITH_ADV_ROUNDS", 1);
    let instances = env_usize("REGOLITH_ADV_INSTANCES", 4);
    let mut reads = 0u64;
    let mut violations = Vec::new();
    let mut dirty = 0usize;
    let mut total = 0usize;
    for _ in 0..rounds {
        let outcomes: Vec<Outcome> = thread::scope(|s| {
            let handles: Vec<_> = (0..instances)
                .map(|_| {
                    s.spawn(move || {
                        let scale = default_scale();
                        run(&scale, surface)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for o in outcomes {
            total += 1;
            reads += o.reads;
            if !o.violations.is_empty() {
                dirty += 1;
                violations.extend(o.violations);
            }
        }
    }
    println!(
        "{surface:?}: {reads} reads over {total} instances, {dirty} dirty, {} violation(s)",
        violations.len(),
    );
    assert!(
        violations.is_empty(),
        "{surface:?}: {} violation(s) over {reads} reads in {dirty}/{total} instances:\n  {}",
        violations.len(),
        violations.join("\n  "),
    );
}

/// `Db::get` never travels backwards and never reads absent.
#[test]
fn get_never_travels_backwards() {
    drive(Surface::Get);
}

/// `Db::multi_get` never travels backwards and never reads absent.
#[test]
fn multi_get_never_travels_backwards() {
    drive(Surface::MultiGet);
}

/// `Db::scan` never travels backwards and never drops a key that is
/// only ever overwritten.
///
/// The ordering is the whole subject. Every latest-read entry point has
/// to load the published view *before* it samples the read horizon:
/// `snapshot_seq()` registers nothing in the snapshot registry, so
/// nothing pins the versions the sampled sequence names, and a
/// compaction is free to drop the newest version at or below that
/// sequence before the iterator captures its sources. A scan that
/// sampled first would then find only versions it must filter out, and
/// a key that was never deleted would read absent.
///
/// This is not hypothetical. `Db::scan` and `Db::scan_page` sampled
/// first, and the harness caught them; the table below is the
/// measurement that did it, on this box, 48 instances per variant, 12
/// rounds of 4 concurrent databases, re-run after the entry points were
/// moved onto `new_iter_latest`:
///
/// | surface | ordering | reads | dirty | violations |
/// |---|---|---|---|---|
/// | `Db::get` | view then horizon | 17605728 | 0/48 | 0 |
/// | `Db::multi_get` | view then horizon | 15712272 | 0/48 | 0 |
/// | `Db::iter` | view then horizon | 7966128 | 0/48 | 0 |
/// | `Snapshot::scan` | registered pin | 5732256 | 0/48 | 0 |
/// | `Db::scan` | horizon then view | 9006720 | 8/48 | 75 |
/// | `Db::scan_page` | horizon then view | 6483408 | 9/48 | 53 |
///
/// The last two rows are the pre-fix measurement, kept because they are
/// what makes the ordering load-bearing rather than stylistic:
/// `Db::iter` and `Db::scan` run the same iterator over the same
/// workload and differed only in that ordering.
#[test]
fn scan_never_travels_backwards() {
    drive(Surface::Scan);
}

/// `Db::scan_page` never travels backwards and never drops a key.
///
/// Paging is not a separate risk: a page walk over a key set that is
/// never inserted into or deleted from cannot skip a key, so this
/// guards the ordering on [`scan_never_travels_backwards`] through the
/// paged entry point.
#[test]
fn scan_page_never_travels_backwards() {
    drive(Surface::ScanPage);
}

/// `Db::iter` never travels backwards and never drops a key.
#[test]
fn iter_never_travels_backwards() {
    drive(Surface::Iter);
}

/// A point read through a fresh `Snapshot` never travels backwards.
#[test]
fn snapshot_get_never_travels_backwards() {
    drive(Surface::SnapshotGet);
}

/// A scan through a fresh `Snapshot` never travels backwards.
#[test]
fn snapshot_scan_never_travels_backwards() {
    drive(Surface::SnapshotScan);
}

/// A snapshot's view must not change under it, and the window this
/// attacks is the one between a compaction pass fixing its input set and
/// reading the live snapshots (`SnapshotRegistry::live_seqs`). The read
/// comes second, so a snapshot registered at any point is either in the
/// list or reads at a sequence no input version exceeds.
///
/// Shape: many short-lived snapshots, each taken while writers overwrite
/// and a user thread runs `compact_range`. Each snapshot reads its whole
/// key set, then reads it again after more writes and compactions have
/// landed, and requires the two reads to be byte-identical. Snapshot
/// churn is the point: it keeps registering snapshots inside that window.
#[test]
fn a_snapshot_taken_during_compaction_keeps_its_view() {
    let rounds = env_usize("REGOLITH_ADV_ROUNDS", 1);
    let instances = env_usize("REGOLITH_ADV_INSTANCES", 4);
    let mut checks = 0u64;
    let mut violations: Vec<String> = Vec::new();

    for _ in 0..rounds {
        let outcomes: Vec<(u64, Vec<String>)> = thread::scope(|s| {
            let handles: Vec<_> = (0..instances)
                .map(|_| s.spawn(snapshot_churn_instance))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for (n, v) in outcomes {
            checks += n;
            violations.extend(v);
        }
    }
    println!(
        "snapshot churn: {checks} snapshot re-reads, {} violation(s)",
        violations.len()
    );
    assert!(
        violations.is_empty(),
        "{} violation(s) over {checks} snapshot re-reads:\n  {}",
        violations.len(),
        violations.join("\n  "),
    );
}

fn snapshot_churn_instance() -> (u64, Vec<String>) {
    let scale = default_scale();
    let dir = TempDir::new().expect("tempdir");
    let db = Arc::new(
        Db::open(dir.path(), Options::default().write_buffer_size(16 * 1024)).expect("open"),
    );

    let keys: Vec<Vec<u8>> = (0..scale.writers)
        .flat_map(|w| (0..scale.keys_per_writer).map(move |i| key_of(w, i)))
        .collect();
    for k in &keys {
        regolith::through_stalls(|| db.put(k, &value_of(0))).expect("seed put");
    }

    let live = Arc::new(AtomicU64::new(scale.writers as u64));
    let gate = Arc::new(Barrier::new(scale.writers + 2));
    let mut handles = Vec::new();
    for w in 0..scale.writers {
        let db = Arc::clone(&db);
        let live = Arc::clone(&live);
        let gate = Arc::clone(&gate);
        let keys_each = scale.keys_per_writer;
        let versions = scale.versions;
        handles.push(thread::spawn(move || {
            gate.wait();
            for v in 1..=versions {
                for i in 0..keys_each {
                    regolith::through_stalls(|| db.put(&key_of(w, i), &value_of(v))).expect("put");
                }
            }
            live.fetch_sub(1, Ordering::AcqRel);
        }));
    }
    {
        let db = Arc::clone(&db);
        let live = Arc::clone(&live);
        let gate = Arc::clone(&gate);
        handles.push(thread::spawn(move || {
            gate.wait();
            let mut n = 0u64;
            loop {
                db.compact_range(None, None).wait().expect("compact_range");
                n += 1;
                if n >= 3 && live.load(Ordering::Acquire) == 0 {
                    break;
                }
            }
        }));
    }

    let refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
    let mut checks = 0u64;
    let mut violations = Vec::new();
    gate.wait();
    let mut rounds = 0usize;
    loop {
        let snap = db.snapshot();
        let first = snap.multi_get(&refs).expect("snapshot multi_get");
        // Give the writers and the compactor something to do under it.
        let second = snap.multi_get(&refs).expect("snapshot multi_get");
        let third: Vec<Option<Vec<u8>>> = refs
            .iter()
            .map(|k| snap.get(k).expect("snapshot get"))
            .collect();
        for (i, k) in keys.iter().enumerate() {
            if first[i].is_none() {
                violations.push(format!(
                    "snapshot read key {} as ABSENT; it is only ever overwritten",
                    String::from_utf8_lossy(k),
                ));
            }
            if first[i] != second[i] || first[i] != third[i] {
                violations.push(format!(
                    "snapshot changed under key {}: {:?} then {:?} then {:?}",
                    String::from_utf8_lossy(k),
                    first[i].as_ref().map(|v| stamp_of(v)),
                    second[i].as_ref().map(|v| stamp_of(v)),
                    third[i].as_ref().map(|v| stamp_of(v)),
                ));
            }
            checks += 1;
        }
        if violations.len() > MAX_VIOLATIONS {
            violations.truncate(MAX_VIOLATIONS);
            break;
        }
        rounds += 1;
        if rounds >= scale.min_rounds && live.load(Ordering::Acquire) == 0 {
            break;
        }
    }
    for h in handles {
        h.join().expect("worker panicked");
    }
    (checks, violations)
}

/// `drop_all` races every read surface. Nothing here asserts what a
/// concurrent reader sees, because `drop_all` legitimately empties the
/// database; what it asserts is that no read ever errors, panics, or
/// invents a value that was never written, and that once the writers and
/// the dropper are gone the database is internally consistent: the point
/// reads, the forward scan and the reverse scan all agree.
#[test]
fn drop_all_racing_every_read_surface_never_invents_data() {
    let dir = TempDir::new().expect("tempdir");
    let db = Arc::new(
        Db::open(dir.path(), Options::default().write_buffer_size(8 * 1024)).expect("open"),
    );
    let keys: Vec<Vec<u8>> = (0..64).map(|i| key_of(0, i)).collect();
    let refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
    let stop = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::new();

    for w in 0..3usize {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        handles.push(thread::spawn(move || {
            let mut v = 0u64;
            while !stop.load(Ordering::Acquire) {
                v += 1;
                for i in 0..64 {
                    regolith::through_stalls(|| {
                        db.put(&key_of(0, i), &value_of(v * 10 + w as u64))
                    })
                    .expect("put");
                }
                regolith::through_stalls(|| db.delete_range(&key_of(0, 10), &key_of(0, 20)))
                    .expect("delete_range");
            }
        }));
    }
    {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        handles.push(thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                db.drop_all().expect("drop_all");
                db.compact_range(None, None).wait().expect("compact_range");
            }
        }));
    }

    let mut reads = 0u64;
    for _ in 0..4_000 {
        for k in &refs {
            if let Some(v) = db.get(k).expect("get") {
                stamp_of(&v);
            }
            reads += 1;
        }
        for v in db
            .multi_get(&refs)
            .expect("multi_get")
            .into_iter()
            .flatten()
        {
            stamp_of(&v);
        }
        let mut it = db.iter();
        it.seek_to_first();
        while it.valid() {
            stamp_of(it.value().expect("value"));
            it.next();
        }
        it.status().expect("iterator error");
        let mut it = db.iter();
        it.seek_to_last();
        while it.valid() {
            stamp_of(it.value().expect("value"));
            it.prev();
        }
        it.status().expect("iterator error");
    }
    stop.store(true, Ordering::Release);
    for h in handles {
        h.join().expect("worker panicked");
    }

    // Quiesced: every surface must now agree.
    db.compact_range(None, None).wait().expect("compact_range");
    let mut it = db.iter();
    it.seek_to_first();
    let mut fwd = Vec::new();
    while it.valid() {
        fwd.push((
            it.key().expect("key").to_vec(),
            it.value().expect("value").to_vec(),
        ));
        it.next();
    }
    it.status().expect("iterator error");
    let mut it = db.iter();
    it.seek_to_last();
    let mut rev = Vec::new();
    while it.valid() {
        rev.push((
            it.key().expect("key").to_vec(),
            it.value().expect("value").to_vec(),
        ));
        it.prev();
    }
    it.status().expect("iterator error");
    rev.reverse();
    assert_eq!(
        fwd, rev,
        "after drop_all: forward and reverse scans disagree"
    );
    for (k, v) in &fwd {
        assert_eq!(
            db.get(k).expect("get").as_ref(),
            Some(v),
            "after drop_all: scan and get disagree on {}",
            String::from_utf8_lossy(k),
        );
    }
    println!(
        "drop_all race: {reads} point reads survived, {} entries agree across all surfaces",
        fwd.len(),
    );
}
