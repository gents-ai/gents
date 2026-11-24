//! Integration tests for the ownership of a `Transaction`.
//!
//! A `Transaction` carries no lifetime and borrows nothing from its
//! database, so it can sit in a `'static` container such as a boxed trait
//! object, which is what a storage abstraction layer needs when it hands
//! transactions out. The tests here are about that boundary: the
//! transaction still commits, still validates, and still works after
//! every named handle on the database is gone.

// Native-only. wasm-pack builds every test target for wasm32, and these
// use the filesystem, which does not exist there. The browser suite lives
// in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use regolith::{
    DbSlice, IsolationLevel, OptimisticTransactionDb, Options, Transaction, TransactionDb,
    TransactionError, TxnOptions,
};
use tempfile::TempDir;

fn optimistic(dir: &TempDir) -> Arc<OptimisticTransactionDb> {
    Arc::new(
        OptimisticTransactionDb::open(dir.path(), Options::default()).expect("open optimistic db"),
    )
}

#[test]
fn a_transaction_commits() {
    let dir = TempDir::new().expect("tempdir");
    let db = optimistic(&dir);

    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    txn.put(b"key", b"value").expect("put");
    txn.commit().expect("commit");

    assert_eq!(
        db.db().get(b"key").expect("get").as_deref(),
        Some(&b"value"[..])
    );
}

#[test]
fn a_transaction_rollback_discards() {
    let dir = TempDir::new().expect("tempdir");
    let db = optimistic(&dir);

    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    txn.put(b"key", b"value").expect("put");
    txn.rollback();

    assert_eq!(db.db().get(b"key").expect("get"), None);
}

#[test]
fn a_transaction_reads_its_own_writes() {
    let dir = TempDir::new().expect("tempdir");
    let db = optimistic(&dir);

    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    txn.put(b"key", b"buffered").expect("put");
    assert_eq!(
        txn.get(b"key").expect("get").as_deref(),
        Some(&b"buffered"[..])
    );
    txn.commit().expect("commit");
}

/// The point of the type: a transaction that outlives every named handle
/// on the database it came from. Dropping `db` here would invalidate a
/// borrowing `Transaction`, so this is what the `Arc` inside buys.
#[test]
fn a_transaction_outlives_every_named_db_handle() {
    let dir = TempDir::new().expect("tempdir");

    let txn = {
        let db = optimistic(&dir);
        let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
        txn.put(b"key", b"value").expect("put");
        txn
    };

    txn.put(b"second", b"value")
        .expect("put after db handle dropped");
    txn.commit().expect("commit after db handle dropped");

    let db = optimistic(&dir);
    assert_eq!(
        db.db().get(b"key").expect("get").as_deref(),
        Some(&b"value"[..])
    );
    assert_eq!(
        db.db().get(b"second").expect("get").as_deref(),
        Some(&b"value"[..])
    );
}

/// A boxed trait object is the shape a storage layer actually stores.
#[test]
fn a_transaction_fits_a_static_trait_object() {
    trait Unit {
        fn run(self: Box<Self>) -> Result<(), TransactionError>;
    }

    impl Unit for Transaction {
        fn run(self: Box<Self>) -> Result<(), TransactionError> {
            self.put(b"boxed", b"value")?;
            (*self).commit().map(|_| ())
        }
    }

    let dir = TempDir::new().expect("tempdir");
    let db = optimistic(&dir);

    let unit: Box<dyn Unit> =
        Box::new(db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable)));
    unit.run().expect("commit through trait object");

    assert_eq!(
        db.db().get(b"boxed").expect("get").as_deref(),
        Some(&b"value"[..])
    );
}

/// Serializable validation must still fire for a transaction held past its
/// database handle's scope: a read the other writer invalidated has to lose
/// at commit.
#[test]
fn a_transaction_still_validates_serializable() {
    let dir = TempDir::new().expect("tempdir");
    let db = optimistic(&dir);
    db.db().put(b"key", b"first").expect("seed");

    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    assert_eq!(
        txn.get(b"key").expect("get").as_deref(),
        Some(&b"first"[..])
    );

    db.db().put(b"key", b"second").expect("concurrent write");

    txn.put(b"other", b"value").expect("put");
    let outcome = txn.commit();
    assert!(
        matches!(outcome, Err(TransactionError::Conflict { .. })),
        "serializable commit must reject a read the concurrent write invalidated, got {outcome:?}"
    );
}

#[test]
fn a_transaction_works_on_the_pessimistic_db() {
    let dir = TempDir::new().expect("tempdir");
    let db = Arc::new(TransactionDb::open(dir.path(), Options::default()).expect("open txn db"));

    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    txn.put(b"key", b"value").expect("put");
    txn.commit().expect("commit");

    assert_eq!(
        db.db().get(b"key").expect("get").as_deref(),
        Some(&b"value"[..])
    );
}

#[test]
fn db_slice_adopts_an_owned_vec() {
    let slice: DbSlice = b"assembled elsewhere".to_vec().into();

    assert_eq!(&*slice, b"assembled elsewhere");
    assert_eq!(slice.len(), 19);
    assert_eq!(slice.to_vec(), b"assembled elsewhere".to_vec());
}

#[test]
fn db_slice_adopts_an_empty_vec() {
    let slice: DbSlice = Vec::new().into();

    assert!(slice.is_empty());
    assert_eq!(&*slice, b"");
}

/// Buffering now takes `&self`, so a shared transaction can be written
/// from several threads at once. Every write must survive: a lost
/// insert here would mean the concurrent write buffer drops data.
#[test]
fn concurrent_writes_through_a_shared_transaction_all_land() {
    const THREADS: usize = 8;
    const PER_THREAD: usize = 64;

    let dir = TempDir::new().expect("tempdir");
    let db = optimistic(&dir);
    let txn = Arc::new(db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable)));

    std::thread::scope(|scope| {
        for thread in 0..THREADS {
            let txn = Arc::clone(&txn);
            scope.spawn(move || {
                for i in 0..PER_THREAD {
                    let key = format!("k/{thread}/{i}");
                    txn.put(key.as_bytes(), key.as_bytes()).expect("put");
                }
            });
        }
    });

    Arc::into_inner(txn)
        .expect("sole owner after the scope joined")
        .commit()
        .expect("commit");

    for thread in 0..THREADS {
        for i in 0..PER_THREAD {
            let key = format!("k/{thread}/{i}");
            assert_eq!(
                db.db().get(key.as_bytes()).expect("get").as_deref(),
                Some(key.as_bytes()),
                "{key} was lost by the concurrent write buffer"
            );
        }
    }
}

/// Reads take `&self` too, and every one of them has to reach the
/// commit-time validation set under `Serializable`. If a concurrent
/// read were lost, the write that invalidated it would go undetected
/// and the transaction would commit when it must not.
#[test]
fn concurrent_reads_through_a_shared_transaction_are_all_validated() {
    const THREADS: usize = 8;
    const KEYS: usize = 32;

    let dir = TempDir::new().expect("tempdir");
    let db = optimistic(&dir);
    for i in 0..KEYS {
        db.db()
            .put(format!("k/{i}").as_bytes(), b"first")
            .expect("seed");
    }

    let txn = Arc::new(db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable)));
    std::thread::scope(|scope| {
        for _ in 0..THREADS {
            let txn = Arc::clone(&txn);
            scope.spawn(move || {
                for i in 0..KEYS {
                    txn.get(format!("k/{i}").as_bytes()).expect("get");
                }
            });
        }
    });

    // Invalidate one of the keys every thread read.
    db.db().put(b"k/17", b"second").expect("concurrent write");

    let txn = Arc::into_inner(txn).expect("sole owner after the scope joined");
    txn.put(b"unrelated", b"value").expect("put");
    let outcome = txn.commit();
    assert!(
        matches!(outcome, Err(TransactionError::Conflict { .. })),
        "a read recorded from another thread must still be validated, got {outcome:?}"
    );
}

/// The transaction's buffer switches lookup strategy once it grows past a
/// threshold. Cross it in both directions and check nothing is lost,
/// reordered, or stale.
#[test]
fn a_transaction_keeps_every_key_across_the_buffer_spill_threshold() {
    // Comfortably past the internal SPILL_AT so the promotion runs.
    const KEYS: usize = 200;

    let dir = TempDir::new().expect("tempdir");
    let db = optimistic(&dir);
    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));

    for i in 0..KEYS {
        txn.put(format!("k/{i:04}").as_bytes(), format!("v{i}").as_bytes())
            .expect("put");
    }
    // Every key must read back through the buffer, including the ones
    // written before the promotion and the ones written after.
    for i in 0..KEYS {
        assert_eq!(
            txn.get(format!("k/{i:04}").as_bytes())
                .expect("get")
                .as_deref(),
            Some(format!("v{i}").as_bytes()),
            "key {i} was lost across the spill threshold"
        );
    }

    txn.commit().expect("commit");
    for i in 0..KEYS {
        assert_eq!(
            db.db()
                .get(format!("k/{i:04}").as_bytes())
                .expect("get")
                .as_deref(),
            Some(format!("v{i}").as_bytes()),
            "key {i} did not survive the commit"
        );
    }
}

/// Overwriting a key has to win no matter which side of the threshold the
/// original and the replacement landed on.
#[test]
fn a_later_write_wins_across_the_buffer_spill_threshold() {
    const KEYS: usize = 200;

    let dir = TempDir::new().expect("tempdir");
    let db = optimistic(&dir);
    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));

    for i in 0..KEYS {
        txn.put(format!("k/{i:04}").as_bytes(), b"first")
            .expect("put");
    }
    for i in 0..KEYS {
        txn.put(format!("k/{i:04}").as_bytes(), b"second")
            .expect("put");
    }
    for i in 0..KEYS {
        assert_eq!(
            txn.get(format!("k/{i:04}").as_bytes())
                .expect("get")
                .as_deref(),
            Some(&b"second"[..]),
            "key {i} kept a stale value"
        );
    }

    txn.commit().expect("commit");
    for i in 0..KEYS {
        assert_eq!(
            db.db()
                .get(format!("k/{i:04}").as_bytes())
                .expect("get")
                .as_deref(),
            Some(&b"second"[..]),
            "key {i} committed the value it replaced"
        );
    }
}

/// A delete after the buffer has spilled must still hide the key.
#[test]
fn a_delete_after_the_spill_threshold_still_applies() {
    const KEYS: usize = 100;

    let dir = TempDir::new().expect("tempdir");
    let db = optimistic(&dir);
    for i in 0..KEYS {
        db.db()
            .put(format!("k/{i:04}").as_bytes(), b"committed")
            .expect("seed");
    }

    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    for i in 0..KEYS {
        txn.delete(format!("k/{i:04}").as_bytes()).expect("delete");
    }
    txn.commit().expect("commit");

    for i in 0..KEYS {
        assert_eq!(
            db.db().get(format!("k/{i:04}").as_bytes()).expect("get"),
            None,
            "key {i} survived a delete made past the spill threshold"
        );
    }
}

/// Concurrent writers pushing one shared transaction past the threshold
/// is where a promotion race would show up as a lost key.
#[test]
fn concurrent_writers_cross_the_spill_threshold_without_losing_a_key() {
    const THREADS: usize = 8;
    const PER_THREAD: usize = 100;

    let dir = TempDir::new().expect("tempdir");
    let db = optimistic(&dir);
    let txn = Arc::new(db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable)));

    std::thread::scope(|scope| {
        for thread in 0..THREADS {
            let txn = Arc::clone(&txn);
            scope.spawn(move || {
                for i in 0..PER_THREAD {
                    let key = format!("k/{thread}/{i:04}");
                    txn.put(key.as_bytes(), key.as_bytes()).expect("put");
                }
            });
        }
    });

    Arc::into_inner(txn)
        .expect("sole owner after the scope joined")
        .commit()
        .expect("commit");

    for thread in 0..THREADS {
        for i in 0..PER_THREAD {
            let key = format!("k/{thread}/{i:04}");
            assert_eq!(
                db.db().get(key.as_bytes()).expect("get").as_deref(),
                Some(key.as_bytes()),
                "{key} was lost while the buffer was promoting"
            );
        }
    }
}

/// The threshold is a knob, so a database opened with a different one has
/// to behave the same way and only differ in when it indexes.
#[test]
fn the_buffer_threshold_is_configurable_and_correct_at_every_setting() {
    const KEYS: usize = 80;

    for keys_inline in [0, 1, 8, 32, 1024] {
        let dir = TempDir::new().expect("tempdir");
        let opts = Options::default().transaction_keys_inline(keys_inline);
        let db = Arc::new(OptimisticTransactionDb::open(dir.path(), opts).expect("open"));
        let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));

        for i in 0..KEYS {
            txn.put(format!("k/{i:04}").as_bytes(), b"first")
                .expect("put");
        }
        for i in (0..KEYS).step_by(2) {
            txn.put(format!("k/{i:04}").as_bytes(), b"second")
                .expect("put");
        }
        for i in 0..KEYS {
            let expected: &[u8] = if i % 2 == 0 { b"second" } else { b"first" };
            assert_eq!(
                txn.get(format!("k/{i:04}").as_bytes())
                    .expect("get")
                    .as_deref(),
                Some(expected),
                "key {i} read wrong at transaction_keys_inline={keys_inline}"
            );
        }
        txn.commit().expect("commit");

        for i in 0..KEYS {
            let expected: &[u8] = if i % 2 == 0 { b"second" } else { b"first" };
            assert_eq!(
                db.db()
                    .get(format!("k/{i:04}").as_bytes())
                    .expect("get")
                    .as_deref(),
                Some(expected),
                "key {i} committed wrong at transaction_keys_inline={keys_inline}"
            );
        }
    }
}
