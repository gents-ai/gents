//! The order the commit pipeline keeps, against a real database: concurrent
//! commits decide as the same commits run one at a time in sequence order,
//! an abort leaves no hole, a thread stopped in any stage holds nobody up,
//! and neither a full ring nor a lap of abandoned reservations wedges it.

use std::collections::BTreeMap;
use std::thread;

use proptest::prelude::*;
use tempfile::TempDir;

use super::super::ValidationSet;
use super::early::EarlyVerdict;
use super::hook::Stage;
use super::park::Help;
use super::pipeline::Turn;
use super::pipeline_tests::{
    arm_sync_failure, durable_put, eventual_put, install, key, open_engine, put_with, seq_of,
    stopped_at,
};
use super::write_unit::Await;
use super::*;
use crate::sync::internal::Mutex;

/// More reservations abandoned than the ring has cells: the cells of the
/// first lap's void slots are voided again for the second lap before either
/// frontier has passed the first. The frontiers pass every one of them.
#[test]
fn more_abandoned_reservations_than_the_ring_holds_do_not_wedge_it() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let ring = &engine.pipe().unwrap().ring;
    let abandoned = 2 * ring.capacity() + 3;
    for _ in 0..abandoned {
        ring.reserve();
    }
    let seq = engine
        .submit(durable_put(b"later", b"value"))
        .expect("a later writer passes every abandoned slot");
    assert_eq!(seq, 1);
    assert!(engine.pipeline_is_idle());
    assert_eq!(
        engine.get(&key(b"later"), engine.snapshot_seq()).unwrap(),
        Some(b"value".to_vec())
    );
}

/// A lap of reservations voided by their owners before either frontier
/// reached them, as an owner that finds its cell busy voids its own. The
/// next lap's commit takes one of their cells, and the frontiers, reaching
/// an index whose cell has moved on, pass it (R6).
#[test]
fn a_void_slot_whose_cell_took_the_next_lap_is_passed_by_both_frontiers() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let ring = &engine.pipe().unwrap().ring;
    let first = ring.reserve();
    for _ in 1..ring.capacity() {
        ring.reserve();
    }
    for index in first..first + ring.capacity() {
        assert!(ring.void(index), "a reserved slot is voided");
    }
    assert_eq!(ring.decided(), first - 1, "no frontier has moved");
    let seq = engine
        .submit(durable_put(b"later", b"value"))
        .expect("a commit whose cell held a void slot commits");
    assert_eq!(seq, 1);
    assert!(engine.pipeline_is_idle());
}

/// A failure found with the ring full. No cell is free, so the rollback
/// cannot wait for a slot of its own: it runs outside the ring, fails every
/// unpublished commit, and the commit that found the ring full goes on.
#[test]
fn a_failed_sync_found_with_the_ring_full_is_rolled_back_without_a_slot() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let capacity = engine.pipe().unwrap().ring.capacity();
    let base = engine.snapshot_seq();
    let commits: Vec<_> = (0..capacity)
        .map(|i| install(&engine, durable_put(format!("f{i:04}").as_bytes(), b"v")))
        .collect();
    {
        let _fault = arm_sync_failure(&dir);
        // The sync this commit runs to make room fails. The commit itself
        // was not decided yet: it is decided after the rollback, and its
        // own sync fails too.
        let err = engine
            .submit(durable_put(b"over", b"v"))
            .expect_err("the commit's own sync fails");
        assert!(err.to_string().contains("injected"), "{err}");
    }
    for (i, (_, desc)) in commits.iter().enumerate() {
        let err = seq_of(desc).expect_err("a commit the failed sync covered must learn it");
        assert!(err.to_string().contains("injected"), "commit {i}: {err}");
    }
    assert_eq!(engine.snapshot_seq(), base);
    assert_eq!(engine.last_decided_seq(), base);
    assert!(engine.pipeline_is_idle());
    let seq = engine
        .submit(durable_put(b"after", b"v"))
        .expect("the ring goes on");
    assert_eq!(seq, base + 1);
}

/// A rollback that runs while a thread is part way through deciding a slot.
/// The thread computed the slot's decision from the state before the
/// rollback and stands between installing it and applying it. The rollback
/// does not wait for it, and when the thread goes on, its decision lands
/// nothing: the slot is failed, its operations are in no memtable a reader
/// is shown, and its sequence is taken by the next commit.
#[test]
fn a_decision_computed_before_a_rollback_lands_nothing() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    engine.submit(eventual_put(b"kept", b"v")).unwrap();
    let base = engine.snapshot_seq();
    let stale = stopped_at(
        Stage::Apply,
        || engine.submit(eventual_put(b"stale", b"v")),
        || {
            // The log fails with nothing decided and unpublished behind
            // the frontier: the round rolls back to where the pipeline
            // already stands, and moves it to a new epoch.
            let pipe = engine.pipe().unwrap();
            let log = Arc::clone(&pipe.tip.load().after.log);
            log.poison(io::Error::other("injected log failure"));
            assert!(engine.sync_round(pipe, Help::Wait).is_ok());
            assert!(!log.is_poisoned(), "the round rolled the log back");
        },
    );
    stale.expect_err("a commit decided from a discarded state did not fail");
    assert_eq!(
        engine.get(&key(b"stale"), u64::MAX).unwrap(),
        None,
        "the stale decision was applied to the memtable readers are shown"
    );
    assert_eq!(engine.last_decided_seq(), base);
    assert_eq!(
        engine.get(&key(b"kept"), engine.snapshot_seq()).unwrap(),
        Some(b"v".to_vec())
    );
    let seq = engine
        .submit(durable_put(b"after", b"v"))
        .expect("the pipeline goes on");
    assert_eq!(seq, base + 1, "the stale decision kept its sequence");
    assert!(engine.pipeline_is_idle());
    drop(engine);
    // Nothing of the stale commit reached the log either.
    let engine = open_engine(&dir);
    assert_eq!(engine.snapshot_seq(), base + 1);
    assert_eq!(engine.get(&key(b"stale"), u64::MAX).unwrap(), None);
    assert_eq!(
        engine.get(&key(b"after"), u64::MAX).unwrap(),
        Some(b"v".to_vec())
    );
}

/// A unit's body runs holding the round: no sync and no rollback of another
/// thread runs under it.
#[test]
fn a_units_body_runs_holding_the_round() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let held = engine
        .ordered(|engine, _| {
            let pipe = engine.pipe()?;
            Ok(!engine.current_round(pipe).job.is_runnable())
        })
        .unwrap();
    assert!(held, "the round could be claimed while a unit's body ran");
    let pipe = engine.pipe().unwrap();
    assert!(
        engine.current_round(pipe).job.is_runnable(),
        "the unit let the round go"
    );
}

/// Copy the directory `from` of a database that is open, as a power cut
/// would leave it if every byte written so far had reached the disk.
///
/// The lock file is left out, as every copy of a live directory in `tests/`
/// leaves it out: it is no byte of the database, and Windows lets no other
/// handle read it while the database holds it. Every other file is read
/// through a second handle. The database's own handles on its log and its
/// manifest share reads, on Windows too (`StdEnv::open_log`), so the read
/// sees every byte written so far.
fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else if entry.file_name() != "LOCK" {
            std::fs::write(target, std::fs::read(entry.path()).unwrap()).unwrap();
        }
    }
}

/// Records are written by position, so a later commit's record can be on
/// disk while an earlier one's is not. Neither commit is published then,
/// and recovery replays neither: it keeps the prefix with no hole
/// (`proofs/tla/WalRecovery.tla`), and the database goes on from there.
#[test]
fn a_record_written_ahead_of_an_unwritten_one_is_not_recovered() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    engine.submit(durable_put(b"kept", b"v")).unwrap();
    let base = engine.snapshot_seq();
    let (_, first) = install(&engine, eventual_put(b"first", b"v"));
    let (second_index, second) = install(&engine, eventual_put(b"second", b"v"));
    let pipe = engine.pipe().unwrap();
    assert!(matches!(
        engine.help_decide(pipe, second_index, Help::Wait, None),
        Turn::Done
    ));
    // Only the second commit's record is written: a hole lies before it.
    // Its owner fills it and the write unit writes it; the first is not
    // filled, so no run takes it.
    engine.fill(&second);
    let _ = engine.bring_written(pipe, &second, Await::Copy);
    assert_eq!(engine.snapshot_seq(), base, "neither commit is published");

    let cut = TempDir::new().unwrap();
    copy_dir(dir.path(), cut.path());
    // The live database finishes both.
    engine.drive(pipe, second_index, &second);
    seq_of(&first).unwrap();
    seq_of(&second).unwrap();
    drop(engine);

    let recovered = open_engine(&cut);
    assert_eq!(
        recovered.get(&key(b"kept"), u64::MAX).unwrap(),
        Some(b"v".to_vec())
    );
    assert_eq!(
        recovered.get(&key(b"second"), u64::MAX).unwrap(),
        None,
        "a record past a hole in the log was replayed"
    );
    assert_eq!(recovered.get(&key(b"first"), u64::MAX).unwrap(), None);
    assert_eq!(recovered.snapshot_seq(), base);
    let seq = recovered
        .submit(durable_put(b"after", b"v"))
        .expect("the recovered database takes writes");
    assert_eq!(seq, base + 1);
    drop(recovered);
    let again = open_engine(&cut);
    assert_eq!(again.snapshot_seq(), base + 1);
    assert_eq!(
        again.get(&key(b"after"), u64::MAX).unwrap(),
        Some(b"v".to_vec())
    );
    assert_eq!(again.get(&key(b"second"), u64::MAX).unwrap(), None);
}

/// A thread stopped at any stage of its own commit holds nobody up: the
/// commits behind it bring its slot through, or void it, and go on. When it
/// runs again its commit is done, or it takes another slot.
#[test]
fn a_thread_stopped_at_any_stage_holds_no_other_commit() {
    for stage in [
        Stage::Reserve,
        Stage::Decide,
        Stage::Apply,
        Stage::Write,
        Stage::Sync,
        Stage::Publish,
    ] {
        for durability in [DurabilityMode::Immediate, DurabilityMode::Eventual] {
            // Only a commit that waits for a sync reaches the sync stage.
            if stage == Stage::Sync && durability == DurabilityMode::Eventual {
                continue;
            }
            let dir = TempDir::new().unwrap();
            let engine = open_engine(&dir);
            let stopped = stopped_at(
                stage,
                || engine.submit(put_with(b"stopped", b"v", durability)),
                || {
                    // Three writers commit while the first stands still.
                    thread::scope(|scope| {
                        for w in 0..3 {
                            let engine = &engine;
                            scope.spawn(move || {
                                for i in 0..20 {
                                    let name = format!("w{w}_{i:02}");
                                    engine
                                        .submit(put_with(name.as_bytes(), b"v", durability))
                                        .unwrap_or_else(|err| {
                                            panic!("{stage:?} {durability:?}: {name}: {err}")
                                        });
                                }
                            });
                        }
                    });
                    for w in 0..3 {
                        for i in 0..20 {
                            let name = format!("w{w}_{i:02}");
                            assert_eq!(
                                engine
                                    .get(&key(name.as_bytes()), engine.snapshot_seq())
                                    .unwrap(),
                                Some(b"v".to_vec()),
                                "{stage:?} {durability:?}: {name} is not visible"
                            );
                        }
                    }
                },
            );
            stopped.unwrap_or_else(|err| panic!("{stage:?} {durability:?}: {err}"));
            assert_eq!(
                engine.get(&key(b"stopped"), engine.snapshot_seq()).unwrap(),
                Some(b"v".to_vec()),
                "{stage:?} {durability:?}"
            );
            assert!(engine.pipeline_is_idle(), "{stage:?} {durability:?}");
        }
    }
}

/// An ordered unit is claimed, not owned: when the thread that asked for it
/// stands still after installing it, the next commit runs its body.
#[test]
fn an_ordered_unit_whose_owner_stopped_is_run_by_the_next_commit() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let ran_on = Arc::new(Mutex::new(None));
    let owner = stopped_at(
        Stage::Decide,
        || {
            let ran_on = Arc::clone(&ran_on);
            let owner = thread::current().id();
            engine
                .ordered(move |engine, ord| {
                    *ran_on.lock() = Some(thread::current().id());
                    assert!(
                        engine.pipeline_slots_taken() > 0
                            && ord.last_seq() == engine.snapshot_seq(),
                        "a unit runs with every commit before it published"
                    );
                    Ok(())
                })
                .unwrap();
            owner
        },
        || {
            engine
                .submit(eventual_put(b"behind", b"v"))
                .expect("a commit behind a unit whose owner stopped commits");
        },
    );
    let ran = ran_on.lock().take().expect("the unit ran");
    assert_ne!(ran, owner, "the commit behind the unit ran it");
}

/// A sequence of commits and aborts leaves no hole: a commit that lost a
/// conflict takes no sequence and no byte of the log.
#[test]
fn an_abort_takes_no_sequence_and_no_record() {
    let dir = TempDir::new().unwrap();
    {
        let engine = open_engine(&dir);
        let base = engine.snapshot_seq();
        let txn = |name: &[u8], value: &[u8], snapshot: u64| {
            let mut points = BTreeMap::new();
            points.insert(key(name), Some(value.to_vec()));
            engine.commit_optimistic(
                ValidationSet {
                    reads: Vec::new(),
                    writes_at: Some(snapshot),
                    blind_merges_commute: false,
                    exempt: Vec::new(),
                    ranges: Vec::new(),
                },
                points,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                DurabilityMode::Eventual,
            )
        };
        assert!(matches!(
            txn(b"a", b"v1", base).unwrap(),
            CommitOutcome::Ok { seq: Some(seq) } if seq == base + 1
        ));
        let wal_before = engine.log_end();
        // A commit that passes the check made before the pipeline, and is
        // stale by the time its slot is decided: `a` is written above its
        // snapshot while it stands between its reservation and its install.
        let lost = stopped_at(
            Stage::Reserve,
            || txn(b"a", b"v0", base + 1),
            || {
                assert!(matches!(
                    txn(b"a", b"v2", base + 1).unwrap(),
                    CommitOutcome::Ok { seq: Some(seq) } if seq == base + 2
                ));
            },
        );
        assert!(matches!(lost.unwrap(), CommitOutcome::Conflict(_)));
        let wal_after = engine.log_end();
        assert_eq!(
            engine.last_decided_seq(),
            base + 2,
            "an abort took a sequence"
        );
        assert!(wal_before < wal_after);
        assert!(matches!(
            txn(b"b", b"v3", base).unwrap(),
            CommitOutcome::Ok { seq: Some(seq) } if seq == base + 3
        ));
        // The two commits around the abort wrote one record each, of one
        // length: the abort between them wrote nothing.
        assert_eq!(
            engine.log_end() - wal_after,
            wal_after - wal_before,
            "an abort took a record"
        );
    }
    // Replay reads a log with no gap in its sequences.
    let engine = open_engine(&dir);
    assert_eq!(engine.snapshot_seq(), 3);
    assert_eq!(
        engine.get(&key(b"a"), u64::MAX).unwrap(),
        Some(b"v2".to_vec())
    );
    assert_eq!(
        engine.get(&key(b"b"), u64::MAX).unwrap(),
        Some(b"v3".to_vec())
    );
}

/// One commit of the model: the keys it writes, with the value each gets.
#[derive(Clone, Debug)]
struct Plan {
    /// Indexes into [`KEYS`].
    keys: Vec<usize>,
    /// A transaction, validated against its snapshot; else a plain write.
    txn: bool,
}

const KEYS: [&[u8]; 4] = [b"a", b"b", b"c", b"d"];

fn plan() -> impl Strategy<Value = Plan> {
    (
        proptest::collection::btree_set(0..KEYS.len(), 1..3),
        prop::bool::weighted(0.7),
    )
        .prop_map(|(keys, txn)| Plan {
            keys: keys.into_iter().collect(),
            txn,
        })
}

/// What one commit of a run did.
#[derive(Debug)]
struct Done {
    snapshot: u64,
    writes: Vec<(usize, Vec<u8>)>,
    txn: bool,
    /// The last sequence it took, or `None` when it lost a conflict.
    seq: Option<u64>,
}

proptest! {
    // Each case opens a database and runs threads, so far fewer than the
    // default 256.
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Commits made by several threads at once decide exactly as the same
    /// commits run one at a time, in sequence order (the order
    /// `PipelineCheck.lean` decides slots in):
    /// - the sequences taken tile a range with no hole and no overlap;
    /// - a transaction committed only if no commit wrote one of its keys
    ///   between its snapshot and its own sequence, and lost only if one
    ///   did;
    /// - every key holds what the commit with the highest sequence wrote,
    ///   live and after a reopen.
    #[test]
    fn concurrent_commits_decide_as_a_serial_run_in_sequence_order(
        plans in proptest::collection::vec(proptest::collection::vec(plan(), 1..10), 2..5),
        immediate in any::<bool>(),
    ) {
        let dir = TempDir::new().unwrap();
        let durability = if immediate { DurabilityMode::Immediate } else { DurabilityMode::Eventual };
        let done: Vec<Done> = {
            let engine = open_engine(&dir);
            let done = Mutex::new(Vec::new());
            thread::scope(|scope| {
                for (t, plans) in plans.iter().enumerate() {
                    let (engine, done) = (&engine, &done);
                    scope.spawn(move || {
                        for (n, plan) in plans.iter().enumerate() {
                            let snapshot = engine.snapshot_seq();
                            let writes: Vec<(usize, Vec<u8>)> = plan
                                .keys
                                .iter()
                                .map(|k| (*k, format!("t{t}n{n}k{k}").into_bytes()))
                                .collect();
                            let ops: Vec<WriteBatchOp> = writes
                                .iter()
                                .map(|(k, value)| WriteBatchOp::Put { key: key(KEYS[*k]), value: value.clone() })
                                .collect();
                            let request = if plan.txn {
                                let checks = ValidationSet {
                                    reads: Vec::new(),
                                    writes_at: Some(snapshot),
                                    blind_merges_commute: false,
                                    exempt: Vec::new(),
                                    ranges: Vec::new(),
                                };
                                match engine.check_early(&checks, &ops).unwrap() {
                                    EarlyVerdict::Conflict(_) => {
                                        done.lock().push(Done { snapshot, writes, txn: true, seq: None });
                                        continue;
                                    }
                                    EarlyVerdict::Marks(early) => WriteRequest::Txn(Box::new(TxnRequest {
                                        checks,
                                        ops,
                                        appends: Vec::new(),
                                        durability,
                                        perf: crate::PerfLevel::Disable,
                                        early,
                                        read_queue: None,
                                    })),
                                }
                            } else {
                                WriteRequest::Batch { ops, durability, disable_wal: false }
                            };
                            let seq = match engine.commit_through_pipeline(request).unwrap() {
                                Settled::Write(seq) => Some(seq),
                                Settled::Committed { seq, .. } => seq,
                                Settled::Conflict { .. } => None,
                            };
                            done.lock().push(Done { snapshot, writes, txn: plan.txn, seq });
                        }
                    });
                }
            });
            prop_assert!(engine.pipeline_is_idle());
            let done = done.into_inner();

            // No hole, no overlap: the committed ranges tile 1..=last.
            let mut ranges: Vec<(u64, u64)> = done
                .iter()
                .filter_map(|d| d.seq.map(|last| (last + 1 - d.writes.len() as u64, last)))
                .collect();
            ranges.sort();
            let mut next = 1;
            for (first, last) in &ranges {
                prop_assert_eq!(*first, next, "a hole or an overlap in the sequences: {:?}", ranges);
                next = last + 1;
            }
            prop_assert_eq!(engine.snapshot_seq(), next - 1);
            prop_assert_eq!(engine.last_decided_seq(), next - 1);

            // Every write, by key, with its sequence.
            let mut written: Vec<Vec<u64>> = vec![Vec::new(); KEYS.len()];
            for d in &done {
                if let Some(last) = d.seq {
                    let first = last + 1 - d.writes.len() as u64;
                    for ((k, _), seq) in d.writes.iter().zip(first..) {
                        written[*k].push(seq);
                    }
                }
            }
            for d in done.iter().filter(|d| d.txn) {
                let overtaken = |upto: u64| {
                    d.writes.iter().any(|(k, _)| {
                        written[*k].iter().any(|seq| *seq > d.snapshot && *seq < upto)
                    })
                };
                match d.seq {
                    Some(last) => {
                        let first = last + 1 - d.writes.len() as u64;
                        prop_assert!(
                            !overtaken(first),
                            "a transaction committed over a write it did not see: {:?}", d
                        );
                    }
                    None => prop_assert!(
                        overtaken(u64::MAX),
                        "a transaction lost a conflict nobody caused: {:?}", d
                    ),
                }
            }
            check_final_state(&engine, &done)?;
            done
        };
        let engine = open_engine(&dir);
        check_final_state(&engine, &done)?;
    }
}

/// Every key holds what the commit with the highest sequence wrote to it.
fn check_final_state(engine: &RegolithEngine, done: &[Done]) -> Result<(), TestCaseError> {
    for (k, name) in KEYS.iter().enumerate() {
        let newest = done
            .iter()
            .filter_map(|d| {
                let last = d.seq?;
                let first = last + 1 - d.writes.len() as u64;
                d.writes
                    .iter()
                    .zip(first..)
                    .find(|((key, _), _)| *key == k)
                    .map(|((_, value), seq)| (seq, value.clone()))
            })
            .max();
        prop_assert_eq!(
            engine.get(&key(name), engine.snapshot_seq()).unwrap(),
            newest.map(|(_, value)| value),
            "key {:?}",
            name
        );
    }
    Ok(())
}
