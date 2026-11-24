//! Concurrency-level integration tests for the transaction API.
//!
//! The workload that matters here is the lost-update one: several
//! threads read-modify-write a shared counter through
//! `get_for_update` -> `put` -> `commit`, and every increment must
//! survive. A stale read inside the critical section shows up as a
//! final count below the number of increments performed, so the
//! assertions are on the exact count, never on a range.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads, the filesystem or proptest, none of which exist there. The browser
// suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::time::Duration;

use regolith::{
    OptimisticTransactionDb, Options, TransactionDb, TransactionError, TxResult, TxnOptions,
};
use tempfile::TempDir;

const THREADS: u64 = 8;
const PER_THREAD: u64 = 50;

/// Retry budget per increment. Generous enough that ordinary
/// contention never exhausts it, bounded so a livelock fails the test
/// loudly instead of hanging it or silently under-counting.
const MAX_ATTEMPTS: usize = 200;

fn decode(raw: Option<Vec<u8>>) -> u64 {
    match raw {
        Some(bytes) => {
            let counter: [u8; 8] = bytes.as_slice().try_into().expect("counter is 8 bytes");
            u64::from_le_bytes(counter)
        }
        None => 0,
    }
}

/// Run `attempt` until it commits, retrying only the two retry-able
/// outcomes. Panics with the attempt budget if it never commits.
fn commit_with_retry<T, F>(key: &[u8], mut attempt: F)
where
    F: FnMut() -> TxResult<T>,
{
    for _ in 0..MAX_ATTEMPTS {
        match attempt() {
            Ok(_) => return,
            Err(TransactionError::Busy(_)) | Err(TransactionError::Conflict { .. }) => {
                std::thread::yield_now();
            }
            Err(e) => panic!("unexpected transaction error on {key:?}: {e}"),
        }
    }
    panic!("increment on {key:?} did not commit within {MAX_ATTEMPTS} attempts");
}

fn pessimistic_increment(db: &TransactionDb, key: &[u8]) -> TxResult<()> {
    let tx = db.begin(&TxnOptions::new());
    let current = decode(tx.get_for_update(key)?);
    tx.put(key, &(current + 1).to_le_bytes())?;
    tx.commit().map(|_| ())
}

fn optimistic_increment(db: &OptimisticTransactionDb, key: &[u8]) -> TxResult<()> {
    let tx = db.begin(&TxnOptions::new());
    let current = decode(tx.get_for_update(key)?);
    tx.put(key, &(current + 1).to_le_bytes())?;
    tx.commit().map(|_| ())
}

#[test]
fn pessimistic_concurrent_increments_do_not_lose_updates() {
    const COUNTER: &[u8] = b"counter";
    let dir = TempDir::new().unwrap();
    let db = TransactionDb::open(dir.path(), Options::default())
        .unwrap()
        .with_lock_timeout(Duration::from_secs(5));

    std::thread::scope(|scope| {
        for _ in 0..THREADS {
            scope.spawn(|| {
                for _ in 0..PER_THREAD {
                    commit_with_retry(COUNTER, || pessimistic_increment(&db, COUNTER));
                }
            });
        }
    });

    let expected = THREADS * PER_THREAD;
    let surviving = decode(db.db().get(COUNTER).unwrap());
    println!("pessimistic surviving increments: {surviving} of {expected}");
    assert_eq!(surviving, expected);
}

#[test]
fn optimistic_concurrent_increments_do_not_lose_updates() {
    const COUNTER: &[u8] = b"counter";
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();

    std::thread::scope(|scope| {
        for _ in 0..THREADS {
            scope.spawn(|| {
                for _ in 0..PER_THREAD {
                    commit_with_retry(COUNTER, || optimistic_increment(&db, COUNTER));
                }
            });
        }
    });

    let expected = THREADS * PER_THREAD;
    let surviving = decode(db.db().get(COUNTER).unwrap());
    println!("optimistic surviving increments: {surviving} of {expected}");
    assert_eq!(surviving, expected);
}

#[test]
fn concurrent_increments_across_multiple_keys() {
    const KEYS: [&[u8]; 4] = [b"k0", b"k1", b"k2", b"k3"];
    const KEY_THREADS: u64 = 4;
    const KEY_ROUNDS: u64 = 25;

    let dir = TempDir::new().unwrap();
    let db = TransactionDb::open(dir.path(), Options::default())
        .unwrap()
        .with_lock_timeout(Duration::from_secs(5));

    std::thread::scope(|scope| {
        for _ in 0..KEY_THREADS {
            scope.spawn(|| {
                for _ in 0..KEY_ROUNDS {
                    for key in KEYS {
                        commit_with_retry(key, || pessimistic_increment(&db, key));
                    }
                }
            });
        }
    });

    for key in KEYS {
        let surviving = decode(db.db().get(key).unwrap());
        println!("key {key:?} surviving increments: {surviving}");
        assert_eq!(surviving, KEY_THREADS * KEY_ROUNDS);
    }
}
