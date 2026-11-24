//! Scenario 10: projected reads under `DefraLevel`.
//!
//! A value is three counters, one per part, and an operand adds to one part
//! and touches no other (`common/parted.rs`). A read through `get_parts`
//! names the parts its decision used.
//!
//! One document. Eight transactions each read one part of the same document
//! and merge an increment into that part, thread `t` on part `t mod 3`, and
//! only when all eight have done so does the first of them commit. At
//! `DefraLevel` on the optimistic flavour a read by parts is refused only by
//! a write that touches a part it named, so the first commit on each part
//! survives: three of the eight. Anywhere else `get_parts` is `get`, the
//! document was read whole, and only the first commit of all survives. The
//! refused retry until they are in, and each part ends at one increment per
//! thread that works on it, also after a compaction has folded the operands.
//!
//! Forced writes. Each of eight readers reads a document of its own by
//! parts, and when all eight have, a write to each document is forced before
//! any reader commits a key of its own. Where the level projects:
//!
//! * a read that named a part is refused by an operand on that part, a put
//!   of other bytes, a delete or a range delete, and by neither an operand on
//!   another part nor a put of the bytes it read;
//! * a read that named no part is never refused;
//! * a read that found nothing is validated in whole: whatever makes the
//!   document appear refuses it.
//!
//! Where it does not, the forced write refuses the commit exactly when the
//! level validates a point read.
//!
//! On `Tables` the first commit on the shared document is made alone and
//! flushed behind, and the forced writes are flushed before the commits, so a
//! refused commit is judged against a version that lives in a table.
//!
//! Threads on the shared document never assert between two rendezvous, since
//! a thread that panicked would strand the rest; they record what they saw
//! and the test judges after the scope has ended.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use regolith::{
    Access, Db, IsolationLevel, OptimisticTransactionDb, Transaction, TransactionDb,
    TransactionError, TxResult, WriteKind,
};

use super::parted::{PARTS, Parted, add, parts_of, value};
use super::support::{Findings, Rendezvous, STORAGES, Storage, forced_between, table_lookups};
use super::{Flavour, LEVELS, THREADS, commit_with_retry, validates_point_reads};

const ROUNDS: usize = 3;

const DOCUMENT: &[u8] = b"document";

fn optimistic(dir: &tempfile::TempDir, storage: Storage) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(
        dir.path(),
        storage.options().merge_operator(Some(Arc::new(Parted))),
    )
    .unwrap()
}

fn pessimistic(dir: &tempfile::TempDir, storage: Storage) -> TransactionDb {
    TransactionDb::open(
        dir.path(),
        storage.options().merge_operator(Some(Arc::new(Parted))),
    )
    .unwrap()
    .with_lock_timeout(std::time::Duration::from_millis(200))
}

fn stored(db: &Db, key: &[u8]) -> Option<[u64; PARTS]> {
    db.get(key)
        .unwrap()
        .map(|bytes| parts_of(&bytes).expect("a value of three parts"))
}

// ---- one document ---------------------------------------------------------

/// The part thread `t` reads and increments.
fn part_of(t: usize) -> u32 {
    (t % PARTS) as u32
}

/// Begin, read thread `t`'s part of the document and merge an increment into
/// that part.
fn increment(
    db: &OptimisticTransactionDb,
    level: IsolationLevel,
    t: usize,
) -> TxResult<Transaction> {
    let tx = db.begin_at(level);
    tx.get_parts(DOCUMENT, &[part_of(t)])?;
    tx.merge(DOCUMENT, &add(part_of(t), 1))?;
    Ok(tx)
}

fn parts_overlap(level: IsolationLevel, storage: Storage) {
    let case = format!("{storage:?} {level:?}");
    let dir = tempfile::tempdir().unwrap();
    let db = optimistic(&dir, storage);
    db.db().put(DOCUMENT, &value([0; PARTS])).unwrap();
    storage.flush(db.db());

    let sync = Rendezvous::new(THREADS);
    let findings = Findings::default();
    let first_commits = AtomicU64::new(0);
    let table_probes = AtomicU64::new(0);
    let projects = level == IsolationLevel::DefraLevel;
    // One commit per part in use where the level projects, one in all where
    // the document is read whole.
    let expected = if projects {
        PARTS.min(THREADS) as u64
    } else {
        1
    };

    thread::scope(|scope| {
        for t in 0..THREADS {
            let (case, db, sync) = (&case, &db, &sync);
            let (findings, first_commits, table_probes) =
                (&findings, &first_commits, &table_probes);
            scope.spawn(move || {
                for round in 0..ROUNDS {
                    sync.wait(); // the previous round is settled
                    let staged = increment(db, level, t);
                    sync.wait(); // all eight have read and merged
                    let tx = match staged {
                        Ok(tx) => tx,
                        Err(e) => panic!("{case} round {round} thread {t}: {e}"),
                    };

                    if storage.is_tables() && t != 0 {
                        sync.wait(); // thread 0 has committed and flushed
                    }
                    let (first, lookups) = table_lookups(|| tx.commit());
                    if storage.is_tables() {
                        if t == 0 {
                            storage.flush(db.db());
                            sync.wait();
                        } else if part_of(t) == part_of(0) {
                            // Refused by thread 0's operand, which only the
                            // table holds.
                            table_probes.fetch_add(lookups, Ordering::Relaxed);
                        }
                        // No retry may commit before every first attempt is
                        // in, or it would be what refuses a first attempt.
                        sync.wait();
                    }
                    match first {
                        Ok(_) => {
                            first_commits.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(TransactionError::Conflict(conflict)) => {
                            findings.check(conflict.key() == DOCUMENT, || {
                                format!("{case} round {round}: conflict on {:?}", conflict.key())
                            });
                            let reason = (conflict.mine(), conflict.theirs());
                            findings.check(
                                !projects || reason == (Access::ReadParts, WriteKind::Merge),
                                || {
                                    format!(
                                        "{case} round {round} thread {t}: refused as {reason:?}"
                                    )
                                },
                            );
                            commit_with_retry("increment a part", || {
                                increment(db, level, t)?.commit()
                            });
                        }
                        Err(other) => panic!("{case} round {round}: {other}"),
                    }
                    sync.wait(); // all eight are in

                    if t == 0 {
                        let probes = table_probes.swap(0, Ordering::Relaxed);
                        findings.check(!storage.is_tables() || probes > 0, || {
                            format!("{case} round {round}: no refused commit probed a table")
                        });
                        let commits = first_commits.swap(0, Ordering::Relaxed);
                        findings.check(commits == expected, || {
                            format!(
                                "{case} round {round}: {commits} of {THREADS} commits \
                                 succeeded at once, expected {expected}"
                            )
                        });
                        let mut model = [0u64; PARTS];
                        for t in 0..THREADS {
                            model[part_of(t) as usize] += round as u64 + 1;
                        }
                        let held = stored(db.db(), DOCUMENT);
                        findings.check(held == Some(model), || {
                            format!("{case} round {round}: the document is {held:?}, not {model:?}")
                        });
                    }
                }
            });
        }
    });

    let before = stored(db.db(), DOCUMENT);
    db.db().compact_range(None, None).wait().unwrap();
    findings.check(stored(db.db(), DOCUMENT) == before, || {
        format!("{case}: a compaction changed the document from {before:?}")
    });
    findings.judge();
}

#[test]
fn increments_of_one_document_refuse_each_other_only_on_the_part_they_share() {
    for storage in STORAGES {
        for level in LEVELS {
            parts_overlap(level, storage);
        }
    }
}

// ---- forced writes --------------------------------------------------------

/// The document reader `r` reads in the `case`-th case of a run.
fn document(case: usize, r: usize) -> Vec<u8> {
    format!("document/{case:03}-{r}").into_bytes()
}

/// The key reader `r` writes its decision to in that case.
fn decision(case: usize, r: usize) -> Vec<u8> {
    format!("decision/{case:03}-{r}").into_bytes()
}

/// What the document holds when the reader reads it.
const SEEDED: [u64; PARTS] = [1, 1, 1];

/// What the reader's `get_parts` meets and names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reads {
    /// The document is there, and the read names part 0.
    OnePart,
    /// The document is there, and the read names no part.
    NoPart,
    /// The document is not there, and the read names part 0.
    Nothing,
}

const READS: [Reads; 3] = [Reads::OnePart, Reads::NoPart, Reads::Nothing];

impl Reads {
    fn parts(self) -> &'static [u32] {
        match self {
            Self::OnePart | Self::Nothing => &[0],
            Self::NoPart => &[],
        }
    }

    fn seeded(self) -> bool {
        self != Self::Nothing
    }
}

/// One write landed on a document between a reader's read and its commit.
#[derive(Clone, Copy, Debug)]
enum Forced {
    /// An operand on the part the reader may have named.
    OperandOnNamed,
    /// An operand on a part the reader did not name.
    OperandOnOther,
    /// A put of the bytes the document was seeded with.
    PutSeeded,
    /// A put that changes a part the reader did not name.
    PutOther,
    Delete,
    RangeDelete,
}

const FORCED: [Forced; 6] = [
    Forced::OperandOnNamed,
    Forced::OperandOnOther,
    Forced::PutSeeded,
    Forced::PutOther,
    Forced::Delete,
    Forced::RangeDelete,
];

impl Forced {
    fn land(self, db: &Db, key: &[u8]) {
        match self {
            Self::OperandOnNamed => db.merge(key, &add(0, 1)).unwrap(),
            Self::OperandOnOther => db.merge(key, &add(2, 1)).unwrap(),
            Self::PutSeeded => db.put(key, &value(SEEDED)).unwrap(),
            Self::PutOther => db.put(key, &value([1, 1, 2])).unwrap(),
            Self::Delete => db.delete(key).unwrap(),
            Self::RangeDelete => {
                // The document and nothing else.
                let mut end = key.to_vec();
                end.push(0);
                db.delete_range(key, &end).unwrap();
            }
        }
    }

    fn kind(self) -> WriteKind {
        match self {
            Self::OperandOnNamed | Self::OperandOnOther => WriteKind::Merge,
            Self::PutSeeded | Self::PutOther => WriteKind::Put,
            Self::Delete => WriteKind::Delete,
            Self::RangeDelete => WriteKind::RangeDelete,
        }
    }

    /// Whether this write refuses a projected read of `reads`.
    fn refuses(self, reads: Reads) -> bool {
        match reads {
            Reads::NoPart => false,
            Reads::OnePart => !matches!(self, Self::OperandOnOther | Self::PutSeeded),
            // The read found nothing, and a removal leaves nothing.
            Reads::Nothing => !matches!(self, Self::Delete | Self::RangeDelete),
        }
    }
}

/// Every kind of read by parts against every forced write, in one database:
/// each case has documents of its own.
fn forced_writes<D: Flavour>(db: &D, level: IsolationLevel, storage: Storage, projects: bool) {
    let cases: Vec<(Reads, Forced)> = READS
        .iter()
        .flat_map(|reads| FORCED.iter().map(move |forced| (*reads, *forced)))
        .collect();
    for (case, (reads, _)) in cases.iter().enumerate() {
        if reads.seeded() {
            for r in 0..THREADS {
                regolith::through_stalls(|| db.raw().put(&document(case, r), &value(SEEDED)))
                    .unwrap();
            }
        }
    }
    storage.flush(db.raw());

    for (case, (reads, forced)) in cases.into_iter().enumerate() {
        let name = format!("{storage:?} {level:?} projects={projects} {reads:?} {forced:?}");
        let refused = if projects {
            forced.refuses(reads).then_some(Access::ReadParts)
        } else {
            validates_point_reads(level).then_some(Access::Read)
        };
        forced_between(
            THREADS,
            |r| {
                let tx = db.begin_at(level);
                let read = tx.get_parts(&document(case, r), reads.parts()).unwrap();
                assert_eq!(
                    read.map(|bytes| parts_of(&bytes).unwrap()),
                    reads.seeded().then_some(SEEDED),
                    "{name}: reader {r} reads the whole value"
                );
                tx
            },
            || {
                for r in 0..THREADS {
                    forced.land(db.raw(), &document(case, r));
                }
                // The commits that follow must find these versions in a table.
                storage.flush(db.raw());
            },
            |r, tx| {
                tx.put(&decision(case, r), b"decided").unwrap();
                let (result, lookups) = table_lookups(|| tx.commit());
                match (refused, result) {
                    (None, Ok(_)) => {}
                    (Some(access), Err(TransactionError::Conflict(conflict))) => {
                        assert_eq!(
                            (conflict.key(), conflict.mine(), conflict.theirs()),
                            (&document(case, r)[..], access, forced.kind()),
                            "{name}: reader {r}"
                        );
                        // A range tombstone is kept with its table's metadata,
                        // so finding one over a key no table holds reads no
                        // block.
                        let reads_a_block =
                            reads.seeded() || !matches!(forced, Forced::RangeDelete);
                        assert!(
                            !storage.is_tables() || !reads_a_block || lookups > 0,
                            "{name}: the refused commit never probed the table"
                        );
                    }
                    (expected, got) => {
                        panic!("{name}: reader {r} expected {expected:?}, got {got:?}")
                    }
                }
            },
        );
    }
}

#[test]
fn a_read_by_parts_is_refused_only_by_a_write_that_changes_a_part_it_named() {
    for storage in STORAGES {
        for level in LEVELS {
            let dir = tempfile::tempdir().unwrap();
            let projects = level == IsolationLevel::DefraLevel;
            forced_writes(&optimistic(&dir, storage), level, storage, projects);
        }
    }
}

#[test]
fn a_read_by_parts_of_a_pessimistic_transaction_is_a_get_at_every_level() {
    for storage in STORAGES {
        for level in LEVELS {
            let dir = tempfile::tempdir().unwrap();
            forced_writes(&pessimistic(&dir, storage), level, storage, false);
        }
    }
}
