//! A blind write of the value a key already holds is not a conflict.
//!
//! Two transactions that both write `K = v`, the first committing, have a
//! serial equivalent: under "first, then second" the second writes `K = v` and
//! the state is `K = v`, exactly what letting both commit produces. Refusing
//! the second rejects a correct history.
//!
//! Content-addressed callers meet this constantly, because there the key is a
//! hash of the value, so "the same key" and "the same bytes" are one statement.
//! The cases below draw the line: byte equality on a key the transaction did
//! not read, and nothing else.

use regolith::{IsolationLevel, MergeOperator, OptimisticTransactionDb, Options, TxnOptions};

mod common;

/// Sums big-endian i64 deltas, so two `+1` operands make `+2` and the
/// operation is plainly not idempotent.
struct CounterMerge;

impl MergeOperator for CounterMerge {
    fn name(&self) -> &'static str {
        "counter"
    }

    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut total: i64 = match base {
            Some(bytes) if bytes.len() == 8 => i64::from_be_bytes(bytes.try_into().unwrap()),
            Some(_) => return None,
            None => 0,
        };
        for operand in operands {
            if operand.len() != 8 {
                return None;
            }
            total = total.wrapping_add(i64::from_be_bytes((*operand).try_into().unwrap()));
        }
        Some(total.to_be_bytes().to_vec())
    }
}

fn db(dir: &std::path::Path) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(dir, Options::default()).unwrap()
}

fn levels() -> [IsolationLevel; 4] {
    [
        IsolationLevel::ReadCommitted,
        IsolationLevel::SnapshotIsolation,
        IsolationLevel::RepeatableRead,
        IsolationLevel::Serializable,
    ]
}

#[test]
fn concurrent_writes_of_identical_bytes_both_commit() {
    for level in levels() {
        let dir = tempfile::tempdir().unwrap();
        let db = db(dir.path());

        let first = db.begin(&TxnOptions::new().isolation(level));
        let second = db.begin(&TxnOptions::new().isolation(level));
        first.put(b"block", b"identical").unwrap();
        second.put(b"block", b"identical").unwrap();

        first.commit().unwrap();
        second
            .commit()
            .unwrap_or_else(|error| panic!("{level:?}: {error:?}"));

        assert_eq!(
            db.db().get(b"block").unwrap().as_deref(),
            Some(b"identical".as_slice())
        );
    }
}

/// The line: same key, different bytes, is a real lost update and still aborts.
#[test]
fn concurrent_writes_of_different_bytes_still_conflict() {
    for level in levels() {
        let dir = tempfile::tempdir().unwrap();
        let db = db(dir.path());

        let first = db.begin(&TxnOptions::new().isolation(level));
        let second = db.begin(&TxnOptions::new().isolation(level));
        first.put(b"key", b"one").unwrap();
        second.put(b"key", b"two").unwrap();

        first.commit().unwrap();
        assert!(
            !common::committed(&second.commit()),
            "{level:?}: a differing blind write is a lost update"
        );
    }
}

/// Two deletes reach the same state, so the second is not a conflict either.
#[test]
fn concurrent_identical_deletes_both_commit() {
    for level in levels() {
        let dir = tempfile::tempdir().unwrap();
        let db = db(dir.path());
        db.db().put(b"key", b"value").unwrap();

        let first = db.begin(&TxnOptions::new().isolation(level));
        let second = db.begin(&TxnOptions::new().isolation(level));
        first.delete(b"key").unwrap();
        second.delete(b"key").unwrap();

        first.commit().unwrap();
        second
            .commit()
            .unwrap_or_else(|error| panic!("{level:?}: {error:?}"));

        assert_eq!(db.db().get(b"key").unwrap(), None);
    }
}

/// A delete against a concurrent put of the same key is not idempotent: the
/// two disagree about the final state.
#[test]
fn a_delete_against_a_concurrent_put_still_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let db = db(dir.path());
    db.db().put(b"key", b"value").unwrap();

    let first = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    let second = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    first.put(b"key", b"changed").unwrap();
    second.delete(b"key").unwrap();

    first.commit().unwrap();
    assert!(
        !common::committed(&second.commit()),
        "delete over a differing put conflicts"
    );
}

/// **The safety boundary.** A transaction that *read* the key must still abort,
/// even when the value it would write matches, because the stale read may have
/// decided what it wrote.
#[test]
fn a_stale_read_still_conflicts_even_when_the_write_matches() {
    let dir = tempfile::tempdir().unwrap();
    let db = db(dir.path());

    let first = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    let second = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));

    // The second transaction observes the key as absent and acts on that.
    assert_eq!(second.get(b"key").unwrap(), None);
    second.put(b"key", b"same").unwrap();
    first.put(b"key", b"same").unwrap();

    first.commit().unwrap();
    assert!(
        !common::committed(&second.commit()),
        "a read that no longer holds must abort whatever the write says"
    );
}

/// **The counterexample that defines the rule.** A read-modify-write produces
/// byte-identical writes from a stale read, and eliding one loses an update.
///
/// Two transactions read a counter at 5 and both write 6. The writes are equal
/// to the byte, so a rule that looked only at the bytes would let both commit
/// and the counter would advance once. No serial order produces that: run
/// second, a transaction reads 6 and writes 7. This is why the elision is
/// refused for any key the transaction read, at every level, rather than only
/// where the level would validate a plain read.
#[test]
fn a_read_modify_write_still_conflicts_at_every_level() {
    for level in levels() {
        let dir = tempfile::tempdir().unwrap();
        let db = db(dir.path());
        db.db().put(b"counter", &5u64.to_le_bytes()).unwrap();

        let first = db.begin(&TxnOptions::new().isolation(level));
        let second = db.begin(&TxnOptions::new().isolation(level));

        // Both read 5 and both intend to store 6.
        for txn in [&first, &second] {
            let current =
                u64::from_le_bytes(txn.get(b"counter").unwrap().unwrap().try_into().unwrap());
            assert_eq!(current, 5);
            txn.put(b"counter", &(current + 1).to_le_bytes()).unwrap();
        }

        first.commit().unwrap();
        assert!(
            !common::committed(&second.commit()),
            "{level:?}: eliding this write would lose an increment"
        );
        assert_eq!(
            db.db().get(b"counter").unwrap().as_deref(),
            Some(6u64.to_le_bytes().as_slice())
        );
    }
}

/// The same boundary through `get_for_update`, which is validated at every
/// level rather than only under serializable.
#[test]
fn a_stale_get_for_update_still_conflicts_when_the_write_matches() {
    for level in levels() {
        let dir = tempfile::tempdir().unwrap();
        let db = db(dir.path());

        let first = db.begin(&TxnOptions::new().isolation(level));
        let second = db.begin(&TxnOptions::new().isolation(level));

        assert_eq!(second.get_for_update(b"key").unwrap(), None);
        second.put(b"key", b"same").unwrap();
        first.put(b"key", b"same").unwrap();

        first.commit().unwrap();
        assert!(
            !common::committed(&second.commit()),
            "{level:?}: get_for_update is a read and must still abort"
        );
    }
}

/// Merges are not idempotent: two `+1` operands are not one `+1`, so a merge
/// on a contended key keeps conflicting.
#[test]
fn concurrent_merges_still_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let options = Options::default().merge_operator(Some(std::sync::Arc::new(CounterMerge)));
    let db = OptimisticTransactionDb::open(dir.path(), options).unwrap();

    let first = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    let second = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    first.merge(b"key", &1i64.to_be_bytes()).unwrap();
    second.merge(b"key", &1i64.to_be_bytes()).unwrap();

    first.commit().unwrap();
    assert!(
        !common::committed(&second.commit()),
        "merges are not idempotent"
    );
}

/// A sequential rewrite of the same bytes was never a conflict and still is
/// not, so the change does not quietly alter the uncontended path.
#[test]
fn sequential_identical_writes_commit() {
    let dir = tempfile::tempdir().unwrap();
    let db = db(dir.path());

    let first = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    first.put(b"key", b"value").unwrap();
    first.commit().unwrap();

    let second = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    second.put(b"key", b"value").unwrap();
    second.commit().unwrap();

    assert_eq!(
        db.db().get(b"key").unwrap().as_deref(),
        Some(b"value".as_slice())
    );
}

fn counter_db(dir: &std::path::Path) -> OptimisticTransactionDb {
    let options = Options::default().merge_operator(Some(std::sync::Arc::new(CounterMerge)));
    OptimisticTransactionDb::open(dir, options).unwrap()
}

/// A merge operator on the database no longer turns the elision off for
/// every key: a key that never saw an operand compares exactly as before.
#[test]
fn identical_writes_commit_with_a_merge_operator_configured() {
    for level in levels() {
        let dir = tempfile::tempdir().unwrap();
        let db = counter_db(dir.path());

        let first = db.begin(&TxnOptions::new().isolation(level));
        let second = db.begin(&TxnOptions::new().isolation(level));
        first.put(b"block", b"identical").unwrap();
        second.put(b"block", b"identical").unwrap();

        first.commit().unwrap();
        second
            .commit()
            .unwrap_or_else(|error| panic!("{level:?}: {error:?}"));
    }
}

/// Operands beneath a newer value are already folded into it, so the
/// committed value a point read returns is what the key stores.
#[test]
fn an_identical_write_over_folded_operands_commits() {
    for flush in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = counter_db(dir.path());
        db.db().put(b"key", &0i64.to_be_bytes()).unwrap();
        db.db().merge(b"key", &1i64.to_be_bytes()).unwrap();

        let first = db.begin(&TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation));
        let second = db.begin(&TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation));
        first.put(b"key", &5i64.to_be_bytes()).unwrap();
        second.put(b"key", &5i64.to_be_bytes()).unwrap();
        first.commit().unwrap();
        if flush {
            db.db().flush().unwrap();
        }
        second
            .commit()
            .unwrap_or_else(|error| panic!("flush={flush}: {error:?}"));
    }
}

/// A newer operand that has not been folded still refuses the elision,
/// even when the write matches what a point read would resolve to: the
/// key stores operands, not that value.
#[test]
fn an_identical_write_under_an_unresolved_operand_still_conflicts() {
    for flush in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = counter_db(dir.path());
        db.db().put(b"key", &0i64.to_be_bytes()).unwrap();

        let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation));
        tx.put(b"key", &1i64.to_be_bytes()).unwrap();
        db.db().merge(b"key", &1i64.to_be_bytes()).unwrap();
        if flush {
            db.db().flush().unwrap();
        }
        assert!(
            !common::committed(&tx.commit()),
            "flush={flush}: a write under an unresolved operand must conflict"
        );
    }
}
