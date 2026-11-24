//! What the lock table holds as transactions come and go, seen from inside:
//! the table stays bounded by the locks in use, and `close` releases the
//! locks of the transactions it aborts and no others.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use super::*;
use crate::{LockWait, WouldBlock};

fn pes_db() -> (TransactionDb, TempDir) {
    let dir = TempDir::new().unwrap();
    let db = TransactionDb::open(dir.path(), Options::default()).unwrap();
    (db, dir)
}

/// The name holding `key`'s lock now, `0` when it is free.
fn holder(db: &TransactionDb, key: &[u8]) -> usize {
    let prefixed = prefix_key(DEFAULT_CF_ID, key);
    let lock = db.locks.pin(&prefixed);
    let holder = lock.lock.holder();
    db.locks.unpin(&prefixed, &lock);
    holder
}

#[test]
fn the_table_holds_only_the_locks_in_use() {
    let (db, _dir) = pes_db();
    assert_eq!(db.locks.len(), 0);
    let holder = db.begin(&TxnOptions::new());
    for i in 0..100u32 {
        holder.put(&i.to_le_bytes(), b"v").unwrap();
    }
    assert_eq!(db.locks.len(), 100);
    let waiter = db.begin(&TxnOptions::new());
    assert!(matches!(
        waiter.try_get_for_update(&7u32.to_le_bytes()),
        Err(TransactionError::WouldBlock(WouldBlock::Locked(_)))
    ));
    holder.commit().unwrap();
    assert_eq!(db.locks.len(), 1, "only the key still waited on");
    waiter.rollback();
    assert_eq!(db.locks.len(), 0, "a wait given up leaves nothing");
    // A wait that timed out leaves nothing either.
    let db = db.with_lock_timeout(Duration::from_millis(10));
    let holder = db.begin(&TxnOptions::new());
    holder.put(b"k", b"v").unwrap();
    let late = db.begin(&TxnOptions::new());
    assert!(matches!(
        late.put(b"k", b"w"),
        Err(TransactionError::Busy(_))
    ));
    holder.rollback();
    assert_eq!(db.locks.len(), 0);
    drop(late);
    assert_eq!(db.locks.len(), 0);
}

#[test]
fn close_releases_the_locks_of_the_transactions_it_aborts() {
    let (db, _dir) = pes_db();
    let aborted = Arc::new(AtomicUsize::new(0));
    let counts = |txn: &mut Transaction| {
        let aborted = Arc::clone(&aborted);
        txn.on_abort(move |_| {
            aborted.fetch_add(1, Ordering::SeqCst);
        });
    };
    // A claim made before the first lock, and one made after it.
    let mut early = db.begin(&TxnOptions::new());
    counts(&mut early);
    early.put(b"early", b"v").unwrap();
    let mut late = db.begin(&TxnOptions::new());
    late.put(b"late", b"v").unwrap();
    counts(&mut late);
    // Nothing for close to abort: it has no callback.
    let plain = db.begin(&TxnOptions::new());
    plain.put(b"plain", b"v").unwrap();
    assert_ne!(holder(&db, b"early"), 0);
    assert_ne!(holder(&db, b"late"), 0);

    db.db().close().unwrap();

    assert_eq!(aborted.load(Ordering::SeqCst), 2, "close aborted both");
    assert_eq!(holder(&db, b"early"), 0, "its lock went with it");
    assert_eq!(holder(&db, b"late"), 0, "its lock went with it");
    assert_ne!(holder(&db, b"plain"), 0, "close did not abort this one");
    // Each ends as usual; a lock close released is not released twice.
    drop((early, late, plain));
    assert_eq!(db.locks.len(), 0);
}

/// Two calls of one transaction find the key held by another transaction at
/// once: the second runs whole inside the first, between the first's lookup
/// and its publication. They must share one wait, so the holder's release
/// reaches a call that polls it, and that call takes the free lock.
#[test]
fn two_calls_behind_a_holder_share_one_wait_and_the_release_reaches_it() {
    let (db, _dir) = pes_db();
    let other = db.begin(&TxnOptions::new());
    other.put(b"k", b"v").unwrap();
    let txn = Arc::new(db.begin(&TxnOptions::new()));
    let key = prefix_key(DEFAULT_CF_ID, b"k");
    let inner = Arc::new(Mutex::new(None));
    {
        let (txn, key, inner) = (Arc::clone(&txn), key.clone(), Arc::clone(&inner));
        locking::before_publish(move || {
            *inner.lock().unwrap() = Some(txn.try_lock_key(&key));
        });
    }
    let waits: Vec<LockWait> = [
        txn.try_lock_key(&key),
        inner.lock().unwrap().take().unwrap(),
    ]
    .into_iter()
    .map(|call| match call {
        Err(TransactionError::WouldBlock(WouldBlock::Locked(wait))) => wait,
        other => panic!("the key is held: {other:?}"),
    })
    .collect();
    assert!(waits.iter().all(|wait| !wait.is_ready()));

    other.commit().unwrap();
    assert!(
        waits.iter().any(LockWait::is_ready),
        "the release told neither call"
    );
    assert!(
        matches!(txn.try_lock_key(&key), Ok(false)),
        "the told call runs again and takes the free lock"
    );
    assert_ne!(holder(&db, b"k"), 0);
}

/// E42. A thread's transactions, one after another, share one lock owner:
/// none is allocated in steady state. A commit whose locks wait for its
/// landing keeps its owner, and the next transaction gets another name.
#[test]
fn owners_are_lent_to_the_next_transaction_unless_a_landing_holds_them() {
    let dir = TempDir::new().unwrap();
    let db = TransactionDb::open(
        dir.path(),
        Options::default().durability(crate::DurabilityMode::Immediate),
    )
    .unwrap();
    let name = |txn: &Transaction| txn.lock_owner.as_ref().unwrap().id().word();
    let first = db.begin(&TxnOptions::new());
    let lent = name(&first);
    first.put(b"a", b"1").unwrap();
    first.commit().unwrap();
    let second = db.begin(&TxnOptions::new());
    assert_eq!(name(&second), lent, "a plain commit gives its owner back");

    let mut queue = db.db().io_queue();
    let landing = db.begin(&TxnOptions::new().io_queue(queue.id()));
    let held = name(&landing);
    landing.put(b"b", b"2").unwrap();
    let ticket = landing.commit_nowait();
    assert!(!ticket.is_ready(), "an Immediate commit waits for its sync");
    let next = db.begin(&TxnOptions::new());
    assert_ne!(
        name(&next),
        held,
        "an owner was lent while its commit's locks wait for the landing"
    );
    queue.block_on(ticket).unwrap();
    drop((second, next));
}
