//! A call blocked on a thread that owns a queue steps that queue's own jobs
//! while it waits ([`IoRuntime::wait_landed`]): each at most once a pass,
//! never to its end in one go, and never a job of another queue.
//!
//! The owner is a thread of the test, blocked for a job the test lands at
//! the end; the test's own thread watches counters with a deadline and
//! decides when each parked job may go on. Nothing is timed.

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Waker;
use std::time::{Duration, Instant};

use super::IoRuntime;
use super::job::{Job, JobBody, Step};
use super::shared::QueueShared;
use crate::io_queue::QueueId;

/// How long the test's thread waits for the owner's before it calls the
/// wait a hang.
const DEADLINE: Duration = Duration::from_secs(60);

/// What a job saw done to it.
#[derive(Default)]
struct Seen {
    /// Steps given to it, as a poll gives them.
    steps: AtomicUsize,
    /// Runs to its end in one go, as a worker gives them.
    runs: AtomicUsize,
    /// Whether it may go on: until then every step parks.
    open: AtomicBool,
    /// The waker its last step parked with.
    waker: Mutex<Option<Waker>>,
}

impl Seen {
    fn steps(&self) -> usize {
        self.steps.load(Ordering::SeqCst)
    }

    /// What the job parked for is free: its next step runs it to its end,
    /// and whoever stepped it last is woken.
    fn open(&self) {
        self.open.store(true, Ordering::SeqCst);
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}

/// A job that parks at every step until it is opened.
struct Parks(Arc<Seen>);

impl JobBody for Parks {
    fn run(self: Box<Self>) {
        self.0.runs.fetch_add(1, Ordering::SeqCst);
    }

    fn step(self: Box<Self>, waker: &Waker) -> Step {
        self.0.steps.fetch_add(1, Ordering::SeqCst);
        if self.0.open.load(Ordering::SeqCst) {
            return Step::Done;
        }
        *self.0.waker.lock().unwrap() = Some(waker.clone());
        // Looked at again once the waker is there, so an opening that came
        // before it is not missed.
        if self.0.open.load(Ordering::SeqCst) {
            return Step::Done;
        }
        Step::Parked(self)
    }

    fn release(self: Box<Self>) {}
}

/// Queue number `n` of `io`.
fn queue(io: &IoRuntime, n: u64) -> Arc<QueueShared> {
    io.open_queue(QueueId::new(NonZeroU64::new(n).unwrap()), 1)
}

/// A parking job left on `queue`.
fn left_on(io: &IoRuntime, queue: &QueueShared) -> (Arc<Job>, Arc<Seen>) {
    let seen = Arc::new(Seen::default());
    let job = Job::new(Box::new(Parks(Arc::clone(&seen))));
    assert!(io.submit(queue, Arc::clone(&job), None).is_ok());
    (job, seen)
}

/// Wait, with a deadline, until `job` has been given its step number
/// `steps` and that step is over: the job is parked again, or ended. A job
/// opened while a step is still under way would end in that step, and the
/// counts below would not be exact.
fn until_stepped(job: &Job, seen: &Seen, steps: usize) {
    let deadline = Instant::now() + DEADLINE;
    while seen.steps() < steps || !(job.is_runnable() || job.is_done()) {
        assert_eq!(
            seen.runs.load(Ordering::SeqCst),
            0,
            "the blocked call ran the job to its end in one go instead of stepping it"
        );
        assert!(
            Instant::now() < deadline,
            "the blocked call never gave the job step {steps} (it has had {})",
            seen.steps()
        );
        std::thread::yield_now();
    }
}

/// Run `watch` on this thread while another thread, which owns `own` when
/// there is one, blocks in `wait_landed` for a job that is landed once
/// `watch` returns.
fn while_a_call_blocks(io: &IoRuntime, own: Option<&Arc<QueueShared>>, watch: impl FnOnce()) {
    let target = Job::passive();
    std::thread::scope(|scope| {
        let blocked = scope.spawn(|| {
            if let Some(queue) = own {
                io.bind_current(queue.id());
                queue.own();
            }
            io.wait_landed(&target);
        });
        // A failed assertion in `watch` must not leave the call blocked.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(watch));
        target.release();
        blocked.join().unwrap();
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    });
}

/// The whole rule on one queue with two jobs. The first pass steps each
/// once and sleeps. One job is opened: the call is woken, and its next pass
/// steps both, ends that one, and sleeps. The other is opened: one more
/// pass ends it. The counts are exact: a call that spun, made a pass with
/// no wake, or gave a job two steps in a pass, would count more.
#[test]
fn a_blocked_call_steps_each_of_its_queues_jobs_once_a_pass_and_sleeps_between() {
    let io = IoRuntime::new();
    let own = queue(&io, 1);
    let (first_job, first) = left_on(&io, &own);
    let (second_job, second) = left_on(&io, &own);
    while_a_call_blocks(&io, Some(&own), || {
        until_stepped(&first_job, &first, 1);
        until_stepped(&second_job, &second, 1);
        second.open();
        // The pass that ends `second` steps `first` once more.
        until_stepped(&first_job, &first, 2);
        until_stepped(&second_job, &second, 2);
        assert!(second_job.is_done(), "its second step ran it to its end");
        first.open();
        until_stepped(&first_job, &first, 3);
        assert!(first_job.is_done());
    });
    assert_eq!(first.steps(), 3, "one step a pass, and one pass a wake");
    assert_eq!(second.steps(), 2);
    assert_eq!(
        (
            first.runs.load(Ordering::SeqCst),
            second.runs.load(Ordering::SeqCst)
        ),
        (0, 0),
        "a job is stepped, never run to its end in one go"
    );
    assert_eq!(io.jobs_in_flight(), 0, "both ended and left the table");
}

/// A job whose wait ends while the call is still in its pass: its first
/// step wakes its own waker before it parks, as a job does when what it
/// waits for is freed by a step later in the same pass. The call has not
/// gone to sleep yet, and must not sleep through it: it makes one more
/// pass, which ends the job.
struct GoesOnAtOnce(Arc<Seen>);

impl JobBody for GoesOnAtOnce {
    fn run(self: Box<Self>) {
        self.0.runs.fetch_add(1, Ordering::SeqCst);
    }

    fn step(self: Box<Self>, waker: &Waker) -> Step {
        if self.0.steps.fetch_add(1, Ordering::SeqCst) > 0 {
            return Step::Done;
        }
        waker.wake_by_ref();
        Step::Parked(self)
    }

    fn release(self: Box<Self>) {}
}

#[test]
fn a_job_that_may_go_on_before_the_pass_is_over_is_stepped_again() {
    let io = IoRuntime::new();
    let own = queue(&io, 1);
    let seen = Arc::new(Seen::default());
    let job = Job::new(Box::new(GoesOnAtOnce(Arc::clone(&seen))));
    assert!(io.submit(&own, Arc::clone(&job), None).is_ok());
    while_a_call_blocks(&io, Some(&own), || {
        until_stepped(&job, &seen, 2);
        assert!(job.is_done());
    });
    assert_eq!(seen.steps(), 2, "one pass for the wake, and no more");
}

/// A job another caller left on another queue is that queue's owner's to
/// run: the blocked call gives it no step, whatever it waits for.
#[test]
fn a_blocked_call_gives_no_step_to_a_job_of_another_queue() {
    let io = IoRuntime::new();
    let own = queue(&io, 1);
    let other = queue(&io, 2);
    let (own_job, mine) = left_on(&io, &own);
    let (theirs_job, theirs) = left_on(&io, &other);
    while_a_call_blocks(&io, Some(&own), || {
        // The call has made its pass: its own job was stepped.
        until_stepped(&own_job, &mine, 1);
        mine.open();
        until_stepped(&own_job, &mine, 2);
    });
    assert_eq!(theirs.steps(), 0, "another queue's job was stepped");
    assert_eq!(theirs.runs.load(Ordering::SeqCst), 0);
    assert!(theirs_job.is_runnable(), "it is still its owner's to run");
    assert_eq!(io.jobs_in_flight(), 1);
}

/// A job another thread is running is left to that thread.
#[test]
fn a_blocked_call_leaves_a_job_another_thread_claimed() {
    let io = IoRuntime::new();
    let own = queue(&io, 1);
    let (claimed_job, claimed) = left_on(&io, &own);
    let (free_job, free) = left_on(&io, &own);
    assert!(claimed_job.claim());
    while_a_call_blocks(&io, Some(&own), || {
        until_stepped(&free_job, &free, 1);
        free.open();
        until_stepped(&free_job, &free, 2);
    });
    assert_eq!(
        claimed.steps(),
        0,
        "a claimed job was stepped a second time over"
    );
}

/// A thread with no queue has nothing of its own to step: it waits.
#[test]
fn a_blocked_call_on_a_thread_with_no_queue_steps_nothing() {
    let io = IoRuntime::new();
    let some = queue(&io, 1);
    let (_job, seen) = left_on(&io, &some);
    while_a_call_blocks(&io, None, || {});
    assert_eq!(seen.steps(), 0);
}
