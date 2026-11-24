//! Where the pessimistic lock table and the lock-free background state meet
//! each other and the rest of Phase 7, tested across them.
//!
//! - **One queue, every kind of unit** (lock table x compaction gate x
//!   commit side). A job parked on its queue behind the compaction gate, a
//!   wait for a key's lock and a commit's ticket sit on one queue. Each is
//!   told at that queue's poll and none holds another back: the lock's
//!   release completes its wait while the job stays parked, and the gate's
//!   release wakes the idle owner for the job.
//! - **Close, in order** (lock table x compaction gate x commit side x
//!   snapshot slots). Close lands the commit group that still owes its
//!   fsync, and with it the key lock that commit holds until it lands; it
//!   completes the wait for that lock, settles the parked job, ends the
//!   transaction left open, and leaves no snapshot pinned.
//! - **Snapshot slots and key locks** (snapshot slots x lock table). A
//!   pessimistic transaction begun on one thread and committed, rolled back
//!   or dropped on another, or dropped while it waits for a lock, releases
//!   its pin and its locks exactly once.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

use common::device_env::{Device, DeviceEnv};
use common::{STUCK, returns};
use regolith::{
    AbortReason, CompactionOutcome, Db, DurabilityMode, Error, IoBudget, IoQueue, LockWait,
    Options, TransactionDb, TransactionError, TxResult, TxnOptions, WouldBlock,
};
use tempfile::TempDir;

fn live_snapshots(db: &Db) -> u64 {
    db.get_int_property("regolith.num-snapshots")
        .expect("the property is always reported")
}

fn on(queue: &IoQueue) -> TxnOptions {
    TxnOptions::new().io_queue(queue.id())
}

/// The wait a refused `try_get_for_update` handed back.
fn locked<T: std::fmt::Debug>(result: TxResult<T>) -> LockWait {
    match result {
        Err(TransactionError::WouldBlock(WouldBlock::Locked(wait))) => wait,
        other => panic!("expected a lock wait, got {other:?}"),
    }
}

/// A pessimistic database with no worker and four cold L0 tables past its
/// compaction trigger, so a compaction pass has work and reads the device to
/// do it, and with `Immediate` durability, so a commit that does not wait
/// leaves its group owing an fsync. The tables hold `k00000` to `k00199`:
/// the keys the tests lock sort after them, so reading one touches no table.
fn cold_locked_db() -> (TempDir, Arc<TransactionDb>, Arc<Device>) {
    let dir = TempDir::new().unwrap();
    let (env, device) = DeviceEnv::new();
    {
        // Filled with compaction off, so the tables are still in L0.
        let db = Db::open(
            dir.path(),
            Options::default()
                .env(Arc::clone(&env) as Arc<dyn regolith::Env>)
                .max_background_compactions(0)
                .l0_compaction_trigger(1000),
        )
        .unwrap();
        for i in 0..200u32 {
            db.put(format!("k{i:05}").as_bytes(), &[7u8; 256]).unwrap();
            if i % 50 == 49 {
                db.flush().unwrap();
            }
        }
        db.close().unwrap();
    }
    let db = TransactionDb::open(
        dir.path(),
        Options::default()
            .env(env as Arc<dyn regolith::Env>)
            .max_background_compactions(0)
            .l0_compaction_trigger(2)
            .durability(DurabilityMode::Immediate),
    )
    .unwrap()
    .with_lock_timeout(STUCK);
    (dir, Arc::new(db), device)
}

/// A compaction pass on a thread of its own, held at the device with the
/// compaction gate shared: a job that needs the gate alone parks behind it.
fn pass_held_at_the_device(
    db: &Arc<TransactionDb>,
    device: &Device,
) -> std::thread::JoinHandle<regolith::Result<CompactionOutcome>> {
    device.hold();
    let pass = {
        let db = Arc::clone(db);
        std::thread::spawn(move || db.db().compact_step())
    };
    device.wait_held(1);
    pass
}

#[test]
fn one_queue_is_told_a_lock_wait_and_a_parked_job_each_at_its_own_poll() {
    let (_dir, db, device) = cold_locked_db();
    let pass = pass_held_at_the_device(&db, &device);

    let owner = {
        let (db, device) = (Arc::clone(&db), Arc::clone(&device));
        move || {
            let mut queue = db.db().io_queue();
            let job = db.db().compact_range(None, None);
            assert_eq!(queue.poll(IoBudget::ALL).completed, 0, "the job parked");

            let holder = db.begin(&TxnOptions::new());
            assert_eq!(holder.get_for_update(b"lock").unwrap(), None);
            let waiter = db.begin(&on(&queue));
            let wait = locked(waiter.try_get_for_update(b"lock"));
            queue.poll(IoBudget::ALL);
            assert!(!wait.is_ready(), "the lock is still held");
            assert!(!job.is_ready(), "the gate is still held");

            // The lock's release completes its wait at this queue's poll;
            // the job, parked on the same queue, stays parked.
            holder.put(b"lock", b"held").unwrap();
            holder.commit().unwrap();
            queue.poll(IoBudget::ALL);
            assert!(wait.is_ready(), "the release told the waiter's queue");
            assert!(!job.is_ready(), "a lock's release ran a job the gate holds");
            assert_eq!(
                waiter.try_get_for_update(b"lock").unwrap(),
                Some(b"held".to_vec())
            );

            // The gate's release wakes the owner, idle in `block_on` with
            // only the parked job on its queue, and the next poll runs it.
            device.release();
            queue.block_on(job).unwrap();
            waiter.put(b"lock", b"taken").unwrap();
            waiter.commit().unwrap();
        }
    };
    returns("the owner of the queue", owner);

    assert_eq!(pass.join().unwrap().unwrap(), CompactionOutcome::DidWork);
    assert_eq!(db.db().get(b"lock").unwrap(), Some(b"taken".to_vec()));
    assert_eq!(
        db.db().get_int_property("regolith.num-files-at-level0"),
        Some(0),
        "the job ran once the gate freed"
    );
    assert_eq!(live_snapshots(db.db()), 0);
}

#[test]
fn close_settles_every_unit_a_queue_holds_and_leaves_nothing_pinned_or_locked() {
    let (dir, db, device) = cold_locked_db();
    let pass = pass_held_at_the_device(&db, &device);

    let owner = {
        let (db, device) = (Arc::clone(&db), Arc::clone(&device));
        move || {
            let mut queue = db.db().io_queue();
            let job = db.db().compact_range(None, None);
            assert_eq!(queue.poll(IoBudget::ALL).completed, 0, "the job parked");
            // The pass ends, and the job stays parked: only its owner's
            // poll runs it, and the owner closes instead. (Close flushes
            // what the commit below writes, and reads its new table back
            // through the device, so the device cannot stay held.)
            device.release();
            assert_eq!(pass.join().unwrap().unwrap(), CompactionOutcome::DidWork);

            // A commit whose group still owes its fsync. It holds its key's
            // lock until it lands.
            let committing = db.begin(&on(&queue));
            assert_eq!(committing.get_for_update(b"lock").unwrap(), None);
            committing.put(b"lock", b"committed").unwrap();
            let commit = committing.commit_nowait();
            assert!(!commit.is_ready(), "setup: the group owes its fsync");
            // A transaction waiting for that lock, on this queue.
            let waiter = db.begin(&on(&queue));
            let wait = locked(waiter.try_get_for_update(b"lock"));
            // A transaction left open, holding another lock.
            let mut open = db.begin(&on(&queue));
            assert_eq!(open.get_for_update(b"other").unwrap(), None);
            open.put(b"other", b"never").unwrap();
            let aborted = Arc::new(AtomicUsize::new(0));
            let count = Arc::clone(&aborted);
            open.on_abort(move |reason| {
                assert!(matches!(reason, AbortReason::Closed));
                count.fetch_add(1, Ordering::SeqCst);
            });

            db.db().close().unwrap();

            assert_eq!(
                aborted.load(Ordering::SeqCst),
                1,
                "close ended the open transaction on the closing thread"
            );
            queue.poll(IoBudget::ALL);
            assert!(commit.is_ready(), "close told the commit's ticket");
            let receipt = queue.block_on(commit).expect("the final sync covered it");
            assert!(receipt.seq() > 0);
            assert!(wait.is_ready(), "close told the lock's waiter");
            assert!(matches!(
                waiter.try_get_for_update(b"lock"),
                Err(TransactionError::Engine(Error::Closed))
            ));
            assert!(job.is_ready(), "close told the parked job's ticket");
            assert!(matches!(job.wait(), Err(Error::Closed)));
            assert!(matches!(
                queue.block_on(open.commit_nowait()),
                Err(TransactionError::Engine(Error::Closed))
            ));
            assert_eq!(aborted.load(Ordering::SeqCst), 1, "exactly one outcome");
            // The committed transaction's pin went at its landing and the
            // open one's at close; the waiter's goes with it, once.
            drop(waiter);
            assert_eq!(live_snapshots(db.db()), 0, "a pin outlived close");
        }
    };
    returns("the owner of the queue", owner);

    drop(db);
    let db = Db::open(dir.path(), Options::default()).unwrap();
    assert_eq!(
        db.get(b"lock").unwrap(),
        Some(b"committed".to_vec()),
        "the commit close landed did not survive"
    );
    assert_eq!(db.get(b"other").unwrap(), None);
}

#[test]
fn pessimistic_transactions_release_their_pins_and_locks_exactly_once() {
    const THREADS: usize = 8;
    const EACH: usize = 24;
    let dir = TempDir::new().unwrap();
    let db = Arc::new(
        TransactionDb::open(dir.path(), Options::default().max_background_compactions(0))
            .unwrap()
            .with_lock_timeout(STUCK),
    );

    // Each transaction locks and writes a key of its own, then moves to
    // another thread to be ended there.
    let (to_end, begun) = mpsc::channel();
    let beginners: Vec<_> = (0..THREADS)
        .map(|t| {
            let (db, to_end) = (Arc::clone(&db), to_end.clone());
            std::thread::spawn(move || {
                for i in 0..EACH {
                    let key = format!("t{t}-{i:02}").into_bytes();
                    let txn = db.begin(&TxnOptions::new());
                    assert_eq!(txn.get_for_update(&key).unwrap(), None);
                    txn.put(&key, b"v").unwrap();
                    to_end.send((i % 3, key, txn)).unwrap();
                }
            })
        })
        .collect();
    drop(to_end);
    for beginner in beginners {
        beginner.join().unwrap();
    }
    let mut open: Vec<_> = begun.try_iter().collect();
    let all = (THREADS * EACH) as u64;
    assert_eq!(open.len() as u64, all);
    assert_eq!(live_snapshots(db.db()), all, "one pin per open transaction");

    // A transaction that waits for a lock holds a pin too, and gives it
    // back, once, when it is dropped still waiting.
    {
        let blocked = db.begin(&TxnOptions::new());
        let wait = locked(blocked.try_get_for_update(&open[0].1));
        assert_eq!(live_snapshots(db.db()), all + 1);
        drop(blocked);
        assert_eq!(live_snapshots(db.db()), all);
        drop(wait);
        assert_eq!(live_snapshots(db.db()), all);
    }

    let ends: Vec<(usize, Vec<u8>)> = open
        .iter()
        .map(|(how, key, _)| (*how, key.clone()))
        .collect();
    let enders: Vec<_> = (0..4)
        .map(|_| {
            let batch: Vec<_> = open.drain(..open.len().min(all as usize / 4)).collect();
            std::thread::spawn(move || {
                for (how, _, txn) in batch {
                    match how {
                        0 => drop(txn.commit().unwrap()),
                        1 => txn.rollback(),
                        _ => drop(txn),
                    }
                }
            })
        })
        .collect();
    for ender in enders {
        ender.join().unwrap();
    }
    assert!(open.is_empty());
    assert_eq!(
        live_snapshots(db.db()),
        0,
        "a pin was kept, or released twice"
    );

    // Every lock is free again: one transaction takes them all without a
    // wait, and reads what the committed third wrote.
    let sweep = db.begin(&TxnOptions::new());
    for (how, key) in &ends {
        assert_eq!(
            sweep.try_get_for_update(key).unwrap().is_some(),
            *how == 0,
            "the lock on {} was not released, or its write is wrong",
            String::from_utf8_lossy(key)
        );
    }
    assert_eq!(live_snapshots(db.db()), 1);
    sweep.rollback();
    assert_eq!(live_snapshots(db.db()), 0);
}
