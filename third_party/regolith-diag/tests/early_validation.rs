//! `TxnOptions::early_validation` (plan D20): a write checks its key against
//! newer versions as it is made and fails at once with the conflict the commit
//! would report, so a doomed transaction stops before it does more work.
//!
//! The check is the commit's own check of one written key, so every case sits
//! beside the commit that passes it: an identical rewrite, a blind merge, a key
//! the transaction read, a content-addressed key.

// Native-only: these use the filesystem.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::sync::Arc;

use common::parted::{Parted, add, value};
use regolith::{
    Access, IsolationLevel, KeyClass, KeyClassifier, OptimisticTransactionDb, Options, Transaction,
    TransactionDb, TransactionError, TxnOptions, WriteKind,
};

fn open(dir: &std::path::Path) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(
        dir,
        Options::default().merge_operator(Some(Arc::new(Parted))),
    )
    .unwrap()
}

fn early(level: IsolationLevel) -> TxnOptions {
    TxnOptions::new().isolation(level).early_validation(true)
}

fn lost<T: std::fmt::Debug>(result: Result<T, TransactionError>) -> (Access, WriteKind, u64, u64) {
    match result {
        Err(TransactionError::Conflict(c)) => {
            (c.mine(), c.theirs(), c.observed_seq(), c.latest_seq())
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
}

#[test]
fn a_write_to_a_key_with_a_newer_version_fails_when_it_is_made() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"k", b"v0").unwrap();
    db.db().put(b"gone", b"v0").unwrap();
    let tx = db.begin(&early(IsolationLevel::SnapshotIsolation));
    let snapshot = db.db().latest_sequence();
    db.db().put(b"k", b"v1").unwrap();
    db.db().delete(b"gone").unwrap();

    let (mine, theirs, observed, latest) = lost(tx.put(b"k", b"mine"));
    assert_eq!(
        (mine, theirs, observed),
        (Access::Put, WriteKind::Put, snapshot)
    );
    assert!(latest > snapshot);
    assert_eq!(
        lost(tx.delete(b"k")).0,
        Access::Delete,
        "a delete is checked as one"
    );
    assert_eq!(lost(tx.put(b"gone", b"mine")).1, WriteKind::Delete);

    // The failed write buffered nothing, and the transaction is still usable.
    assert_eq!(tx.get(b"k").unwrap().as_deref(), Some(&b"v0"[..]));
    tx.put(b"fresh", b"ok").unwrap();
    tx.commit().unwrap();
    assert_eq!(db.db().get(b"k").unwrap().as_deref(), Some(&b"v1"[..]));
}

#[test]
fn it_is_off_unless_asked_for_and_the_commit_decides_then() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"k", b"v0").unwrap();
    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation));
    db.db().put(b"k", b"v1").unwrap();
    tx.put(b"k", b"mine").unwrap();
    assert_eq!(lost(tx.commit()).0, Access::Put);

    let tx = db.begin(
        &TxnOptions::new()
            .early_validation(true)
            .early_validation(false),
    );
    db.db().put(b"k", b"v2").unwrap();
    tx.put(b"k", b"mine").unwrap();
    assert_eq!(lost(tx.commit()).0, Access::Put);
}

#[test]
fn a_write_the_commit_would_pass_is_not_refused() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"k", b"v0").unwrap();
    let tx = db.begin(&early(IsolationLevel::SnapshotIsolation));
    db.db().put(b"k", b"v1").unwrap();
    db.db().delete(b"absent").unwrap();
    // The key already holds exactly these bytes: a serial order reaches the
    // same state, and the commit elides the write.
    tx.put(b"k", b"v1").unwrap();
    tx.delete(b"absent").unwrap();
    tx.commit().unwrap();

    // Nothing newer than the snapshot is not a conflict.
    let tx = db.begin(&early(IsolationLevel::SnapshotIsolation));
    tx.put(b"k", b"v2").unwrap();
    tx.commit().unwrap();
}

#[test]
fn a_blind_merge_commutes_at_defralevel_and_not_below_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"k", &value([0; 3])).unwrap();
    let tx = db.begin(&early(IsolationLevel::DefraLevel));
    db.db().merge(b"k", &add(0, 1)).unwrap();
    tx.merge(b"k", &add(1, 1)).unwrap();
    tx.commit().unwrap();

    let tx = db.begin(&early(IsolationLevel::SnapshotIsolation));
    db.db().merge(b"k", &add(0, 1)).unwrap();
    let (mine, theirs, ..) = lost(tx.merge(b"k", &add(1, 1)));
    assert_eq!((mine, theirs), (Access::Merge, WriteKind::Merge));

    // A newer replacement does conflict, however many operands sit on top.
    let tx = db.begin(&early(IsolationLevel::DefraLevel));
    db.db().put(b"k", &value([5, 5, 5])).unwrap();
    db.db().merge(b"k", &add(0, 1)).unwrap();
    let (mine, theirs, ..) = lost(tx.merge(b"k", &add(1, 1)));
    assert_eq!((mine, theirs), (Access::Merge, WriteKind::Put));
}

#[test]
fn a_key_the_transaction_read_is_left_to_the_commit() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"k", b"v0").unwrap();
    let tx = db.begin(&early(IsolationLevel::DefraLevel));
    assert_eq!(tx.get(b"k").unwrap().as_deref(), Some(&b"v0"[..]));
    // The key is rewritten with the bytes the read returned: the read is
    // current, so the commit passes a write that a bare check would refuse.
    db.db().put(b"k", b"v0").unwrap();
    tx.put(b"k", b"v1").unwrap();
    tx.commit().unwrap();

    // A read that really went stale is still caught, at commit.
    let tx = db.begin(&early(IsolationLevel::DefraLevel));
    tx.get(b"k").unwrap();
    db.db().put(b"k", b"changed").unwrap();
    tx.put(b"k", b"v2").unwrap();
    assert_eq!(lost(tx.commit()).0, Access::Read);
}

#[test]
fn a_transaction_that_has_scanned_is_left_to_the_commit() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"k", b"v0").unwrap();
    let tx = db.begin(&early(IsolationLevel::DefraLevel));
    assert_eq!(tx.scan_stream(None, None).count(), 1);
    db.db().put(b"k", b"v0").unwrap();
    // A write inside the stretch is a read from the snapshot, and the read is
    // current.
    tx.put(b"k", b"v1").unwrap();
    tx.commit().unwrap();
}

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

#[test]
fn a_put_of_a_content_addressed_key_is_never_validated_but_a_delete_is() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path()).with_policy(Arc::new(Blocks));
    db.db().put(b"b/x", b"bytes").unwrap();
    let tx = db.begin(&early(IsolationLevel::DefraLevel));
    db.db().put(b"b/x", b"bytes").unwrap();
    db.db().put(b"b/y", b"bytes").unwrap();
    tx.put(b"b/x", b"bytes").unwrap();
    tx.put(b"b/y", b"bytes").unwrap();
    assert_eq!(lost(tx.delete(b"b/x")).0, Access::Delete);
    tx.commit().unwrap();
}

#[test]
fn a_pessimistic_transaction_ignores_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = TransactionDb::open(dir.path(), Options::default()).unwrap();
    db.db().put(b"k", b"v0").unwrap();
    let tx = db.begin(&early(IsolationLevel::SnapshotIsolation));
    db.db().put(b"k", b"v1").unwrap();
    tx.put(b"k", b"mine").unwrap();
    tx.commit().unwrap();
    assert_eq!(db.db().get(b"k").unwrap().as_deref(), Some(&b"mine"[..]));
}

#[test]
fn an_early_conflict_names_the_key_without_the_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"the-key", b"v0").unwrap();
    let tx: Transaction = db.begin(&early(IsolationLevel::RepeatableRead));
    db.db().put(b"the-key", b"v1").unwrap();
    match tx.put(b"the-key", b"mine") {
        Err(TransactionError::Conflict(c)) => {
            assert_eq!(c.key(), b"the-key");
            assert!(!c.to_string().contains("the-key"));
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
}
