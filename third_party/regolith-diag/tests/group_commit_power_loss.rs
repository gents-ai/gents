//! Optimistic transactions whose commits share syncs of the log, across a
//! power cut.
//!
//! The workload runs in a child process under the `LD_PRELOAD` shim: several
//! threads commit optimistic transactions that each bump one shared counter
//! (read through `get_for_update`, so concurrent commits conflict, whether
//! or not one sync covers them) and write a pair of keys of their own. The child
//! dies at the nth write or fsync of the log, the directory is rebuilt as the
//! filesystem would have left it, discarding every byte never synced, and the
//! database is reopened. Small memtables make the run rotate logs and flush
//! tables on the way.
//!
//! - Each transaction's writes survive together or not at all, whichever
//!   sync covered it and wherever the cut tore the log.
//! - Each thread's surviving commits are a prefix of the commits it made.
//! - The counter equals the number of surviving commits: no surviving commit
//!   was lost under another, and none survived without its increment.
//! - At Immediate every acknowledged commit survives; a commit is
//!   acknowledged only once the fsync that covers it returned.
//!
//! Every run is made twice: plain, and encrypted at rest, where each commit
//! is one sealed record (#266 x #265).
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
use common::keys::Keys;
use regolith::DurabilityMode;
use regolith::prelude::*;
use tempfile::TempDir;

const THREADS: usize = 4;
const PAIRS: &str = "transaction_pairs";
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

fn open(path: &Path, durability: DurabilityMode, encrypted: bool) -> OptimisticTransactionDb {
    let options = Options::default()
        .durability(durability)
        .write_buffer_size(4 * 1024);
    let options = if encrypted {
        options.key_provider(Keys::new(&[1]))
    } else {
        options
    };
    OptimisticTransactionDb::open(path, options).unwrap()
}

fn number(bytes: Option<Vec<u8>>) -> u64 {
    bytes.map_or(0, |b| String::from_utf8(b).unwrap().parse().unwrap())
}

fn pair_key(t: usize, i: usize, half: &str) -> Vec<u8> {
    format!("pair/{t}/{i:04}/{half}").into_bytes()
}

fn pairs_child(spec: &ChildSpec) {
    let db = Arc::new(open(&spec.db_path, spec.durability, spec.encrypted));
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let db = Arc::clone(&db);
            let mut acks = fault::Acks::open(&spec.ack_path);
            let commits = spec.ops;
            std::thread::spawn(move || {
                for i in 0..commits {
                    loop {
                        let tx = db.begin(&TxnOptions::new());
                        let counter = number(tx.get_for_update(b"counter").unwrap());
                        tx.put(b"counter", (counter + 1).to_string().as_bytes())
                            .unwrap();
                        tx.put(&pair_key(t, i, "a"), b"a").unwrap();
                        tx.put(&pair_key(t, i, "b"), b"b").unwrap();
                        match tx.commit() {
                            Ok(_) => break,
                            Err(TransactionError::Conflict(_)) => continue,
                            Err(other) => panic!("child: commit: {other}"),
                        }
                    }
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
    encrypted: bool,
) -> (TempDir, ChildOutcome) {
    let dir = TempDir::new().unwrap();
    let spec = ChildSpec::new(Phase::Custom(PAIRS.to_string()), dir.path().join("db"))
        .ops(commits)
        .durability(durability)
        .encrypted(encrypted);
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
    let db = open(&out.spec.db_path, durability, out.spec.encrypted);
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
    // The database goes on committing where it was left.
    let tx = db.begin(&TxnOptions::new());
    let counter = number(tx.get_for_update(b"counter").unwrap());
    tx.put(b"counter", (counter + 1).to_string().as_bytes())
        .unwrap();
    tx.commit().unwrap();
    assert_eq!(number(db.db().get(b"counter").unwrap()), total as u64 + 1);
}

const TEARS: [TearMode; 2] = [TearMode::Truncate, TearMode::TornSector];
/// Commits per writer. A writer makes its next commit only once the last is
/// durable, so one sync covers at most one commit of each of the `THREADS`
/// writers, and this many commits per writer make at least this many log
/// writes and log fsyncs: enough for every cut below to fire.
const COMMITS: usize = 100;
/// Plain, then encrypted at rest.
const SEALED: [bool; 2] = [false, true];

#[test]
fn a_power_cut_keeps_whole_transactions_and_every_acknowledged_one_at_immediate() {
    for encrypted in SEALED {
        for nth in [3, 15, 40, 90] {
            for tear in TEARS {
                let (_dir, out) = crash_and_cut(
                    DurabilityMode::Immediate,
                    Trigger::wal_write(nth),
                    tear,
                    COMMITS,
                    encrypted,
                );
                check_recovered(&out, DurabilityMode::Immediate, COMMITS);
            }
        }
    }
}

#[test]
fn a_power_cut_keeps_whole_transactions_at_eventual() {
    for encrypted in SEALED {
        for nth in [3, 40, 90] {
            for tear in TEARS {
                let (_dir, out) = crash_and_cut(
                    DurabilityMode::Eventual,
                    Trigger::wal_write(nth),
                    tear,
                    COMMITS,
                    encrypted,
                );
                check_recovered(&out, DurabilityMode::Eventual, COMMITS);
            }
        }
    }
}

#[test]
fn a_power_cut_at_a_group_fsync_keeps_every_commit_it_acknowledged() {
    for encrypted in SEALED {
        for nth in [2, 10, 30] {
            let (_dir, out) = crash_and_cut(
                DurabilityMode::Immediate,
                Trigger::wal_fsync(nth),
                TearMode::Truncate,
                COMMITS,
                encrypted,
            );
            check_recovered(&out, DurabilityMode::Immediate, COMMITS);
        }
    }
}

#[test]
fn the_clean_run_lands_every_commit() {
    for encrypted in SEALED {
        let dir = TempDir::new().unwrap();
        let spec = ChildSpec::new(Phase::Custom(PAIRS.to_string()), dir.path().join("db"))
            .ops(10)
            .durability(DurabilityMode::Immediate)
            .encrypted(encrypted);
        let out = CrashRun::new(spec).trigger(Trigger::None).run();
        out.assert_clean();
        check_recovered(&out, DurabilityMode::Immediate, 10);
        let db = open(&out.spec.db_path, DurabilityMode::Immediate, encrypted);
        assert_eq!(
            number(db.db().get(b"counter").unwrap()),
            THREADS as u64 * 10 + 1
        );
    }
}
