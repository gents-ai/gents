//! A commit that waits a write stall out is told what a write arriving then
//! is told when the stop will not clear (E35 and E36, for a commit).
//!
//! A transaction's commit is not handed the stall's wait: a blocking one
//! sleeps on the stall's unit, a `commit_nowait` one is left behind it on
//! its queue. Whatever says the stop will not clear lands that unit: an
//! engine that latches (`stall_failure_tests.rs`, `stall_latch_tests.rs`), a
//! relief that fails. The commit, woken or driven again, is classified as a
//! write that arrives then is, and ends with that error: it does not wait
//! until the database closes.
//!
//! None of these sleeps: a blocked thread is watched through a channel with
//! a deadline, and a relief is held in flight by the code it calls.

use std::sync::{Arc, mpsc};
use std::time::Instant;

use super::super::hook;
use super::failure_tests::{DEADLINE, fail_the_held_relief, fill, held_relief};
use super::tests::stopped_engine_with;
use crate::engine::EngineOptions;
use crate::engine::background_health::Job;
use crate::engine::wal_rotation_tests::SyncFault;
use crate::{
    CommitReceipt, CommitTicket, Error, IoBudget, IoQueue, OptimisticTransactionDb, Options,
    Statistics, Ticker, TransactionDb, TransactionError, TxResult, TxnOptions, WouldBlock,
};

/// The outcome of `ticket`, which a few polls of its queue must deliver:
/// nothing these tests wait for is still under way on another thread. A
/// ticket that stays pending fails the test here instead of hanging it.
fn delivered(queue: &mut IoQueue, ticket: CommitTicket) -> TxResult<CommitReceipt> {
    for _ in 0..64 {
        if ticket.is_ready() {
            break;
        }
        queue.poll(IoBudget::ALL);
    }
    assert!(ticket.is_ready(), "the commit's ticket was never completed");
    queue.block_on(ticket)
}

/// A worker, one L0 table at a stop of one, a compaction trigger the worker
/// never reaches, and a write in the memtable for a flush to seal: stopped
/// writers wait for a relief that does not come.
fn stopped_with_a_worker() -> (tempfile::TempDir, OptimisticTransactionDb) {
    let dir = tempfile::TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(
        dir.path(),
        Options::default()
            .level0_slowdown_writes_trigger(1)
            .level0_stop_writes_trigger(1)
            .l0_compaction_trigger(100),
    )
    .unwrap();
    db.db().put(b"k", b"v").unwrap();
    db.db().flush().unwrap();
    // Admitted before the stop is looked at again: below the admission, as
    // a write that was let in is.
    fill(db.db().engine(), b"unflushed").unwrap();
    assert!(matches!(
        db.db().put(b"probe", b"v"),
        Err(Error::WouldBlock(WouldBlock::Stall(_)))
    ));
    (dir, db)
}

/// Latch the engine as a flush whose log cannot be synced does.
fn latch(dir: &tempfile::TempDir, db: &OptimisticTransactionDb) -> SyncFault {
    let fault = SyncFault::arm(dir);
    db.db()
        .flush()
        .expect_err("the flush seals a log it cannot sync");
    fault
}

/// Wait, with a deadline, until a writer the stop holds has installed the
/// stall's unit.
fn until_stopped(db: &OptimisticTransactionDb) {
    let deadline = Instant::now() + DEADLINE;
    while !db.db().engine().stall_signal().has_unit() {
        assert!(Instant::now() < deadline, "the commit never met the stop");
        std::thread::yield_now();
    }
}

/// Run a commit of one write on another thread while `fail` makes the stop
/// one that will not clear, and return what the commit was told.
fn told_while_it_waited(db: &OptimisticTransactionDb, fail: impl FnOnce()) -> String {
    // The plain write that probed the stop left a unit behind; nothing
    // waits on it, and the commit below must be seen installing its own.
    db.db().engine().stall_signal().land();
    let (told, when_told) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let tx = db.begin(&TxnOptions::new());
            tx.put(b"stopped", b"v").unwrap();
            told.send(tx.commit().map(|_| ()).map_err(|err| err.to_string()))
                .unwrap();
        });
        until_stopped(db);
        fail();
        let outcome = when_told.recv_timeout(DEADLINE);
        if outcome.is_err() {
            // Closing completes every wait, so the thread ends and this
            // test fails instead of hanging with it.
            let _ = db.db().close();
        }
        outcome
            .expect("the commit was still waiting after the stop became one that will not clear")
            .expect_err("the engine takes no write")
    })
}

/// E35, the blocking form: a commit waits the stall out, the engine
/// latches, and the commit returns the latch a fresh write gets.
#[test]
fn a_commit_waiting_out_a_stall_returns_the_latch() {
    let (dir, db) = stopped_with_a_worker();
    let mut fault = None;
    let told = told_while_it_waited(&db, || fault = Some(latch(&dir, &db)));
    assert!(told.contains("unknown state"), "{told}");
    assert_eq!(
        told,
        db.db().put(b"fresh", b"v").unwrap_err().to_string(),
        "the commit is told what a fresh write is told"
    );
    drop(fault);
}

/// E35 for `commit_nowait`: the commit is left behind the stall on its
/// queue, the engine latches, and the queue's poll completes the ticket with
/// the latch.
#[test]
fn a_nowait_commit_waiting_for_a_stall_is_told_the_latch_at_its_poll() {
    let (dir, db) = stopped_with_a_worker();
    let mut queue = db.db().io_queue();
    let tx = db.begin(&TxnOptions::new().io_queue(queue.id()));
    tx.put(b"stopped", b"v").unwrap();
    let ticket = tx.commit_nowait();
    queue.poll(IoBudget::ALL);
    assert!(!ticket.is_ready(), "the commit waits for the stop");
    let _fault = latch(&dir, &db);
    assert!(
        !ticket.is_ready(),
        "told only at the poll of the commit's own queue"
    );
    match delivered(&mut queue, ticket) {
        Err(TransactionError::Engine(told)) => {
            let told = told.to_string();
            assert!(told.contains("unknown state"), "{told}");
            assert_eq!(told, db.db().put(b"fresh", b"v").unwrap_err().to_string());
        }
        other => panic!("expected the latch, got {other:?}"),
    }
    assert_eq!(db.db().get(b"stopped").unwrap(), None);
}

/// A worker, two L0 tables at a stop of two, and the compaction that would
/// relieve it held in flight: the stop is on, and its relief has not failed
/// yet. Returns the channel that lets the compaction go, to fail.
fn stopped_with_the_relief_held() -> (tempfile::TempDir, OptimisticTransactionDb, mpsc::Sender<()>)
{
    let (held, when_entered, release) = held_relief();
    let dir = tempfile::TempDir::new().unwrap();
    // Two L0 tables, written with no worker and nothing that compacts them.
    {
        let db = crate::Db::open(
            dir.path(),
            Options::default()
                .max_background_compactions(0)
                .l0_compaction_trigger(100),
        )
        .unwrap();
        for key in [b"k1", b"k2"] {
            db.put(key, b"v").unwrap();
            db.flush().unwrap();
        }
        db.close().unwrap();
    }
    // Reopened with a worker whose only work is their compaction.
    held.hold_and_fail();
    let db = OptimisticTransactionDb::open(
        dir.path(),
        Options::default()
            .level0_slowdown_writes_trigger(2)
            .level0_stop_writes_trigger(2)
            .l0_compaction_trigger(2)
            .rate_limiter(Some(held)),
    )
    .unwrap();
    when_entered
        .recv_timeout(DEADLINE)
        .expect("the worker never started the compaction");
    (dir, db, release)
}

/// E36, the blocking form: a commit waits the stall out, the compaction
/// that would relieve it fails, and the commit returns that failure, the one
/// a fresh write gets, instead of waiting for a stop that will not clear.
#[test]
fn a_commit_waiting_out_a_stall_returns_the_failing_relief() {
    let (_dir, db, release) = stopped_with_the_relief_held();
    let told = told_while_it_waited(&db, || {
        fail_the_held_relief(db.db().engine(), &release, Job::Compaction);
    });
    assert!(told.contains("compaction"), "{told}");
    assert_eq!(
        told,
        db.db().put(b"fresh", b"v").unwrap_err().to_string(),
        "the commit is told what a fresh write is told"
    );
}

/// E36 for `commit_nowait`: the commit is left behind the stall on its
/// queue, the relief fails, and the queue's poll completes the ticket with
/// that failure.
#[test]
fn a_nowait_commit_waiting_for_a_stall_is_told_the_failing_relief_at_its_poll() {
    let (_dir, db, release) = stopped_with_the_relief_held();
    let mut queue = db.db().io_queue();
    let tx = db.begin(&TxnOptions::new().io_queue(queue.id()));
    tx.put(b"stopped", b"v").unwrap();
    let ticket = tx.commit_nowait();
    queue.poll(IoBudget::ALL);
    assert!(!ticket.is_ready(), "the commit waits for the stop");
    fail_the_held_relief(db.db().engine(), &release, Job::Compaction);
    match delivered(&mut queue, ticket) {
        Err(TransactionError::Engine(told)) => {
            let told = told.to_string();
            assert!(told.contains("compaction"), "{told}");
            assert_eq!(told, db.db().put(b"fresh", b"v").unwrap_err().to_string());
        }
        other => panic!("expected the relief's failure, got {other:?}"),
    }
    assert_eq!(db.db().get(b"stopped").unwrap(), None);
}

/// Where stopped writers relieve the stop themselves and no step can (level
/// 0 is below its compaction trigger), a commit that waits does not run
/// steps for ever: it is told the stop is one nothing relieves, as a plain
/// write there is.
#[test]
fn a_commit_no_step_can_relieve_is_told_busy() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(
        dir.path(),
        Options::default()
            .max_background_compactions(0)
            .inline_compaction(true)
            .level0_slowdown_writes_trigger(1)
            .level0_stop_writes_trigger(1)
            .l0_compaction_trigger(100),
    )
    .unwrap();
    db.db().put(b"k", b"v").unwrap();
    db.db().flush().unwrap();
    let plain = db.db().put(b"plain", b"v").unwrap_err();
    assert!(matches!(plain, Error::Busy(_)), "{plain:?}");
    let tx = db.begin(&TxnOptions::new());
    tx.put(b"stopped", b"v").unwrap();
    match tx.commit() {
        Err(TransactionError::Engine(Error::Busy(reason))) => {
            assert_eq!(Error::Busy(reason).to_string(), plain.to_string());
        }
        other => panic!("expected Busy, got {other:?}"),
    }
}

/// The same on a queue, for a pessimistic transaction: the queue's polls
/// run the step that would relieve the stop, it finds nothing to do, and
/// the commit is told so instead of staying on the queue. Its key locks go
/// with the refusal: another transaction takes the key. That transaction
/// holds a lock of its own from before the refusal, so its name in the lock
/// table is not one the refused transaction left behind.
#[test]
fn a_nowait_commit_no_step_can_relieve_is_told_busy_and_gives_its_locks_back() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = TransactionDb::open(
        dir.path(),
        Options::default()
            .max_background_compactions(0)
            .inline_compaction(true)
            .level0_slowdown_writes_trigger(1)
            .level0_stop_writes_trigger(1)
            .l0_compaction_trigger(100),
    )
    .unwrap();
    db.db().put(b"k", b"v").unwrap();
    db.db().flush().unwrap();
    let mut queue = db.db().io_queue();
    let next = db.begin(&TxnOptions::new());
    assert_eq!(next.get_for_update(b"its own").unwrap(), None);
    let tx = db.begin(&TxnOptions::new().io_queue(queue.id()));
    tx.put(b"locked", b"v").unwrap();
    assert!(
        next.try_get_for_update(b"locked").is_err(),
        "setup: the transaction holds the key's lock"
    );
    let ticket = tx.commit_nowait();
    assert!(!ticket.is_ready(), "the commit waits for the stop");
    match delivered(&mut queue, ticket) {
        Err(TransactionError::Engine(Error::Busy(_))) => {}
        other => panic!("expected Busy, got {other:?}"),
    }
    assert_eq!(
        next.try_get_for_update(b"locked")
            .expect("the refused commit gave its key lock back"),
        None
    );
}

/// Where stopped writers relieve the stop themselves and the relief is
/// failing, a commit is told that failure as a plain write is, and does not
/// run the relief again to find out: the stop still holds afterwards.
#[test]
fn a_commit_is_told_a_failing_relief_where_writers_relieve() {
    let (_dir, engine) = stopped_engine_with(EngineOptions {
        inline_compaction: true,
        l0_compaction_trigger: 1,
        ..EngineOptions::default()
    });
    engine
        .background_health
        .record_failure(Job::Compaction, &std::io::Error::other("the relief fails"));
    let plain = engine
        .admit_write(false)
        .expect_err("the relief is failing")
        .to_string();
    assert!(plain.contains("compaction"), "{plain}");
    let told = engine
        .admit_commit()
        .expect_err("the relief is failing")
        .to_string();
    assert_eq!(told, plain, "the commit is told what a plain write is told");
    assert!(
        matches!(engine.stall_signal().classify(), Some((_, true))),
        "the commit did not run the relief"
    );
}

/// The window the second look closes. The commit finds writes stopped; the
/// stop clears before the commit has taken the stall's unit, so the clearer
/// finds no unit to land; the commit then installs one. It looks at the
/// stall again and lands the unit itself: left behind a unit nobody would
/// land, its ticket would never complete.
#[test]
fn a_nowait_commit_whose_stop_cleared_before_it_took_the_unit_goes_on() {
    let (_dir, db) = stopped_with_a_worker();
    let db = Arc::new(db);
    let mut queue = db.db().io_queue();
    let tx = db.begin(&TxnOptions::new().io_queue(queue.id()));
    tx.put(b"stopped", b"v").unwrap();
    let clearer = Arc::clone(&db);
    let mut met = 0;
    hook::set(move |stage| {
        if stage != hook::Stage::Stall {
            return;
        }
        met += 1;
        // The commit is classified twice before it is left on its queue:
        // by the call itself, then where it is left behind the unit. The
        // second time is the last look before the unit.
        if met == 2 {
            clearer.db().compact_range(None, None).wait().unwrap();
        }
    });
    let ticket = tx.commit_nowait();
    hook::clear();
    delivered(&mut queue, ticket).expect("the commit goes on once the stop cleared");
    assert_eq!(db.db().get(b"stopped").unwrap(), Some(b"v".to_vec()));
}

/// The same window for a stop that will not clear: the engine latches after
/// the commit was classified and before it took the unit, so the latch found
/// no unit of the commit's to land. The commit reads the latch once it has
/// the unit and lands it, and is told the latch at its queue's next poll.
#[test]
fn a_nowait_commit_that_took_the_unit_after_the_engine_latched_is_told_the_latch() {
    let (dir, db) = stopped_with_a_worker();
    let (dir, db) = (Arc::new(dir), Arc::new(db));
    let mut queue = db.db().io_queue();
    let tx = db.begin(&TxnOptions::new().io_queue(queue.id()));
    tx.put(b"stopped", b"v").unwrap();
    let (at, latched) = (Arc::clone(&dir), Arc::clone(&db));
    let mut met = 0;
    hook::set(move |stage| {
        if stage != hook::Stage::Stall {
            return;
        }
        met += 1;
        if met == 2 {
            drop(latch(&at, &latched));
        }
    });
    let ticket = tx.commit_nowait();
    hook::clear();
    match delivered(&mut queue, ticket) {
        Err(TransactionError::Engine(told)) => {
            let told = told.to_string();
            assert!(told.contains("unknown state"), "{told}");
        }
        other => panic!("expected the latch, got {other:?}"),
    }
}

/// The time a commit sleeps for a stop is counted where the time a writer
/// spends relieving one is: the stall ticker says how long threads were
/// held, whoever did the relieving. The stop is cleared only once the
/// commit is seen asleep on the stall's unit, so its wait is under way.
#[test]
fn a_commit_that_sleeps_for_a_stop_counts_the_time_it_was_held() {
    let stats = Arc::new(Statistics::new());
    let dir = tempfile::TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(
        dir.path(),
        Options::default()
            .level0_slowdown_writes_trigger(1)
            .level0_stop_writes_trigger(1)
            .l0_compaction_trigger(100)
            .statistics(Some(Arc::clone(&stats))),
    )
    .unwrap();
    db.db().put(b"k", b"v").unwrap();
    db.db().flush().unwrap();
    assert_eq!(stats.get_ticker(Ticker::WriteStallMicros), 0);
    std::thread::scope(|scope| {
        let committer = scope.spawn(|| {
            let tx = db.begin(&TxnOptions::new());
            tx.put(b"stopped", b"v").unwrap();
            tx.commit()
        });
        let deadline = Instant::now() + DEADLINE;
        while !db.db().engine().stall_signal().has_sleeper() {
            assert!(
                Instant::now() < deadline,
                "the commit never slept for the stop"
            );
            std::thread::yield_now();
        }
        db.db().compact_range(None, None).wait().unwrap();
        committer
            .join()
            .unwrap()
            .expect("the commit goes through once the stop is relieved");
    });
    assert!(
        stats.get_ticker(Ticker::WriteStallMicros) > 0,
        "the time the commit slept for the stop was not counted"
    );
}
