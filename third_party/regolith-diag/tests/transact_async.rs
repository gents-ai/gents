//! `transact_async` (plan 3.10, D39): an attempt that misses the cache is
//! suspended on its wait, not ended; the commit goes through `commit_nowait`;
//! a conflict re-runs the closure with its reason, up to the policy's bound.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use regolith::{
    AbortReason, DurabilityMode, Error, OptimisticTransactionDb, Options, RetryPolicy,
    TransactError, TransactionDb, TransactionError, TxnOptions, ready,
};
use tempfile::TempDir;

const KEYS: u32 = 2_000;

fn key(i: u32) -> Vec<u8> {
    format!("k{i:05}").into_bytes()
}

/// A database whose keys all live in tables, reopened with a cold cache.
fn cold(dir: &TempDir) -> OptimisticTransactionDb {
    let options = || Options::default().block_size(256);
    {
        let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
        for i in 0..KEYS {
            db.db()
                .put(&key(i), format!("value-{i}").as_bytes())
                .unwrap();
        }
        db.db().flush().unwrap();
        db.db().close().unwrap();
    }
    OptimisticTransactionDb::open(dir.path(), options().durability(DurabilityMode::Immediate))
        .unwrap()
}

/// Reads that miss suspend the attempt on their waits; the closure runs once,
/// keeps what it did before each miss, and its commit lands through the
/// queue.
#[test]
fn misses_suspend_the_attempt_instead_of_ending_it() {
    let dir = TempDir::new().unwrap();
    let db = cold(&dir);
    let mut queue = db.db().io_queue();
    let runs = AtomicUsize::new(0);
    let reads = AtomicUsize::new(0);
    let (sum, receipt) = queue
        .block_on(db.transact_async(
            queue.id(),
            &RetryPolicy::default(),
            async |txn, previous| {
                assert!(previous.is_none());
                runs.fetch_add(1, Ordering::SeqCst);
                let mut sum = Vec::new();
                for i in [100u32, 900, 1_700] {
                    let value = ready(|| {
                        reads.fetch_add(1, Ordering::SeqCst);
                        txn.get(&key(i))
                    })
                    .await?;
                    sum.extend(value.expect("the key exists"));
                }
                txn.put(b"sum", &sum)?;
                Ok::<_, regolith::TransactionError>(sum)
            },
        ))
        .unwrap();
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "a miss never ends the attempt"
    );
    assert!(
        reads.load(Ordering::SeqCst) > 3,
        "at least one read missed and ran again"
    );
    assert_eq!(db.db().get(b"sum").unwrap(), Some(sum));
    assert!(receipt.seq() > 0);
}

/// A conflict re-runs the closure in a fresh transaction, handing it the
/// conflict; the re-run sees the winner and commits.
#[test]
fn a_conflict_reruns_the_closure_with_its_reason() {
    let dir = TempDir::new().unwrap();
    let db = cold(&dir);
    let mut queue = db.db().io_queue();
    let runs = AtomicUsize::new(0);
    let reasons = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (seen, _) = queue
        .block_on(
            db.transact_async(queue.id(), &RetryPolicy::new(3), async |txn, previous| {
                reasons
                    .lock()
                    .unwrap()
                    .push(previous.map(|c| c.key().to_vec()));
                let run = runs.fetch_add(1, Ordering::SeqCst);
                let value = ready(|| txn.get_for_update(&key(5))).await?;
                if run == 0 {
                    // A rival commits the same key between the read and the
                    // commit: this attempt loses.
                    db.db().put(&key(5), b"rival").unwrap();
                }
                txn.put(&key(5), b"mine")?;
                Ok::<_, regolith::TransactionError>(value)
            }),
        )
        .unwrap();
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    assert_eq!(seen, Some(b"rival".to_vec()), "the re-run sees the winner");
    assert_eq!(*reasons.lock().unwrap(), vec![None, Some(key(5))]);
    assert_eq!(db.db().get(&key(5)).unwrap(), Some(b"mine".to_vec()));
}

#[test]
fn the_policy_bounds_the_reruns() {
    let dir = TempDir::new().unwrap();
    let db = cold(&dir);
    let mut queue = db.db().io_queue();
    let runs = AtomicUsize::new(0);
    let outcome =
        queue.block_on(
            db.transact_async(queue.id(), &RetryPolicy::new(2), async |txn, _| {
                runs.fetch_add(1, Ordering::SeqCst);
                ready(|| txn.get_for_update(&key(7))).await?;
                db.db().put(&key(7), b"rival").unwrap();
                txn.put(&key(7), b"mine")?;
                Ok::<_, regolith::TransactionError>(())
            }),
        );
    assert!(matches!(outcome, Err(TransactError::Exhausted(_))));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

#[test]
fn an_error_from_the_closure_ends_the_attempt_unretried() {
    let dir = TempDir::new().unwrap();
    let db = cold(&dir);
    let mut queue = db.db().io_queue();
    let runs = AtomicUsize::new(0);
    let outcome =
        queue.block_on(
            db.transact_async(queue.id(), &RetryPolicy::default(), async |txn, _| {
                runs.fetch_add(1, Ordering::SeqCst);
                txn.put(b"k", b"v").map_err(|_| "put")?;
                Err::<(), _>("business rule")
            }),
        );
    assert!(matches!(
        outcome,
        Err(TransactError::Closure("business rule"))
    ));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(db.db().get(b"k").unwrap(), None);
}

/// `before_commit` callbacks whose reads miss are prepared by the attempt,
/// which waits out each miss instead of failing the commit.
#[test]
fn a_before_commit_read_that_misses_is_waited_out() {
    let dir = TempDir::new().unwrap();
    let db = cold(&dir);
    let mut queue = db.db().io_queue();
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    queue
        .block_on(
            db.transact_async(queue.id(), &RetryPolicy::default(), async move |txn, _| {
                let counted = Arc::clone(&counted);
                txn.before_commit(move |txn| {
                    counted.fetch_add(1, Ordering::SeqCst);
                    let value = txn.get(&key(1_234))?.expect("the key exists");
                    txn.put(b"copy", &value)
                });
                txn.put(b"k", b"v")?;
                Ok::<_, regolith::TransactionError>(())
            }),
        )
        .unwrap();
    assert!(
        calls.load(Ordering::SeqCst) >= 2,
        "the callback missed, waited, and ran again"
    );
    assert_eq!(db.db().get(b"copy").unwrap(), Some(b"value-1234".to_vec()));
}

#[test]
fn a_pessimistic_database_runs_the_same_contract() {
    let dir = TempDir::new().unwrap();
    drop(cold(&dir));
    let db = TransactionDb::open(
        dir.path(),
        Options::default()
            .block_size(256)
            .durability(DurabilityMode::Immediate),
    )
    .unwrap();
    let mut queue = db.db().io_queue();
    let (value, _) = queue
        .block_on(
            db.transact_async(queue.id(), &RetryPolicy::default(), async |txn, _| {
                let value = ready(|| txn.get_for_update(&key(42))).await?;
                txn.put(b"copy", value.as_deref().unwrap_or_default())?;
                Ok::<_, regolith::TransactionError>(value)
            }),
        )
        .unwrap();
    assert_eq!(value, Some(b"value-42".to_vec()));
    assert_eq!(db.db().get(b"copy").unwrap(), Some(b"value-42".to_vec()));
    let _ = TxnOptions::new();
}

/// The future does not borrow the policy it was made from: it is made in one
/// statement, from a policy that does not outlive that statement, and run in
/// another. On both databases.
#[test]
fn the_future_outlives_the_policy_it_was_made_from() {
    let dir = TempDir::new().unwrap();
    let db = cold(&dir);
    let mut queue = db.db().io_queue();
    let attempt = db.transact_async(queue.id(), &RetryPolicy::default(), async |txn, _| {
        txn.put(b"one-line", b"optimistic")?;
        Ok::<_, regolith::TransactionError>(())
    });
    queue.block_on(attempt).unwrap();
    assert_eq!(
        db.db().get(b"one-line").unwrap(),
        Some(b"optimistic".to_vec())
    );
    drop(queue);
    drop(db);

    let db = TransactionDb::open(dir.path(), Options::default().block_size(256)).unwrap();
    let mut queue = db.db().io_queue();
    let attempt = db.transact_async(queue.id(), &RetryPolicy::new(2), async |txn, _| {
        txn.put(b"one-line", b"pessimistic")?;
        Ok::<_, regolith::TransactionError>(())
    });
    queue.block_on(attempt).unwrap();
    assert_eq!(
        db.db().get(b"one-line").unwrap(),
        Some(b"pessimistic".to_vec())
    );
}

/// A task's waker that says whether it was woken since it was last asked.
#[derive(Default)]
struct Woken(std::sync::atomic::AtomicBool);

impl std::task::Wake for Woken {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl Woken {
    fn take(&self) -> bool {
        self.0.swap(false, Ordering::SeqCst)
    }
}

/// Poll `attempt`, and its queue between polls, for as long as either makes
/// progress. `Pending` means the attempt waits on something no poll of the
/// queue completes yet. Nothing here blocks or reads a clock.
fn drive<F: Future>(
    mut attempt: std::pin::Pin<&mut F>,
    queue: &mut regolith::IoQueue,
    woken: &Arc<Woken>,
) -> std::task::Poll<F::Output> {
    let waker = std::task::Waker::from(Arc::clone(woken));
    let mut cx = std::task::Context::from_waker(&waker);
    loop {
        if let std::task::Poll::Ready(output) = attempt.as_mut().poll(&mut cx) {
            return std::task::Poll::Ready(output);
        }
        let progress = queue.poll(regolith::IoBudget::ALL);
        if progress.completed == 0 && !woken.take() {
            return std::task::Poll::Pending;
        }
    }
}

/// An in-memory database on a clock only the test moves, with its queue.
fn on_a_stopped_clock(name: &str) -> (regolith::MemEnv, OptimisticTransactionDb) {
    let env = regolith::MemEnv::new();
    let shared: Arc<dyn regolith::Env> = Arc::new(env.clone());
    let db = OptimisticTransactionDb::open(name, Options::memory().env(shared)).unwrap();
    (env, db)
}

/// The wait a backoff takes between attempts blocks nothing: it is a wait
/// the queue completes, at the first poll that finds the environment's clock
/// past the delay, and at no poll before. Until then every poll says how long
/// the owner may idle.
#[test]
fn a_backoff_between_attempts_is_a_wait_the_queue_completes() {
    use std::task::Poll;
    use std::time::Duration;

    let (env, db) = on_a_stopped_clock("transact-async-backoff");
    let mut queue = db.db().io_queue();
    let woken = Arc::new(Woken::default());
    let runs = AtomicUsize::new(0);
    let policy = RetryPolicy::new(4).backoff(Duration::from_millis(10), Duration::from_millis(15));
    let mut attempt = std::pin::pin!(db.transact_async(queue.id(), &policy, async |txn, _| {
        let run = runs.fetch_add(1, Ordering::SeqCst);
        if run < 2 {
            // A rival writes the key first: this attempt loses.
            db.db().put(b"k", format!("rival-{run}").as_bytes())?;
        }
        txn.put(b"k", b"mine")?;
        Ok::<_, regolith::TransactionError>(run)
    }));

    // The first attempt lost. The attempt now waits out the first delay, and
    // no number of polls ends that while the clock stands still.
    assert!(drive(attempt.as_mut(), &mut queue, &woken).is_pending());
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    for _ in 0..3 {
        let progress = queue.poll(regolith::IoBudget::ALL);
        assert_eq!(progress.completed, 0);
        assert!(progress.more_pending);
        assert_eq!(progress.wake_in, Some(Duration::from_millis(10)));
        assert!(drive(attempt.as_mut(), &mut queue, &woken).is_pending());
    }
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    // One microsecond short of the delay: still waiting, and told how long.
    env.advance_micros(9_999);
    let progress = queue.poll(regolith::IoBudget::ALL);
    assert_eq!(
        (progress.completed, progress.wake_in),
        (0, Some(Duration::from_micros(1)))
    );
    assert!(!woken.take(), "a wait that is not over wakes no task");

    // At the delay the poll completes the wait and wakes the task, which
    // runs again, loses again, and waits out twice the delay, held to the
    // policy's bound: 15 ms, not 20.
    env.advance_micros(1);
    let progress = queue.poll(regolith::IoBudget::ALL);
    assert_eq!((progress.completed, progress.wake_in), (1, None));
    assert!(
        woken.take(),
        "the poll that completed the wait woke its task"
    );
    assert!(drive(attempt.as_mut(), &mut queue, &woken).is_pending());
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    assert_eq!(
        queue.poll(regolith::IoBudget::ALL).wake_in,
        Some(Duration::from_millis(15))
    );

    // Past the second delay the third run finds no rival and commits.
    env.advance_micros(15_000);
    match drive(attempt.as_mut(), &mut queue, &woken) {
        Poll::Ready(outcome) => assert_eq!(outcome.unwrap().0, 2),
        Poll::Pending => panic!("the wait was over and the attempt did not go on"),
    }
    assert_eq!(runs.load(Ordering::SeqCst), 3);
    assert_eq!(db.db().get(b"k").unwrap(), Some(b"mine".to_vec()));
    let idle = queue.poll(regolith::IoBudget::ALL);
    assert_eq!((idle.more_pending, idle.wake_in), (false, None));
}

/// Without a backoff a re-run waits for nothing: the attempt runs to its end
/// with the clock standing still. And once the policy is spent the last
/// conflict comes back at once, with no wait after it.
#[test]
fn without_a_backoff_and_after_the_last_attempt_nothing_waits() {
    use std::task::Poll;
    use std::time::Duration;

    let (env, db) = on_a_stopped_clock("transact-async-no-wait");
    let mut queue = db.db().io_queue();
    let woken = Arc::new(Woken::default());
    let losing = async |txn: &mut regolith::Transaction, _: Option<&regolith::Conflict>| {
        db.db().put(b"k", b"rival")?;
        txn.put(b"k", b"mine")?;
        Ok::<_, regolith::TransactionError>(())
    };

    let plain = RetryPolicy::new(3);
    let attempt = std::pin::pin!(db.transact_async(queue.id(), &plain, losing));
    assert!(matches!(
        drive(attempt, &mut queue, &woken),
        Poll::Ready(Err(TransactError::Exhausted(_)))
    ));

    // Two attempts, one wait between them, none after the second.
    let policy = RetryPolicy::new(2).backoff(Duration::from_millis(1), Duration::from_millis(1));
    let mut attempt = std::pin::pin!(db.transact_async(queue.id(), &policy, losing));
    assert!(drive(attempt.as_mut(), &mut queue, &woken).is_pending());
    env.advance_micros(1_000);
    assert!(matches!(
        drive(attempt.as_mut(), &mut queue, &woken),
        Poll::Ready(Err(TransactError::Exhausted(_)))
    ));
    assert_eq!(queue.poll(regolith::IoBudget::ALL).wake_in, None);
}

/// A policy whose one re-run comes an hour after the first attempt.
fn an_hour() -> RetryPolicy {
    let hour = std::time::Duration::from_secs(3_600);
    RetryPolicy::new(2).backoff(hour, hour)
}

/// Where the environment has no clock a delay cannot be measured, so the
/// wait is over at the queue's next poll.
#[test]
fn where_the_environment_has_no_clock_the_wait_is_over_at_the_next_poll() {
    let (env, db) = on_a_stopped_clock("transact-async-no-clock");
    env.set_clocks(None, None);
    let mut queue = db.db().io_queue();
    let woken = Arc::new(Woken::default());
    let runs = AtomicUsize::new(0);
    let attempt = std::pin::pin!(db.transact_async(queue.id(), &an_hour(), async |txn, _| {
        if runs.fetch_add(1, Ordering::SeqCst) == 0 {
            db.db().put(b"k", b"rival")?;
        }
        txn.put(b"k", b"mine")?;
        Ok::<_, regolith::TransactionError>(())
    }));
    assert!(matches!(
        drive(attempt, &mut queue, &woken),
        std::task::Poll::Ready(Ok(_))
    ));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

/// A queue dropped while an attempt waits on it completes the wait, since
/// nothing else would: the task is woken and goes on, instead of waiting for
/// a poll that can no longer come.
#[test]
fn a_queue_dropped_under_a_waiting_attempt_ends_the_wait() {
    let (_env, db) = on_a_stopped_clock("transact-async-dropped-queue");
    let mut queue = db.db().io_queue();
    let woken = Arc::new(Woken::default());
    let runs = AtomicUsize::new(0);
    let mut attempt = std::pin::pin!(db.transact_async(queue.id(), &an_hour(), async |txn, _| {
        if runs.fetch_add(1, Ordering::SeqCst) == 0 {
            db.db().put(b"k", b"rival")?;
        }
        txn.put(b"k", b"mine")?;
        Ok::<_, regolith::TransactionError>(())
    }));
    assert!(drive(attempt.as_mut(), &mut queue, &woken).is_pending());
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    drop(queue);
    assert!(
        woken.take(),
        "the drop completed the wait and woke its task"
    );
    let waker = std::task::Waker::from(Arc::clone(&woken));
    let mut cx = std::task::Context::from_waker(&waker);
    assert!(
        attempt.as_mut().poll(&mut cx).is_ready(),
        "the attempt went on without the queue"
    );
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

/// On a real clock, on a thread with nothing else to run: `block_on` idles
/// through the wait and no longer, and the re-run starts no sooner than the
/// policy's delay after the attempt that lost.
#[test]
fn block_on_idles_through_a_backoff_and_runs_the_attempt_again() {
    let dir = TempDir::new().unwrap();
    let options = Options::default();
    let clock = Arc::clone(options.get_env());
    let db = OptimisticTransactionDb::open(dir.path(), options).unwrap();
    let mut queue = db.db().io_queue();
    let starts = std::sync::Mutex::new(Vec::new());
    let delay = std::time::Duration::from_millis(3);
    let policy = RetryPolicy::new(3).backoff(delay, delay);
    queue
        .block_on(db.transact_async(queue.id(), &policy, async |txn, _| {
            let run = {
                let mut starts = starts.lock().unwrap();
                starts.push(clock.now_micros().unwrap());
                starts.len()
            };
            if run < 3 {
                db.db().put(b"k", format!("rival-{run}").as_bytes())?;
            }
            txn.put(b"k", b"mine")?;
            Ok::<_, regolith::TransactionError>(())
        }))
        .unwrap();
    let starts = starts.into_inner().unwrap();
    assert_eq!(starts.len(), 3);
    for pair in starts.windows(2) {
        assert!(
            pair[1] - pair[0] >= 3_000,
            "a re-run came {} µs on",
            pair[1] - pair[0]
        );
    }
}

/// A `before_commit` callback that fails once and would succeed if called
/// again: `transact` ends the attempt with its error, so `transact_async` must
/// too, with the callback run once, the abort callbacks run once, and nothing
/// committed.
#[test]
fn a_before_commit_error_ends_the_attempt_without_running_the_callback_again() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
    let mut queue = db.db().io_queue();
    let (calls, aborts, runs) = (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    );
    let attempt = |txn: &mut regolith::Transaction| {
        runs.fetch_add(1, Ordering::SeqCst);
        let counted = Arc::clone(&calls);
        let mut failures_left = 1;
        txn.before_commit(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            if failures_left > 0 {
                failures_left -= 1;
                return Err(Error::InvalidArgument("rejected".into()).into());
            }
            Ok(())
        });
        let aborted = Arc::clone(&aborts);
        txn.on_abort(move |why| {
            if matches!(why, AbortReason::Error(_)) {
                aborted.fetch_add(1, Ordering::SeqCst);
            }
        });
        txn.put(b"k", b"v")
    };
    let rejected = |outcome: &Result<((), regolith::CommitReceipt), TransactError<_>>| {
        matches!(
            outcome,
            Err(TransactError::Engine(TransactionError::Engine(
                Error::InvalidArgument(_)
            )))
        )
    };

    let blocking = db.transact(&RetryPolicy::default(), |txn, _| attempt(txn));
    assert!(rejected(&blocking), "{blocking:?}");
    let counts = || [&runs, &calls, &aborts].map(|count| count.swap(0, Ordering::SeqCst));
    assert_eq!(
        counts(),
        [1, 1, 1],
        "transact: runs, callback calls, aborts"
    );

    let nonblocking = queue.block_on(db.transact_async(
        queue.id(),
        &RetryPolicy::default(),
        async |txn, _| attempt(txn),
    ));
    assert!(rejected(&nonblocking), "{nonblocking:?}");
    assert_eq!(
        counts(),
        [1, 1, 1],
        "transact_async: runs, callback calls, aborts"
    );
    assert_eq!(db.db().get(b"k").unwrap(), None, "nothing committed");
}
