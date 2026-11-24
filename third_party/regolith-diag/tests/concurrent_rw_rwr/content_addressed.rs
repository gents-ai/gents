//! Scenario 8: content-addressed keys under `DefraLevel`, on the optimistic
//! flavour (the pessimistic one takes no key classifier).
//!
//! The database declares the keys under `b/` content-addressed: a block is
//! named by its bytes, so whoever writes the key writes the same value.
//!
//! Creates. Eight transactions each read whether the block of the round is
//! stored, put it with the same bytes and write a document of their own, and
//! only when all eight have done so does the first of them commit. With the
//! classifier, at `DefraLevel`, neither the put nor the read before it is
//! validated, so all eight commit. Anywhere else the read found nothing and
//! is validated, and only the first commit survives: every other level, and
//! `DefraLevel` with no classifier. The refused seven retry until they are
//! in, and every document and the block must be there afterwards.
//!
//! Forced writes. Each of eight transactions takes a stored block of its own,
//! and when all eight have, a write to each block is forced before any of
//! them commits. What the commit makes of it is the class's contract:
//!
//! * a read that found the block is validated for presence only, so a write
//!   that leaves the block there (the same bytes again, an operand) never
//!   refuses it and one that takes it away always does, where an ordinary
//!   read at the same level is refused by an operand too, and one at
//!   `RepeatableRead` by any write at all;
//! * a put of the block is never validated;
//! * a delete of the block is validated like a delete of any key: a write
//!   that leaves the block there refuses it, and one that already took it
//!   away is what the delete would have left.
//!
//! On `Tables` the first create commits alone and is flushed behind, and the
//! forced writes are flushed before the commits, so a refused commit is judged
//! against a version that lives in a table.
//!
//! Threads in the creates never assert between two rendezvous, since a thread
//! that panicked would strand the rest; they record what they saw and the
//! test judges after the scope has ended.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use regolith::{
    Access, IsolationLevel, KeyClass, KeyClassifier, MergeOperator, OptimisticTransactionDb,
    Transaction, TransactionError, TxnOptions, WriteKind,
};

use super::support::{Findings, Rendezvous, STORAGES, Storage, forced_between, table_lookups};
use super::{Flavour, LEVELS, Read, THREADS, commit_with_retry};

const ROUNDS: usize = 3;

/// What every writer of a block stores under it.
const BYTES: &[u8] = b"the bytes the key is the hash of";

/// Keys under `b/` are blocks named by their content.
struct Blocks;

impl KeyClassifier for Blocks {
    fn classify(&self, key: &[u8]) -> KeyClass {
        if key.starts_with(b"b/") {
            KeyClass::ContentAddressed
        } else {
            KeyClass::Ordinary
        }
    }
}

/// An operand that leaves a stored value as it is and stores its own bytes
/// where there is none: the one merge a content-addressed key can take.
struct Keep;

impl MergeOperator for Keep {
    fn name(&self) -> &'static str {
        "keep"
    }

    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        base.or(operands.first().copied()).map(<[u8]>::to_vec)
    }

    fn partial_merge(&self, _key: &[u8], left: &[u8], _right: &[u8]) -> Option<Vec<u8>> {
        Some(left.to_vec())
    }
}

fn open(dir: &tempfile::TempDir, storage: Storage, classified: bool) -> OptimisticTransactionDb {
    let db = OptimisticTransactionDb::open(
        dir.path(),
        storage.options().merge_operator(Some(Arc::new(Keep))),
    )
    .unwrap();
    if classified {
        db.with_policy(Arc::new(Blocks))
    } else {
        db
    }
}

// ---- creates --------------------------------------------------------------

/// The block every thread creates in `round`.
fn shared_block(round: usize) -> Vec<u8> {
    format!("b/round-{round}").into_bytes()
}

/// The document thread `t` writes in `round`.
fn document(round: usize, t: usize) -> Vec<u8> {
    format!("doc/{round}-{t}").into_bytes()
}

/// Begin, read whether the block is stored, store it when it is not, and
/// write thread `t`'s document. Returns whether the read found the block.
fn create(
    db: &OptimisticTransactionDb,
    level: IsolationLevel,
    round: usize,
    t: usize,
) -> (Transaction, bool) {
    let tx = db.begin(&TxnOptions::new().isolation(level));
    let stored = tx.get(&shared_block(round)).unwrap().is_some();
    if !stored {
        tx.put(&shared_block(round), BYTES).unwrap();
    }
    tx.put(&document(round, t), &[t as u8]).unwrap();
    (tx, stored)
}

fn creates_overlap(level: IsolationLevel, classified: bool, storage: Storage) {
    let case = format!("{storage:?} {level:?} classified={classified}");
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir, storage, classified);

    let sync = Rendezvous::new(THREADS);
    let findings = Findings::default();
    let first_commits = AtomicU64::new(0);
    let table_probes = AtomicU64::new(0);
    let all_commit = level == IsolationLevel::DefraLevel && classified;
    let expected = if all_commit { THREADS as u64 } else { 1 };

    thread::scope(|scope| {
        for t in 0..THREADS {
            let (case, db, sync) = (&case, &db, &sync);
            let (findings, first_commits, table_probes) =
                (&findings, &first_commits, &table_probes);
            scope.spawn(move || {
                for round in 0..ROUNDS {
                    sync.wait(); // the previous round is settled
                    let (tx, stored) = create(db, level, round, t);
                    findings.check(!stored, || {
                        format!("{case} round {round} thread {t}: the block was there already")
                    });
                    sync.wait(); // all eight have read and written

                    if storage.is_tables() && t != 0 {
                        sync.wait(); // thread 0 has committed and flushed
                    }
                    let (first, lookups) = table_lookups(|| tx.commit());
                    if storage.is_tables() {
                        if t == 0 {
                            storage.flush(db.db());
                            sync.wait();
                        } else if first.is_err() {
                            table_probes.fetch_add(lookups, Ordering::Relaxed);
                        }
                        // No retry may commit before every first attempt is
                        // in, or a probe would meet its write in the memtable.
                        sync.wait();
                    }
                    match first {
                        Ok(_) => {
                            first_commits.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(TransactionError::Conflict(conflict)) => {
                            let key = conflict.key();
                            findings.check(key == shared_block(round), || {
                                format!("{case} round {round}: conflict on {key:?}")
                            });
                            commit_with_retry("create", || create(db, level, round, t).0.commit());
                        }
                        Err(other) => panic!("{case} round {round}: {other}"),
                    }
                    sync.wait(); // all eight are in

                    if t == 0 {
                        // The first of the refused commits meets the block in
                        // the table thread 0's flush wrote.
                        let probes = table_probes.swap(0, Ordering::Relaxed);
                        findings.check(!storage.is_tables() || all_commit || probes > 0, || {
                            format!("{case} round {round}: no refused commit probed a table")
                        });
                        let commits = first_commits.swap(0, Ordering::Relaxed);
                        findings.check(commits == expected, || {
                            format!(
                                "{case} round {round}: {commits} of {THREADS} commits \
                                 succeeded at once, expected {expected}"
                            )
                        });
                        let documents = (0..THREADS).all(|t| {
                            db.db().get(&document(round, t)).unwrap() == Some(vec![t as u8])
                        });
                        let block = db.db().get(&shared_block(round)).unwrap();
                        findings.check(documents && block.as_deref() == Some(BYTES), || {
                            format!("{case} round {round}: a document or the block is missing")
                        });
                    }
                }
            });
        }
    });
    findings.judge();
}

#[test]
fn creates_of_one_block_all_commit_only_with_the_classifier_at_defra_level() {
    for storage in STORAGES {
        for level in LEVELS {
            for classified in [true, false] {
                creates_overlap(level, classified, storage);
            }
        }
    }
}

// ---- forced writes --------------------------------------------------------

/// The block transaction `r` works on in the `case`-th case of a run.
fn own_block(case: usize, r: usize) -> Vec<u8> {
    format!("b/own-{case:03}-{r}").into_bytes()
}

/// The key transaction `r` records its reference under in that case.
fn reference(case: usize, r: usize) -> Vec<u8> {
    format!("ref/{case:03}-{r}").into_bytes()
}

/// One write landed on a block between a transaction's begin and its commit.
#[derive(Clone, Copy, Debug)]
enum Forced {
    /// The same bytes again, as any other creator of the block stores them.
    Rewrite,
    /// An operand, which leaves the block as it is.
    Operand,
    Delete,
    RangeDelete,
    /// A delete committed by a transaction at `DefraLevel`: the collector.
    Collected,
}

const FORCED: [Forced; 5] = [
    Forced::Rewrite,
    Forced::Operand,
    Forced::Delete,
    Forced::RangeDelete,
    Forced::Collected,
];

impl Forced {
    fn land(self, db: &OptimisticTransactionDb, block: &[u8]) {
        match self {
            Self::Rewrite => db.db().put(block, BYTES).unwrap(),
            Self::Operand => db.db().merge(block, BYTES).unwrap(),
            Self::Delete => db.db().delete(block).unwrap(),
            Self::RangeDelete => {
                // The block and nothing else.
                let mut end = block.to_vec();
                end.push(0);
                db.db().delete_range(block, &end).unwrap();
            }
            Self::Collected => {
                let collector = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
                collector.delete(block).unwrap();
                collector.commit().unwrap();
            }
        }
    }

    /// Whether the block is gone once this write has landed.
    fn removes(self) -> bool {
        matches!(self, Self::Delete | Self::RangeDelete | Self::Collected)
    }

    fn kind(self) -> WriteKind {
        match self {
            Self::Rewrite => WriteKind::Put,
            Self::Operand => WriteKind::Merge,
            Self::Delete | Self::Collected => WriteKind::Delete,
            Self::RangeDelete => WriteKind::RangeDelete,
        }
    }
}

/// What a transaction does with its block before the forced write lands.
#[derive(Clone, Copy, Debug)]
enum Mine {
    /// Reads it, finds it, and writes a reference to it under another key.
    Reads(Read),
    /// Stores it again without reading it.
    Puts,
    /// Deletes it without reading it.
    Deletes,
}

const MINE: [Mine; 4] = [
    Mine::Reads(Read::Plain),
    Mine::Reads(Read::ForUpdate),
    Mine::Puts,
    Mine::Deletes,
];

impl Mine {
    /// What the commit must report as its own access when the forced write
    /// refuses it, or `None` when the commit must go through.
    fn refused(self, level: IsolationLevel, classified: bool, forced: Forced) -> Option<Access> {
        let exempt = classified && level == IsolationLevel::DefraLevel;
        match self {
            // Presence only: the block has to be gone.
            Self::Reads(_) if exempt => forced.removes().then_some(Access::ReadPresence),
            Self::Reads(read) => {
                let changed = if level == IsolationLevel::DefraLevel {
                    // Compared by value: the same bytes again are no change,
                    // and an operand on top of them is one.
                    !matches!(forced, Forced::Rewrite)
                } else {
                    true
                };
                (changed && read.validated_at(level)).then_some(match read {
                    Read::Plain => Access::Read,
                    Read::ForUpdate => Access::ReadForUpdate,
                })
            }
            Self::Puts if exempt => None,
            // A blind write of what the key already holds is no change to it.
            Self::Puts => (!matches!(forced, Forced::Rewrite)).then_some(Access::Put),
            Self::Deletes => (!forced.removes()).then_some(Access::Delete),
        }
    }
}

/// Every way of taking a block against every forced write, in one database:
/// each case has blocks of its own.
fn forced_writes(level: IsolationLevel, classified: bool, storage: Storage) {
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir, storage, classified);
    let cases: Vec<(Mine, Forced)> = MINE
        .iter()
        .flat_map(|mine| FORCED.iter().map(move |forced| (*mine, *forced)))
        .collect();
    for case in 0..cases.len() {
        for r in 0..THREADS {
            regolith::through_stalls(|| db.db().put(&own_block(case, r), BYTES)).unwrap();
        }
    }
    storage.flush(db.db());

    for (case, (mine, forced)) in cases.into_iter().enumerate() {
        let name = format!("{storage:?} {level:?} classified={classified} {mine:?} {forced:?}");
        let refused = mine.refused(level, classified, forced);
        forced_between(
            THREADS,
            |r| {
                let block = own_block(case, r);
                let tx = db.begin_at(level);
                match mine {
                    Mine::Reads(read) => {
                        assert_eq!(
                            read.of(&tx, &block).as_deref(),
                            Some(BYTES),
                            "{name}: reader {r} finds its block"
                        );
                        tx.put(&reference(case, r), &block).unwrap();
                    }
                    Mine::Puts => tx.put(&block, BYTES).unwrap(),
                    Mine::Deletes => tx.delete(&block).unwrap(),
                }
                tx
            },
            || {
                for r in 0..THREADS {
                    forced.land(&db, &own_block(case, r));
                }
                // The commits that follow must find these versions in a table.
                storage.flush(db.db());
            },
            |r, tx| {
                let block = own_block(case, r);
                let (result, lookups) = table_lookups(|| tx.commit());
                match (refused, result) {
                    (None, Ok(_)) => {}
                    (Some(access), Err(TransactionError::Conflict(conflict))) => {
                        assert_eq!(
                            (conflict.key(), conflict.mine(), conflict.theirs()),
                            (&block[..], access, forced.kind()),
                            "{name}: reader {r}"
                        );
                        assert!(
                            !storage.is_tables() || lookups > 0,
                            "{name}: the refused commit never probed the table"
                        );
                        assert_eq!(
                            db.db().get(&reference(case, r)).unwrap(),
                            None,
                            "{name}: a refused commit applied nothing"
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
fn a_block_is_validated_for_presence_when_read_never_when_put_and_in_full_when_deleted() {
    for storage in STORAGES {
        for classified in [true, false] {
            forced_writes(IsolationLevel::DefraLevel, classified, storage);
        }
    }
}

#[test]
fn below_defra_level_a_classifier_changes_nothing_about_a_block() {
    for storage in STORAGES {
        for level in LEVELS {
            if level != IsolationLevel::DefraLevel {
                forced_writes(level, true, storage);
            }
        }
    }
}
