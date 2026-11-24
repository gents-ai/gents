//! Chaos probe for the published read view (the published read view), against the
//! background operations the shipped adversarial suite does not run
//! alongside its readers: column-family creation and drop, external SST
//! ingestion, checkpoint capture, and a block cache small enough that
//! every scan has to go back to the file descriptors a compaction has
//! already unlinked.
//!
//! Invariant: every key has exactly one writer and is only ever
//! overwritten, in a column family nothing else touches. A read that
//! answers "absent", or answers with a stamp below one the same reader
//! already saw, is a read-path violation.
//!
//! Every workload thread bumps its own progress counter after each
//! operation, and the coordinator fails the instance with every thread's
//! stack once any unfinished thread stops advancing, so a deadlock, or a
//! livelock where some threads spin while one waits forever, surfaces as
//! a failure instead of a harness timeout.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use regolith::{Db, IngestOptions, Options, SstFileWriter, through_stalls};
use tempfile::TempDir;

const WRITERS: usize = 4;
const KEYS_PER_WRITER: usize = 12;
/// Reader threads per instance. Overridable so a wedge can be attributed
/// to the read path or to the write path.
fn readers() -> usize {
    env("REGOLITH_CHAOS_READERS", 4) as usize
}
/// External tables ingested per instance. Bounded so the key space the
/// readers walk stays flat while the writers keep overwriting.
const INGESTS_PER_INSTANCE: u64 = 32;
/// Checkpoints per instance. Each one rotates the memtable and holds the
/// compaction lock while it copies the whole table set, so an unbounded
/// loop starves the writers instead of racing them.
const CHECKPOINTS_PER_INSTANCE: u64 = 32;
/// Foreground `compact_range` passes per instance.
///
/// Each pass rewrites the whole database, and the database grows with
/// the version count, so the product of this bound and
/// `REGOLITH_CHAOS_VERSIONS` is the run's dominant cost and it is quadratic.
/// 64 rather than 256: the race this workload hunts is between one
/// compaction and one read, so it is the number of *chances* that
/// matters, and 64 passes against four reader threads sweeping
/// continuously already gives thousands of overlaps per instance.
const COMPACT_PASSES_PER_INSTANCE: u64 = 64;

fn key_of(w: usize, i: usize) -> Vec<u8> {
    format!("w{w:03}k{i:04}").into_bytes()
}

fn value_of(stamp: u64) -> Vec<u8> {
    format!("v{stamp:016}").into_bytes()
}

fn stamp_of(v: &[u8]) -> Option<u64> {
    std::str::from_utf8(v.get(1..)?).ok()?.parse().ok()
}

fn env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// Takes its writer out of the live count when the writer ends, by a
/// return or by a panic.
struct Leaving(Arc<AtomicU64>);

impl Drop for Leaving {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// One workload thread and the counter it bumps after every operation.
struct Worker {
    role: String,
    beat: Arc<AtomicU64>,
    handle: thread::JoinHandle<()>,
}

fn worker(role: String, body: impl FnOnce(&AtomicU64) + Send + 'static) -> Worker {
    let beat = Arc::new(AtomicU64::new(0));
    let counter = Arc::clone(&beat);
    Worker {
        role,
        beat,
        handle: thread::spawn(move || body(&counter)),
    }
}

/// Every thread's stack, from whichever debugger the host has: no
/// in-process API can walk the stack of a thread other than the caller.
fn stack_dump() -> String {
    let pid = std::process::id().to_string();
    let tools: [(&str, &[&str]); 2] = [
        ("eu-stack", &["-i", "-s", "-p", &pid]),
        ("gdb", &["-batch", "-ex", "thread apply all bt", "-p", &pid]),
    ];
    for (tool, args) in tools {
        if let Ok(out) = std::process::Command::new(tool).args(args).output()
            && out.status.success()
            && !out.stdout.is_empty()
        {
            return String::from_utf8_lossy(&out.stdout).into_owned();
        }
    }
    "no stack dump: neither eu-stack nor gdb could attach to this process".to_string()
}

/// Wait for every worker, failing with a report the moment an unfinished
/// one has not advanced its counter for `limit`. A live thread that never
/// advances is the hang this workload hunts, whether everything else is
/// parked too (a deadlock) or still spinning (a livelock); a watch over
/// the readers alone misses the second, because they spin until every
/// writer is done.
fn watch(workers: &[Worker], db: &Db, limit: Duration) -> Result<(), String> {
    let mut seen: Vec<(u64, Instant)> = workers
        .iter()
        .map(|w| (w.beat.load(Ordering::Relaxed), Instant::now()))
        .collect();
    let mut backoff = Duration::from_millis(1);
    while workers.iter().any(|w| !w.handle.is_finished()) {
        thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_millis(200));
        for (w, (beat, since)) in workers.iter().zip(&mut seen) {
            let now = w.beat.load(Ordering::Relaxed);
            if now != *beat {
                *beat = now;
                *since = Instant::now();
            } else if !w.handle.is_finished() && since.elapsed() > limit {
                let threads: Vec<String> = workers
                    .iter()
                    .map(|w| {
                        let done = w.handle.is_finished();
                        format!(
                            "{} done={done} at {}",
                            w.role,
                            w.beat.load(Ordering::Relaxed)
                        )
                    })
                    .collect();
                return Err(format!(
                    "WEDGED: {} made no progress for {limit:?} at {now} operations\n\
                     L0 files {:?}, memtable bytes {:?}\n\
                     threads: {}\n{}",
                    w.role,
                    db.get_int_property("regolith.num-files-at-level0"),
                    db.get_int_property("regolith.cur-size-all-mem-tables"),
                    threads.join(", "),
                    stack_dump(),
                ));
            }
        }
    }
    Ok(())
}

/// Which read surface a reader thread hammers.
#[derive(Clone, Copy)]
enum Surface {
    Get,
    MultiGet,
    Iter,
}

fn read(db: &Db, surface: Surface, keys: &[Vec<u8>]) -> Vec<Option<u64>> {
    match surface {
        Surface::Get => keys
            .iter()
            .map(|k| db.get(k).expect("get").as_deref().and_then(stamp_of))
            .collect(),
        Surface::MultiGet => {
            let refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
            db.multi_get(&refs)
                .expect("multi_get")
                .iter()
                .map(|v| v.as_deref().and_then(stamp_of))
                .collect()
        }
        Surface::Iter => {
            let mut it = db.iter();
            it.seek_to_first();
            let mut pairs = Vec::new();
            while it.valid() {
                pairs.push((
                    it.key().expect("key").to_vec(),
                    it.value().expect("value").to_vec(),
                ));
                it.next();
            }
            it.status().expect("iter status");
            keys.iter()
                .map(|k| {
                    pairs
                        .iter()
                        .find(|(pk, _)| pk == k)
                        .and_then(|(_, v)| stamp_of(v))
                })
                .collect()
        }
    }
}

fn external_table(dir: &std::path::Path, n: u64) -> std::path::PathBuf {
    let path = dir.join(format!("ext_{n}.sst"));
    let mut w = SstFileWriter::create(&path, &Options::default()).expect("create sst");
    for i in 0..64u64 {
        w.put(
            format!("zzz_{n:04}_{i:04}").as_bytes(),
            format!("ext_{n}").as_bytes(),
        )
        .expect("sst put");
    }
    w.finish().expect("finish");
    path
}

fn run_instance(versions: u64, min_rounds: u64) -> Vec<String> {
    let dir = TempDir::new().expect("tempdir");
    let db = Arc::new(
        Db::open(
            dir.path(),
            Options::default()
                .write_buffer_size(8 * 1024)
                .block_cache_size(4 * 1024)
                .max_write_buffer_number(env("REGOLITH_CHAOS_MAX_MEMTABLES", 2) as usize)
                .level0_stop_writes_trigger(env("REGOLITH_CHAOS_L0_STOP", 36) as usize),
        )
        .expect("open"),
    );
    let ext_dir = TempDir::new().expect("tempdir");
    let cp_dir = TempDir::new().expect("tempdir");

    let keys: Vec<Vec<u8>> = (0..WRITERS)
        .flat_map(|w| (0..KEYS_PER_WRITER).map(move |i| key_of(w, i)))
        .collect();
    for k in &keys {
        through_stalls(|| db.put(k, &value_of(0))).expect("seed");
    }

    let live = Arc::new(AtomicU64::new(WRITERS as u64));
    let bad: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mask = env("REGOLITH_CHAOS_MASK", 0b1111);
    let enabled = |bit: u64| mask & bit != 0;
    let chaos_threads = [1u64, 2, 4, 8].iter().filter(|b| enabled(**b)).count();
    let gate_participants = WRITERS + readers() + chaos_threads + 1;
    let gate = Arc::new(Barrier::new(gate_participants));
    let mut workers = Vec::new();

    for w in 0..WRITERS {
        let (db, live, gate) = (Arc::clone(&db), Arc::clone(&live), Arc::clone(&gate));
        workers.push(worker(format!("writer {w}"), move |beat| {
            // Counted out however the writer ends: a writer that panicked
            // and stayed counted would keep every other thread looping, and
            // the run would hang where it should fail on the join below.
            let _leaving = Leaving(live);
            gate.wait();
            for v in 1..=versions {
                for i in 0..KEYS_PER_WRITER {
                    // A write a stall stops waits for the stall to clear, as
                    // a blocking caller does: on a host with few cores the
                    // compaction worker falls behind these writers.
                    through_stalls(|| db.put(&key_of(w, i), &value_of(v))).expect("put");
                    beat.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }

    // Chaos 1: user-thread compact_range.
    if enabled(1) {
        let (db, live, gate) = (Arc::clone(&db), Arc::clone(&live), Arc::clone(&gate));
        workers.push(worker("compact_range".to_string(), move |beat| {
            gate.wait();
            let mut n = 0u64;
            loop {
                db.compact_range(None, None).wait().expect("compact_range");
                n += 1;
                beat.fetch_add(1, Ordering::Relaxed);
                if n >= COMPACT_PASSES_PER_INSTANCE || (n >= 2 && live.load(Ordering::Acquire) == 0)
                {
                    break;
                }
            }
        }));
    }

    // Chaos 2: column families created and dropped underneath the readers.
    if enabled(2) {
        let (db, live, gate) = (Arc::clone(&db), Arc::clone(&live), Arc::clone(&gate));
        workers.push(worker("cf churn".to_string(), move |beat| {
            gate.wait();
            let mut n = 0u64;
            loop {
                let name = format!("cf_{n}");
                if let Ok(h) = db.create_column_family(&name) {
                    through_stalls(|| db.put_cf(&h, b"x", b"y")).expect("put_cf");
                    let _ = db.list_column_families();
                    through_stalls(|| db.drop_column_family(h.clone())).expect("drop_cf");
                }
                n += 1;
                beat.fetch_add(1, Ordering::Relaxed);
                if n >= 2 && live.load(Ordering::Acquire) == 0 {
                    break;
                }
            }
        }));
    }

    // Chaos 3: external SST ingestion. Bounded, because each ingest adds
    // keys the readers then have to walk, and an unbounded ingest loop
    // turns the reader cost quadratic in the run length.
    if enabled(4) {
        let (db, live, gate) = (Arc::clone(&db), Arc::clone(&live), Arc::clone(&gate));
        let ext = ext_dir.path().to_path_buf();
        workers.push(worker("ingest".to_string(), move |beat| {
            gate.wait();
            let mut n = 0u64;
            loop {
                let p = external_table(&ext, n);
                db.ingest_external_files(&[p], IngestOptions::default())
                    .wait()
                    .expect("ingest");
                n += 1;
                beat.fetch_add(1, Ordering::Relaxed);
                if n >= INGESTS_PER_INSTANCE || (n >= 2 && live.load(Ordering::Acquire) == 0) {
                    break;
                }
            }
        }));
    }

    // Chaos 4: checkpoint capture, which rotates the memtable and holds
    // the compaction lock.
    if enabled(8) {
        let (db, live, gate) = (Arc::clone(&db), Arc::clone(&live), Arc::clone(&gate));
        let cp = cp_dir.path().to_path_buf();
        workers.push(worker("checkpoint".to_string(), move |beat| {
            gate.wait();
            let mut n = 0u64;
            loop {
                let target = cp.join(format!("cp_{n}"));
                db.checkpoint(&target).wait().expect("checkpoint");
                std::fs::remove_dir_all(&target).expect("rm checkpoint");
                n += 1;
                beat.fetch_add(1, Ordering::Relaxed);
                if n >= CHECKPOINTS_PER_INSTANCE || (n >= 2 && live.load(Ordering::Acquire) == 0) {
                    break;
                }
            }
        }));
    }

    for r in 0..readers() {
        let surface = match r % 3 {
            0 => Surface::Get,
            1 => Surface::MultiGet,
            _ => Surface::Iter,
        };
        let (db, live, gate, bad, keys) = (
            Arc::clone(&db),
            Arc::clone(&live),
            Arc::clone(&gate),
            Arc::clone(&bad),
            keys.clone(),
        );
        workers.push(worker(format!("reader {r}"), move |beat| {
            let mut seen = vec![0u64; keys.len()];
            gate.wait();
            let mut round = 0u64;
            loop {
                for (idx, obs) in read(&db, surface, &keys).iter().enumerate() {
                    match obs {
                        None => bad.lock().expect("lock").push(format!(
                            "reader {r} round {round}: {} read back ABSENT (last seen {})",
                            String::from_utf8_lossy(&keys[idx]),
                            seen[idx],
                        )),
                        Some(s) => {
                            if *s < seen[idx] {
                                bad.lock().expect("lock").push(format!(
                                    "reader {r} round {round}: {} went BACKWARDS {} -> {s}",
                                    String::from_utf8_lossy(&keys[idx]),
                                    seen[idx],
                                ));
                            }
                            seen[idx] = seen[idx].max(*s);
                        }
                    }
                }
                round += 1;
                beat.fetch_add(1, Ordering::Relaxed);
                if round >= min_rounds && live.load(Ordering::Acquire) == 0 {
                    break;
                }
            }
        }));
    }

    // A barrier sized for a thread that never arrives is a permanent,
    // silent wedge, so fail loudly here instead, before anything blocks.
    assert_eq!(
        workers.len() + 1,
        gate_participants,
        "barrier is sized for {gate_participants} threads but {} will reach it \
         (+1 coordinator); every thread counted by the gate must call gate.wait()",
        workers.len(),
    );

    gate.wait();
    let stall_limit = Duration::from_secs(env("REGOLITH_CHAOS_STALL_SECS", 60));
    if let Err(wedged) = watch(&workers, &db, stall_limit) {
        // The wedged threads still use these directories, and they are
        // what a post-mortem needs, so they outlive the failure.
        return vec![format!(
            "{wedged}\ndatabase left at {}, ingest sources at {}, checkpoints at {}",
            dir.keep().display(),
            ext_dir.keep().display(),
            cp_dir.keep().display(),
        )];
    }
    for w in workers {
        w.handle.join().expect("worker panicked");
    }
    std::mem::take(&mut *bad.lock().expect("lock"))
}

#[test]
fn the_read_view_survives_compaction_cf_churn_ingest_and_checkpoint() {
    // Defaults sized for the ordinary gate, measured at 0.07s. The full
    // workload (6 / 2 / 400 / 40) is `just chaos`, and it is over 20
    // minutes wall and 4h of CPU unoptimized, which is why it is not
    // what `cargo test` runs. Every value is overridable, so a wedge
    // can be reproduced at whatever size exposed it.
    let instances = env("REGOLITH_CHAOS_INSTANCES", 2) as usize;
    let rounds = env("REGOLITH_CHAOS_ROUNDS", 1);
    let versions = env("REGOLITH_CHAOS_VERSIONS", 50);
    let min_rounds = env("REGOLITH_CHAOS_MIN_ROUNDS", 10);

    let mut bad = Vec::new();
    for _ in 0..rounds {
        let outs: Vec<Vec<String>> = thread::scope(|s| {
            let hs: Vec<_> = (0..instances)
                .map(|_| s.spawn(move || run_instance(versions, min_rounds)))
                .collect();
            hs.into_iter()
                .map(|h| h.join().expect("instance"))
                .collect()
        });
        for o in outs {
            bad.extend(o);
        }
    }
    println!(
        "chaos: {} instances x {rounds} rounds, {} violation(s)",
        instances,
        bad.len()
    );
    assert!(
        bad.is_empty(),
        "{}",
        bad.iter()
            .take(15)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}
