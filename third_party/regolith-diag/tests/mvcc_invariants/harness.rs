//! The shared workload harness for `tests/mvcc_invariants.rs`.
//!
//! Three things live here: stamped keys and values, so a reader can
//! tell *which* version it observed rather than only that it observed
//! something; the scale-parameterized runners that the fast and the
//! full-scale tests both drive, so the two can never drift apart; and
//! the guards that make a vacuous pass impossible.
//!
//! # The shape every runner shares
//!
//! The writers do a **bounded** amount of work and then leave. The
//! readers do a fixed minimum number of checks and then keep going
//! until every writer has left ([`Live`]), so a slower machine gets
//! *more* overlap, never less, and the run still terminates. Nothing
//! is asserted about a count the scheduler decides: the counts are
//! returned to the caller and only floors are checked. There is no
//! `sleep` here.
//!
//! Bounding the writers matters for more than runtime. A long-lived
//! snapshot pins every version written under it, so a reader-paced
//! writer loop feeds back on itself: more retained versions make each
//! snapshot scan slower, which lets the writers write more. Fixing the
//! writer op count breaks that loop and keeps the data volume flat.

use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

use regolith::{Db, Options, Statistics, Ticker, WriteBatch};
use tempfile::TempDir;

// ── shared scaffolding ─────────────────────────────────────────────

/// Digits of the generation stamp carried in every value.
pub const STAMP_WIDTH: usize = 12;
/// Total value width. The padding keeps values wide enough that a
/// small write buffer really does flush and compact.
pub const VALUE_LEN: usize = 64;

pub type Entries = Vec<(Vec<u8>, Vec<u8>)>;

pub fn key_at(i: usize) -> Vec<u8> {
    format!("mvcc_key_{i:06}").into_bytes()
}

pub fn mono_key(writer: usize, i: usize) -> Vec<u8> {
    format!("mono_{writer}_{i:04}").into_bytes()
}

/// A value that carries its generation in its own bytes, so a reader
/// can tell *which* version it observed rather than only that it
/// observed something.
pub fn stamped_value(stamp: u64) -> Vec<u8> {
    let mut v = format!("v{stamp:0width$}", width = STAMP_WIDTH).into_bytes();
    v.resize(VALUE_LEN, b'.');
    v
}

pub fn stamp_of(value: &[u8]) -> u64 {
    assert_eq!(
        value.len(),
        VALUE_LEN,
        "value {:?} is not a stamped value",
        String::from_utf8_lossy(value),
    );
    let text = std::str::from_utf8(&value[1..=STAMP_WIDTH])
        .expect("stamp field is not valid utf-8, the value bytes were corrupted");
    text.parse()
        .expect("stamp field is not a number, the value bytes were corrupted")
}

/// Deterministic LCG step. Seeded per thread from a fixed constant so
/// a workload replays identically.
pub fn next_rand(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state >> 11
}

pub fn writer_seed(t: usize) -> u64 {
    0x5EED_0000_0000_0001u64 ^ (t as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// Shared "how many writers are still running" counter. A reader loop
/// runs its minimum and then until this reaches zero, which is what
/// makes the run terminate without any timing assumption.
#[derive(Clone)]
pub struct Live(Arc<AtomicUsize>);

impl Live {
    pub fn new(count: usize) -> Self {
        Self(Arc::new(AtomicUsize::new(count)))
    }

    pub fn done_one(&self) {
        self.0.fetch_sub(1, Ordering::Release);
    }

    pub fn any(&self) -> bool {
        self.0.load(Ordering::Acquire) > 0
    }
}

pub fn instrumented(write_buffer_size: usize) -> (Options, Arc<Statistics>) {
    let stats = Arc::new(Statistics::new());
    let opts = Options::default()
        .write_buffer_size(write_buffer_size)
        .statistics(Some(Arc::clone(&stats)));
    (opts, stats)
}

pub fn open_instrumented(path: &Path, write_buffer_size: usize) -> (Db, Arc<Statistics>) {
    let (opts, stats) = instrumented(write_buffer_size);
    (Db::open(path, opts).expect("open failed"), stats)
}

/// Fail loudly if the workload never reached the background paths the
/// test claims to cover. Without this a future tuning change could let
/// every test here pass without a single flush or compaction.
pub fn assert_background_work_happened(stats: &Statistics, what: &str) {
    assert!(
        stats.get_ticker(Ticker::FlushCount) > 0,
        "{what}: no memtable flush ran, the workload never left the memtable",
    );
    assert!(
        stats.get_ticker(Ticker::CompactionCount) > 0,
        "{what}: no compaction ran, the workload never exercised the compaction path",
    );
}

/// Compare two materialized views and report the *first* divergence,
/// so a failure names one key instead of dumping thousands.
pub fn assert_same_view(baseline: &Entries, view: &Entries, ctx: &str) {
    for (i, (want, got)) in baseline.iter().zip(view.iter()).enumerate() {
        assert_eq!(
            want.0,
            got.0,
            "{ctx}: entry {i} changed key from {:?} to {:?}",
            String::from_utf8_lossy(&want.0),
            String::from_utf8_lossy(&got.0),
        );
        assert_eq!(
            want.1,
            got.1,
            "{ctx}: key {:?} changed value from stamp {} to stamp {}",
            String::from_utf8_lossy(&want.0),
            stamp_of(&want.1),
            stamp_of(&got.1),
        );
    }
    assert_eq!(
        baseline.len(),
        view.len(),
        "{ctx}: view changed length; the two views agree on their common prefix",
    );
}

/// Walk a fresh iterator to exhaustion and materialize it.
pub fn drain_iter(db: &Db) -> Entries {
    let mut it = db.iter();
    it.seek_to_first();
    let mut out = Entries::new();
    while it.valid() {
        out.push((
            it.key().expect("valid iterator has a key").to_vec(),
            it.value().expect("valid iterator has a value").to_vec(),
        ));
        it.next();
    }
    it.status().expect("iterator reported an error");
    out
}

// ── 1. snapshot stability ──────────────────────────────────────────

pub struct StabilityScale {
    pub keys: usize,
    pub writers: usize,
    pub ops_per_writer: usize,
    pub min_reads: usize,
    pub min_compactions: usize,
}

/// Measured counts from one stability run, so the test reports real
/// numbers instead of guessed ones.
pub struct StabilityCounts {
    pub snapshot_reads: u64,
    pub entries_compared: u64,
    pub writer_ops: u64,
    pub compactions: u64,
}

pub fn run_snapshot_stability(scale: &StabilityScale) -> StabilityCounts {
    let dir = TempDir::new().unwrap();
    let (db, stats) = open_instrumented(dir.path(), 16 * 1024);
    let db = Arc::new(db);

    for i in 0..scale.keys {
        regolith::through_stalls(|| db.put(&key_at(i), &stamped_value(0))).unwrap();
    }
    db.compact_range(None, None).wait().unwrap();

    let snap = db.snapshot();
    let baseline = snap.scan(None, None).unwrap();
    assert_eq!(
        baseline.len(),
        scale.keys,
        "the snapshot did not observe the seed data it was taken over",
    );

    let live = Live::new(scale.writers);
    let compactions = Arc::new(AtomicU64::new(0));
    // Writers, compactor and the reading main thread leave the gate
    // together; the reader then runs until the last writer is gone.
    let gate = Arc::new(Barrier::new(scale.writers + 2));

    let mut workers = Vec::new();
    for t in 0..scale.writers {
        let db = Arc::clone(&db);
        let live = live.clone();
        let gate = Arc::clone(&gate);
        let keys = scale.keys;
        let ops = scale.ops_per_writer;
        workers.push(thread::spawn(move || {
            let mut seed = writer_seed(t);
            gate.wait();
            for n in 1..=ops as u64 {
                let k = key_at(next_rand(&mut seed) as usize % keys);
                if next_rand(&mut seed).is_multiple_of(4) {
                    regolith::through_stalls(|| db.delete(&k)).unwrap();
                } else {
                    regolith::through_stalls(|| db.put(&k, &stamped_value(n))).unwrap();
                }
            }
            live.done_one();
        }));
    }

    let compactor = {
        let db = Arc::clone(&db);
        let live = live.clone();
        let gate = Arc::clone(&gate);
        let compactions = Arc::clone(&compactions);
        let min_compactions = scale.min_compactions;
        thread::spawn(move || {
            gate.wait();
            let mut n = 0u64;
            loop {
                db.compact_range(None, None).wait().unwrap();
                n += 1;
                if n as usize >= min_compactions && !live.any() {
                    break;
                }
            }
            compactions.store(n, Ordering::Relaxed);
        })
    };

    gate.wait();
    let mut entries_compared = 0u64;
    let mut reads = 0u64;
    loop {
        let view = snap.scan(None, None).unwrap();
        assert_same_view(&baseline, &view, &format!("snapshot scan round {reads}"));
        entries_compared += view.len() as u64;
        for i in [0, scale.keys / 2, scale.keys - 1] {
            assert_eq!(
                snap.get(&key_at(i)).unwrap().as_deref(),
                Some(baseline[i].1.as_slice()),
                "snapshot point read of key {i} disagreed with its own scan at round {reads}",
            );
        }
        reads += 1;
        if reads as usize >= scale.min_reads && !live.any() {
            break;
        }
    }

    // Collect every join result before unwrapping any of them: a worker
    // still running while the `TempDir` is dropped would fail with a
    // confusing secondary error and bury the real assertion.
    let outcomes: Vec<_> = workers
        .into_iter()
        .chain(std::iter::once(compactor))
        .map(|h| h.join())
        .collect();
    for o in outcomes {
        o.unwrap();
    }

    // The view must still hold after every writer and compactor has
    // finished, not only while they were racing.
    assert_same_view(&baseline, &snap.scan(None, None).unwrap(), "post-join scan");

    // Guard against a vacuous pass: the writers must actually have
    // changed the live database out from under the snapshot.
    assert!(
        db.scan(None, None).unwrap() != baseline,
        "the writers never changed the live view, so the snapshot proved nothing",
    );
    assert_background_work_happened(&stats, "snapshot stability");

    StabilityCounts {
        snapshot_reads: reads,
        entries_compared,
        writer_ops: (scale.writers * scale.ops_per_writer) as u64,
        compactions: compactions.load(Ordering::Relaxed),
    }
}

// ── 2. WriteBatch atomicity under concurrent readers ───────────────

pub struct AtomicityScale {
    pub width: usize,
    pub readers: usize,
    pub min_checks_per_reader: usize,
    pub generations: u64,
}

pub fn assert_uniform_generation(keys: &[Vec<u8>], got: &[Option<Vec<u8>>], ctx: &str) {
    assert_eq!(
        keys.len(),
        got.len(),
        "{ctx}: read {} values for {} batch keys",
        got.len(),
        keys.len(),
    );
    let first = got[0].as_deref().unwrap_or_else(|| {
        panic!(
            "{ctx}: key {:?} vanished",
            String::from_utf8_lossy(&keys[0])
        )
    });
    let want = stamp_of(first);
    for (k, v) in keys.iter().zip(got.iter()) {
        let v = v.as_deref().unwrap_or_else(|| {
            panic!(
                "{ctx}: key {:?} vanished while the rest of its batch was at generation {want}",
                String::from_utf8_lossy(k),
            )
        });
        assert_eq!(
            stamp_of(v),
            want,
            "{ctx}: torn batch - key {:?} is at generation {} while key {:?} is at generation \
             {want}",
            String::from_utf8_lossy(k),
            stamp_of(v),
            String::from_utf8_lossy(&keys[0]),
        );
    }
}

pub fn run_batch_atomicity(scale: &AtomicityScale) -> (u64, u64) {
    let dir = TempDir::new().unwrap();
    let (db, stats) = open_instrumented(dir.path(), 16 * 1024);
    let db = Arc::new(db);

    let batch_keys: Vec<Vec<u8>> = (0..scale.width)
        .map(|i| format!("atomic_{i:03}").into_bytes())
        .collect();

    // Generation 0 so a reader always finds the key set present and
    // asserts on uniformity rather than on presence.
    let mut seed_batch = WriteBatch::new();
    for k in &batch_keys {
        seed_batch.put(k, &stamped_value(0));
    }
    regolith::through_stalls(|| db.write(seed_batch.clone())).unwrap();

    let live = Live::new(1);
    let checks = Arc::new(AtomicU64::new(0));
    let gate = Arc::new(Barrier::new(scale.readers + 2));

    let writer = {
        let db = Arc::clone(&db);
        let live = live.clone();
        let gate = Arc::clone(&gate);
        let batch_keys = batch_keys.clone();
        let generations = scale.generations;
        thread::spawn(move || {
            gate.wait();
            for generation in 1..=generations {
                let mut batch = WriteBatch::new();
                for k in &batch_keys {
                    batch.put(k, &stamped_value(generation));
                }
                regolith::through_stalls(|| db.write(batch.clone())).unwrap();
            }
            live.done_one();
        })
    };

    let compactor = {
        let db = Arc::clone(&db);
        let live = live.clone();
        let gate = Arc::clone(&gate);
        thread::spawn(move || {
            gate.wait();
            let mut n = 0u64;
            loop {
                db.compact_range(None, None).wait().unwrap();
                n += 1;
                if n >= 2 && !live.any() {
                    break;
                }
            }
        })
    };

    let mut readers = Vec::new();
    for r in 0..scale.readers {
        let db = Arc::clone(&db);
        let live = live.clone();
        let gate = Arc::clone(&gate);
        let checks = Arc::clone(&checks);
        let batch_keys = batch_keys.clone();
        let min_rounds = scale.min_checks_per_reader;
        readers.push(thread::spawn(move || {
            let refs: Vec<&[u8]> = batch_keys.iter().map(|k| k.as_slice()).collect();
            gate.wait();
            let mut round = 0usize;
            let mut local = 0u64;
            loop {
                // Three independent consistent-read surfaces, because a
                // torn batch could hide in any one of them alone.
                let got = db.multi_get(&refs).unwrap();
                assert_uniform_generation(&batch_keys, &got, &format!("multi_get r{r} #{round}"));
                local += 1;

                let snap = db.snapshot();
                let got: Vec<Option<Vec<u8>>> =
                    batch_keys.iter().map(|k| snap.get(k).unwrap()).collect();
                assert_uniform_generation(&batch_keys, &got, &format!("snapshot r{r} #{round}"));
                local += 1;

                let scanned = snap.scan(Some(b"atomic_"), Some(b"atomic`")).unwrap();
                let as_opts: Vec<Option<Vec<u8>>> =
                    scanned.into_iter().map(|e| Some(e.1)).collect();
                assert_uniform_generation(&batch_keys, &as_opts, &format!("scan r{r} #{round}"));
                local += 1;

                round += 1;
                if round >= min_rounds && !live.any() {
                    break;
                }
            }
            checks.fetch_add(local, Ordering::Relaxed);
        }));
    }

    let outcomes: Vec<_> = readers
        .into_iter()
        .chain([writer, compactor])
        .map(|h| h.join())
        .collect();
    for o in outcomes {
        o.unwrap();
    }

    assert_background_work_happened(&stats, "batch atomicity");
    (checks.load(Ordering::Relaxed), scale.generations)
}

// ── 3. monotonic reads ─────────────────────────────────────────────

pub struct MonotonicScale {
    pub writers: usize,
    pub keys_per_writer: usize,
    pub readers: usize,
    pub min_rounds_per_reader: usize,
    pub versions: u64,
}

/// Everything one monotonic run observed. Violations are collected
/// rather than panicked on inside the reader threads, so a run reports
/// *every* regression it saw instead of only the first, and so the
/// same runner can be driven many times over to measure a rate.
pub struct MonotonicOutcome {
    pub reads: u64,
    pub writes: u64,
    pub violations: Vec<String>,
}

impl MonotonicOutcome {
    /// Panic with every violation the run recorded, if any.
    pub fn assert_clean(&self, what: &str) {
        assert!(
            self.violations.is_empty(),
            "{what}: {} monotonic-read violation(s) over {} reads:\n  {}",
            self.violations.len(),
            self.reads,
            self.violations.join("\n  "),
        );
    }
}

pub fn run_monotonic_reads(scale: &MonotonicScale) -> MonotonicOutcome {
    let dir = TempDir::new().unwrap();
    let (db, stats) = open_instrumented(dir.path(), 16 * 1024);
    let db = Arc::new(db);

    for w in 0..scale.writers {
        for i in 0..scale.keys_per_writer {
            regolith::through_stalls(|| db.put(&mono_key(w, i), &stamped_value(0))).unwrap();
        }
    }

    let live = Live::new(scale.writers);
    let reads = Arc::new(AtomicU64::new(0));
    let violations: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new(Barrier::new(scale.writers + scale.readers + 1));

    let mut workers = Vec::new();
    for w in 0..scale.writers {
        let db = Arc::clone(&db);
        let live = live.clone();
        let gate = Arc::clone(&gate);
        let keys = scale.keys_per_writer;
        let versions = scale.versions;
        workers.push(thread::spawn(move || {
            gate.wait();
            // Exactly one writer owns each key, so the stamp on a key
            // is a strictly increasing version number by construction.
            for v in 1..=versions {
                for i in 0..keys {
                    regolith::through_stalls(|| db.put(&mono_key(w, i), &stamped_value(v)))
                        .unwrap();
                }
            }
            live.done_one();
        }));
    }

    let compactor = {
        let db = Arc::clone(&db);
        let live = live.clone();
        let gate = Arc::clone(&gate);
        thread::spawn(move || {
            gate.wait();
            let mut n = 0u64;
            loop {
                db.compact_range(None, None).wait().unwrap();
                n += 1;
                if n >= 3 && !live.any() {
                    break;
                }
            }
        })
    };

    let mut readers = Vec::new();
    for r in 0..scale.readers {
        let db = Arc::clone(&db);
        let live = live.clone();
        let gate = Arc::clone(&gate);
        let reads = Arc::clone(&reads);
        let violations = Arc::clone(&violations);
        let writers = scale.writers;
        let keys = scale.keys_per_writer;
        let min_rounds = scale.min_rounds_per_reader;
        readers.push(thread::spawn(move || {
            let mut seen = vec![0u64; writers * keys];
            gate.wait();
            let mut round = 0usize;
            let mut local = 0u64;
            loop {
                for w in 0..writers {
                    for i in 0..keys {
                        let k = mono_key(w, i);
                        let slot = &mut seen[w * keys + i];
                        match db.get(&k).unwrap() {
                            None => record(
                                &violations,
                                format!(
                                    "reader {r} round {round}: key {:?} read back as absent at \
                                     version {}; it is only ever overwritten, never deleted",
                                    String::from_utf8_lossy(&k),
                                    *slot,
                                ),
                            ),
                            Some(got) => {
                                let stamp = stamp_of(&got);
                                if stamp < *slot {
                                    record(
                                        &violations,
                                        format!(
                                            "reader {r} round {round}: key {:?} went backwards \
                                             from version {} to version {stamp}",
                                            String::from_utf8_lossy(&k),
                                            *slot,
                                        ),
                                    );
                                }
                                *slot = (*slot).max(stamp);
                            }
                        }
                        local += 1;
                    }
                }
                round += 1;
                if round >= min_rounds && !live.any() {
                    break;
                }
            }
            reads.fetch_add(local, Ordering::Relaxed);
        }));
    }

    let outcomes: Vec<_> = readers
        .into_iter()
        .chain(workers)
        .chain(std::iter::once(compactor))
        .map(|h| h.join())
        .collect();
    for o in outcomes {
        o.unwrap();
    }

    assert_background_work_happened(&stats, "monotonic reads");
    let recorded = {
        let mut log = violations.lock().expect("violation log poisoned");
        std::mem::take(&mut *log)
    };
    MonotonicOutcome {
        reads: reads.load(Ordering::Relaxed),
        writes: scale.versions * (scale.writers * scale.keys_per_writer) as u64,
        violations: recorded,
    }
}

/// Cap the log so a run that goes badly wrong reports the first few
/// violations instead of gigabytes of them.
const MAX_VIOLATIONS: usize = 16;

fn record(log: &Mutex<Vec<String>>, message: String) {
    let mut log = log.lock().expect("violation log poisoned");
    if log.len() < MAX_VIOLATIONS {
        log.push(message);
    }
}

/// Drive [`run_monotonic_reads`] over `instances` independent databases
/// at once. Oversubscribing the machine is what makes the read path
/// interleave with a user-thread `compact_range` often enough to be a
/// usable regression gate; one instance on an idle box almost never
/// hits the window.
pub fn run_monotonic_reads_in_parallel(
    scale: &MonotonicScale,
    instances: usize,
) -> Vec<MonotonicOutcome> {
    thread::scope(|s| {
        let handles: Vec<_> = (0..instances)
            .map(|_| s.spawn(|| run_monotonic_reads(scale)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    })
}
