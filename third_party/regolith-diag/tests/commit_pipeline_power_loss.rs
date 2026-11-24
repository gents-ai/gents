//! The lock-free commit pipeline across a power cut at every stage, plain
//! and encrypted at rest.
//!
//! The workload runs in a child process under the `LD_PRELOAD` shim. Four
//! threads commit at once, each a different kind of slot of the commit ring,
//! so every stage is reached by more than one thread and any of them may be
//! the one that decides, writes, syncs or publishes another's commit:
//!
//! - thread 0 commits optimistic transactions with the blocking `commit`;
//! - thread 1 commits them with `commit_nowait` on a queue of its own and
//!   waits for the ticket by polling, so the sync is a round a poll claims;
//! - thread 2 writes plain batches of two keys with `WriteOptions::sync`,
//!   durable whatever the database's own durability, so slots that wait for
//!   a sync sit between slots that do not;
//! - thread 3 writes single keys at the database's own durability.
//!
//! Each transaction bumps one shared counter, read through `get_for_update`,
//! so concurrent commits conflict and are decided against each other, and
//! writes a pair of keys of its own. Small memtables make the run rotate
//! logs and flush tables, each an ordered step through the ring.
//!
//! # Where the power is cut
//!
//! The pipeline's stages are reserve, decide, write, sync, apply and
//! publish. Only two of them touch the device, so those are where a cut
//! leaves a different disk:
//!
//! | stage a commit had reached | the cut |
//! |---|---|
//! | reserved or decided, no byte written | before the nth write of the log |
//! | written, in part or whole, not synced | after the nth write, with the unsynced bytes truncated, zeroed, replaced by garbage, or torn inside a sector |
//! | written, about to be synced | before the nth fsync of the log |
//! | durable, not yet applied, published or acknowledged | after the nth fsync |
//! | any, at a sync boundary earlier in the run | the journal cut before or after its kth sync |
//!
//! A commit applied or published and not yet durable is on disk exactly as
//! one written and not synced is, so the same cuts cover it; what those
//! stages add is the acknowledgement, and the check is that nothing
//! acknowledged as durable is lost.
//!
//! # What must hold after any of them
//!
//! - Each transaction's writes, and each batch's, survive together or not
//!   at all.
//! - Each thread's surviving commits are a prefix of the commits it made:
//!   recovery keeps the log's prefix with no hole, though records are
//!   written by position and not in order.
//! - The counter equals the number of surviving transactions: none was
//!   lost under another, none survived without its increment.
//! - Every commit acknowledged as durable survives: a synced batch always,
//!   and everything else when the database's durability is `Immediate`.
//! - The database reopens and goes on committing.
//!
//! # Linux only
//!
//! `LD_PRELOAD` interposition is a glibc mechanism; the file is compiled out
//! elsewhere. See `power_loss.rs` for what the shim does and does not prove.
#![cfg(target_os = "linux")]

mod common;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::fault::journal::OpKind;
use common::fault::{
    self, ChildOutcome, ChildSpec, CrashRun, CutPoint, DieKind, Phase, PowerLossOptions, TearMode,
    Trigger,
};
use common::keys::Keys;
use regolith::prelude::*;
use regolith::{DurabilityMode, WriteBatch, WriteOptions};
use tempfile::TempDir;

const STAGES: &str = "pipeline_stages";
const CHILD_TIMEOUT: Duration = Duration::from_secs(180);
/// The four kinds of writer, by thread number.
const BLOCKING_TXN: usize = 0;
const NOWAIT_TXN: usize = 1;
const SYNCED_BATCH: usize = 2;
const SINGLE_PUT: usize = 3;
const THREADS: usize = 4;

/// Child process entry point. Returns immediately unless this process was
/// re-executed by the crash harness.
#[test]
fn crash_child() {
    fault::child_entrypoint(dispatch);
}

fn dispatch(spec: &ChildSpec) {
    match &spec.phase {
        Phase::Custom(name) if name == STAGES => stages_child(spec),
        _ => fault::builtin_workload(spec),
    }
}

/// Memtables small enough that the run rotates logs and flushes tables.
const SMALL_BUFFER: usize = 4 * 1024;
/// One memtable for the whole run: no log is retired, see
/// [`a_cut_at_a_sync_boundary_keeps_the_prefix_the_sync_covered`].
const ONE_BUFFER: usize = 8 << 20;

fn open(
    path: &Path,
    durability: DurabilityMode,
    encrypted: bool,
    write_buffer: usize,
) -> OptimisticTransactionDb {
    let options = Options::default()
        .durability(durability)
        .write_buffer_size(write_buffer);
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

fn key(t: usize, i: usize, half: &str) -> Vec<u8> {
    format!("w/{t}/{i:04}/{half}").into_bytes()
}

/// One attempt at transaction `i` of thread `t`: `true` once it committed,
/// `false` on a conflict or a write stall, which is waited out here.
fn commit_txn(
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
    tx.put(&key(t, i, "a"), b"a").unwrap();
    tx.put(&key(t, i, "b"), b"b").unwrap();
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

/// Commit `i` of thread `t`, of the thread's own kind, returning once its
/// outcome is known.
fn commit(db: &OptimisticTransactionDb, queue: Option<&mut IoQueue>, t: usize, i: usize) {
    match t {
        BLOCKING_TXN => while !commit_txn(db, None, t, i) {},
        NOWAIT_TXN => {
            let queue = queue.expect("the nowait writer has a queue");
            while !commit_txn(db, Some(queue), t, i) {}
        }
        SYNCED_BATCH => {
            let mut batch = WriteBatch::new();
            batch.put(&key(t, i, "a"), b"a");
            batch.put(&key(t, i, "b"), b"b");
            let synced = WriteOptions {
                sync: true,
                ..WriteOptions::default()
            };
            regolith::through_stalls(|| db.db().write_opt(&synced, batch.clone()))
                .unwrap_or_else(|e| panic!("child: batch: {e}"));
        }
        _ => {
            regolith::through_stalls(|| db.db().put(&key(t, i, "a"), b"a"))
                .unwrap_or_else(|e| panic!("child: put: {e}"));
        }
    }
}

fn stages_child(spec: &ChildSpec) {
    let db = Arc::new(open(
        &spec.db_path,
        spec.durability,
        spec.encrypted,
        spec.write_buffer_size,
    ));
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let db = Arc::clone(&db);
            let mut acks = fault::Acks::open(&spec.ack_path);
            let commits = spec.ops;
            std::thread::spawn(move || {
                let mut queue = (t == NOWAIT_TXN).then(|| db.db().io_queue());
                for i in 0..commits {
                    commit(&db, queue.as_mut(), t, i);
                    acks.ack([t as u64, i as u64]);
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
}

/// Where the journal of a run is cut.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Where {
    /// Where the child died.
    End,
    /// Just before the sync that is `part` of the way through the syncs
    /// the run recorded (`part.0 / part.1`), or just after it.
    Sync { after: bool, part: (usize, usize) },
}

/// One run of the child and one cut of its journal.
struct Cut {
    durability: DurabilityMode,
    encrypted: bool,
    trigger: Trigger,
    point: Where,
    tear: TearMode,
    write_buffer: usize,
}

impl Cut {
    fn describe(&self) -> String {
        format!(
            "{:?}, {}, {:?}, cut {:?}, tear {:?}",
            self.durability,
            if self.encrypted { "encrypted" } else { "plain" },
            self.trigger,
            self.point,
            self.tear
        )
    }

    /// Run the child to its kill point, discard what was never synced as
    /// the cut says, and check the recovered database.
    fn run_and_check(&self, commits: usize) {
        let dir = TempDir::new().unwrap();
        let spec = ChildSpec::new(Phase::Custom(STAGES.to_string()), dir.path().join("db"))
            .ops(commits)
            .durability(self.durability)
            .encrypted(self.encrypted)
            .write_buffer_size(self.write_buffer);
        let out = CrashRun::new(spec)
            .trigger(self.trigger.clone())
            .timeout(CHILD_TIMEOUT)
            .run();
        if self.trigger == Trigger::None {
            out.assert_clean();
        } else {
            out.assert_killed();
        }
        let point = match self.point {
            Where::End => CutPoint::End,
            Where::Sync { after, part } => {
                let syncs = out.journal.sync_seqs();
                assert!(
                    !syncs.is_empty(),
                    "{}: the run recorded no sync",
                    self.describe()
                );
                let nth = (syncs.len() * part.0 / part.1).max(1);
                // The reconstruction does not bring back a file unlinked
                // after the cut, which a real cut there would still have,
                // so it is sound only for a run that retires no log. A
                // flush is what retires one, and the unlink a dying child
                // made may not have reached the journal, so the run must
                // not have flushed at all: it wrote no table.
                let flushed = out
                    .journal
                    .records
                    .iter()
                    .filter(|r| {
                        r.kind == OpKind::Unlink || r.path.extension() == Some("sst".as_ref())
                    })
                    .count();
                assert_eq!(
                    flushed,
                    0,
                    "{}: the run flushed a table or unlinked a file",
                    self.describe()
                );
                if after {
                    CutPoint::AfterNthSync(nth)
                } else {
                    CutPoint::BeforeNthSync(nth)
                }
            }
        };
        let options = PowerLossOptions::default().tear(self.tear);
        fault::simulate_power_loss_with(&out.spec.db_path, &out.journal, point, &options);
        // An acknowledgement is of what was on disk when the child died. A
        // cut earlier in the journal than that may have discarded it.
        let acks_hold = self.point == Where::End;
        check_recovered(&out, self, commits, acks_hold);
    }
}

fn log_syscall(kind: DieKind, nth: u64, before: bool) -> Trigger {
    Trigger::Syscall {
        kind,
        path_contains: "/wal/".to_string(),
        nth,
        before,
    }
}

/// The commits of thread `t` that survived, checked to be a prefix of the
/// commits it made, with each commit whole.
fn survivors(db: &OptimisticTransactionDb, t: usize, commits: usize, what: &str) -> usize {
    let mut survived = BTreeSet::new();
    for i in 0..commits {
        let a = db.db().get(&key(t, i, "a")).unwrap();
        if t != SINGLE_PUT {
            let b = db.db().get(&key(t, i, "b")).unwrap();
            assert_eq!(
                a.is_some(),
                b.is_some(),
                "{what}: thread {t} commit {i}: half of a commit survived"
            );
        }
        if a.is_some() {
            survived.insert(i);
        }
    }
    let prefix: BTreeSet<usize> = (0..survived.len()).collect();
    assert_eq!(
        survived, prefix,
        "{what}: thread {t}: its surviving commits are not a prefix of its commits"
    );
    survived.len()
}

fn check_recovered(out: &ChildOutcome, cut: &Cut, commits: usize, acks_hold: bool) {
    let what = cut.describe();
    let db = open(
        &out.spec.db_path,
        cut.durability,
        cut.encrypted,
        cut.write_buffer,
    );
    let survived: Vec<usize> = (0..THREADS)
        .map(|t| survivors(&db, t, commits, &what))
        .collect();
    let transactions = (survived[BLOCKING_TXN] + survived[NOWAIT_TXN]) as u64;
    assert_eq!(
        number(db.db().get(b"counter").unwrap()),
        transactions,
        "{what}: the counter must count exactly the surviving transactions"
    );
    if acks_hold {
        for [t, i] in out.acks.of() {
            let (t, i) = (t as usize, i as usize);
            let durable = t == SYNCED_BATCH || cut.durability == DurabilityMode::Immediate;
            assert!(
                !durable || i < survived[t],
                "{what}: acknowledged commit {i} of thread {t} was lost"
            );
        }
    }
    // The database goes on committing where it was left.
    let tx = db.begin(&TxnOptions::new());
    let counter = number(tx.get_for_update(b"counter").unwrap());
    tx.put(b"counter", (counter + 1).to_string().as_bytes())
        .unwrap();
    tx.commit().unwrap();
    assert_eq!(
        number(db.db().get(b"counter").unwrap()),
        transactions + 1,
        "{what}"
    );
    drop(db);
    // And what it committed is there after a clean reopen.
    let db = open(
        &out.spec.db_path,
        cut.durability,
        cut.encrypted,
        cut.write_buffer,
    );
    assert_eq!(
        number(db.db().get(b"counter").unwrap()),
        transactions + 1,
        "{what}"
    );
}

/// Commits per writer. Every commit that takes a sequence writes one record
/// to the log, and the synced batches alone make this many fsyncs at either
/// durability: enough for every cut below to fire.
const COMMITS: usize = 60;
/// Plain, then encrypted at rest.
const SEALED: [bool; 2] = [false, true];
const DURABILITIES: [DurabilityMode; 2] = [DurabilityMode::Immediate, DurabilityMode::Eventual];
const TEARS: [TearMode; 4] = [
    TearMode::Truncate,
    TearMode::Zero,
    TearMode::Garbage,
    TearMode::TornSector,
];

/// Decided, not written: the commit holds its slot, its sequences and its
/// place in the log, and no byte of its record is on disk.
#[test]
fn a_cut_before_a_decided_record_is_written() {
    for encrypted in SEALED {
        for durability in DURABILITIES {
            for nth in [2, 17, 60, 140] {
                Cut {
                    durability,
                    encrypted,
                    trigger: log_syscall(DieKind::Write, nth, true),
                    point: Where::End,
                    tear: TearMode::Truncate,
                    write_buffer: SMALL_BUFFER,
                }
                .run_and_check(COMMITS);
            }
        }
    }
}

/// Written, not synced, with everything a device does to bytes it never
/// promised to keep. Also the state of a commit applied or published at
/// `Eventual` when the power goes.
#[test]
fn a_cut_after_a_record_is_written_and_before_it_is_synced() {
    for encrypted in SEALED {
        for durability in DURABILITIES {
            for nth in [3, 40, 120] {
                for tear in TEARS {
                    Cut {
                        durability,
                        encrypted,
                        trigger: log_syscall(DieKind::Write, nth, false),
                        point: Where::End,
                        tear,
                        write_buffer: SMALL_BUFFER,
                    }
                    .run_and_check(COMMITS);
                }
            }
        }
    }
}

/// Written and about to be synced: the round is claimed and its fsync has
/// not happened.
#[test]
fn a_cut_before_a_round_syncs() {
    for encrypted in SEALED {
        for durability in DURABILITIES {
            for nth in [1, 9, 30] {
                for tear in [TearMode::Truncate, TearMode::TornSector] {
                    Cut {
                        durability,
                        encrypted,
                        trigger: log_syscall(DieKind::Fsync, nth, true),
                        point: Where::End,
                        tear,
                        write_buffer: SMALL_BUFFER,
                    }
                    .run_and_check(COMMITS);
                }
            }
        }
    }
}

/// Durable, and not yet published or acknowledged: the fsync returned and
/// the round has not gone on.
#[test]
fn a_cut_after_a_round_synced_and_before_it_published() {
    for encrypted in SEALED {
        for durability in DURABILITIES {
            for nth in [1, 9, 30] {
                Cut {
                    durability,
                    encrypted,
                    trigger: log_syscall(DieKind::Fsync, nth, false),
                    point: Where::End,
                    tear: TearMode::Truncate,
                    write_buffer: SMALL_BUFFER,
                }
                .run_and_check(COMMITS);
            }
        }
    }
}

/// The durable watermark itself: the journal of a long run cut exactly
/// before and exactly after one of its syncs (the first, the middle one, the
/// last), whatever the threads were doing then.
///
/// One memtable holds the whole run. The reconstruction cannot bring back a
/// log a flush retired after the cut, so a cut placed before the child died
/// is sound only for a run that retires none: with small memtables this
/// test fails, each time on a log unlinked after the cut point, which is the
/// harness's limit and not the engine's. The cuts placed where the child
/// died, above, are the ones that run with rotations and flushes.
#[test]
fn a_cut_at_a_sync_boundary_keeps_the_prefix_the_sync_covered() {
    for encrypted in SEALED {
        for durability in DURABILITIES {
            for part in [(0, 1), (1, 2), (1, 1)] {
                for after in [false, true] {
                    Cut {
                        durability,
                        encrypted,
                        trigger: log_syscall(DieKind::Write, 150, false),
                        point: Where::Sync { after, part },
                        tear: TearMode::Truncate,
                        write_buffer: ONE_BUFFER,
                    }
                    .run_and_check(COMMITS);
                }
            }
        }
    }
}

#[test]
fn the_clean_run_lands_every_commit() {
    for encrypted in SEALED {
        for durability in DURABILITIES {
            let cut = Cut {
                durability,
                encrypted,
                trigger: Trigger::None,
                point: Where::End,
                tear: TearMode::Truncate,
                write_buffer: SMALL_BUFFER,
            };
            let dir = TempDir::new().unwrap();
            let spec = ChildSpec::new(Phase::Custom(STAGES.to_string()), dir.path().join("db"))
                .ops(10)
                .durability(durability)
                .encrypted(encrypted)
                .write_buffer_size(SMALL_BUFFER);
            let out = CrashRun::new(spec).trigger(Trigger::None).run();
            out.assert_clean();
            let db = open(&out.spec.db_path, durability, encrypted, SMALL_BUFFER);
            for t in 0..THREADS {
                assert_eq!(
                    survivors(&db, t, 10, &cut.describe()),
                    10,
                    "a clean run lost a commit of thread {t}"
                );
            }
            assert_eq!(number(db.db().get(b"counter").unwrap()), 20);
        }
    }
}
