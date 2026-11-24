//! Whose reads the decision of a slot may make: a thread that may read the
//! device decides a slot with it, a `CacheOnly` commit's included, and a
//! thread that must not read the device decides nobody's slot with it, its
//! own included: its queue's poll does.
//!
//! Each test stops a committer twice with the stage hooks: between its
//! reservation and its install, while a flush raises the version past the
//! horizon its early check ran at, so that deciding its slot has to consult
//! the tables; then before it decides its own slot, so that another thread
//! decides it.

use std::sync::mpsc;
use std::time::Duration;

use tempfile::TempDir;

use super::super::hook::{self, Stage};
use crate::{OptimisticTransactionDb, Options, ReadMode, TxnOptions, WouldBlock};

/// A key in a table no block of which is in the cache.
const COLD: &[u8] = b"k01000";

/// A database reopened over one table of small blocks, so its cache holds
/// none of them.
fn cold(dir: &TempDir) -> OptimisticTransactionDb {
    let options = || Options::default().block_size(256);
    {
        let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
        for i in 0..2_000u32 {
            db.db().put(format!("k{i:05}").as_bytes(), b"x").unwrap();
        }
        db.db().flush().unwrap();
        db.db().close().unwrap();
    }
    OptimisticTransactionDb::open(dir.path(), options()).unwrap()
}

/// Run `call` on a scoped thread. Stop it the first time it is between a
/// reservation and its install and run `voided`, which commits and so voids
/// the reservation; let it go on, stop it again before it decides a slot,
/// and run `decided`; then let it finish and return what it returned.
fn stopped_twice<T: Send>(
    call: impl FnOnce() -> T + Send,
    voided: impl FnOnce(),
    decided: impl FnOnce(),
) -> T {
    let (reached, at_stage) = mpsc::channel();
    let (resume, resumed) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let stopped = scope.spawn(move || {
            let mut stops = [Stage::Reserve, Stage::Decide].into_iter().peekable();
            hook::set(move |now| {
                if stops.peek() == Some(&now) {
                    stops.next();
                    reached.send(()).unwrap();
                    resumed.recv().unwrap();
                }
            });
            let out = call();
            hook::clear();
            out
        });
        let reach = || {
            at_stage
                .recv_timeout(Duration::from_secs(30))
                .expect("the call never reached the stage");
        };
        reach();
        voided();
        resume.send(()).unwrap();
        reach();
        decided();
        resume.send(()).unwrap();
        stopped.join().unwrap()
    })
}

/// A commit after the committer's early check, and a flush of it: the
/// version's last sequence is then above the horizon the check ran at, and
/// deciding the committer's slot looks its keys up in the tables.
fn flush_past_the_horizon(db: &OptimisticTransactionDb) {
    db.db().put(b"other", b"x").unwrap();
    db.db().flush().unwrap();
}

/// The first rule. The slot of a commit made through a `CacheOnly` handle is
/// decided here by another thread, one that makes a blocking write and may
/// read the device: it decides the slot with it, and the commit lands. The
/// earlier contract decided the slot from the cache alone and refused the
/// commit with the wait, and the transaction it consumed was lost
/// (`NowaitMiss.tla`).
#[test]
fn a_cache_only_commit_is_never_validated_with_the_device_whoever_decides_it() {
    let dir = TempDir::new().unwrap();
    let db = cold(&dir);
    let mut queue = db.db().io_queue();
    let options = TxnOptions::new().read_mode(ReadMode::CacheOnly(queue.id()));
    let txn = db.begin(&options);
    txn.put(COLD, b"mine").unwrap();
    let before = db.db().latest_sequence();

    let ticket = stopped_twice(
        move || txn.commit_nowait(),
        || flush_past_the_horizon(&db),
        // A blocking write behind the commit decides the commit's slot.
        || db.db().put(b"another", b"x").unwrap(),
    );
    let receipt = queue
        .block_on(ticket)
        .expect("the blocking writer decided the commit's slot with the device");
    assert_eq!(
        db.db().latest_sequence(),
        before + 3,
        "the commit took no sequence"
    );
    assert!(
        receipt.seq() < before + 3,
        "the commit is ordered before the write that decided its slot"
    );
    assert_eq!(db.db().get(COLD).unwrap(), Some(b"mine".to_vec()));
}

/// The committer finds the block its slot's decision needs missing, and no
/// other thread decides the slot: the call leaves the decision on its queue
/// and returns, having read no block, and the queue's poll decides the slot
/// with the device. The earlier contract refused the commit with the wait.
#[test]
fn a_cache_only_commit_whose_decision_misses_is_decided_at_its_queues_poll() {
    let dir = TempDir::new().unwrap();
    let db = cold(&dir);
    let mut queue = db.db().io_queue();
    let probe = db.db().io_queue();
    let txn = db.begin(&TxnOptions::new().read_mode(ReadMode::CacheOnly(queue.id())));
    txn.put(COLD, b"mine").unwrap();
    let before = db.db().latest_sequence();

    let ticket = stopped_twice(
        move || txn.commit_nowait(),
        || flush_past_the_horizon(&db),
        || {},
    );
    assert!(!ticket.is_ready(), "the miss ended the commit");
    assert_eq!(
        db.db().latest_sequence(),
        before + 1,
        "the slot was decided without its block"
    );
    let cold_still = db
        .db()
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(probe.id()))
        .get(COLD);
    assert!(
        matches!(cold_still, Err(crate::Error::WouldBlock(WouldBlock::Io(_)))),
        "the committing call read the device"
    );

    queue
        .block_on(ticket)
        .expect("the queue's poll decides the slot with the device");
    assert_eq!(db.db().get(COLD).unwrap(), Some(b"mine".to_vec()));
}

/// The second rule. A `commit_nowait` call made through a `CacheOnly`
/// handle helps a blocking commit's slot before its own, and that slot's
/// validation needs a block the cache does not hold. The call neither reads
/// the device nor refuses the other commit: it leaves the slot's decision on
/// its queue and returns, and its queue's poll decides both.
#[test]
fn a_cache_only_helper_decides_nobodys_slot_with_the_device() {
    let dir = TempDir::new().unwrap();
    let db = cold(&dir);
    let mut queue = db.db().io_queue();
    let blocking = db.begin(&TxnOptions::new());
    blocking.put(COLD, b"theirs").unwrap();

    let mut helped = None;
    let committed = stopped_twice(
        move || blocking.commit(),
        || flush_past_the_horizon(&db),
        || {
            let txn = db.begin(&TxnOptions::new().read_mode(ReadMode::CacheOnly(queue.id())));
            txn.put(b"helper", b"mine").unwrap();
            let ticket = txn.commit_nowait();
            assert!(
                !ticket.is_ready(),
                "the helper decided the slot before its own, which needed the device"
            );
            assert_eq!(db.db().get(b"helper").unwrap(), None);
            assert_eq!(
                db.db().get(COLD).unwrap(),
                Some(b"x".to_vec()),
                "the blocking commit is not decided yet"
            );
            // The queue's owner does its I/O when it polls: the slot before
            // the helper's is decided there, with the device, and then the
            // helper's own.
            let receipt = queue.block_on(ticket).expect("the helper's commit lands");
            helped = Some(receipt.seq());
        },
    );
    let committed = committed.expect("the blocking commit was not refused for the helper's miss");
    assert!(
        committed.seq() < helped.unwrap(),
        "the blocking commit is ordered before the helper's"
    );
    assert_eq!(db.db().get(COLD).unwrap(), Some(b"theirs".to_vec()));
    assert_eq!(db.db().get(b"helper").unwrap(), Some(b"mine".to_vec()));
}
