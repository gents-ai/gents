//! Scenario 5: blind merges under `DefraLevel`, on the optimistic flavour.
//!
//! Eight transactions each merge `+1` into one hot counter without reading
//! it, and every one has a write forced onto the counter between its begin
//! and its commit, through the helper thread. All eight snapshots are taken
//! before any forced write lands and every commit comes after the last of
//! them, so all eight forced writes sit inside every transaction's window.
//! A round gives the eight the same kind of write, so whatever lands in a
//! window is of that kind: a round of operands must commit at `DefraLevel`
//! and a round with a put, a delete or a range delete in it must not.
//!
//! The counter is a sum with a partial merge, so compaction may fold operands
//! by itself. A flush or a compaction can follow the first forced write or
//! the last one, which puts a replacement in the memtable, in a table, under
//! operands, or compacted away from its snapshot. After every round, and at
//! the end, the counter must equal a model replayed from what the helper
//! landed.
//!
//! A round of transactions that read the counter before they merge is the
//! control: their merge is no longer blind, so they abort whatever lands.
//!
//! The pessimistic flavour has no counterpart here. Its first merge takes the
//! counter's lock, so eight transactions cannot all have merged before any
//! commits, and its blind writes are not validated at all.
//!
//! Threads here never assert between two rendezvous, since a thread that
//! panicked would strand the rest; they record what they saw and the test
//! judges after the scope has ended.

use std::sync::{Arc, Mutex};

use regolith::{Db, IsolationLevel, MergeOperator, OptimisticTransactionDb, TransactionError};

use super::support::{Findings, Rendezvous, STORAGES, Storage, table_lookups};
use super::{Flavour, LEVELS, THREADS, commit_with_retry, with_forced_writes};

const COUNTER: &[u8] = b"counter";

fn le(bytes: &[u8]) -> Option<i64> {
    Some(i64::from_le_bytes(bytes.try_into().ok()?))
}

fn counter(db: &Db) -> Option<i64> {
    db.get(COUNTER)
        .unwrap()
        .map(|raw| le(&raw).expect("an 8-byte counter"))
}

/// Sums little-endian `i64` operands onto a base. It folds two adjacent
/// operands too, so a compaction may collapse a chain without a base.
struct Sum;

impl MergeOperator for Sum {
    fn name(&self) -> &'static str {
        "sum"
    }

    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut total = base.map_or(Some(0), le)?;
        for operand in operands {
            total = total.wrapping_add(le(operand)?);
        }
        Some(total.to_le_bytes().to_vec())
    }

    fn partial_merge(&self, _key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        Some(le(left)?.wrapping_add(le(right)?).to_le_bytes().to_vec())
    }
}

/// One write the helper lands on the counter outside any transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Write {
    Operand(i64),
    Put(i64),
    Delete,
    RangeDelete,
}

impl Write {
    fn land(self, db: &Db) {
        match self {
            Self::Operand(delta) => db.merge(COUNTER, &delta.to_le_bytes()).unwrap(),
            Self::Put(value) => db.put(COUNTER, &value.to_le_bytes()).unwrap(),
            Self::Delete => db.delete(COUNTER).unwrap(),
            // `c..d` covers the counter and nothing else the scenario writes.
            Self::RangeDelete => db.delete_range(b"c", b"d").unwrap(),
        }
    }

    /// The counter after this write lands on `before`; `None` is absent.
    fn model(self, before: Option<i64>) -> Option<i64> {
        match self {
            Self::Operand(delta) => Some(before.unwrap_or(0).wrapping_add(delta)),
            Self::Put(value) => Some(value),
            Self::Delete | Self::RangeDelete => None,
        }
    }
}

/// What the eight transactions force onto the counter in a round.
#[derive(Clone, Copy, Debug)]
enum Kind {
    /// An operand of its own each.
    Operands,
    /// A put of its own each.
    Puts,
    Deletes,
    RangeDeletes,
    /// Reader 0 puts, and lands first; the other seven land operands on top
    /// of it, so the replacement is the oldest write above every snapshot.
    PutUnderOperands,
}

const KINDS: [Kind; 5] = [
    Kind::Operands,
    Kind::Puts,
    Kind::Deletes,
    Kind::RangeDeletes,
    Kind::PutUnderOperands,
];

impl Kind {
    fn write_for(self, reader: usize) -> Write {
        let operand = Write::Operand(10 + reader as i64);
        match self {
            Self::Operands => operand,
            Self::Puts => Write::Put(1000 * (reader as i64 + 1)),
            Self::Deletes => Write::Delete,
            Self::RangeDeletes => Write::RangeDelete,
            Self::PutUnderOperands if reader == 0 => Write::Put(5000),
            Self::PutUnderOperands => operand,
        }
    }

    /// Whether some forced write replaces the counter outright.
    fn replaces(self) -> bool {
        !matches!(self, Self::Operands)
    }
}

/// How a transaction touches the counter before it merges.
#[derive(Clone, Copy, Debug)]
enum Shape {
    /// Only merges: a blind merge.
    Blind,
    /// Reads the counter first, which makes the merge a read-modify-write.
    ReadsFirst,
}

/// When in a round a flush or a compaction runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// After reader 0's forced write and before the other seven.
    Mid,
    /// After all eight, before the commits.
    End,
}

#[derive(Clone, Copy, Debug)]
enum Maintenance {
    None,
    Flush(Stage),
    Compact(Stage),
}

impl Maintenance {
    /// Run at `now` if this is when the maintenance is due.
    fn run_at(self, now: Stage, db: &Db) {
        match self {
            Self::Flush(at) if at == now => db.flush().unwrap(),
            Self::Compact(at) if at == now => db.compact_range(None, None).wait().unwrap(),
            _ => {}
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Round {
    kind: Kind,
    shape: Shape,
    maintenance: Maintenance,
}

/// Every kind under each maintenance, `passes` times over, with the order of
/// the kinds rotated so that each kind follows a different one.
fn schedule(passes: usize, shape: Shape, maintenance: &[Maintenance]) -> Vec<Round> {
    let mut rounds = Vec::new();
    for pass in 0..passes {
        for (i, &maintenance) in maintenance.iter().enumerate() {
            for k in 0..KINDS.len() {
                rounds.push(Round {
                    kind: KINDS[(k + pass + i) % KINDS.len()],
                    shape,
                    maintenance,
                });
            }
        }
    }
    rounds
}

fn blind_merge_rounds(level: IsolationLevel, storage: Storage, rounds: &[Round]) {
    let case = format!("{storage:?} {level:?}");
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(
        dir.path(),
        storage.options().merge_operator(Some(Arc::new(Sum))),
    )
    .unwrap();
    db.db().put(COUNTER, &0i64.to_le_bytes()).unwrap();

    let lockstep = Rendezvous::new(THREADS);
    let findings = Findings::default();
    // The write each reader has staged for the helper, with its round.
    let staged: Vec<Mutex<Option<(usize, Write)>>> =
        (0..THREADS).map(|_| Mutex::new(None)).collect();
    // What the helper landed, in the order it landed it.
    let landed: Mutex<Vec<(usize, Write)>> = Mutex::new(Vec::new());
    // The counter reader 0 found at the end of each round.
    let settled: Mutex<Vec<Option<i64>>> = Mutex::new(Vec::new());

    with_forced_writes(
        &db,
        |db, r| {
            let (round, write) = staged[r]
                .lock()
                .unwrap()
                .take()
                .expect("the reader staged its write before asking");
            write.land(db.raw());
            landed.lock().unwrap().push((round, write));
        },
        |r, force| {
            for (round, plan) in rounds.iter().enumerate() {
                lockstep.wait(); // the previous round is settled
                let tx = db.begin_at(level);
                if matches!(plan.shape, Shape::ReadsFirst) {
                    tx.get(COUNTER).unwrap();
                }
                tx.merge(COUNTER, &1i64.to_le_bytes()).unwrap();
                lockstep.wait(); // all eight snapshots are taken
                *staged[r].lock().unwrap() = Some((round, plan.kind.write_for(r)));
                if r == 0 {
                    force();
                    plan.maintenance.run_at(Stage::Mid, db.db());
                }
                lockstep.wait(); // reader 0's write has landed, and any maintenance after it
                if r != 0 {
                    force();
                }
                lockstep.wait(); // all eight forced writes have landed
                if r == 0 {
                    plan.maintenance.run_at(Stage::End, db.db());
                }
                lockstep.wait(); // the maintenance is done

                let (first, lookups) = table_lookups(|| tx.commit());
                // After a flush or a compaction of all eight writes the memtable
                // holds none of them, so a blind merge's probe, which has to
                // walk down to the snapshot, goes to a table whatever a faster
                // reader's retry has put in the memtable since. A read of the
                // counter is answered by the newest version, and that may be
                // such a retry.
                if matches!(plan.shape, Shape::Blind)
                    && matches!(
                        plan.maintenance,
                        Maintenance::Flush(Stage::End) | Maintenance::Compact(Stage::End)
                    )
                {
                    findings.check(lookups > 0, || {
                        format!("{case} round {round} {plan:?} reader {r}: the commit never reached a table")
                    });
                }
                let commits = level == IsolationLevel::DefraLevel
                    && matches!(plan.shape, Shape::Blind)
                    && !plan.kind.replaces();
                findings.check(first.is_ok() == commits, || {
                    format!(
                        "{case} round {round} {plan:?} reader {r}: expected {}, got {first:?}",
                        if commits { "a commit" } else { "a conflict" }
                    )
                });
                let refused = match first {
                    Ok(_) => 0,
                    Err(TransactionError::Conflict(conflict)) => {
                        let key = conflict.key();
                        findings.check(key == COUNTER, || {
                            format!("{case} round {round} reader {r}: conflict on {key:?}")
                        });
                        commit_with_retry("increment", || {
                            let tx = db.begin_at(level);
                            tx.merge(COUNTER, &1i64.to_le_bytes())?;
                            tx.commit()
                        })
                    }
                    Err(other) => panic!("{case} round {round} reader {r}: {other}"),
                };
                // Operands commute at `DefraLevel`, and no retry has a
                // replacement to meet, so none of them is refused.
                findings.check(level != IsolationLevel::DefraLevel || refused == 0, || {
                    format!("{case} round {round} reader {r}: {refused} retries refused")
                });
                lockstep.wait(); // all eight increments are in
                if r == 0 {
                    settled.lock().unwrap().push(counter(db.db()));
                }
            }
        },
    );

    // Every transaction increments once, after all the forced writes of its
    // round, so a round ends on what the helper left plus one per thread.
    let landed = landed.into_inner().unwrap();
    let settled = settled.into_inner().unwrap();
    let mut model = Some(0);
    for (round, plan) in rounds.iter().enumerate() {
        let writes: Vec<Write> = landed
            .iter()
            .filter(|(landed_in, _)| *landed_in == round)
            .map(|&(_, write)| write)
            .collect();
        findings.check(writes.len() == THREADS, || {
            format!(
                "{case} round {round}: the helper landed {} writes",
                writes.len()
            )
        });
        model = writes
            .into_iter()
            .fold(model, |before, write| write.model(before));
        model = Some(model.unwrap_or(0) + THREADS as i64);
        findings.check(settled.get(round) == Some(&model), || {
            format!(
                "{case} round {round} {plan:?}: the counter is {:?}, the model says {model:?}",
                settled.get(round)
            )
        });
    }
    findings.check(counter(db.db()) == model, || {
        format!(
            "{case}: the final counter is {:?}, the model says {model:?}",
            counter(db.db())
        )
    });
    db.db().compact_range(None, None).wait().unwrap();
    findings.check(counter(db.db()) == model, || {
        format!(
            "{case}: after a last compaction the counter is {:?}, the model says {model:?}",
            counter(db.db())
        )
    });
    findings.judge();
}

#[test]
fn a_blind_merge_aborts_on_a_forced_replacement_and_never_on_a_forced_operand() {
    let maintenance = [
        Maintenance::None,
        Maintenance::Flush(Stage::Mid),
        Maintenance::Flush(Stage::End),
        Maintenance::Compact(Stage::Mid),
        Maintenance::Compact(Stage::End),
    ];
    for storage in STORAGES {
        blind_merge_rounds(
            IsolationLevel::DefraLevel,
            storage,
            &schedule(2, Shape::Blind, &maintenance),
        );
    }
}

#[test]
fn a_merge_that_read_the_counter_first_aborts_on_a_forced_operand_at_defra_level() {
    let maintenance = [Maintenance::None, Maintenance::Flush(Stage::End)];
    for storage in STORAGES {
        blind_merge_rounds(
            IsolationLevel::DefraLevel,
            storage,
            &schedule(1, Shape::ReadsFirst, &maintenance),
        );
    }
}

#[test]
fn below_defra_level_a_blind_merge_aborts_on_a_forced_operand_too() {
    let rounds = schedule(1, Shape::Blind, &[Maintenance::None]);
    for storage in STORAGES {
        for level in LEVELS {
            if level != IsolationLevel::DefraLevel {
                blind_merge_rounds(level, storage, &rounds);
            }
        }
    }
}
