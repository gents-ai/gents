//! The pessimistic lock table (plan 4.6, 4.10, 4.11; D49, D53): one holder
//! per key, `try_get_for_update` that never waits, a `LockWait` completed on
//! the waiter's own queue, barging with bounded bypass, the lock timeout,
//! and what close does to a waiter.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::task::{Wake, Waker};
use std::time::{Duration, Instant};

use regolith::sync::MAX_BYPASS;
use regolith::{
    Error, IoBudget, IoQueue, LockWait, OptimisticTransactionDb, Options, Transaction,
    TransactionDb, TransactionError, TxResult, TxnOptions, WouldBlock,
};
use tempfile::TempDir;

/// Long enough that no test meets it by accident, short enough that a hang
/// fails the test instead of the run.
const PATIENCE: Duration = Duration::from_secs(60);

fn open(dir: &TempDir) -> TransactionDb {
    TransactionDb::open(dir.path(), Options::default())
        .unwrap()
        .with_lock_timeout(PATIENCE)
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

/// Counts wakes, as a `Waker`.
struct Wakes(AtomicUsize);

impl Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn counting_waker() -> (Arc<Wakes>, Waker) {
    let wakes = Arc::new(Wakes(AtomicUsize::new(0)));
    (Arc::clone(&wakes), Waker::from(Arc::clone(&wakes)))
}

/// The holder and the caller are on one thread, so a call that waited for
/// the lock would never return: returning at all is the proof it does not
/// wait.
#[test]
fn try_get_for_update_never_waits_and_takes_nothing() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    db.db().put(b"k", b"v0").unwrap();
    let holder = db.begin(&TxnOptions::new());
    assert_eq!(
        holder.try_get_for_update(b"k").unwrap(),
        Some(b"v0".to_vec()),
        "a free lock is taken and the key read"
    );
    assert_eq!(
        holder.try_get_for_update(b"k").unwrap(),
        Some(b"v0".to_vec()),
        "its holder takes it again"
    );
    let rival = db.begin(&TxnOptions::new());
    let wait = locked(rival.try_get_for_update(b"k"));
    assert!(!wait.is_ready());
    assert_eq!(wait.queue(), None, "the rival named no queue");
    assert!(
        locked(rival.try_get_for_update(b"k")).queue().is_none(),
        "asked again, it still does not wait"
    );
    // The refused call took nothing: the holder still writes and commits.
    holder.put(b"k", b"v1").unwrap();
    holder.commit().unwrap();
    assert!(wait.is_ready(), "with no queue the releaser completes it");
    assert_eq!(
        rival.try_get_for_update(b"k").unwrap(),
        Some(b"v1".to_vec())
    );
}

#[test]
fn an_optimistic_transaction_takes_no_lock() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
    db.db().put(b"k", b"v").unwrap();
    let (a, b) = (db.begin(&TxnOptions::new()), db.begin(&TxnOptions::new()));
    assert_eq!(a.try_get_for_update(b"k").unwrap(), Some(b"v".to_vec()));
    assert_eq!(b.try_get_for_update(b"k").unwrap(), Some(b"v".to_vec()));
}

#[test]
fn a_lock_wait_completes_on_its_own_queue_and_nowhere_else() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    let mut mine = db.db().io_queue();
    let mut other = db.db().io_queue();
    let holder = db.begin(&TxnOptions::new());
    holder.get_for_update(b"k").unwrap();
    let waiter = db.begin(&on(&mine));
    let wait = locked(waiter.try_get_for_update(b"k"));
    assert_eq!(wait.queue(), Some(mine.id()));
    assert_eq!(mine.poll(IoBudget::ALL).completed, 0);
    assert!(!wait.is_ready(), "still held");
    holder.rollback();
    assert!(!wait.is_ready(), "released, but not delivered yet");
    other.poll(IoBudget::ALL);
    assert!(!wait.is_ready(), "another queue's poll does not deliver it");
    assert_eq!(mine.poll(IoBudget::ALL).completed, 1);
    assert!(wait.is_ready());
    assert_eq!(waiter.try_get_for_update(b"k").unwrap(), None);
}

#[test]
fn an_idle_owner_is_woken_once_and_a_busy_one_never() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    let mut queue = db.db().io_queue();
    let holder = db.begin(&TxnOptions::new());
    holder.get_for_update(b"a").unwrap();
    holder.get_for_update(b"b").unwrap();
    let waiter = db.begin(&on(&queue));
    let first = locked(waiter.try_get_for_update(b"a"));
    let second = locked(waiter.try_get_for_update(b"b"));
    let (wakes, waker) = counting_waker();
    queue.poll(IoBudget::ALL);
    queue.idle_waker(&waker);
    assert_eq!(wakes.0.load(Ordering::SeqCst), 0, "nothing to take in yet");
    // Two locks released, two completions pushed: one wake.
    holder.rollback();
    assert_eq!(wakes.0.load(Ordering::SeqCst), 1, "an idle owner, once");
    assert_eq!(queue.poll(IoBudget::ALL).completed, 2);
    assert!(first.is_ready() && second.is_ready());
    assert_eq!(waiter.try_get_for_update(b"a").unwrap(), None);
    assert_eq!(waiter.try_get_for_update(b"b").unwrap(), None);

    // A busy owner: it polled and did not register to idle.
    let rival = db.begin(&on(&queue));
    let wait = locked(rival.try_get_for_update(b"a"));
    waiter.rollback();
    assert_eq!(wakes.0.load(Ordering::SeqCst), 1, "a busy owner, never");
    assert!(!wait.is_ready());
    assert_eq!(queue.poll(IoBudget::ALL).completed, 1);
    assert!(wait.is_ready());
}

#[test]
fn a_wait_whose_queue_was_dropped_is_completed_by_the_releaser() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    let queue = db.db().io_queue();
    let holder = db.begin(&TxnOptions::new());
    holder.get_for_update(b"k").unwrap();
    let waiter = db.begin(&on(&queue));
    let wait = locked(waiter.try_get_for_update(b"k"));
    drop(queue);
    assert!(!wait.is_ready());
    holder.rollback();
    assert!(wait.is_ready());
    // Asked to wait again, the transaction names a queue that is gone.
    let again = db.begin(&TxnOptions::new());
    again.get_for_update(b"other").unwrap();
    assert!(matches!(
        waiter.try_get_for_update(b"other"),
        Err(TransactionError::Engine(Error::InvalidArgument(_)))
    ));
}

/// D49: a free lock goes to whoever asks first, a waiter that loses keeps
/// its place, and after `MAX_BYPASS` losses the next release hands it the
/// lock, which nobody can take past it.
#[test]
fn a_waiter_passed_over_the_bound_is_handed_the_lock() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    let mut queue = db.db().io_queue();
    let mut holder = db.begin(&TxnOptions::new());
    holder.get_for_update(b"k").unwrap();
    let waiter = db.begin(&on(&queue));
    let mut wait = locked(waiter.try_get_for_update(b"k"));
    for round in 1..=MAX_BYPASS {
        holder.rollback();
        assert_eq!(queue.poll(IoBudget::ALL).completed, 1, "round {round}");
        assert!(
            wait.is_ready(),
            "round {round}: the release told the waiter"
        );
        // Someone else asks first and takes the free lock.
        holder = db.begin(&TxnOptions::new());
        holder
            .try_get_for_update(b"k")
            .unwrap_or_else(|e| panic!("round {round}: a free lock is barged: {e:?}"));
        wait = locked(waiter.try_get_for_update(b"k"));
    }
    // The waiter has lost MAX_BYPASS times: this release hands it the lock.
    holder.rollback();
    let barger = db.begin(&TxnOptions::new());
    let refused = locked(barger.try_get_for_update(b"k"));
    assert_eq!(queue.poll(IoBudget::ALL).completed, 1);
    assert!(wait.is_ready());
    assert_eq!(waiter.try_get_for_update(b"k").unwrap(), None);
    assert!(!refused.is_ready(), "the barger waits behind the handoff");
    waiter.rollback();
    assert!(refused.is_ready());
    assert_eq!(barger.try_get_for_update(b"k").unwrap(), None);
}

#[test]
fn a_lock_handed_to_a_transaction_that_ends_passes_on() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir);
    let mut holder = db.begin(&TxnOptions::new());
    holder.get_for_update(b"k").unwrap();
    let first = db.begin(&TxnOptions::new());
    let second = db.begin(&TxnOptions::new());
    let mut wait = locked(first.try_get_for_update(b"k"));
    let behind = locked(second.try_get_for_update(b"k"));
    for _ in 0..MAX_BYPASS {
        holder.rollback();
        assert!(wait.is_ready());
        holder = db.begin(&TxnOptions::new());
        holder.try_get_for_update(b"k").unwrap();
        wait = locked(first.try_get_for_update(b"k"));
    }
    holder.rollback();
    assert!(wait.is_ready(), "handed to the first waiter");
    assert!(!behind.is_ready());
    // It ends without ever running its call again: the lock passes on.
    drop(first);
    assert!(behind.is_ready());
    assert_eq!(second.try_get_for_update(b"k").unwrap(), None);
}

#[test]
fn a_blocking_wait_is_woken_by_the_release_long_before_its_timeout() {
    let dir = TempDir::new().unwrap();
    let db = Arc::new(open(&dir));
    let holder = db.begin(&TxnOptions::new());
    holder.put(b"k", b"v1").unwrap();
    let waiting = Arc::new(Barrier::new(2));
    let blocked = {
        let (db, waiting) = (Arc::clone(&db), Arc::clone(&waiting));
        std::thread::spawn(move || {
            let txn = db.begin(&TxnOptions::new());
            waiting.wait();
            let started = Instant::now();
            let read = txn.get_for_update(b"k");
            (read, started.elapsed())
        })
    };
    waiting.wait();
    std::thread::sleep(Duration::from_millis(50));
    holder.commit().unwrap();
    let (read, took) = blocked.join().unwrap();
    assert_eq!(read.unwrap(), Some(b"v1".to_vec()));
    assert!(
        took < PATIENCE / 2,
        "woken by the release, not by the timeout"
    );
}

#[test]
fn the_lock_timeout_ends_a_wait_and_a_deadlock_with_busy() {
    let dir = TempDir::new().unwrap();
    let timeout = Duration::from_millis(100);
    let db = Arc::new(
        TransactionDb::open(dir.path(), Options::default())
            .unwrap()
            .with_lock_timeout(timeout),
    );
    // A plain timeout: the holder never lets go.
    let holder = db.begin(&TxnOptions::new());
    holder.put(b"k", b"v").unwrap();
    let rival = db.begin(&TxnOptions::new());
    let started = Instant::now();
    match rival.put(b"k", b"w") {
        Err(TransactionError::Busy(key)) => assert_eq!(key, b"k"),
        other => panic!("expected Busy, got {other:?}"),
    }
    assert!(started.elapsed() >= timeout, "it waited the timeout out");
    // The wait it gave up left nothing behind: the lock is free once released.
    holder.rollback();
    rival.put(b"k", b"w").unwrap();
    rival.rollback();

    // A deadlock: each holds what the other wants. Nothing detects the
    // cycle; the timeout ends it, with Busy for at least one.
    let ready = Arc::new(Barrier::new(2));
    let cross = |mine: &'static [u8], theirs: &'static [u8]| {
        let (db, ready) = (Arc::clone(&db), Arc::clone(&ready));
        std::thread::spawn(move || {
            let txn = db.begin(&TxnOptions::new());
            txn.put(mine, b"x").unwrap();
            ready.wait();
            let second = txn.put(theirs, b"x");
            // Hold on until both have asked, so neither is served by the
            // other giving up early.
            std::thread::sleep(Duration::from_millis(20));
            second.map(|()| txn.rollback())
        })
    };
    let (left, right) = (cross(b"a", b"b"), cross(b"b", b"a"));
    let outcomes = [left.join().unwrap(), right.join().unwrap()];
    assert!(
        outcomes
            .iter()
            .any(|outcome| matches!(outcome, Err(TransactionError::Busy(_)))),
        "a deadlock ends in Busy: {outcomes:?}"
    );
}

/// Runs `call` until it stops asking to wait for a lock, waiting each wait
/// out on `queue`. A wait nobody completes fails the test instead of hanging.
fn through_locks<T>(queue: &mut IoQueue, mut call: impl FnMut() -> TxResult<T>) -> TxResult<T> {
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    loop {
        let wait = match call() {
            Err(TransactionError::WouldBlock(WouldBlock::Locked(wait))) => wait,
            other => return other,
        };
        let started = Instant::now();
        loop {
            queue.poll(IoBudget::ALL);
            if wait.is_ready() {
                break;
            }
            assert!(
                started.elapsed() < PATIENCE,
                "a lock wait was never completed: a wake-up was lost"
            );
            // Idle until a completion is pushed; the timeout only bounds a
            // lost wake so the assertion above can report it.
            queue.idle_waker(&waker);
            if !wait.is_ready() {
                std::thread::park_timeout(Duration::from_secs(1));
            }
        }
    }
}

/// Read-modify-write on a few hot keys from blocking threads and from
/// threads that never wait, all at once: every increment lands, and no two
/// transactions are ever inside one key's lock together.
#[test]
fn one_holder_per_key_under_stress_and_no_wake_is_lost() {
    const KEYS: usize = 3;
    const BLOCKING: usize = 4;
    const POLLING: usize = 4;
    const ROUNDS: usize = if cfg!(miri) { 4 } else { 400 };

    let dir = TempDir::new().unwrap();
    let db = Arc::new(open(&dir));
    let inside: Arc<Vec<AtomicUsize>> = Arc::new((0..KEYS).map(|_| AtomicUsize::new(0)).collect());
    let key = |n: usize| format!("counter-{}", n % KEYS).into_bytes();
    let bump = move |txn: &Transaction, inside: &[AtomicUsize], n: usize, read: Option<Vec<u8>>| {
        let slot = &inside[n % KEYS];
        assert_eq!(
            slot.fetch_add(1, Ordering::SeqCst),
            0,
            "two holders of one key"
        );
        let count = read.map_or(0, |bytes| u64::from_le_bytes(bytes.try_into().unwrap()));
        txn.put(&key(n), &(count + 1).to_le_bytes()).unwrap();
        std::hint::spin_loop();
        slot.fetch_sub(1, Ordering::SeqCst);
    };
    let mut threads = Vec::new();
    for worker in 0..BLOCKING {
        let (db, inside) = (Arc::clone(&db), Arc::clone(&inside));
        threads.push(std::thread::spawn(move || {
            for round in 0..ROUNDS {
                let n = worker + round;
                let txn = db.begin(&TxnOptions::new());
                let read = txn.get_for_update(&key(n)).unwrap();
                bump(&txn, &inside, n, read);
                txn.commit().unwrap();
            }
        }));
    }
    for worker in 0..POLLING {
        let (db, inside) = (Arc::clone(&db), Arc::clone(&inside));
        threads.push(std::thread::spawn(move || {
            let mut queue = db.db().io_queue();
            for round in 0..ROUNDS {
                let n = worker + round;
                let txn = db.begin(&on(&queue));
                let read = through_locks(&mut queue, || txn.try_get_for_update(&key(n))).unwrap();
                bump(&txn, &inside, n, read);
                txn.commit().unwrap();
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    let total: u64 = (0..KEYS)
        .map(|n| {
            let bytes = db.db().get(&key(n)).unwrap().expect("every key was bumped");
            u64::from_le_bytes(bytes.try_into().unwrap())
        })
        .sum();
    assert_eq!(
        total,
        ((BLOCKING + POLLING) * ROUNDS) as u64,
        "an increment was lost"
    );
}

/// One transaction used from two threads at once on one key: both calls
/// come back holding it, as two calls on one thread would. A call that waited
/// for its own transaction would meet the timeout, which is short here.
#[test]
fn two_threads_of_one_transaction_share_its_lock() {
    let dir = TempDir::new().unwrap();
    let db = open(&dir).with_lock_timeout(Duration::from_secs(5));
    for _ in 0..if cfg!(miri) { 2 } else { 200 } {
        let txn = db.begin(&TxnOptions::new());
        let start = Barrier::new(2);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                scope.spawn(|| {
                    start.wait();
                    txn.put(b"k", b"v").unwrap();
                    assert_eq!(txn.get_for_update(b"k").unwrap(), Some(b"v".to_vec()));
                });
            }
        });
        txn.rollback();
        let next = db.begin(&TxnOptions::new());
        assert_eq!(
            next.try_get_for_update(b"k").unwrap(),
            None,
            "released once, fully"
        );
    }
}

#[test]
fn close_makes_a_pending_lock_wait_ready_and_the_call_fails_with_closed() {
    let dir = TempDir::new().unwrap();
    let db = Arc::new(open(&dir));
    let mut queue = db.db().io_queue();
    let holder = db.begin(&TxnOptions::new());
    holder.get_for_update(b"k").unwrap();
    let waiter = db.begin(&on(&queue));
    let wait = locked(waiter.try_get_for_update(b"k"));
    let direct = db.begin(&TxnOptions::new());
    let direct_wait = locked(direct.try_get_for_update(b"k"));
    // A blocking call waits too, on another thread.
    let asked = Arc::new(Barrier::new(2));
    let blocked = {
        let (db, asked) = (Arc::clone(&db), Arc::clone(&asked));
        std::thread::spawn(move || {
            let txn = db.begin(&TxnOptions::new());
            asked.wait();
            let started = Instant::now();
            (txn.get_for_update(b"k"), started.elapsed())
        })
    };
    asked.wait();
    std::thread::sleep(Duration::from_millis(50));

    db.db().close().unwrap();

    assert!(
        direct_wait.is_ready(),
        "a wait with no queue is told by close"
    );
    assert!(!wait.is_ready(), "a wait on a queue is told at its poll");
    assert_eq!(queue.poll(IoBudget::ALL).completed, 1);
    assert!(wait.is_ready());
    for txn in [&waiter, &direct] {
        assert!(
            matches!(
                txn.try_get_for_update(b"k"),
                Err(TransactionError::Engine(Error::Closed))
            ),
            "the call run again finds the database closed"
        );
    }
    let (read, took) = blocked.join().unwrap();
    assert!(matches!(read, Err(TransactionError::Engine(Error::Closed))));
    assert!(took < PATIENCE / 2, "woken by close, not by the timeout");
    drop(holder);
}
