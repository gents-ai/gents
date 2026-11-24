//! Commit-ordered append and allocation across a power cut.
//!
//! The workload runs in a child process under the `LD_PRELOAD` shim and dies
//! at the nth write to the log. The directory is then rebuilt as the
//! filesystem would have left it, discarding every byte that was never
//! synced (`CommitOrderedAppendCrash.tla`, `Allocate.tla`), and the database
//! is reopened.
//!
//! - Append, at either durability: the log is a gap-free prefix of the commit
//!   order (dense, unique, each writer's entries in order), each entry and its
//!   once key were committed together or not at all, and the next commit
//!   continues at the recovered head. At Immediate every acknowledged commit
//!   is in the log.
//! - Allocation, at either durability: no value that a surviving commit used
//!   is returned again, because the allocation record is ordered before the
//!   commit that uses it (`INV_UseDurable`, `INV_UsesUnique`).
//!
//! # Linux only
//!
//! `LD_PRELOAD` interposition is a glibc mechanism; the file is compiled out
//! elsewhere. See `power_loss.rs` for what the shim does and does not prove.
#![cfg(target_os = "linux")]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::fault::{
    self, ChildOutcome, ChildSpec, CrashRun, CutPoint, Phase, PowerLossOptions, TearMode, Trigger,
};
use regolith::prelude::*;
use regolith::{Db, DurabilityMode};
use tempfile::TempDir;

const THREADS: usize = 3;
const APPEND: &str = "append_commits";
const ALLOCATE: &str = "allocate_and_use";
const CHILD_TIMEOUT: Duration = Duration::from_secs(180);

/// Child process entry point. Returns immediately unless this process was
/// re-executed by the crash harness.
#[test]
fn crash_child() {
    fault::child_entrypoint(dispatch);
}

fn dispatch(spec: &ChildSpec) {
    match &spec.phase {
        Phase::Custom(name) if name == APPEND => append_child(spec),
        Phase::Custom(name) if name == ALLOCATE => allocate_child(spec),
        _ => fault::builtin_workload(spec),
    }
}

struct Journal;

impl LogLayout for Journal {
    fn head_key(&self) -> &[u8] {
        b"journal-head"
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(format!("journal/{position:020}").as_bytes());
    }

    fn max_entry_key_len(&self) -> usize {
        8 + 20
    }
}

fn open(path: &Path, durability: DurabilityMode) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(
        path,
        Options::default()
            .durability(durability)
            // Small enough that the run rotates the log and flushes tables.
            .write_buffer_size(4 * 1024),
    )
    .unwrap()
}

/// Run `attempt`, a transaction built and committed. A write stall does not
/// end its commit, which waits the stall out, so the attempt is made once;
/// any failure fails the child.
fn commit_through_stalls(attempt: impl FnOnce() -> TxResult<CommitReceipt>) {
    if let Err(other) = attempt() {
        panic!("child: commit: {other}");
    }
}

fn append_child(spec: &ChildSpec) {
    let db = Arc::new(open(&spec.db_path, spec.durability));
    let log: Arc<dyn LogLayout> = Arc::new(Journal);
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let (db, log) = (Arc::clone(&db), Arc::clone(&log));
            let mut acks = fault::Acks::open(&spec.ack_path);
            let commits = spec.ops;
            std::thread::spawn(move || {
                for i in 0..commits {
                    commit_through_stalls(|| {
                        let tx = db.begin(&TxnOptions::new());
                        tx.append(
                            &log,
                            format!("t{t}-{i}").as_bytes(),
                            Some(format!("once/{t}-{i}").as_bytes()),
                        )
                        .unwrap();
                        tx.commit()
                    });
                    acks.ack([t as u64, i as u64]);
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
}

fn allocate_child(spec: &ChildSpec) {
    let db = Arc::new(open(&spec.db_path, spec.durability));
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let db = Arc::clone(&db);
            let mut acks = fault::Acks::open(&spec.ack_path);
            let rounds = spec.ops;
            std::thread::spawn(move || {
                for round in 0..rounds {
                    let range = db.allocate(b"ids", 3).unwrap();
                    // Every fifth round aborts after allocating: a gap.
                    if round % 5 == 4 {
                        let tx = db.begin(&TxnOptions::new());
                        for v in range.clone() {
                            tx.put(format!("use/{v:020}/{t}").as_bytes(), b"").unwrap();
                        }
                        tx.rollback();
                        continue;
                    }
                    commit_through_stalls(|| {
                        let tx = db.begin(&TxnOptions::new());
                        for v in range.clone() {
                            tx.put(format!("use/{v:020}/{t}").as_bytes(), b"").unwrap();
                        }
                        tx.commit()
                    });
                    acks.ack_all(range.map(|v| [v]));
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
}

/// Run the child to its kill point, then discard what was never synced.
fn crash_and_cut(
    phase: &str,
    durability: DurabilityMode,
    trigger: Trigger,
    tear: TearMode,
    commits: usize,
) -> (TempDir, ChildOutcome) {
    let dir = TempDir::new().unwrap();
    let spec = ChildSpec::new(Phase::Custom(phase.to_string()), dir.path().join("db"))
        .ops(commits)
        .durability(durability);
    let out = CrashRun::new(spec)
        .trigger(trigger)
        .timeout(CHILD_TIMEOUT)
        .run();
    out.assert_killed();
    let opts = PowerLossOptions::default().tear(tear);
    fault::simulate_power_loss_with(&out.spec.db_path, &out.journal, CutPoint::End, &opts);
    (dir, out)
}

fn be(bytes: Vec<u8>) -> u64 {
    u64::from_be_bytes(bytes.try_into().expect("a position is eight bytes"))
}

fn check_recovered_log(out: &ChildOutcome, durability: DurabilityMode) {
    let db = open(&out.spec.db_path, durability);
    let head = db.db().get(b"journal-head").unwrap().map_or(0, be);

    // Dense: every position up to the head has its entry.
    let mut seen = BTreeSet::new();
    let mut last_of_thread: BTreeMap<usize, usize> = BTreeMap::new();
    for p in 1..=head {
        let entry = db
            .db()
            .get(format!("journal/{p:020}").as_bytes())
            .unwrap()
            .unwrap_or_else(|| panic!("position {p} is missing below the head {head}"));
        let text = String::from_utf8(entry).unwrap();
        let (t, i) = text
            .strip_prefix('t')
            .and_then(|r| r.split_once('-'))
            .map(|(t, i)| (t.parse::<usize>().unwrap(), i.parse::<usize>().unwrap()))
            .unwrap();
        assert!(seen.insert((t, i)), "{text} appears twice");
        // Atomic with its once key, which names its position.
        let once = db
            .db()
            .get(format!("once/{t}-{i}").as_bytes())
            .unwrap()
            .map(be);
        assert_eq!(
            once,
            Some(p),
            "{text}: entry and once key must commit together"
        );
        // A writer's commits are in order, so its entries are too.
        if let Some(&previous) = last_of_thread.get(&t) {
            assert!(previous < i, "thread {t} went from {previous} to {i}");
        }
        last_of_thread.insert(t, i);
    }
    // No once key without its entry, and nothing past the head.
    let onces = db.db().scan(Some(b"once/"), Some(b"once0")).unwrap().len() as u64;
    assert_eq!(onces, head, "a once key survived without its entry");
    assert_eq!(
        db.db()
            .get(format!("journal/{:020}", head + 1).as_bytes())
            .unwrap(),
        None
    );

    // Immediate: an acknowledged commit survives any cut.
    if durability == DurabilityMode::Immediate {
        for [t, i] in out.acks.of() {
            let key = (t as usize, i as usize);
            assert!(seen.contains(&key), "acknowledged commit {key:?} was lost");
        }
    }

    // The log continues at the recovered head, with no hole and no repeat.
    let log: Arc<dyn LogLayout> = Arc::new(Journal);
    let tx = db.begin(&TxnOptions::new());
    tx.append(&log, b"after-the-cut", None).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        db.db().get(b"journal-head").unwrap().map(be),
        Some(head + 1)
    );
    assert_eq!(
        db.db()
            .get(format!("journal/{:020}", head + 1).as_bytes())
            .unwrap(),
        Some(b"after-the-cut".to_vec())
    );
}

const TEARS: [TearMode; 2] = [TearMode::Truncate, TearMode::TornSector];

/// Commits per writer for the cuts at the nth log write. Concurrent commits
/// share a group's one write, and a group holds at most one commit of each
/// of the `THREADS` writers, so this many commits per writer make at least
/// this many log writes: enough for every cut below to fire.
const APPEND_COMMITS: usize = 100;

/// Rounds per writer for the cuts at the nth log write. A round makes a
/// reservation's record and, four rounds in five, a commit's; a run of the
/// write unit holds at most those two of each of the `THREADS` writers (a
/// reservation returns before its record is written, a commit only once its
/// own is), so this many rounds make at least 120 log writes: enough for the
/// last cut below to fire.
const ALLOCATE_ROUNDS: usize = 140;

#[test]
fn a_power_cut_leaves_a_gap_free_prefix_of_the_appends_at_immediate_durability() {
    for nth in [4, 17, 40, 90] {
        for tear in TEARS {
            let (_dir, out) = crash_and_cut(
                APPEND,
                DurabilityMode::Immediate,
                Trigger::wal_write(nth),
                tear,
                APPEND_COMMITS,
            );
            check_recovered_log(&out, DurabilityMode::Immediate);
        }
    }
}

#[test]
fn a_power_cut_leaves_a_gap_free_prefix_of_the_appends_at_eventual_durability() {
    for nth in [4, 40, 90] {
        for tear in TEARS {
            let (_dir, out) = crash_and_cut(
                APPEND,
                DurabilityMode::Eventual,
                Trigger::wal_write(nth),
                tear,
                APPEND_COMMITS,
            );
            check_recovered_log(&out, DurabilityMode::Eventual);
        }
    }
}

#[test]
fn a_power_cut_at_an_fsync_keeps_every_append_it_acknowledged() {
    for nth in [3, 12, 30] {
        let (_dir, out) = crash_and_cut(
            APPEND,
            DurabilityMode::Immediate,
            Trigger::wal_fsync(nth),
            TearMode::Truncate,
            40,
        );
        check_recovered_log(&out, DurabilityMode::Immediate);
    }
}

fn check_recovered_counter(out: &ChildOutcome, durability: DurabilityMode) {
    let db = open(&out.spec.db_path, durability);
    let uses = db.db().scan(Some(b"use/"), Some(b"use0")).unwrap();
    let mut values = BTreeSet::new();
    for (key, _) in &uses {
        let text = String::from_utf8(key.clone()).unwrap();
        let v: u64 = text.split('/').nth(1).unwrap().parse().unwrap();
        assert!(values.insert(v), "value {v} was used by two commits");
    }
    let max_used = values.last().copied().unwrap_or(0);
    let counter = db.db().get(b"ids").unwrap().map_or(0, be);
    assert!(
        counter >= max_used,
        "the recovered counter {counter} is behind a surviving use of {max_used}"
    );
    let next = db.allocate(b"ids", 1).unwrap();
    assert!(
        next.start > max_used,
        "value {} was handed out again after a crash that kept its use",
        next.start
    );
    if durability == DurabilityMode::Immediate {
        for [v] in out.acks.of() {
            assert!(values.contains(&v), "acknowledged use of {v} was lost");
        }
    }
}

#[test]
fn a_power_cut_never_returns_a_value_a_surviving_commit_used_at_immediate_durability() {
    for nth in [5, 20, 60, 120] {
        for tear in TEARS {
            let (_dir, out) = crash_and_cut(
                ALLOCATE,
                DurabilityMode::Immediate,
                Trigger::wal_write(nth),
                tear,
                ALLOCATE_ROUNDS,
            );
            check_recovered_counter(&out, DurabilityMode::Immediate);
        }
    }
}

#[test]
fn a_power_cut_never_returns_a_value_a_surviving_commit_used_at_eventual_durability() {
    for nth in [5, 60, 120] {
        for tear in TEARS {
            let (_dir, out) = crash_and_cut(
                ALLOCATE,
                DurabilityMode::Eventual,
                Trigger::wal_write(nth),
                tear,
                ALLOCATE_ROUNDS,
            );
            check_recovered_counter(&out, DurabilityMode::Eventual);
        }
    }
}

#[test]
fn the_clean_run_of_each_workload_is_what_the_cut_runs_are_prefixes_of() {
    for phase in [APPEND, ALLOCATE] {
        let dir = TempDir::new().unwrap();
        let spec = ChildSpec::new(Phase::Custom(phase.to_string()), dir.path().join("db"))
            .ops(10)
            .durability(DurabilityMode::Immediate);
        let out = CrashRun::new(spec).trigger(Trigger::None).run();
        out.assert_clean();
        if phase == APPEND {
            check_recovered_log(&out, DurabilityMode::Immediate);
            let db = Db::open(&out.spec.db_path, Options::default()).unwrap();
            assert_eq!(
                db.get(b"journal-head").unwrap().map(be),
                Some(THREADS as u64 * 10 + 1)
            );
        } else {
            check_recovered_counter(&out, DurabilityMode::Immediate);
        }
    }
}
