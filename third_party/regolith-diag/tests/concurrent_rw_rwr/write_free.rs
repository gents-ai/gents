//! Scenario 11: transactions that write nothing.
//!
//! A transaction with no put, delete, merge or append read one snapshot and
//! changes nothing. On the optimistic flavour at `DefraLevel` its commit
//! therefore validates no plain read, and returns the snapshot as its
//! receipt: it is placed where it read, not where it committed. A read it
//! asked to have checked, through `get_for_update`, is still checked. At
//! every other level, and for a pessimistic transaction at `DefraLevel`, a
//! commit with no writes validates what the level validates.
//!
//! Forced. Each of eight readers reads a key, a write to it commits through
//! the helper thread and is acknowledged, the reader reads the key again and
//! finds the value it read first, and commits without having written
//! anything.
//!
//! Free. Eight writers transfer between accounts while audits read every
//! account in one transaction and commit it. The total is the initial total
//! in every audit, and where a write-free commit validates nothing, no audit
//! is refused, the ones that had a transfer land inside them included.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Instant;

use regolith::{Access, IsolationLevel, TransactionError};

use super::support::{Storage, table_lookups};
use super::{
    ACCOUNTS, BALANCE, Flavour, HANDOFF, READS, Read, StopOnDrop, THREADS, TOTAL, account, audit,
    commit_with_retry, decode, each_case, encode, optimistic, pessimistic, shared, transfer,
    with_forced_writes,
};

/// Whether a commit that wrote nothing still validates a key read this way.
fn validated(read: Read, level: IsolationLevel, is_optimistic: bool) -> bool {
    if is_optimistic && level == IsolationLevel::DefraLevel {
        // Only the read the caller asked to have checked.
        matches!(read, Read::ForUpdate)
    } else {
        read.validated_at(level)
    }
}

fn write_free_reader<D: Flavour>(
    db: &D,
    level: IsolationLevel,
    storage: Storage,
    read: Read,
    is_optimistic: bool,
) {
    for r in 0..THREADS {
        db.raw().put(&shared(r), &encode(1)).unwrap();
    }
    storage.flush(db.raw());
    with_forced_writes(
        db,
        |db, r| {
            let value = decode(db.raw().get(&shared(r)).unwrap());
            db.raw().put(&shared(r), &encode(value + 1)).unwrap();
            // The commit that follows must find this version in a table.
            storage.flush(db.raw());
        },
        |r, force| {
            let tx = db.begin_at(level);
            // Nothing this transaction can read is newer than this.
            let horizon = db.raw().latest_sequence();
            let first = decode(read.of(&tx, &shared(r)));
            force();
            assert_eq!(
                decode(db.raw().get(&shared(r)).unwrap()),
                first + 1,
                "the forced commit is visible outside the transaction"
            );
            let second = decode(read.of(&tx, &shared(r)));
            assert_eq!(
                first, second,
                "{level:?} {read:?}: reader {r} read one snapshot"
            );
            let (result, lookups) = table_lookups(|| tx.commit());
            if validated(read, level, is_optimistic) {
                match result {
                    Err(TransactionError::Conflict(conflict)) => {
                        assert_eq!(conflict.key(), shared(r), "{level:?} {read:?}");
                        let mine = match read {
                            Read::Plain => Access::Read,
                            Read::ForUpdate => Access::ReadForUpdate,
                        };
                        assert_eq!(conflict.mine(), mine, "{level:?} {read:?}");
                    }
                    other => panic!(
                        "{level:?} {read:?}: a validated read overtaken by a commit aborts, \
                         written or not: {other:?}"
                    ),
                }
                assert!(
                    !storage.is_tables() || lookups > 0,
                    "{level:?} {read:?}: the refused commit never probed the table"
                );
            } else {
                let receipt = result.unwrap_or_else(|e| {
                    panic!("{level:?} {read:?}: a write-free commit was refused: {e}")
                });
                if is_optimistic && level == IsolationLevel::DefraLevel {
                    assert!(
                        receipt.seq() <= horizon,
                        "{level:?} {read:?}: a write-free commit is placed at its snapshot, \
                         at or below {horizon}, and reported {}",
                        receipt.seq()
                    );
                }
            }
        },
    );
}

#[test]
fn a_commit_with_no_writes_validates_only_what_was_asked_at_defra_level_optimistic() {
    each_case(|storage, level| {
        for read in READS {
            let dir = tempfile::tempdir().unwrap();
            write_free_reader(&optimistic(&dir, storage), level, storage, read, true);
        }
    });
}

#[test]
fn a_commit_with_no_writes_validates_as_its_level_does_pessimistic() {
    each_case(|storage, level| {
        for read in READS {
            let dir = tempfile::tempdir().unwrap();
            write_free_reader(&pessimistic(&dir, storage), level, storage, read, false);
        }
    });
}

/// Whether an audit's commit may be refused: where a commit with no writes
/// validates its plain reads.
fn audit_may_conflict(level: IsolationLevel, is_optimistic: bool) -> bool {
    validated(Read::Plain, level, is_optimistic)
}

fn audits_commit<D: Flavour>(db: &D, level: IsolationLevel, storage: Storage, is_optimistic: bool) {
    for i in 0..ACCOUNTS {
        db.raw().put(&account(i), &encode(BALANCE)).unwrap();
    }
    storage.flush(db.raw());

    // An audit overlapped when the engine's sequence advanced between just
    // after its transaction began and its last read: a transfer committed
    // strictly inside it. A transfer that moves nothing writes nothing, so
    // it cannot fake one.
    let overlapped = AtomicBool::new(false);
    let reached_tables = AtomicBool::new(!storage.is_tables());
    let may_conflict = audit_may_conflict(level, is_optimistic);
    let enough = || overlapped.load(Ordering::Acquire) && reached_tables.load(Ordering::Acquire);
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let _stop = StopOnDrop(&stop);
        for t in 0..THREADS as u64 {
            let (enough, reached_tables, stop) = (&enough, &reached_tables, &stop);
            scope.spawn(move || {
                // A writer that panics stops the auditor too.
                let _stop = StopOnDrop(stop);
                let mut n = 0u64;
                while !stop.load(Ordering::Acquire) && (!enough() || n < 50) {
                    let (from, to) = ((t + n) % ACCOUNTS, (t + n + 3) % ACCOUNTS);
                    let (_, lookups) = table_lookups(|| {
                        commit_with_retry("transfer", || transfer(db, level, from, to))
                    });
                    if lookups > 0 {
                        reached_tables.store(true, Ordering::Release);
                    }
                    n += 1;
                    storage.churn(db.raw(), n);
                }
            });
        }
        let deadline = Instant::now() + HANDOFF;
        let mut audits = 0u64;
        while !enough() || audits < 20 {
            assert!(
                !stop.load(Ordering::Acquire) || enough(),
                "{level:?}: the writers stopped before the evidence was in"
            );
            assert!(
                Instant::now() < deadline,
                "{level:?}: within {HANDOFF:?} an audit overlapped a commit: {}, a transfer \
                 reached a table: {}",
                overlapped.load(Ordering::Acquire),
                reached_tables.load(Ordering::Acquire)
            );
            let tx = db.begin_at(level);
            let began = db.raw().snapshot().sequence();
            assert_eq!(audit(&tx), TOTAL, "{level:?}: audit {audits}");
            if db.raw().snapshot().sequence() > began {
                overlapped.store(true, Ordering::Release);
            }
            // Where a commit with no writes validates nothing, every audit
            // commits, the ones a transfer landed inside included.
            match tx.commit() {
                Ok(_) => {}
                Err(TransactionError::Conflict(_)) if may_conflict => {}
                Err(e) => panic!("{level:?}: an audit that wrote nothing was refused: {e}"),
            }
            audits += 1;
        }
    });

    let tx = db.begin_at(level);
    assert_eq!(audit(&tx), TOTAL, "{level:?}: final total");
}

#[test]
fn an_audit_that_commits_is_never_refused_where_it_validates_nothing_optimistic() {
    each_case(|storage, level| {
        let dir = tempfile::tempdir().unwrap();
        audits_commit(&optimistic(&dir, storage), level, storage, true);
    });
}

#[test]
fn an_audit_that_commits_is_never_refused_where_it_validates_nothing_pessimistic() {
    each_case(|storage, level| {
        let dir = tempfile::tempdir().unwrap();
        audits_commit(&pessimistic(&dir, storage), level, storage, false);
    });
}
