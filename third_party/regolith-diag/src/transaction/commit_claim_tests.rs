//! `close` never aborts a transaction whose commit began, even when the
//! transaction made its claim late.
//!
//! A transaction with no `on_abort` callback and no database hook has no claim
//! until one is registered, and the commit it starts cannot be told apart from
//! one that has not started. A callback registered by a `before_commit`
//! callback then creates the claim in the middle of the commit. These tests
//! sweep the way `close` does, from inside that commit, which is the moment
//! the claim must already read as committing.

use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use super::{
    AbortReason, OptimisticTransactionDb, Options, TransactionDb, TransactionError, TxnOptions,
};
use crate::Error;

type Log = Arc<Mutex<Vec<String>>>;

fn note(log: &Log, entry: &str) {
    log.lock().unwrap().push(entry.to_string());
}

fn entries(log: &Log) -> Vec<String> {
    log.lock().unwrap().clone()
}

#[test]
fn a_sweep_in_the_middle_of_the_commit_leaves_it_to_commit() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
    let log = Log::default();
    let mut txn = db.begin(&TxnOptions::new());
    txn.put(b"k", b"v").unwrap();

    // The first `on_abort` of the transaction, registered once its commit is
    // underway: this is what creates the claim.
    let l = Arc::clone(&log);
    txn.before_commit(move |txn| {
        let abort = Arc::clone(&l);
        txn.on_abort(move |why| {
            note(
                &abort,
                if matches!(why, AbortReason::Closed) {
                    "abort:closed"
                } else {
                    "abort"
                },
            )
        });
        Ok(())
    });
    // What `close` does first, at a moment when the commit has begun and the
    // claim exists.
    let engine = Arc::clone(&txn.engine);
    txn.before_commit(move |_| {
        engine.open_transactions().abort_all(&engine);
        Ok(())
    });
    let l = Arc::clone(&log);
    txn.on_commit(move |_| note(&l, "commit"));

    txn.commit().unwrap();
    assert_eq!(
        entries(&log),
        ["commit"],
        "the sweep aborted a transaction that went on to commit"
    );
    assert_eq!(db.db().get(b"k").unwrap().as_deref(), Some(&b"v"[..]));
}

/// A transaction `close` already tracks (its `on_abort` was registered
/// before the commit) is swept while its commit runs: the sweep loses the
/// claim to the commit, so only `on_commit` runs.
#[test]
fn a_sweep_during_the_commit_of_a_tracked_transaction_leaves_it_to_commit() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
    let log = Log::default();
    let mut txn = db.begin(&TxnOptions::new());
    txn.put(b"k", b"v").unwrap();
    let l = Arc::clone(&log);
    txn.on_abort(move |_| note(&l, "abort"));
    let engine = Arc::clone(&txn.engine);
    txn.before_commit(move |_| {
        engine.open_transactions().abort_all(&engine);
        Ok(())
    });
    let l = Arc::clone(&log);
    txn.on_commit(move |_| note(&l, "commit"));
    txn.commit().unwrap();
    assert_eq!(entries(&log), ["commit"], "the sweep won a claimed commit");
}

#[test]
fn the_same_holds_for_a_pessimistic_transaction() {
    let dir = TempDir::new().unwrap();
    let db = TransactionDb::open(dir.path(), Options::default()).unwrap();
    let log = Log::default();
    let mut txn = db.begin(&TxnOptions::new());
    txn.put(b"k", b"v").unwrap();
    let l = Arc::clone(&log);
    txn.before_commit(move |txn| {
        let abort = Arc::clone(&l);
        txn.on_abort(move |_| note(&abort, "abort"));
        Ok(())
    });
    let engine = Arc::clone(&txn.engine);
    txn.before_commit(move |_| {
        engine.open_transactions().abort_all(&engine);
        Ok(())
    });
    let l = Arc::clone(&log);
    txn.on_commit(move |_| note(&l, "commit"));
    txn.commit().unwrap();
    assert_eq!(entries(&log), ["commit"]);
}

#[test]
fn a_transaction_that_has_not_begun_to_commit_is_still_swept() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
    let log = Log::default();
    let mut txn = db.begin(&TxnOptions::new());
    let l = Arc::clone(&log);
    txn.on_abort(move |_| note(&l, "abort"));
    let engine = Arc::clone(&txn.engine);
    engine.open_transactions().abort_all(&engine);
    assert_eq!(entries(&log), ["abort"]);
    let refused = txn.commit();
    assert!(
        matches!(refused, Err(TransactionError::Engine(Error::Closed))),
        "a swept transaction cannot commit: {refused:?}"
    );
    assert_eq!(entries(&log), ["abort"], "nothing runs twice");
}
