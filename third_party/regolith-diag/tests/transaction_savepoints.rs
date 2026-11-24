//! Savepoints: `set_savepoint`, `rollback_to_savepoint` and
//! `release_savepoint`, checked against a plain model of what a transaction
//! has written.
//!
//! A savepoint is a mark on the write buffer's entry count, so rolling back
//! drops the newest entries; the model below is the semantics that must
//! survive that: after any mix of writes, savepoints, rollbacks and releases,
//! reads, scans and the commit see exactly the writes that were not rolled
//! back, in the order they were made.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// the filesystem or proptest, which do not exist there.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;

use proptest::prelude::*;
use regolith::{
    IsolationLevel, OptimisticTransactionDb, Options, TransactionDb, TransactionError, TxnOptions,
};
use tempfile::TempDir;

fn open(inline: usize) -> (OptimisticTransactionDb, TempDir) {
    let dir = TempDir::new().unwrap();
    let options = Options::default().transaction_keys_inline(inline);
    (
        OptimisticTransactionDb::open(dir.path(), options).unwrap(),
        dir,
    )
}

#[test]
fn release_forgets_the_savepoint_and_keeps_the_writes() {
    let (db, _dir) = open(32);
    let mut txn = db.begin(&TxnOptions::new());
    txn.put(b"a", b"1").unwrap();
    txn.set_savepoint();
    txn.put(b"b", b"2").unwrap();
    txn.release_savepoint().unwrap();
    assert!(matches!(
        txn.rollback_to_savepoint(),
        Err(TransactionError::NoSavepoint)
    ));
    assert_eq!(txn.get(b"b").unwrap().as_deref(), Some(&b"2"[..]));
    txn.commit().unwrap();
    assert_eq!(db.db().get(b"a").unwrap().as_deref(), Some(&b"1"[..]));
    assert_eq!(db.db().get(b"b").unwrap().as_deref(), Some(&b"2"[..]));
}

#[test]
fn releasing_the_inner_savepoint_leaves_the_outer_one_current() {
    let (db, _dir) = open(32);
    let mut txn = db.begin(&TxnOptions::new());
    txn.put(b"a", b"1").unwrap();
    txn.set_savepoint();
    txn.put(b"b", b"2").unwrap();
    txn.set_savepoint();
    txn.put(b"c", b"3").unwrap();
    txn.release_savepoint().unwrap();
    txn.rollback_to_savepoint().unwrap();
    assert_eq!(txn.get(b"a").unwrap().as_deref(), Some(&b"1"[..]));
    assert_eq!(txn.get(b"b").unwrap(), None);
    assert_eq!(txn.get(b"c").unwrap(), None);
}

#[test]
fn release_and_rollback_without_a_savepoint_report_it() {
    let (db, _dir) = open(32);
    let mut txn = db.begin(&TxnOptions::new());
    assert!(matches!(
        txn.release_savepoint(),
        Err(TransactionError::NoSavepoint)
    ));
    assert!(matches!(
        txn.rollback_to_savepoint(),
        Err(TransactionError::NoSavepoint)
    ));
    assert_eq!(
        TransactionError::NoSavepoint.to_string(),
        "no savepoint is set"
    );
}

#[test]
fn a_rollback_restores_what_a_later_put_and_delete_replaced() {
    for inline in [0, 1, 32] {
        let (db, _dir) = open(inline);
        db.db().put(b"base", b"db").unwrap();
        let mut txn = db.begin(&TxnOptions::new());
        txn.put(b"k", b"first").unwrap();
        txn.delete(b"base").unwrap();
        txn.set_savepoint();
        txn.put(b"k", b"second").unwrap();
        txn.put(b"base", b"again").unwrap();
        txn.delete(b"k").unwrap();
        assert_eq!(txn.get(b"k").unwrap(), None);
        txn.rollback_to_savepoint().unwrap();
        assert_eq!(txn.get(b"k").unwrap().as_deref(), Some(&b"first"[..]));
        assert_eq!(txn.get(b"base").unwrap(), None);
        txn.commit().unwrap();
        assert_eq!(db.db().get(b"k").unwrap().as_deref(), Some(&b"first"[..]));
        assert_eq!(db.db().get(b"base").unwrap(), None);
    }
}

#[test]
fn a_rollback_is_the_same_below_and_above_the_index_threshold() {
    // 200 distinct keys put an index over the buffer at a threshold of 8; the
    // savepoint is set before it is built, and rolled back after.
    let (db, _dir) = open(8);
    let mut txn = db.begin(&TxnOptions::new());
    for i in 0..4u32 {
        txn.put(&i.to_be_bytes(), b"early").unwrap();
    }
    txn.set_savepoint();
    for i in 0..200u32 {
        txn.put(&(i % 40).to_be_bytes(), format!("late{i}").as_bytes())
            .unwrap();
    }
    txn.rollback_to_savepoint().unwrap();
    for i in 0..4u32 {
        assert_eq!(
            txn.get(&i.to_be_bytes()).unwrap().as_deref(),
            Some(&b"early"[..])
        );
    }
    assert_eq!(txn.get(&39u32.to_be_bytes()).unwrap(), None);
    let scanned: Vec<_> = txn
        .scan_stream(None, None)
        .map(|row| row.unwrap().0)
        .collect();
    assert_eq!(scanned.len(), 4);
    txn.put(&39u32.to_be_bytes(), b"after").unwrap();
    assert_eq!(
        txn.get(&39u32.to_be_bytes()).unwrap().as_deref(),
        Some(&b"after"[..])
    );
}

#[test]
fn a_pessimistic_rollback_drops_the_writes_and_keeps_the_locks() {
    let dir = TempDir::new().unwrap();
    let db = TransactionDb::open(dir.path(), Options::default())
        .unwrap()
        .with_lock_timeout(std::time::Duration::from_millis(50));
    let mut txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    txn.set_savepoint();
    txn.put(b"locked", b"x").unwrap();
    txn.rollback_to_savepoint().unwrap();
    assert_eq!(txn.get(b"locked").unwrap(), None);
    let other = db.begin(&TxnOptions::new());
    assert!(matches!(
        other.put(b"locked", b"y"),
        Err(TransactionError::Busy(_))
    ));
}

#[derive(Debug, Clone)]
enum Op {
    Put(u8, u8),
    Delete(u8),
    Set,
    Rollback,
    Release,
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(
        prop_oneof![
            6 => (0u8..8, any::<u8>()).prop_map(|(k, v)| Op::Put(k, v)),
            2 => (0u8..8).prop_map(Op::Delete),
            3 => Just(Op::Set),
            3 => Just(Op::Rollback),
            1 => Just(Op::Release),
        ],
        0..80,
    )
}

proptest! {
    /// After any mix of writes, savepoints, rollbacks and releases, a read, a
    /// scan and the commit all see the writes that survived, and a rollback
    /// with no savepoint set reports it.
    #[test]
    fn savepoints_behave_like_a_stack_of_snapshots_of_the_writes(
        inline in prop_oneof![Just(0usize), Just(2), Just(32)],
        ops in ops(),
    ) {
        let (db, _dir) = open(inline);
        db.db().put(&[3], b"db3").unwrap();
        db.db().put(&[6], b"db6").unwrap();
        let mut txn = db.begin(&TxnOptions::new());

        // What the transaction wrote, and the length of `writes` each open
        // savepoint was set at.
        let mut writes: Vec<(u8, Option<u8>)> = Vec::new();
        let mut marks: Vec<usize> = Vec::new();
        let expected = |writes: &[(u8, Option<u8>)]| {
            let mut view: BTreeMap<u8, Vec<u8>> =
                [(3u8, b"db3".to_vec()), (6, b"db6".to_vec())].into();
            for &(key, value) in writes {
                match value {
                    Some(v) => view.insert(key, vec![v]),
                    None => view.remove(&key),
                };
            }
            view
        };
        for op in ops {
            match op {
                Op::Put(k, v) => {
                    txn.put(&[k], &[v]).unwrap();
                    writes.push((k, Some(v)));
                }
                Op::Delete(k) => {
                    txn.delete(&[k]).unwrap();
                    writes.push((k, None));
                }
                Op::Set => {
                    txn.set_savepoint();
                    marks.push(writes.len());
                }
                Op::Rollback => match (txn.rollback_to_savepoint(), marks.pop()) {
                    (Ok(()), Some(mark)) => writes.truncate(mark),
                    (Err(TransactionError::NoSavepoint), None) => {}
                    (other, mark) => prop_assert!(false, "{other:?} with mark {mark:?}"),
                },
                Op::Release => match (txn.release_savepoint(), marks.pop()) {
                    (Ok(()), Some(_)) | (Err(TransactionError::NoSavepoint), None) => {}
                    (other, mark) => prop_assert!(false, "{other:?} with mark {mark:?}"),
                },
            }
            let view = expected(&writes);
            for key in 0u8..8 {
                prop_assert_eq!(txn.get(&[key]).unwrap(), view.get(&key).cloned());
            }
        }
        let view = expected(&writes);
        let scanned: BTreeMap<u8, Vec<u8>> = txn
            .scan_stream(None, None)
            .map(|row| row.map(|(k, v)| (k[0], v.to_vec())).unwrap())
            .collect();
        prop_assert_eq!(&scanned, &view);
        txn.commit().unwrap();
        for key in 0u8..8 {
            prop_assert_eq!(db.db().get(&[key]).unwrap(), view.get(&key).cloned());
        }
    }
}
