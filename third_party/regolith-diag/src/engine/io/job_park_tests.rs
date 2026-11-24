//! A job several callers wait on, parked by one of them: nobody is left
//! waiting on a thread that no longer runs it (`job.rs`, "Parking").

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Wake, Waker};

use super::super::shared::{Message, QueueShared, test_id};
use super::{Job, JobBody, Step, nested_waker, unpark_waker};

/// Parks until `open` is set; then runs, counting.
struct Gated {
    open: Arc<AtomicBool>,
    runs: Arc<AtomicUsize>,
}

impl JobBody for Gated {
    fn run(self: Box<Self>) {
        self.runs.fetch_add(1, Ordering::SeqCst);
    }

    fn step(self: Box<Self>, _waker: &Waker) -> Step {
        if self.open.load(Ordering::SeqCst) {
            self.run();
            Step::Done
        } else {
            Step::Parked(self)
        }
    }

    fn release(self: Box<Self>) {}
}

fn gated() -> (Arc<Job>, Arc<AtomicBool>, Arc<AtomicUsize>) {
    let open = Arc::new(AtomicBool::new(false));
    let runs = Arc::new(AtomicUsize::new(0));
    let job = Job::new(Box::new(Gated {
        open: Arc::clone(&open),
        runs: Arc::clone(&runs),
    }));
    (job, open, runs)
}

struct Wakes(AtomicUsize);

impl Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn counting() -> (Arc<Wakes>, Waker) {
    let wakes = Arc::new(Wakes(AtomicUsize::new(0)));
    (Arc::clone(&wakes), Waker::from(Arc::clone(&wakes)))
}

fn queue() -> Arc<QueueShared> {
    Arc::new(QueueShared::new(test_id(), 1))
}

/// What `queue`'s inbox holds, as one letter a message: `R` for ready, `D`
/// for done.
fn inbox(queue: &QueueShared, job: &Arc<Job>) -> String {
    queue
        .take_inbox()
        .map(|message| match message {
            Message::Ready(told) if Arc::ptr_eq(&told, job) => 'R',
            Message::JobDone(told) if Arc::ptr_eq(&told, job) => 'D',
            _ => '?',
        })
        .collect()
}

#[test]
fn whoever_watches_a_claimed_job_is_woken_when_its_holder_parks_it() {
    let (job, _, _) = gated();
    let (wakes, waker) = counting();
    assert!(job.claim());
    assert!(job.watch(&waker), "claimed: the watcher parks");
    assert_eq!(wakes.0.load(Ordering::SeqCst), 0);
    assert!(!job.step(Waker::noop()), "the holder parks it");
    assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
    // The watcher's next look finds it free, and says so rather than park.
    assert!(!job.watch(&waker), "parked: look again, and claim it");
    assert!(job.claim());
}

#[test]
fn whoever_watches_a_claimed_job_is_woken_when_it_lands() {
    let (job, open, runs) = gated();
    let (wakes, waker) = counting();
    open.store(true, Ordering::SeqCst);
    assert!(job.claim());
    assert!(job.watch(&waker));
    assert!(job.step(Waker::noop()));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
    assert!(!job.watch(&waker), "landed: nothing to park for");
    assert_eq!(
        wakes.0.load(Ordering::SeqCst),
        1,
        "a landed job takes no watcher"
    );
}

/// The wake of a parked job tells the queue that stepped it and every other
/// queue registered on it, and leaves each of them registered: all are told
/// when it lands.
#[test]
fn a_parked_jobs_wake_tells_every_queue_registered_on_it() {
    let (job, open, _) = gated();
    let (stepper, other) = (queue(), queue());
    assert!(job.register(Arc::clone(&stepper)));
    assert!(job.register(Arc::clone(&other)));
    assert!(job.claim());
    let waker = unpark_waker(&job, &stepper);
    assert!(!job.step(&waker));
    assert_eq!(inbox(&stepper, &job), "", "parking tells no queue");
    assert_eq!(inbox(&other, &job), "");

    waker.wake_by_ref();
    assert_eq!(
        inbox(&stepper, &job),
        "R",
        "once, though it is registered too"
    );
    assert_eq!(inbox(&other, &job), "R");

    // Whichever owner polls next steps it: here, the other queue's.
    open.store(true, Ordering::SeqCst);
    assert!(job.claim());
    assert!(job.step(&unpark_waker(&job, &other)));
    assert_eq!(inbox(&stepper, &job), "D");
    assert_eq!(inbox(&other, &job), "D");
}

#[test]
fn a_wake_after_the_landing_tells_nobody() {
    let (job, open, _) = gated();
    let registered = queue();
    assert!(job.register(Arc::clone(&registered)));
    assert!(job.claim());
    let waker = unpark_waker(&job, &registered);
    assert!(!job.step(&waker));
    open.store(true, Ordering::SeqCst);
    assert!(job.claim());
    job.run();
    assert_eq!(inbox(&registered, &job), "D");
    waker.wake_by_ref();
    assert_eq!(inbox(&registered, &job), "");
    assert!(!job.register(registered), "a landed job takes no queue");
}

/// A job stepped inside another job's step is woken with a waker that
/// serves both: the queues registered on it, and the step it ran in.
#[test]
fn a_nested_wake_tells_the_inner_jobs_queues_and_wakes_the_outer_step() {
    let (inner, _, _) = gated();
    let registered = queue();
    assert!(inner.register(Arc::clone(&registered)));
    let (outer, outer_waker) = counting();
    assert!(inner.claim());
    let waker = nested_waker(&inner, &outer_waker);
    assert!(!inner.step(&waker));
    waker.wake_by_ref();
    assert_eq!(inbox(&registered, &inner), "R");
    assert_eq!(outer.0.load(Ordering::SeqCst), 1);
    waker.wake_by_ref();
    assert_eq!(inbox(&registered, &inner), "", "one wake a step");
    assert_eq!(outer.0.load(Ordering::SeqCst), 1);
}

/// A thread blocked on a job a poll has is told when the poll parks it, and
/// runs it itself. Whichever comes first, the park or the block.
#[test]
fn a_thread_blocked_on_a_job_claims_it_once_its_holder_parks_it() {
    let (job, _, runs) = gated();
    assert!(job.claim());
    std::thread::scope(|scope| {
        let blocked = scope.spawn(|| {
            // A blocking caller's loop (`engine::commit::park`).
            loop {
                if job.claim() {
                    job.run();
                    return;
                }
                if job.wait_turn() {
                    return;
                }
            }
        });
        assert!(!job.step(Waker::noop()), "still gated: it parks");
        blocked.join().unwrap();
    });
    assert!(job.is_done());
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "run by the thread that waited"
    );
}

#[test]
fn a_blocked_thread_still_wakes_when_the_job_lands() {
    let (job, open, _) = gated();
    open.store(true, Ordering::SeqCst);
    assert!(job.claim());
    std::thread::scope(|scope| {
        let blocked = scope.spawn(|| job.wait_turn());
        assert!(job.step(Waker::noop()));
        assert!(blocked.join().unwrap(), "landed, not parked");
    });
}

/// Wakes the waker its step was given before it hands itself back: the wait
/// it registered for ended while the step was still on its way out. Takes
/// what the other queue's inbox holds at that moment, as that queue's poll
/// would.
struct WokenBeforeItParked {
    job: std::sync::Weak<Job>,
    other: Arc<QueueShared>,
    told_while_claimed: Arc<AtomicUsize>,
}

impl JobBody for WokenBeforeItParked {
    fn run(self: Box<Self>) {}

    fn step(self: Box<Self>, waker: &Waker) -> Step {
        waker.wake_by_ref();
        let job = self
            .job
            .upgrade()
            .expect("the job is alive while it is stepped");
        let told = inbox(&self.other, &job);
        self.told_while_claimed
            .fetch_add(told.len(), Ordering::SeqCst);
        Step::Parked(self)
    }

    fn release(self: Box<Self>) {}
}

/// A wait that ends before the job finished parking: the queues are not
/// told while the thread that stepped it still holds the claim, since one
/// told then would try the claim, lose, and drop the job from its line.
/// They are told when the claim is given up, with the job free.
#[test]
fn a_wake_that_comes_before_the_park_tells_the_queues_once_the_claim_is_given_up() {
    let (stepper, other) = (queue(), queue());
    let told_while_claimed = Arc::new(AtomicUsize::new(0));
    let job = Arc::new_cyclic(|job| {
        Job::fresh(Box::new(WokenBeforeItParked {
            job: job.clone(),
            other: Arc::clone(&other),
            told_while_claimed: Arc::clone(&told_while_claimed),
        }))
    });
    assert!(job.register(Arc::clone(&stepper)));
    assert!(job.register(Arc::clone(&other)));
    assert!(job.claim());
    assert!(!job.step(&unpark_waker(&job, &stepper)));
    assert_eq!(
        told_while_claimed.load(Ordering::SeqCst),
        0,
        "a queue was told the job may go on while it was still claimed"
    );
    assert!(job.is_runnable());
    assert_eq!(inbox(&stepper, &job), "R");
    assert_eq!(inbox(&other, &job), "R", "told when the claim was given up");
    assert!(job.claim(), "and the queue told then gets the claim");
}
