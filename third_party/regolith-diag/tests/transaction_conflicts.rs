//! Conflict-detection regression tests for the transaction API.
//!
//! Every test here reproduced a lost update, a spurious abort, or a
//! non-monotonic read before the conflict-tracking rework. The
//! assertions are on exact counts and exact outcomes, never on ranges:
//! a lost update shows up as a final count below the number of
//! increments performed.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads, the filesystem or proptest, none of which exist there. The browser
// suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use regolith::{
    Db, IsolationLevel, MergeOperator, OptimisticTransactionDb, Options, TransactionDb,
    TransactionError, TxResult, TxnOptions,
};
use tempfile::TempDir;

/// Retry budget per increment. Generous enough that ordinary
/// contention never exhausts it, bounded so a livelock fails loudly
/// instead of hanging or silently under-counting.
const MAX_ATTEMPTS: usize = 2_000;

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
fn commit_with_retry<T, F>(what: &str, mut attempt: F)
where
    F: FnMut() -> TxResult<T>,
{
    for _ in 0..MAX_ATTEMPTS {
        match attempt() {
            Ok(_) => return,
            Err(TransactionError::Busy(_)) | Err(TransactionError::Conflict { .. }) => {
                std::thread::yield_now();
            }
            Err(e) => panic!("unexpected transaction error in {what}: {e}"),
        }
    }
    panic!("{what} did not commit within {MAX_ATTEMPTS} attempts");
}

fn pes_db(dir: &TempDir) -> TransactionDb {
    TransactionDb::open(dir.path(), Options::default())
        .unwrap()
        .with_lock_timeout(Duration::from_secs(10))
}

// ---- lost updates through a plain read, no lock ----

#[test]
fn pessimistic_plain_read_then_write_never_loses_an_update() {
    const THREADS: u64 = 8;
    const PER_THREAD: u64 = 50;
    const COUNTER: &[u8] = b"counter";

    let dir = TempDir::new().unwrap();
    let db = pes_db(&dir);
    std::thread::scope(|scope| {
        for _ in 0..THREADS {
            scope.spawn(|| {
                for _ in 0..PER_THREAD {
                    commit_with_retry("plain-read increment", || {
                        let tx = db.begin(&TxnOptions::new());
                        let current = decode(tx.get(COUNTER)?);
                        tx.put(COUNTER, &(current + 1).to_le_bytes())?;
                        tx.commit()
                    });
                }
            });
        }
    });
    assert_eq!(decode(db.db().get(COUNTER).unwrap()), THREADS * PER_THREAD);
}

#[test]
fn optimistic_plain_read_then_write_never_loses_an_update() {
    const THREADS: u64 = 8;
    const PER_THREAD: u64 = 50;
    const COUNTER: &[u8] = b"counter";

    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
    std::thread::scope(|scope| {
        for _ in 0..THREADS {
            scope.spawn(|| {
                for _ in 0..PER_THREAD {
                    commit_with_retry("plain-read increment", || {
                        let tx = db.begin(&TxnOptions::new());
                        let current = decode(tx.get(COUNTER)?);
                        tx.put(COUNTER, &(current + 1).to_le_bytes())?;
                        tx.commit()
                    });
                }
            });
        }
    });
    assert_eq!(decode(db.db().get(COUNTER).unwrap()), THREADS * PER_THREAD);
}

// ---- writers that bypass the lock manager ----

#[test]
fn a_write_batch_around_the_lock_manager_is_detected() {
    let dir = TempDir::new().unwrap();
    let db = pes_db(&dir);
    regolith::through_stalls(|| db.db().put(b"k", b"v0")).unwrap();

    let tx = db.begin(&TxnOptions::new());
    assert_eq!(tx.get_for_update(b"k").unwrap(), Some(b"v0".to_vec()));
    let mut batch = regolith::WriteBatch::new();
    batch.put(b"k", b"racer");
    regolith::through_stalls(|| db.db().write(batch.clone())).unwrap();
    tx.put(b"k", b"mine").unwrap();
    assert!(matches!(
        tx.commit(),
        Err(TransactionError::Conflict { .. })
    ));
    assert_eq!(db.db().get(b"k").unwrap(), Some(b"racer".to_vec()));
}

/// The property the storm test below races for, forced instead of
/// raced: a plain `Db::put` landing between a transaction's
/// `get_for_update` and its `commit` must abort that commit.
#[test]
fn a_raw_put_between_read_and_commit_is_detected() {
    let dir = TempDir::new().unwrap();
    let db = pes_db(&dir);
    regolith::through_stalls(|| db.db().put(b"k", &0u64.to_le_bytes())).unwrap();

    let tx = db.begin(&TxnOptions::new());
    let current = decode(tx.get_for_update(b"k").unwrap());
    assert_eq!(current, 0);

    // The bypassing writer, placed exactly in the window rather than
    // left to a scheduler to hit by chance.
    regolith::through_stalls(|| db.db().put(b"k", &1_000_000u64.to_le_bytes())).unwrap();

    tx.put(b"k", &(current + 1).to_le_bytes()).unwrap();
    assert!(
        matches!(tx.commit(), Err(TransactionError::Conflict { .. })),
        "a raw put inside the read-commit window must abort the transaction"
    );
    assert_eq!(
        decode(db.db().get(b"k").unwrap()),
        1_000_000,
        "the bypassing write stands and the aborted increment did not land"
    );
}
#[test]
fn a_range_delete_around_the_lock_manager_is_detected() {
    for pessimistic in [true, false] {
        let dir = TempDir::new().unwrap();
        let outcome = if pessimistic {
            let db = pes_db(&dir);
            regolith::through_stalls(|| db.db().put(b"k", b"v0")).unwrap();
            let tx = db.begin(&TxnOptions::new());
            assert_eq!(tx.get_for_update(b"k").unwrap(), Some(b"v0".to_vec()));
            regolith::through_stalls(|| db.db().delete_range(b"a", b"z")).unwrap();
            tx.put(b"k", b"resurrected").unwrap();
            let r = tx.commit();
            assert_eq!(db.db().get(b"k").unwrap(), None);
            r
        } else {
            let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
            regolith::through_stalls(|| db.db().put(b"k", b"v0")).unwrap();
            let tx = db.begin(&TxnOptions::new());
            assert_eq!(tx.get_for_update(b"k").unwrap(), Some(b"v0".to_vec()));
            regolith::through_stalls(|| db.db().delete_range(b"a", b"z")).unwrap();
            tx.put(b"k", b"resurrected").unwrap();
            let r = tx.commit();
            assert_eq!(db.db().get(b"k").unwrap(), None);
            r
        };
        assert!(
            matches!(outcome, Err(TransactionError::Conflict { .. })),
            "pessimistic={pessimistic}: {outcome:?}"
        );
    }
}

#[derive(Debug)]
struct Concat;

impl MergeOperator for Concat {
    fn name(&self) -> &'static str {
        "concat"
    }

    fn full_merge(
        &self,
        _key: &[u8],
        existing: Option<&[u8]>,
        operands: &[&[u8]],
    ) -> Option<Vec<u8>> {
        let mut out = existing.map(|e| e.to_vec()).unwrap_or_default();
        for op in operands {
            out.extend_from_slice(op);
        }
        Some(out)
    }
}

#[test]
fn an_external_merge_on_a_tracked_key_is_detected() {
    let dir = TempDir::new().unwrap();
    let db = TransactionDb::open(
        dir.path(),
        Options::default().merge_operator(Some(Arc::new(Concat))),
    )
    .unwrap();
    regolith::through_stalls(|| db.db().put(b"k", b"a")).unwrap();

    let tx = db.begin(&TxnOptions::new());
    assert_eq!(tx.get_for_update(b"k").unwrap(), Some(b"a".to_vec()));
    regolith::through_stalls(|| db.db().merge(b"k", b"b")).unwrap();
    tx.put(b"k", b"mine").unwrap();
    assert!(matches!(
        tx.commit(),
        Err(TransactionError::Conflict { .. })
    ));
}

// ---- savepoints ----

#[test]
fn savepoint_rollback_does_not_launder_a_bypassing_write() {
    let dir = TempDir::new().unwrap();
    let db = pes_db(&dir);
    regolith::through_stalls(|| db.db().put(b"k", b"v0")).unwrap();

    let mut tx = db.begin(&TxnOptions::new());
    assert_eq!(tx.get_for_update(b"k").unwrap(), Some(b"v0".to_vec()));
    tx.set_savepoint();
    tx.put(b"k", b"staged").unwrap();
    tx.rollback_to_savepoint().unwrap();
    regolith::through_stalls(|| db.db().put(b"k", b"racer")).unwrap();
    tx.put(b"k", b"mine").unwrap();
    assert!(matches!(
        tx.commit(),
        Err(TransactionError::Conflict { .. })
    ));
    assert_eq!(db.db().get(b"k").unwrap(), Some(b"racer".to_vec()));
}

#[test]
fn reads_inside_one_transaction_never_travel_backwards() {
    let dir = TempDir::new().unwrap();
    let db = pes_db(&dir);
    regolith::through_stalls(|| db.db().put(b"k", b"v0")).unwrap();

    let mut tx = db.begin(&TxnOptions::new());
    regolith::through_stalls(|| db.db().put(b"k", b"v1")).unwrap();
    let first = tx.get_for_update(b"k").unwrap();
    assert_eq!(first, Some(b"v1".to_vec()));
    tx.set_savepoint();
    tx.put(b"k", b"staged").unwrap();
    tx.rollback_to_savepoint().unwrap();
    assert_eq!(tx.get(b"k").unwrap(), first);
}

// ---- blind writes must not abort spuriously ----

#[test]
fn a_blind_write_never_conflicts_with_a_non_transactional_writer() {
    for external_first in [true, false] {
        let dir = TempDir::new().unwrap();
        let db = pes_db(&dir);
        regolith::through_stalls(|| db.db().put(b"k", b"v0")).unwrap();
        let tx = db.begin(&TxnOptions::new());
        if external_first {
            regolith::through_stalls(|| db.db().put(b"k", b"external")).unwrap();
            tx.put(b"k", b"mine").unwrap();
        } else {
            tx.put(b"k", b"mine").unwrap();
            regolith::through_stalls(|| db.db().put(b"k", b"external")).unwrap();
        }
        tx.commit()
            .unwrap_or_else(|e| panic!("external_first={external_first}: {e}"));
        assert_eq!(db.db().get(b"k").unwrap(), Some(b"mine".to_vec()));
    }
}

// ---- contention, locking, visibility ----

#[test]
fn high_contention_counter_keeps_every_increment() {
    const THREADS: u64 = 32;
    const PER_THREAD: u64 = 100;
    const COUNTER: &[u8] = b"counter";

    let dir = TempDir::new().unwrap();
    let db = pes_db(&dir);
    std::thread::scope(|scope| {
        for _ in 0..THREADS {
            scope.spawn(|| {
                for _ in 0..PER_THREAD {
                    commit_with_retry("contended increment", || {
                        let tx = db.begin(&TxnOptions::new());
                        let current = decode(tx.get_for_update(COUNTER)?);
                        tx.put(COUNTER, &(current + 1).to_le_bytes())?;
                        tx.commit()
                    });
                }
            });
        }
    });
    assert_eq!(decode(db.db().get(COUNTER).unwrap()), THREADS * PER_THREAD);
}

#[test]
fn increments_survive_flush_and_compaction_mid_run() {
    const THREADS: u64 = 6;
    const PER_THREAD: u64 = 60;
    const COUNTER: &[u8] = b"counter";

    let dir = TempDir::new().unwrap();
    let db = TransactionDb::open(dir.path(), Options::default().write_buffer_size(4 * 1024))
        .unwrap()
        .with_lock_timeout(Duration::from_secs(10));

    std::thread::scope(|scope| {
        for _ in 0..THREADS {
            scope.spawn(|| {
                for _ in 0..PER_THREAD {
                    commit_with_retry("increment across a flush", || {
                        let tx = db.begin(&TxnOptions::new());
                        let current = decode(tx.get_for_update(COUNTER)?);
                        tx.put(COUNTER, &(current + 1).to_le_bytes())?;
                        tx.commit()
                    });
                }
            });
        }
    });
    assert_eq!(decode(db.db().get(COUNTER).unwrap()), THREADS * PER_THREAD);
}

#[test]
fn read_modify_writes_interleaved_with_blind_puts_keep_their_reads() {
    const ROUNDS: u64 = 300;
    let dir = TempDir::new().unwrap();
    let db = Arc::new(pes_db(&dir));
    regolith::through_stalls(|| db.db().put(b"k", &0u64.to_le_bytes())).unwrap();

    let committed = Arc::new(AtomicU64::new(0));
    std::thread::scope(|scope| {
        let db_rmw = Arc::clone(&db);
        let committed = Arc::clone(&committed);
        scope.spawn(move || {
            for _ in 0..ROUNDS {
                commit_with_retry("rmw", || {
                    let tx = db_rmw.begin(&TxnOptions::new());
                    let current = decode(tx.get_for_update(b"k")?);
                    tx.put(b"k", &(current + 1).to_le_bytes())?;
                    tx.commit()
                });
                committed.fetch_add(1, Ordering::Relaxed);
            }
        });
        let db_blind = Arc::clone(&db);
        scope.spawn(move || {
            for _ in 0..ROUNDS {
                commit_with_retry("blind", || {
                    let tx = db_blind.begin(&TxnOptions::new());
                    tx.put(b"other", b"v")?;
                    tx.commit()
                });
            }
        });
    });
    assert_eq!(committed.load(Ordering::Relaxed), ROUNDS);
    assert_eq!(decode(db.db().get(b"k").unwrap()), ROUNDS);
}

#[test]
fn reverse_order_two_key_locking_does_not_deadlock() {
    // Each genuine deadlock costs one lock timeout, so the timeout is
    // short and the round count small: the property under test is
    // "always resolves", not throughput.
    const ROUNDS: u64 = 30;
    let dir = TempDir::new().unwrap();
    let db = TransactionDb::open(dir.path(), Options::default())
        .unwrap()
        .with_lock_timeout(Duration::from_millis(50));
    regolith::through_stalls(|| db.db().put(b"a", &0u64.to_le_bytes())).unwrap();
    regolith::through_stalls(|| db.db().put(b"b", &0u64.to_le_bytes())).unwrap();

    let bump = |db: &TransactionDb, first: &[u8], second: &[u8]| {
        commit_with_retry("two-key bump", || {
            let tx = db.begin(&TxnOptions::new());
            let x = decode(tx.get_for_update(first)?);
            let y = decode(tx.get_for_update(second)?);
            tx.put(first, &(x + 1).to_le_bytes())?;
            tx.put(second, &(y + 1).to_le_bytes())?;
            tx.commit()
        });
    };

    std::thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..ROUNDS {
                bump(&db, b"a", b"b");
            }
        });
        scope.spawn(|| {
            for _ in 0..ROUNDS {
                bump(&db, b"b", b"a");
            }
        });
    });
    assert_eq!(decode(db.db().get(b"a").unwrap()), 2 * ROUNDS);
    assert_eq!(decode(db.db().get(b"b").unwrap()), 2 * ROUNDS);
}

#[test]
fn buffered_writes_are_invisible_until_commit() {
    let dir = TempDir::new().unwrap();
    let db = pes_db(&dir);
    let tx = db.begin(&TxnOptions::new());
    tx.put(b"k", b"staged").unwrap();
    assert_eq!(db.db().get(b"k").unwrap(), None);
    let snap = db.db().snapshot();
    assert_eq!(snap.get(b"k").unwrap(), None);
    tx.commit().unwrap();
    assert_eq!(db.db().get(b"k").unwrap(), Some(b"staged".to_vec()));
    assert_eq!(snap.get(b"k").unwrap(), None);
}

#[test]
fn a_non_transactional_writer_racing_transactions_never_hides_a_lost_update() {
    const ROUNDS: u64 = 200;
    let dir = TempDir::new().unwrap();
    let db: Db = Db::open(dir.path(), Options::default()).unwrap();
    drop(db);
    let db = Arc::new(pes_db(&dir));
    regolith::through_stalls(|| db.db().put(b"k", &0u64.to_le_bytes())).unwrap();

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let noise = {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                regolith::through_stalls(|| db.db().put(b"unrelated", b"v")).unwrap();
            }
        })
    };

    for _ in 0..ROUNDS {
        commit_with_retry("increment beside noise", || {
            let tx = db.begin(&TxnOptions::new());
            let current = decode(tx.get_for_update(b"k")?);
            tx.put(b"k", &(current + 1).to_le_bytes())?;
            tx.commit()
        });
    }
    stop.store(true, Ordering::Relaxed);
    noise.join().unwrap();
    assert_eq!(decode(db.db().get(b"k").unwrap()), ROUNDS);
}

// ---- range-tombstone coverage of a blind write ----

/// A range delete is a write too: a blind write into a range deleted
/// after the transaction began has no serial equivalent, and one deleted
/// before begin is simply the state the transaction started from.
#[test]
fn a_blind_write_conflicts_with_a_range_delete_that_landed_after_begin() {
    for level in [
        IsolationLevel::ReadCommitted,
        IsolationLevel::SnapshotIsolation,
        IsolationLevel::Serializable,
    ] {
        let dir = TempDir::new().unwrap();
        let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
        let tx = db.begin(&TxnOptions::new().isolation(level));
        regolith::through_stalls(|| db.db().delete_range(b"a", b"z")).unwrap();
        tx.put(b"k", b"v").unwrap();
        assert!(
            matches!(
                tx.commit(),
                Err(TransactionError::Conflict(ref conflict)) if conflict.key() == b"k"
            ),
            "{level:?}: a range delete landing after begin must conflict with a blind write"
        );

        let dir = TempDir::new().unwrap();
        let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
        regolith::through_stalls(|| db.db().delete_range(b"a", b"z")).unwrap();
        let tx = db.begin(&TxnOptions::new().isolation(level));
        tx.put(b"k", b"v").unwrap();
        tx.commit()
            .unwrap_or_else(|error| panic!("{level:?}: {error:?}"));
        assert_eq!(db.db().get(b"k").unwrap(), Some(b"v".to_vec()));
    }
}
