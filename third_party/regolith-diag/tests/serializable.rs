//! Serializability, checked by the anomaly it has to exclude.
//!
//! Write skew is the anomaly that separates snapshot isolation from
//! serializability, and it is the one a test has to exhibit: an engine
//! that permits it is not serializable however many other properties it
//! satisfies.
//!
//! The classic shape, from the SI literature: an invariant spans two
//! keys, each transaction checks the *other* key and writes its own, and
//! neither sees the other's write because both read from the same
//! snapshot. Both commit, and the invariant they each preserved
//! individually is broken jointly.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use regolith::{IsolationLevel, OptimisticTransactionDb, Options, TransactionError, TxnOptions};
use tempfile::TempDir;

mod common;

/// Run the two-key write-skew schedule with both transactions
/// interleaved, and report how many of the pairs committed together.
///
/// The barrier is what makes the schedule the anomaly rather than two
/// transactions that merely ran near each other: both must complete
/// their reads before either commits, which is exactly the interleaving
/// snapshot isolation permits and serializability forbids.
fn write_skew_pairs(isolation: IsolationLevel, rounds: usize) -> usize {
    let dir = TempDir::new().expect("tempdir");
    let db = Arc::new(
        OptimisticTransactionDb::open(dir.path(), Options::default())
            .expect("open")
            .with_isolation(isolation),
    );
    let both = Arc::new(AtomicUsize::new(0));

    for round in 0..rounds {
        let x = format!("x{round:04}").into_bytes();
        let y = format!("y{round:04}").into_bytes();
        db.db().put(&x, b"0").expect("seed x");
        db.db().put(&y, b"0").expect("seed y");

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for (mine, theirs) in [(x.clone(), y.clone()), (y.clone(), x.clone())] {
            let db = Arc::clone(&db);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                let tx = db.begin(&TxnOptions::new());
                // Read the key the *other* transaction is about to write.
                // A plain read: this is the edge snapshot isolation does
                // not validate.
                let seen = tx.get(&theirs).expect("read");
                // Both transactions have now read, and neither has
                // written. Whatever happens next, they overlap.
                barrier.wait();
                if seen.as_deref() == Some(b"0".as_ref()) {
                    tx.put(&mine, b"1").expect("write");
                }
                common::committed(&tx.commit())
            }));
        }
        let committed = handles
            .into_iter()
            .filter(|_| true)
            .map(|h| h.join().expect("join"))
            .filter(|ok| *ok)
            .count();
        if committed == 2 {
            let vx = db.db().get(&x).expect("get x");
            let vy = db.db().get(&y).expect("get y");
            // Both wrote only because each saw the other still at "0".
            // Serially, the second would have seen "1" and written
            // nothing.
            if vx.as_deref() == Some(b"1".as_ref()) && vy.as_deref() == Some(b"1".as_ref()) {
                both.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    both.load(Ordering::Relaxed)
}

/// Serializable must exclude write skew outright.
#[test]
fn serializable_excludes_write_skew() {
    let skews = write_skew_pairs(IsolationLevel::Serializable, 40);
    assert_eq!(
        skews, 0,
        "{skews} write-skew pair(s) committed under Serializable; validating the whole \
         read set should have aborted the second of each pair"
    );
}

/// RepeatableRead keeps the point-read half of Serializable. The schedule reads
/// through `get`, so it must be refused there exactly as it is above.
#[test]
fn repeatable_read_excludes_the_write_skew_of_point_reads() {
    let skews = write_skew_pairs(IsolationLevel::RepeatableRead, 40);
    assert_eq!(
        skews, 0,
        "{skews} write-skew pair(s) committed under RepeatableRead; a point read is validated \
         there and should have aborted the second of each pair"
    );
}

/// The calibration: the same schedule under snapshot isolation must be
/// able to produce the anomaly.
///
/// Without this, `serializable_excludes_write_skew` could pass because
/// the schedule never actually interleaves, and the stronger level would
/// be proving nothing.
#[test]
fn snapshot_isolation_admits_the_write_skew_serializable_excludes() {
    let skews = write_skew_pairs(IsolationLevel::SnapshotIsolation, 40);
    assert!(
        skews > 0,
        "the schedule produced no write skew even under snapshot isolation, so it is not \
         exercising the anomaly and the serializable test above proves nothing"
    );
}

/// Serializable must still let non-conflicting work through. A level
/// that aborts everything trivially excludes every anomaly.
#[test]
fn serializable_commits_transactions_that_do_not_conflict() {
    let dir = TempDir::new().expect("tempdir");
    let db = OptimisticTransactionDb::open(dir.path(), Options::default())
        .expect("open")
        .with_isolation(IsolationLevel::Serializable);

    for i in 0..200u64 {
        let tx = db.begin(&TxnOptions::new());
        let k = format!("k{i:04}");
        tx.get(k.as_bytes()).expect("read");
        tx.put(k.as_bytes(), b"v").expect("write");
        tx.commit()
            .expect("a transaction touching only its own key must commit");
    }
    for i in 0..200u64 {
        let k = format!("k{i:04}");
        assert_eq!(db.db().get(k.as_bytes()).expect("get"), Some(b"v".to_vec()));
    }
}

/// A read-only transaction must commit under Serializable even while
/// other keys are being written, or the level is unusable for queries.
#[test]
fn serializable_read_only_transactions_commit_under_concurrent_writes() {
    let dir = TempDir::new().expect("tempdir");
    let db = OptimisticTransactionDb::open(dir.path(), Options::default())
        .expect("open")
        .with_isolation(IsolationLevel::Serializable);
    for i in 0..50u64 {
        db.db()
            .put(format!("r{i:04}").as_bytes(), b"v")
            .expect("seed");
    }

    let tx = db.begin(&TxnOptions::new());
    for i in 0..50u64 {
        tx.get(format!("r{i:04}").as_bytes()).expect("read");
    }
    // Writes to keys this transaction never read.
    for i in 0..50u64 {
        db.db()
            .put(format!("other{i:04}").as_bytes(), b"v")
            .expect("write");
    }
    tx.commit()
        .expect("a read-only transaction must not conflict with writes it never read");
}

/// The level is per transaction, not only per database: one serializable
/// unit of work in an otherwise snapshot-isolated database must get
/// serializable validation.
#[test]
fn the_level_can_be_chosen_per_transaction() {
    let dir = TempDir::new().expect("tempdir");
    let db = OptimisticTransactionDb::open(dir.path(), Options::default()).expect("open");
    assert_eq!(db.isolation(), IsolationLevel::SnapshotIsolation);

    db.db().put(b"a", b"0").expect("seed");
    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    tx.get(b"a").expect("read");
    // A concurrent commit to the key this transaction read.
    db.db().put(b"a", b"1").expect("concurrent write");

    match tx.commit() {
        Err(TransactionError::Conflict { .. }) => {}
        other => panic!(
            "a serializable transaction must abort when a key it read was written; got {other:?}"
        ),
    }
}
