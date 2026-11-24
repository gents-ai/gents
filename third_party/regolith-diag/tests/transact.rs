//! `transact`: bounded retries owned by regolith.
//!
//! The closure runs in a fresh transaction, and when its commit loses a race it
//! runs again at once with the conflict that lost. Each case races the closure
//! against a write made around it, from inside the closure, so the outcome of
//! every attempt is decided and no timing is involved.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads and the filesystem. The browser suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use regolith::{
    Access, Error, OptimisticTransactionDb, Options, RetryPolicy, Statistics, Ticker,
    TransactError, TransactionDb, TransactionError, WriteKind,
};

fn open(dir: &std::path::Path) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(dir, Options::default()).unwrap()
}

#[test]
fn a_rerun_gets_the_conflict_the_previous_attempt_lost_to_and_the_receipt_comes_back() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"k", b"start").unwrap();

    let mut seen = Vec::new();
    let (read, receipt) = db
        .transact(&RetryPolicy::new(5), |tx, previous| {
            seen.push(previous.map(|c| (c.key().to_vec(), c.mine(), c.theirs())));
            let read = tx.get_for_update(b"k")?;
            if previous.is_none() {
                db.db().put(b"k", b"racer").unwrap();
            }
            tx.put(b"k", b"mine")?;
            Ok::<_, TransactionError>(read)
        })
        .unwrap();

    assert_eq!(
        seen,
        [
            None,
            Some((b"k".to_vec(), Access::ReadForUpdate, WriteKind::Put))
        ],
        "the first run has no conflict, the re-run has the one that lost"
    );
    assert_eq!(read, Some(b"racer".to_vec()), "the re-run sees the winner");
    assert_eq!(db.db().get(b"k").unwrap(), Some(b"mine".to_vec()));
    assert_eq!(receipt.seq(), db.db().latest_sequence());
}

#[test]
fn it_stops_at_max_attempts_and_returns_the_last_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = OptimisticTransactionDb::open(
        dir.path(),
        Options::default().statistics(Some(Arc::clone(&stats))),
    )
    .unwrap();
    db.db().put(b"k", b"start").unwrap();

    let runs = AtomicU64::new(0);
    let result = db.transact(&RetryPolicy::new(3), |tx, _| {
        let run = runs.fetch_add(1, Ordering::Relaxed);
        tx.get_for_update(b"k")?;
        db.db()
            .put(b"k", format!("racer {run}").as_bytes())
            .unwrap();
        tx.put(b"elsewhere", b"x")?;
        Ok::<_, TransactionError>(())
    });

    assert_eq!(runs.load(Ordering::Relaxed), 3);
    assert_eq!(stats.get_ticker(Ticker::CommitConflicts), 3);
    match result {
        Err(TransactError::Exhausted(conflict)) => {
            assert_eq!(conflict.key(), b"k");
            assert_eq!(
                (conflict.mine(), conflict.theirs()),
                (Access::ReadForUpdate, WriteKind::Put)
            );
        }
        other => panic!("expected the last conflict, got {other:?}"),
    }
    assert_eq!(
        db.db().get(b"elsewhere").unwrap(),
        None,
        "nothing committed"
    );
}

#[test]
fn zero_attempts_still_runs_the_closure_once() {
    assert_eq!(RetryPolicy::new(0).max_attempts(), 1);
    assert!(
        RetryPolicy::default().max_attempts() > 1,
        "a default that never retries would defeat transact"
    );

    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"k", b"start").unwrap();
    let runs = AtomicU64::new(0);
    let result = db.transact(&RetryPolicy::new(0), |tx, _| {
        runs.fetch_add(1, Ordering::Relaxed);
        tx.get_for_update(b"k")?;
        db.db().put(b"k", b"racer").unwrap();
        tx.put(b"elsewhere", b"x")?;
        Ok::<_, TransactionError>(())
    });
    assert_eq!(runs.load(Ordering::Relaxed), 1);
    assert!(
        matches!(result, Err(TransactError::Exhausted(_))),
        "{result:?}"
    );
}

#[test]
fn an_error_from_the_closure_ends_the_attempt_without_a_retry() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());

    let runs = AtomicU64::new(0);
    let result = db.transact(&RetryPolicy::new(5), |tx, _| {
        runs.fetch_add(1, Ordering::Relaxed);
        tx.put(b"written", b"x").unwrap();
        Err::<(), _>("boom")
    });
    assert_eq!(runs.load(Ordering::Relaxed), 1);
    match result {
        Err(TransactError::Closure("boom")) => {}
        other => panic!("expected the closure's error, got {other:?}"),
    }
    assert_eq!(
        db.db().get(b"written").unwrap(),
        None,
        "the attempt was rolled back"
    );
}

#[test]
fn the_closure_can_settle_the_race_from_the_conflict() {
    #[derive(Debug, PartialEq, Eq)]
    struct AlreadyDeleted;

    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"doc", b"body").unwrap();

    let mut reasons = Vec::new();
    let result = db.transact(&RetryPolicy::new(5), |tx, previous| {
        reasons.push(previous.map(|c| c.theirs()));
        if tx.get_for_update(b"doc").unwrap().is_none() {
            return Err(AlreadyDeleted);
        }
        if previous.is_none() {
            db.db().delete(b"doc").unwrap();
        }
        tx.delete(b"doc").unwrap();
        Ok(())
    });

    assert_eq!(reasons, [None, Some(WriteKind::Delete)]);
    match result {
        Err(TransactError::Closure(AlreadyDeleted)) => {}
        other => panic!("expected the closure to see the delete, got {other:?}"),
    }
}

#[test]
fn a_commit_that_fails_for_another_reason_is_not_retried() {
    let dir = tempfile::tempdir().unwrap();
    let db =
        OptimisticTransactionDb::open(dir.path(), Options::default().max_value_size(8)).unwrap();

    let runs = AtomicU64::new(0);
    let result = db.transact(&RetryPolicy::new(5), |tx, _| {
        runs.fetch_add(1, Ordering::Relaxed);
        tx.put(b"k", &[0; 64])?;
        Ok::<_, TransactionError>(())
    });
    assert_eq!(runs.load(Ordering::Relaxed), 1);
    assert!(
        matches!(
            result,
            Err(TransactError::Engine(TransactionError::Engine(
                Error::InvalidArgument(_)
            )))
        ),
        "{result:?}"
    );
}

#[test]
fn the_error_prints_the_closures_message_or_the_conflict_without_the_key() {
    fn assert_error<E: std::error::Error + 'static>() {}
    assert_error::<TransactError<std::io::Error>>();

    let closure: TransactError<String> = TransactError::Closure("boom".to_string());
    assert_eq!(closure.to_string(), "boom");

    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"SECRET-key", b"start").unwrap();
    let result = db.transact(&RetryPolicy::new(1), |tx, _| {
        tx.get_for_update(b"SECRET-key")?;
        db.db().put(b"SECRET-key", b"racer").unwrap();
        tx.put(b"elsewhere", b"x")?;
        Ok::<_, TransactionError>(())
    });
    let err = result.unwrap_err();
    for text in [err.to_string(), format!("{err:?}")] {
        assert!(!text.contains("SECRET"), "{text}");
    }
    assert!(err.to_string().contains("10-byte key"), "{err}");
}

#[test]
fn eight_threads_of_transacted_increments_lose_none() {
    const THREADS: u64 = 8;
    const PER_THREAD: u64 = 50;

    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"counter", &0u64.to_le_bytes()).unwrap();

    std::thread::scope(|scope| {
        for _ in 0..THREADS {
            scope.spawn(|| {
                for _ in 0..PER_THREAD {
                    db.transact(&RetryPolicy::new(1_000), |tx, _| {
                        let raw = tx.get_for_update(b"counter")?.unwrap();
                        let current = u64::from_le_bytes(raw.as_slice().try_into().unwrap());
                        tx.put(b"counter", &(current + 1).to_le_bytes())?;
                        Ok::<_, TransactionError>(())
                    })
                    .expect("a one-winner race resolves within the policy");
                }
            });
        }
    });

    let raw = db.db().get(b"counter").unwrap().unwrap();
    assert_eq!(
        u64::from_le_bytes(raw.as_slice().try_into().unwrap()),
        THREADS * PER_THREAD
    );
}

#[test]
fn a_pessimistic_database_transacts_the_same_way() {
    let dir = tempfile::tempdir().unwrap();
    let db = TransactionDb::open(dir.path(), Options::default()).unwrap();
    db.db().put(b"k", b"start").unwrap();

    let mut previous_seen = Vec::new();
    let (read, receipt) = db
        .transact(&RetryPolicy::default(), |tx, previous| {
            previous_seen.push(previous.map(|c| (c.mine(), c.theirs())));
            let read = tx.get_for_update(b"k")?;
            if previous.is_none() {
                // Around the lock manager, so the commit's check catches it.
                db.db().put(b"k", b"racer").unwrap();
            }
            tx.put(b"k", b"mine")?;
            Ok::<_, TransactionError>(read)
        })
        .unwrap();

    assert_eq!(
        previous_seen,
        [None, Some((Access::ReadForUpdate, WriteKind::Put))]
    );
    assert_eq!(read, Some(b"racer".to_vec()));
    assert_eq!(db.db().get(b"k").unwrap(), Some(b"mine".to_vec()));
    assert_eq!(receipt.seq(), db.db().latest_sequence());
}

/// Run a closure through `transact` that loses its first `losing` commits (a
/// rival writes its key from inside the closure each time) and then wins.
/// Returns the reading of `clock` at the start of each run.
fn run_starts(
    db: &OptimisticTransactionDb,
    clock: &dyn Fn() -> u64,
    policy: &RetryPolicy,
    losing: u64,
) -> Vec<u64> {
    let mut starts = Vec::new();
    let outcome = db.transact(policy, |tx, _| {
        let run = starts.len() as u64;
        starts.push(clock());
        if run < losing {
            db.db().put(b"k", format!("rival-{run}").as_bytes())?;
        }
        tx.put(b"k", b"mine")?;
        Ok::<_, TransactionError>(())
    });
    outcome.expect("the run after the losing ones commits");
    starts
}

fn gaps(starts: &[u64]) -> Vec<u64> {
    starts.windows(2).map(|pair| pair[1] - pair[0]).collect()
}

/// A policy that asks for a backoff waits before each re-run: its first delay
/// before the first, twice as long before each one after, never longer than
/// its bound. The waits are measured on the clock of the database's
/// environment, so each run starts at least its delay after the one before.
#[test]
fn a_backoff_waits_before_each_rerun_and_no_longer_than_its_bound() {
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let options = Options::default();
    let env = Arc::clone(options.get_env());
    let db = OptimisticTransactionDb::open(dir.path(), options).unwrap();
    let clock = || {
        env.now_micros()
            .expect("the standard environment has a clock")
    };

    let policy = RetryPolicy::new(5).backoff(Duration::from_millis(2), Duration::from_millis(5));
    let waited = gaps(&run_starts(&db, &clock, &policy, 4));
    assert_eq!(waited.len(), 4, "four losing runs, four re-runs");
    for (waited, at_least) in waited.iter().zip([2_000, 4_000, 5_000, 5_000]) {
        assert!(
            *waited >= at_least,
            "{waited} µs before a re-run owed {at_least}"
        );
    }

    // The delays the policy answers with are the ones it took.
    let delays: Vec<Duration> = (0..=5).map(|rerun| policy.backoff_before(rerun)).collect();
    let millis = |ms: u64| Duration::from_millis(ms);
    assert_eq!(
        delays,
        [
            Duration::ZERO,
            millis(2),
            millis(4),
            millis(5),
            millis(5),
            millis(5)
        ]
    );
}

/// Nothing waits unless the policy asks: without a backoff every re-run
/// starts at once, on an environment whose clock never moves, where a wait
/// measured on that clock would never end.
#[test]
fn without_a_backoff_a_rerun_waits_for_nothing() {
    let options = Options::memory();
    let env = Arc::clone(options.get_env());
    let db = OptimisticTransactionDb::open("transact-no-backoff", options).unwrap();
    let clock = || {
        env.now_micros()
            .expect("the memory environment has a clock")
    };

    let starts = run_starts(&db, &clock, &RetryPolicy::new(4), 3);
    assert_eq!(starts, [0, 0, 0, 0], "four runs, and the clock stood still");
    assert_eq!(
        RetryPolicy::default().backoff_before(1),
        std::time::Duration::ZERO
    );
}

/// The wait is measured on the environment's clock and on no other: on an
/// environment whose clock the test moves, each re-run starts once that clock
/// has passed its delay. A pessimistic database waits the same way.
#[test]
fn a_backoff_is_measured_on_the_clock_of_the_environment() {
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    let env = regolith::MemEnv::new();
    let shared: Arc<dyn regolith::Env> = Arc::new(env.clone());
    let options = || Options::memory().env(Arc::clone(&shared));
    let policy = RetryPolicy::new(4).backoff(Duration::from_millis(3), Duration::from_millis(4));
    let done = AtomicBool::new(false);

    let (optimistic, pessimistic) = std::thread::scope(|scope| {
        let runs = scope.spawn(|| {
            let db = OptimisticTransactionDb::open("transact-env-clock", options()).unwrap();
            let clock = || shared.now_micros().unwrap();
            let optimistic = run_starts(&db, &clock, &policy, 3);

            let db = TransactionDb::open("transact-env-clock-locks", options()).unwrap();
            let mut starts = Vec::new();
            db.transact(&policy, |tx, _| {
                let run = starts.len();
                starts.push(clock());
                tx.get_for_update(b"k")?;
                if run == 0 {
                    // Around the lock manager, so the commit's check sees it.
                    db.db().put(b"k", b"rival")?;
                }
                tx.put(b"k", b"mine")?;
                Ok::<_, TransactionError>(())
            })
            .unwrap();
            done.store(true, Ordering::SeqCst);
            (optimistic, starts)
        });
        // The only thing that moves the clock.
        while !done.load(Ordering::SeqCst) {
            env.advance_micros(250);
            std::thread::yield_now();
        }
        runs.join().unwrap()
    });

    for (waited, at_least) in gaps(&optimistic).iter().zip([3_000, 4_000, 4_000]) {
        assert!(
            *waited >= at_least,
            "{waited} µs before a re-run owed {at_least}"
        );
    }
    assert_eq!(gaps(&optimistic).len(), 3);
    let waited = gaps(&pessimistic);
    assert_eq!(waited.len(), 1);
    assert!(waited[0] >= 3_000, "{} µs before the re-run", waited[0]);
}
