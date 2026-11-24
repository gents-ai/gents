//! A commit never loses its transaction to a write stall.
//!
//! A plain write a stop holds returns the stall's wait: its caller still has
//! the data and writes again. A transaction's commit cannot be made again,
//! so it is never handed that wait:
//!
//! - a blocking `commit` waits the stop out, on its own thread, and where
//!   nothing else relieves the stop (no worker, inline compaction on) does
//!   the relieving itself;
//! - a `commit_nowait` commit stays pending and its ticket completes once
//!   the stop is relieved;
//! - `transact` and `transact_async` commit the attempt they ran: the
//!   closure is not run a second time for a stall.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

use regolith::{
    CommitReceipt, CommitTicket, Db, Error, IoBudget, IoQueue, OptimisticTransactionDb, Options,
    RetryPolicy, TransactionDb, TransactionError, TxResult, TxnOptions, WouldBlock,
};
use tempfile::TempDir;

/// Writes stop once L0 holds two tables, and nothing compacts on its own.
fn stop_options() -> Options {
    Options::default()
        .write_buffer_size(4 * 1024)
        .l0_compaction_trigger(1000)
        .level0_slowdown_writes_trigger(0)
        .level0_stop_writes_trigger(2)
        .max_write_buffer_number(0)
}

/// The same stop on a database with no worker, where a stopped writer
/// relieves the stop itself and one compaction step clears it.
fn self_relieving_options() -> Options {
    stop_options()
        .max_background_compactions(0)
        .inline_compaction(true)
        .l0_compaction_trigger(2)
}

fn l0(db: &Db) -> u64 {
    db.get_int_property("regolith.num-files-at-level0").unwrap()
}

/// Bring L0 to the stop with explicit flushes.
fn stop(db: &Db) {
    for round in 0..2 {
        regolith::through_stalls(|| db.put(format!("fill{round}").as_bytes(), b"v")).unwrap();
        db.flush().unwrap();
    }
    assert_eq!(l0(db), 2);
    assert!(
        matches!(
            db.put(b"probe", b"v"),
            Err(Error::WouldBlock(WouldBlock::Stall(_)))
        ),
        "setup: a plain write is stopped"
    );
}

fn key(i: usize) -> Vec<u8> {
    format!("txn{i}").into_bytes()
}

/// The outcome of `ticket`, which a few polls of its queue must deliver:
/// what it waited for is over when this is called. A ticket that stays
/// pending fails the test here instead of hanging it.
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

/// A `commit_nowait` commit that meets a stop is not refused: its ticket
/// stays pending, nothing of it is stored and no callback runs, and once the
/// stop is relieved the ticket completes with the commit, every write in.
#[test]
fn a_nowait_commit_under_a_stop_completes_once_the_stop_is_relieved() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), stop_options()).unwrap();
    stop(db.db());
    let mut queue = db.db().io_queue();
    let mut txn = db.begin(&TxnOptions::new().io_queue(queue.id()));
    for i in 0..5 {
        txn.put(&key(i), b"v").unwrap();
    }
    let committed = Arc::new(AtomicUsize::new(0));
    let aborted = Arc::new(AtomicUsize::new(0));
    let (on_commit, on_abort) = (Arc::clone(&committed), Arc::clone(&aborted));
    txn.on_commit(move |_| {
        on_commit.fetch_add(1, Ordering::SeqCst);
    });
    txn.on_abort(move |_| {
        on_abort.fetch_add(1, Ordering::SeqCst);
    });

    let ticket = txn.commit_nowait();
    assert!(
        !ticket.is_ready(),
        "a commit under a stop waits for it; it is not ended with the stall's wait"
    );
    queue.poll(IoBudget::ALL);
    assert!(!ticket.is_ready(), "the stop still holds");
    assert_eq!(db.db().get(&key(0)).unwrap(), None);
    assert_eq!(aborted.load(Ordering::SeqCst), 0, "the transaction lives");

    db.db().compact_range(None, None).wait().unwrap();
    delivered(&mut queue, ticket).expect("the commit goes through once the stop is relieved");
    for i in 0..5 {
        assert_eq!(db.db().get(&key(i)).unwrap(), Some(b"v".to_vec()));
    }
    assert_eq!(committed.load(Ordering::SeqCst), 1);
    assert_eq!(aborted.load(Ordering::SeqCst), 0);
}

/// The same for a pessimistic transaction, whose key locks are held until
/// the commit lands.
#[test]
fn a_pessimistic_nowait_commit_under_a_stop_completes_once_the_stop_is_relieved() {
    let dir = TempDir::new().unwrap();
    let db = TransactionDb::open(dir.path(), stop_options()).unwrap();
    stop(db.db());
    let mut queue = db.db().io_queue();
    let txn = db.begin(&TxnOptions::new().io_queue(queue.id()));
    txn.put(&key(0), b"v").unwrap();
    let ticket = txn.commit_nowait();
    assert!(!ticket.is_ready());
    db.db().compact_range(None, None).wait().unwrap();
    delivered(&mut queue, ticket).expect("the commit goes through once the stop is relieved");
    assert_eq!(db.db().get(&key(0)).unwrap(), Some(b"v".to_vec()));
    // The lock went with the landing: another transaction takes the key.
    let next = db.begin(&TxnOptions::new());
    next.put(&key(0), b"w").unwrap();
    next.commit().unwrap();
}

/// A blocking commit waits the stop out on its own thread: it returns only
/// once another thread's compaction relieved the stop, with the commit.
#[test]
fn a_blocking_commit_waits_a_stop_out() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), stop_options()).unwrap();
    stop(db.db());
    let (entered, has_entered) = mpsc::channel();
    std::thread::scope(|scope| {
        let committer = scope.spawn(|| {
            let mut txn = db.begin(&TxnOptions::new());
            for i in 0..5 {
                txn.put(&key(i), b"v").unwrap();
            }
            // The last thing the transaction does before its commit is
            // admitted.
            txn.before_commit(move |_| {
                entered.send(()).unwrap();
                Ok(())
            });
            txn.commit()
        });
        has_entered.recv().unwrap();
        db.db().compact_range(None, None).wait().unwrap();
        committer
            .join()
            .unwrap()
            .expect("a blocking commit waits a stop out and commits");
    });
    for i in 0..5 {
        assert_eq!(db.db().get(&key(i)).unwrap(), Some(b"v".to_vec()));
    }
}

/// Where stopped writers relieve the stop themselves, a blocking commit
/// does so too, whether or not its thread has a queue: it chose to wait, and
/// nobody else would run the step it would leave on that queue.
#[test]
fn a_blocking_commit_relieves_the_stop_itself_on_a_thread_with_a_queue() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), self_relieving_options()).unwrap();
    let _queue = db.db().io_queue();
    stop(db.db());
    let txn = db.begin(&TxnOptions::new());
    txn.put(&key(0), b"v").unwrap();
    txn.commit()
        .expect("a blocking commit relieves the stop it meets and commits");
    assert_eq!(db.db().get(&key(0)).unwrap(), Some(b"v".to_vec()));
    assert!(
        l0(db.db()) < 2,
        "the commit ran the step that clears the stop"
    );
}

/// `transact` commits the attempt it ran: a stop makes the commit wait, and
/// the closure is not run again for it.
#[test]
fn transact_runs_its_closure_once_through_a_stop() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), stop_options()).unwrap();
    stop(db.db());
    let runs = AtomicUsize::new(0);
    let (ran, has_run) = mpsc::channel();
    std::thread::scope(|scope| {
        let attempt = scope.spawn(|| {
            db.transact(&RetryPolicy::new(3), |txn, _| {
                runs.fetch_add(1, Ordering::SeqCst);
                txn.put(&key(0), b"v")?;
                ran.send(()).unwrap();
                Ok::<_, TransactionError>(())
            })
        });
        has_run.recv().unwrap();
        db.db().compact_range(None, None).wait().unwrap();
        attempt.join().unwrap().expect("the attempt commits");
    });
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "a stall is waited out by the commit, not by a second run of the closure"
    );
    assert_eq!(db.db().get(&key(0)).unwrap(), Some(b"v".to_vec()));
}

/// `transact_async` the same: its commit stays pending through the stop,
/// the queue's polls run the step that relieves it, and the closure ran
/// once.
#[test]
fn transact_async_runs_its_closure_once_through_a_stop() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), self_relieving_options()).unwrap();
    let mut queue = db.db().io_queue();
    stop(db.db());
    let runs = AtomicUsize::new(0);
    queue
        .block_on(
            db.transact_async(queue.id(), &RetryPolicy::new(3), async |txn, _| {
                runs.fetch_add(1, Ordering::SeqCst);
                txn.put(&key(0), b"v")?;
                Ok::<_, TransactionError>(())
            }),
        )
        .expect("the attempt commits");
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "a stall is waited out by the commit, not by a second run of the closure"
    );
    assert_eq!(db.db().get(&key(0)).unwrap(), Some(b"v".to_vec()));
}

/// Closing the database ends a commit that waits for a stop: it is told the
/// database closed, as a write that arrives then is.
#[test]
fn close_ends_a_nowait_commit_that_waits_for_a_stop() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), stop_options()).unwrap();
    stop(db.db());
    let mut queue = db.db().io_queue();
    let txn = db.begin(&TxnOptions::new().io_queue(queue.id()));
    txn.put(&key(0), b"v").unwrap();
    let ticket = txn.commit_nowait();
    assert!(!ticket.is_ready());
    db.db().close().unwrap();
    match delivered(&mut queue, ticket) {
        Err(TransactionError::Engine(Error::Closed)) => {}
        other => panic!("a commit waiting for a stop on a closed database, got {other:?}"),
    }
}

/// A queue dropped while a commit waits behind a stop on it cannot be polled
/// again, so the drop brings the commit through as a blocking call would:
/// it waits the stop out first (here by relieving it, there being no
/// worker), then commits, and the ticket is complete when the drop returns.
#[test]
fn dropping_the_queue_brings_a_commit_that_waits_for_a_stop_through() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), self_relieving_options()).unwrap();
    let queue = db.db().io_queue();
    stop(db.db());
    let txn = db.begin(&TxnOptions::new().io_queue(queue.id()));
    txn.put(&key(0), b"v").unwrap();
    let ticket = txn.commit_nowait();
    assert!(!ticket.is_ready(), "the commit waits for the stop");
    drop(queue);
    assert!(ticket.is_ready(), "the drop brought the commit through");
    let mut ticket = std::pin::pin!(ticket);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    match std::future::Future::poll(ticket.as_mut(), &mut cx) {
        std::task::Poll::Ready(Ok(_)) => {}
        other => panic!("the commit did not go through: {other:?}"),
    }
    assert_eq!(db.db().get(&key(0)).unwrap(), Some(b"v".to_vec()));
    assert!(
        l0(db.db()) < 2,
        "the stop was relieved before the commit took its place"
    );
}

/// A commit that writes nothing takes no capacity: a stop does not hold it.
#[test]
fn a_commit_that_writes_nothing_does_not_wait_for_a_stop() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), stop_options()).unwrap();
    stop(db.db());
    let txn = db.begin(&TxnOptions::new());
    assert_eq!(txn.get_for_update(b"fill0").unwrap(), Some(b"v".to_vec()));
    txn.commit().expect("a read-only commit is not stopped");

    let mut queue = db.db().io_queue();
    let txn = db.begin(&TxnOptions::new().io_queue(queue.id()));
    assert_eq!(txn.get_for_update(b"fill0").unwrap(), Some(b"v".to_vec()));
    let ticket = txn.commit_nowait();
    assert!(
        ticket.is_ready(),
        "a read-only commit_nowait is not left behind the stop"
    );
    queue
        .block_on(ticket)
        .expect("a read-only commit is not stopped");
}
