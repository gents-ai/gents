//! `commit_nowait` across a power cut, at both durabilities.
//!
//! The workload runs in a child process under the `LD_PRELOAD` shim: several
//! threads commit optimistic transactions that each bump one shared counter
//! (read through `get_for_update`, so concurrent commits conflict) and write
//! a pair of keys of their own. Half the threads commit with `commit_nowait`
//! on an I/O queue of their own and wait for the ticket by polling that
//! queue; the other half commit with the blocking `commit`, so groups mix
//! members whose fsync is a claimable unit with members that run it
//! themselves. A thread records a commit as acknowledged only once its
//! outcome is known: the ticket's at its own poll, or `commit`'s return. The
//! child dies at the nth write or fsync of the log, the directory is rebuilt
//! as the filesystem would have left it, discarding every byte never synced,
//! and the database is reopened.
//!
//! - Each transaction's writes survive together or not at all.
//! - Each thread's surviving commits are a prefix of the commits it made:
//!   groups land in the order they were written, whoever runs their fsync.
//! - The counter equals the number of surviving commits.
//! - At Immediate every acknowledged commit survives: a ticket is ready only
//!   once its group's fsync returned.
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
use regolith::DurabilityMode;
use regolith::prelude::*;
use tempfile::TempDir;

const THREADS: usize = 4;
const PAIRS: &str = "nowait_pairs";
const CHILD_TIMEOUT: Duration = Duration::from_secs(180);

/// Child process entry point. Returns immediately unless this process was
/// re-executed by the crash harness.
#[test]
fn crash_child() {
    fault::child_entrypoint(dispatch);
}

fn dispatch(spec: &ChildSpec) {
    match &spec.phase {
        Phase::Custom(name) if name == PAIRS => pairs_child(spec),
        _ => fault::builtin_workload(spec),
    }
}

fn open(path: &Path, durability: DurabilityMode) -> OptimisticTransactionDb {
    let options = Options::default()
        .durability(durability)
        .write_buffer_size(4 * 1024);
    OptimisticTransactionDb::open(path, options).unwrap()
}

fn number(bytes: Option<Vec<u8>>) -> u64 {
    bytes.map_or(0, |b| String::from_utf8(b).unwrap().parse().unwrap())
}

fn pair_key(t: usize, i: usize, half: &str) -> Vec<u8> {
    format!("pair/{t}/{i:04}/{half}").into_bytes()
}

/// Commit transaction `i` of thread `t`, through `queue` with
/// `commit_nowait` when there is one, else with `commit`. `true` once it
/// committed, `false` on a conflict.
fn commit_once(
    db: &OptimisticTransactionDb,
    queue: Option<&mut IoQueue>,
    t: usize,
    i: usize,
) -> bool {
    let options = match &queue {
        Some(queue) => TxnOptions::new().io_queue(queue.id()),
        None => TxnOptions::new(),
    };
    let tx = db.begin(&options);
    let counter = number(tx.get_for_update(b"counter").unwrap());
    tx.put(b"counter", (counter + 1).to_string().as_bytes())
        .unwrap();
    tx.put(&pair_key(t, i, "a"), b"a").unwrap();
    tx.put(&pair_key(t, i, "b"), b"b").unwrap();
    let outcome = match queue {
        Some(queue) => queue.block_on(tx.commit_nowait()),
        None => tx.commit(),
    };
    match outcome {
        Ok(_) => true,
        Err(TransactionError::Conflict(_)) => false,
        Err(other) => panic!("child: commit: {other}"),
    }
}

fn pairs_child(spec: &ChildSpec) {
    let db = Arc::new(open(&spec.db_path, spec.durability));
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let db = Arc::clone(&db);
            let mut acks = fault::Acks::open(&spec.ack_path);
            let commits = spec.ops;
            std::thread::spawn(move || {
                let mut queue = (t % 2 == 0).then(|| db.db().io_queue());
                for i in 0..commits {
                    while !commit_once(&db, queue.as_mut(), t, i) {}
                    acks.ack([t as u64, i as u64]);
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
    durability: DurabilityMode,
    trigger: Trigger,
    tear: TearMode,
    commits: usize,
) -> (TempDir, ChildOutcome) {
    let dir = TempDir::new().unwrap();
    let spec = ChildSpec::new(Phase::Custom(PAIRS.to_string()), dir.path().join("db"))
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

fn check_recovered(out: &ChildOutcome, durability: DurabilityMode, commits: usize) {
    let db = open(&out.spec.db_path, durability);
    let mut survived: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for t in 0..THREADS {
        for i in 0..commits {
            let a = db.db().get(&pair_key(t, i, "a")).unwrap();
            let b = db.db().get(&pair_key(t, i, "b")).unwrap();
            assert_eq!(
                a.is_some(),
                b.is_some(),
                "thread {t} commit {i}: half of a transaction survived"
            );
            if a.is_some() {
                survived.entry(t).or_default().insert(i);
            }
        }
    }
    let mut total = 0;
    for (t, done) in &survived {
        let expected: BTreeSet<usize> = (0..done.len()).collect();
        assert_eq!(
            done, &expected,
            "thread {t}: its surviving commits are not a prefix of its commits"
        );
        total += done.len();
    }
    assert_eq!(
        number(db.db().get(b"counter").unwrap()),
        total as u64,
        "the counter must count exactly the surviving commits"
    );
    if durability == DurabilityMode::Immediate {
        for [t, i] in out.acks.of() {
            let (t, i) = (t as usize, i as usize);
            assert!(
                survived.get(&t).is_some_and(|done| done.contains(&i)),
                "acknowledged commit {i} of thread {t} was lost"
            );
        }
    }
    // The database goes on committing where it was left, through a ticket.
    let mut queue = db.db().io_queue();
    let tx = db.begin(&TxnOptions::new().io_queue(queue.id()));
    let counter = number(tx.get_for_update(b"counter").unwrap());
    tx.put(b"counter", (counter + 1).to_string().as_bytes())
        .unwrap();
    queue.block_on(tx.commit_nowait()).unwrap();
    assert_eq!(number(db.db().get(b"counter").unwrap()), total as u64 + 1);
}

const TEARS: [TearMode; 2] = [TearMode::Truncate, TearMode::TornSector];
/// Commits per writer: enough log writes and fsyncs for every cut below.
const COMMITS: usize = 100;

#[test]
fn a_power_cut_keeps_whole_transactions_and_every_acknowledged_one_at_immediate() {
    for nth in [3, 15, 40, 90] {
        for tear in TEARS {
            let (_dir, out) = crash_and_cut(
                DurabilityMode::Immediate,
                Trigger::wal_write(nth),
                tear,
                COMMITS,
            );
            check_recovered(&out, DurabilityMode::Immediate, COMMITS);
        }
    }
}

#[test]
fn a_power_cut_keeps_whole_transactions_at_eventual() {
    for nth in [3, 40, 90] {
        for tear in TEARS {
            let (_dir, out) = crash_and_cut(
                DurabilityMode::Eventual,
                Trigger::wal_write(nth),
                tear,
                COMMITS,
            );
            check_recovered(&out, DurabilityMode::Eventual, COMMITS);
        }
    }
}

/// A cut at the fsync a ticket's group owes: whichever member thread ran it,
/// every commit acknowledged before survives.
#[test]
fn a_power_cut_at_a_claimed_group_fsync_keeps_every_acknowledged_commit() {
    for nth in [2, 10, 30] {
        let (_dir, out) = crash_and_cut(
            DurabilityMode::Immediate,
            Trigger::wal_fsync(nth),
            TearMode::Truncate,
            COMMITS,
        );
        check_recovered(&out, DurabilityMode::Immediate, COMMITS);
    }
}

#[test]
fn the_clean_run_lands_every_commit() {
    for durability in [DurabilityMode::Immediate, DurabilityMode::Eventual] {
        let dir = TempDir::new().unwrap();
        let spec = ChildSpec::new(Phase::Custom(PAIRS.to_string()), dir.path().join("db"))
            .ops(10)
            .durability(durability);
        let out = CrashRun::new(spec).trigger(Trigger::None).run();
        out.assert_clean();
        check_recovered(&out, durability, 10);
        let db = open(&out.spec.db_path, durability);
        assert_eq!(
            number(db.db().get(b"counter").unwrap()),
            THREADS as u64 * 10 + 1
        );
    }
}
