//! Timed waits on a queue: recorded through the inbox from any thread,
//! completed by the owner's poll at their due time, and by nothing else.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Wake, Waker};
use std::time::Duration;

use crate::{Db, IoBudget, MemEnv, Options};

/// A database on a clock only the test moves.
fn on_a_stopped_clock() -> (MemEnv, Db) {
    let env = MemEnv::new();
    let shared: Arc<dyn crate::Env> = Arc::new(env.clone());
    let db = Db::open("timed-waits", Options::memory().env(shared)).unwrap();
    (env, db)
}

#[derive(Default)]
struct Wakes(AtomicUsize);

impl Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl Wakes {
    fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

#[test]
fn a_timed_wait_is_over_at_the_poll_that_finds_it_due_and_at_none_before() {
    let (env, db) = on_a_stopped_clock();
    let mut queue = db.io_queue();
    let later = queue.runtime().timer(queue.id(), 30);
    let sooner = queue.runtime().timer(queue.id(), 10);

    // Not due: any number of polls completes neither and says how long.
    for _ in 0..3 {
        let progress = queue.poll(IoBudget::ALL);
        assert_eq!(progress.completed, 0);
        assert!(progress.more_pending);
        assert_eq!(progress.wake_in, Some(Duration::from_micros(10)));
    }
    assert!(!sooner.is_ready() && !later.is_ready());

    // A poll that may run nothing still completes a wait that is due.
    env.advance_micros(10);
    let progress = queue.poll(IoBudget::units(0));
    assert_eq!(progress.completed, 1);
    assert_eq!(progress.wake_in, Some(Duration::from_micros(20)));
    assert!(sooner.is_ready() && !later.is_ready());

    env.advance_micros(25);
    let progress = queue.poll(IoBudget::ALL);
    assert_eq!((progress.completed, progress.more_pending), (1, false));
    assert_eq!(progress.wake_in, None);
    assert!(later.is_ready());
}

/// An owner about to idle is woken at once for a wait that is already due,
/// and not for one that is not: that one it polls for by `wake_in`.
#[test]
fn an_owner_about_to_idle_is_woken_only_for_a_wait_that_is_due() {
    let (env, db) = on_a_stopped_clock();
    let mut queue = db.io_queue();
    let wakes = Arc::new(Wakes::default());
    let waker = Waker::from(Arc::clone(&wakes));
    let wait = queue.runtime().timer(queue.id(), 50);
    assert_eq!(
        queue.poll(IoBudget::ALL).wake_in,
        Some(Duration::from_micros(50))
    );

    queue.idle_waker(&waker);
    assert_eq!(wakes.count(), 0, "not due: the owner idles");
    env.advance_micros(50);
    assert_eq!(wakes.count(), 0, "a clock that moves wakes nobody");
    queue.idle_waker(&waker);
    assert_eq!(wakes.count(), 1, "due: the owner has work");
    assert_eq!(queue.poll(IoBudget::ALL).completed, 1);
    assert!(wait.is_ready());
}

/// A wait recorded while the owner idles wakes it, once, so its next poll
/// tells it how long it may idle from then on.
#[test]
fn a_wait_recorded_while_the_owner_idles_wakes_it_once() {
    let (_env, db) = on_a_stopped_clock();
    let mut queue = db.io_queue();
    let wakes = Arc::new(Wakes::default());
    queue.idle_waker(&Waker::from(Arc::clone(&wakes)));

    let (runtime, id) = (queue.runtime(), queue.id());
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let _first = runtime.timer(id, 70);
            let _second = runtime.timer(id, 90);
        });
    });
    assert_eq!(wakes.count(), 1, "one wake for the rest, not one a wait");
    assert_eq!(
        queue.poll(IoBudget::ALL).wake_in,
        Some(Duration::from_micros(70))
    );
}

/// Nothing would ever complete a wait on a queue that is not open, so it is
/// over when it is asked for. A queue that is dropped completes the waits it
/// holds, taken in from its inbox or not.
#[test]
fn a_wait_does_not_outlive_its_queue() {
    let (_env, db) = on_a_stopped_clock();
    let mut queue = db.io_queue();
    let (taken_in, id) = (queue.runtime().timer(queue.id(), u64::MAX), queue.id());
    queue.poll(IoBudget::ALL);
    let in_the_inbox = queue.runtime().timer(id, u64::MAX);
    assert!(!taken_in.is_ready() && !in_the_inbox.is_ready());

    let other = db.io_queue();
    drop(queue);
    assert!(taken_in.is_ready() && in_the_inbox.is_ready());
    assert!(other.runtime().timer(id, u64::MAX).is_ready());
}
