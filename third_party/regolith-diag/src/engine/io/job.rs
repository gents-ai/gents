//! One unit of I/O that is not a block read, run once however many queues
//! wait on it (D53).
//!
//! regolith hands every piece of I/O a non-blocking caller owes to the queue
//! of the thread that asked for it, as a job:
//!
//! - a sync round of the commit pipeline, which every commit it covers
//!   waits on, and which the first of their threads to poll runs
//!   (`engine::commit::round`); an ordered unit of the pipeline, and the
//!   decision of a slot left to a thread that may read the device, are jobs
//!   too (`engine::commit::ordered`, `engine::commit::read_mode`);
//! - the bounded step of background work a write left owing with no worker,
//!   on the writing thread's queue (`engine::background_step`);
//! - the stall a stopped write waits out, which does no work of its own and
//!   lands when the stall clears (`engine::commit::stall`);
//! - a foreground job with no worker: `compact_range`, an ingest, a
//!   checkpoint (`engine::foreground`).
//!
//! A job is a [`Flight`]: claimed by one compare-and-swap and landed once,
//! pushing one `JobDone` to every queue that registered on it. A waiter with
//! no queue (a blocking call, a ticket whose caller has none) listens on the
//! job directly and is told by the thread that lands it, and a thread that
//! blocks on the job parks until then.
//!
//! A *passive* job has no body: no poll ever runs it, and only the event it
//! stands for lands it, through [`Job::release`]. That is the stall: its
//! readiness is the stall clearing, which a flush or a compaction makes
//! happen elsewhere.
//!
//! # Parking
//!
//! A queue's poll never waits for another thread, so it runs a job through
//! [`JobBody::step`]. A job whose next step needs something another thread
//! holds (a foreground job needs the compaction gate) does not wait for it:
//! it registers the waker its step was given where the wait ends and hands
//! itself back ([`Step::Parked`]). The job gives its claim back and stays on
//! its queue, not runnable. When what it waits for may be free, the waker
//! pushes [`Message::Ready`] to the queue, which wakes an idle owner once,
//! and the owner's next poll claims the job and steps it again. The message
//! sits in the inbox until the owner takes it, so a wake that comes before
//! the job finished parking is not lost. A parked job is still found and
//! settled by close, by its queue's drop, and by a blocking
//! [`JobTicket::wait`](crate::JobTicket::wait), each through a claim.
//!
//! A job several callers wait on (a unit of the commit pipeline) may be
//! parked by one of them while the others wait for it. None of them is left
//! waiting on a thread that is no longer running it:
//!
//! - **Who waited for the thread that held the claim is told when it parks.**
//!   A thread blocked in [`Job::wait_turn`] wakes and claims the job itself.
//!   A job that parked because this one was claimed ([`Job::watch`]) is
//!   woken, and its next step claims this one.
//! - **Every queue registered on the job is told when its wait ends**, not
//!   only the one that stepped it ([`Job::ready`]): a queue whose claim lost
//!   while the job was being stepped dropped it from its line, and the owner
//!   of the queue that parked it may never poll again. Whichever owner polls
//!   next steps it. They are told while the job is free to be claimed: a
//!   wait that ends while the thread that parked the job still holds its
//!   claim tells them when that thread lets go.

#![allow(unsafe_code)]

use std::sync::{Arc, Weak};
use std::task::{Wake, Waker};

// A blocked thread parks, and loom has to see the park to check that the
// landing wakes it.
#[cfg(loom)]
use loom::thread::{self, Thread};
#[cfg(not(loom))]
use std::thread::{self, Thread};

use super::flight::Flight;
use super::shared::{Message, QueueShared};
use super::stack::{SHUT, Stack};
use crate::sync::internal::{Ordering, UnsafeCell, fence};

/// What one step of a job did.
pub(crate) enum Step {
    /// The job ran to its end.
    Done,
    /// The job cannot go on without waiting for another thread. It hands
    /// itself back, having registered the step's waker to be woken once it
    /// may go on.
    Parked(Box<dyn JobBody>),
}

/// The work a job does.
pub(crate) trait JobBody: Send {
    /// Do the work, on the thread that claimed the job, waiting on this
    /// thread for whatever it needs: a worker's way, and a blocking
    /// caller's.
    fn run(self: Box<Self>);
    /// Do as much of the work as needs no wait for another thread: a
    /// queue's poll's way. A body that would wait registers `waker` where
    /// its wait ends and parks instead. A body that never waits for another
    /// thread keeps this default, which runs it.
    fn step(self: Box<Self>, _waker: &Waker) -> Step {
        self.run();
        Step::Done
    }
    /// Settle the job without running it, when the database closes or the
    /// queue that would have run it is dropped. A job whose work cannot be
    /// left undone (a sync round commits wait on) runs it here instead.
    fn release(self: Box<Self>);
}

/// Something a caller waits for, told on its owner's thread when the work
/// it waits on is done: a read's wait, a commit's ticket, a job's ticket.
pub(crate) trait Delivery: Send + Sync {
    /// Mark the wait ready, run what it owes on this thread, and wake the
    /// task awaiting it.
    fn deliver(&self);
}

pub(crate) struct Job {
    flight: Flight,
    /// Taken by the claim holder. `None` from the start for a passive job.
    body: UnsafeCell<Option<Box<dyn JobBody>>>,
    passive: bool,
    /// Waiters with no queue, told by the lander, closed with [`SHUT`].
    direct: Stack<Arc<dyn Delivery>>,
    /// Threads blocked until the job lands or its holder parks it, closed
    /// with [`SHUT`].
    sleepers: Stack<Thread>,
    /// Wakers of jobs that parked because this one was claimed, woken when
    /// it lands or its holder parks it, closed with [`SHUT`].
    watchers: Stack<Waker>,
}

// SAFETY: `body` is only touched by the one thread whose claim succeeded
// (`Flight::claim`), and a `JobBody` is `Send`. Everything else is atomic.
unsafe impl Sync for Job {}

impl Job {
    /// A job that runs `body` when a waiting queue's owner claims it.
    pub(crate) fn new(body: Box<dyn JobBody>) -> Arc<Self> {
        Arc::new(Self::fresh(body))
    }

    /// [`Self::new`] before it is shared: for a caller that has an
    /// allocation to put it in.
    pub(crate) fn fresh(body: Box<dyn JobBody>) -> Self {
        Self {
            flight: Flight::new(),
            body: UnsafeCell::new(Some(body)),
            passive: false,
            direct: Stack::new(),
            sleepers: Stack::new(),
            watchers: Stack::new(),
        }
    }

    /// A job no poll runs, landed only by [`Job::release`].
    pub(crate) fn passive() -> Arc<Self> {
        Arc::new(Self {
            flight: Flight::new(),
            body: UnsafeCell::new(None),
            passive: true,
            direct: Stack::new(),
            sleepers: Stack::new(),
            watchers: Stack::new(),
        })
    }

    pub(crate) fn is_done(&self) -> bool {
        self.flight.is_done()
    }

    /// No poll runs it; only the event it stands for lands it.
    pub(crate) fn is_passive(&self) -> bool {
        self.passive
    }

    /// Test-only: whether a thread is blocked on the job, or about to be.
    #[cfg(test)]
    pub(crate) fn has_sleeper(&self) -> bool {
        !self.sleepers.is_empty()
    }

    /// A waiting queue's owner may claim and run it now.
    pub(crate) fn is_runnable(&self) -> bool {
        !self.passive && self.flight.is_free()
    }

    /// Take the job to run it, with one compare-and-swap. Never for a
    /// passive job. `true` for exactly one caller; it must then
    /// [`run`](Self::run) it.
    pub(crate) fn claim(&self) -> bool {
        !self.passive && self.flight.claim()
    }

    /// Record `queue` as waiting. `false` when the job already landed.
    pub(crate) fn register(&self, queue: Arc<QueueShared>) -> bool {
        self.flight.register(queue)
    }

    /// Tell `waiter` directly, on the landing thread, when the job lands.
    /// `false` when it already landed: the caller tells `waiter` itself.
    pub(crate) fn listen(&self, waiter: Arc<dyn Delivery>) -> bool {
        self.direct.push(waiter, SHUT).is_ok()
    }

    /// Have `waker` woken when the thread that claimed the job lets it go:
    /// when the job lands, or when that thread parks it. For a caller whose
    /// claim just lost and which may not wait: a job a queue's poll steps.
    /// `true` when the job is still claimed and `waker` will be woken;
    /// `false` when it landed or was parked meanwhile, and the caller looks
    /// again now.
    pub(crate) fn watch(&self, waker: &Waker) -> bool {
        if self.watchers.push(waker.clone(), SHUT).is_err() {
            return false;
        }
        // With the fence in `let_go`: a holder that parked the job before
        // this push was seen is seen here, so one of the two sides acts.
        fence(Ordering::SeqCst);
        !self.is_done() && !self.is_runnable()
    }

    /// Run the body on this thread, after this caller's claim won, and land
    /// the job. A panic in the body still lands it, so no waiter is left on
    /// a claim nobody will complete; the panic then goes on unwinding.
    pub(crate) fn run(self: &Arc<Self>) {
        struct Unwind<'a>(Option<&'a Arc<Job>>);
        impl Drop for Unwind<'_> {
            fn drop(&mut self) {
                if let Some(job) = self.0.take() {
                    job.land();
                }
            }
        }
        let mut unwind = Unwind(Some(self));
        // SAFETY: the claim gave this thread the body.
        if let Some(body) = self.body.with_mut(|body| unsafe { (*body).take() }) {
            body.run();
        }
        unwind.0 = None;
        self.land();
    }

    /// [`run`](Self::run) for a queue's poll, after this caller's claim won:
    /// step the body, never waiting for another thread. `true` when the job
    /// ran to its end and landed. `false` when it parked: the body is back
    /// in the job and the claim given up, and `waker` is woken once the job
    /// may go on. A panic in the body lands the job, as in `run`.
    pub(crate) fn step(self: &Arc<Self>, waker: &Waker) -> bool {
        struct Unwind<'a>(Option<&'a Arc<Job>>);
        impl Drop for Unwind<'_> {
            fn drop(&mut self) {
                if let Some(job) = self.0.take() {
                    job.land();
                }
            }
        }
        let mut unwind = Unwind(Some(self));
        // SAFETY: the claim gave this thread the body.
        let body = self.body.with_mut(|body| unsafe { (*body).take() });
        let step = {
            // Every step of every job passes here: nothing in it may wait
            // for another thread, and a path that does fails a build with
            // debug assertions when the step ends.
            let _no_wait = super::scope::no_wait();
            body.map(|body| body.step(waker))
        };
        unwind.0 = None;
        if let Some(Step::Parked(body)) = step {
            // SAFETY: still under the claim, which `park` gives up only
            // after the body is back.
            self.body.with_mut(|slot| unsafe { *slot = Some(body) });
            self.flight.park();
            self.let_go();
            return false;
        }
        self.land();
        true
    }

    /// The claim was given back without a landing: whoever waited for the
    /// thread that held it looks again, and may claim the job itself.
    fn let_go(&self) {
        // With the fences in `watch` and `park_until`: a waiter that did not
        // see the job parked pushed itself before the lists are taken here.
        fence(Ordering::SeqCst);
        // Open takes: a thread that claimed the job since may be landing it,
        // and its closing of these lists must stand.
        for waker in self.watchers.take_open() {
            waker.wake();
        }
        for sleeper in self.sleepers.take_open() {
            sleeper.unpark();
        }
    }

    /// A parked job may go on: tell the queue that stepped it, and every
    /// other queue registered on it. Whichever of their owners polls next
    /// claims the job and steps it.
    ///
    /// The wake may come before the job finished parking: the thread that
    /// stepped it registered its waker, looked again, and has not given the
    /// claim up yet. A queue told then would try the claim, lose, and drop
    /// the job from its line, and nobody would tell it again. So a wake
    /// that finds the job claimed leaves the telling to the moment its
    /// holder lets go ([`Self::watch`], whose handshake that is): when it
    /// parks the job the queues are told here, and when it lands it they
    /// learn that.
    fn ready(self: &Arc<Self>, stepper: Option<&Arc<QueueShared>>) {
        if self.is_done() {
            // Run to its end since, by a blocking caller or another poll.
            return;
        }
        if !self.is_runnable() && self.watch(&later_waker(self, stepper)) {
            return;
        }
        if self.is_done() {
            return;
        }
        if let Some(queue) = stepper {
            // A queue dropped meanwhile settled the job in its drop.
            let _ = queue.deliver(Message::Ready(Arc::clone(self)));
        }
        for queue in self.flight.registered() {
            if !stepper.is_some_and(|stepper| Arc::ptr_eq(stepper, &queue)) {
                let _ = queue.deliver(Message::Ready(Arc::clone(self)));
            }
            if !self.flight.register(Arc::clone(&queue)) {
                // The job landed while this queue was off the list, so its
                // lander did not see the queue: it is told here.
                let _ = queue.deliver(Message::JobDone(Arc::clone(self)));
            }
        }
    }

    /// Settle a job nobody is running without running it (its body says how)
    /// and land it. `false` when another thread had already claimed it; that
    /// thread lands it.
    pub(crate) fn release(self: &Arc<Self>) -> bool {
        if !self.flight.claim() {
            return false;
        }
        // SAFETY: the claim above gave this thread the body.
        if let Some(body) = self.body.with_mut(|body| unsafe { (*body).take() }) {
            body.release();
        }
        self.land();
        true
    }

    /// Block this thread until the job has landed.
    pub(crate) fn wait_landed(&self) {
        if self.is_done() {
            return;
        }
        // A wait for another thread: release what this thread's last read
        // holds back first, and what the views retired meanwhile placed on
        // it once the wait is over (`read_view::idle`).
        super::super::read_view::idle();
        self.park_until_landed();
        super::super::read_view::idle();
    }

    /// Block this thread until the job has landed (`true`), or until the
    /// thread that claimed it parked it (`false`): the caller, whose claim
    /// lost before, may then claim it. What a blocking caller does about a
    /// job another thread has, so that it never sleeps on a job nobody is
    /// running.
    pub(crate) fn wait_turn(&self) -> bool {
        if self.is_done() {
            return true;
        }
        // As in `wait_landed`.
        super::super::read_view::idle();
        let landed = self.park_until(true);
        super::super::read_view::idle();
        landed
    }

    /// [`Self::wait_landed`]'s parking.
    fn park_until_landed(&self) {
        self.park_until(false);
    }

    /// Park this thread until the job has landed (`true`), or, with
    /// `or_parked`, until its holder parked it (`false`).
    fn park_until(&self, or_parked: bool) -> bool {
        loop {
            // A refused push means the lander already took the sleepers: the
            // job is done.
            if self.is_done() || self.sleepers.push(thread::current(), SHUT).is_err() {
                return true;
            }
            // With the fence in `let_go`, as in `watch`.
            fence(Ordering::SeqCst);
            if self.is_done() {
                return true;
            }
            if or_parked && self.is_runnable() {
                return false;
            }
            super::scope::waits("a job another thread is running");
            // An unpark that raced ahead of this park is kept by the thread,
            // so the park returns at once. Whoever woke this thread took its
            // entry off the list, so it goes back on before the next park.
            thread::park();
        }
    }

    /// Publish `Done` and tell everyone waiting: one `JobDone` per queue that
    /// registered, each direct waiter on this thread, and every blocked
    /// thread.
    fn land(self: &Arc<Self>) {
        for queue in self.flight.land() {
            // A queue dropped since it registered refuses the message; its
            // drop told its own waiters.
            let _ = queue.deliver(Message::JobDone(Arc::clone(self)));
        }
        for waiter in self.direct.take(SHUT) {
            waiter.deliver();
        }
        for waker in self.watchers.take(SHUT) {
            waker.wake();
        }
        for sleeper in self.sleepers.take(SHUT) {
            sleeper.unpark();
        }
    }
}

/// Wakes a parked job: pushes it back to the queue that stepped it and to
/// every other queue registered on it, which wakes each owner that is idle
/// ([`Job::ready`]).
struct Unpark {
    /// Weak, since the job's body holds this waker while it is parked.
    job: Weak<Job>,
    queue: Weak<QueueShared>,
    /// Each step gets its own waker and one push brings the next step, so a
    /// waker pushes once: what a parked job leaves in an inbox nobody takes
    /// is one message, however often it is woken.
    fired: crate::portability::AtomicBool,
}

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if self.fired.swap(true, crate::portability::Ordering::AcqRel) {
            return;
        }
        if let Some(job) = self.job.upgrade() {
            job.ready(self.queue.upgrade().as_ref());
        }
    }
}

/// Tells the queues a parked job may go on, once the thread that still held
/// its claim when the wake came has let go ([`Job::ready`]).
struct Later {
    job: Weak<Job>,
    stepper: Option<Weak<QueueShared>>,
}

impl Wake for Later {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if let Some(job) = self.job.upgrade() {
            let stepper = self.stepper.as_ref().and_then(Weak::upgrade);
            job.ready(stepper.as_ref());
        }
    }
}

fn later_waker(job: &Arc<Job>, stepper: Option<&Arc<QueueShared>>) -> Waker {
    Waker::from(Arc::new(Later {
        job: Arc::downgrade(job),
        stepper: stepper.map(Arc::downgrade),
    }))
}

/// The waker `queue`'s poll steps `job` with.
pub(crate) fn unpark_waker(job: &Arc<Job>, queue: &Arc<QueueShared>) -> Waker {
    Waker::from(Arc::new(Unpark {
        job: Arc::downgrade(job),
        queue: Arc::downgrade(queue),
        fired: crate::portability::AtomicBool::new(false),
    }))
}

/// Wakes a job stepped inside another job's step: the queues registered on
/// it are told it may go on, and the step it ran in is woken through
/// `outer`, since that step parked with it.
struct Nested {
    job: Weak<Job>,
    outer: Waker,
    /// One wake brings the next step, as for [`Unpark`].
    fired: crate::portability::AtomicBool,
}

impl Wake for Nested {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if self.fired.swap(true, crate::portability::Ordering::AcqRel) {
            return;
        }
        if let Some(job) = self.job.upgrade() {
            job.ready(None);
        }
        self.outer.wake_by_ref();
    }
}

/// The waker `job` is stepped with when the step of another job, whose
/// waker is `outer`, claimed it on its way.
pub(crate) fn nested_waker(job: &Arc<Job>, outer: &Waker) -> Waker {
    Waker::from(Arc::new(Nested {
        job: Arc::downgrade(job),
        outer: outer.clone(),
        fired: crate::portability::AtomicBool::new(false),
    }))
}

/// What a thread sleeps on while it blocks for a job and steps its own
/// queue's jobs meanwhile ([`IoRuntime::wait_landed`](super::IoRuntime::wait_landed)):
/// stirred by the landing of the job it waits for, and by the end of the
/// wait of a job it stepped.
pub(crate) struct Stir {
    thread: Thread,
    stirred: crate::portability::AtomicBool,
}

impl Stir {
    /// For the calling thread.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            thread: thread::current(),
            stirred: crate::portability::AtomicBool::new(false),
        })
    }

    /// Park the thread this was made on until it is stirred, and take the
    /// stir. A stir that came before the call is taken at once.
    pub(crate) fn sleep(&self) {
        while !self
            .stirred
            .swap(false, crate::portability::Ordering::AcqRel)
        {
            super::scope::waits("a job this call blocks for");
            // A stir sets the flag before it unparks, and an unpark that
            // raced ahead of this park is kept by the thread, so the park
            // returns and the flag is read again.
            thread::park();
        }
    }

    fn stir(&self) {
        self.stirred
            .store(true, crate::portability::Ordering::Release);
        self.thread.unpark();
    }
}

impl Wake for Stir {
    fn wake(self: Arc<Self>) {
        self.stir();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.stir();
    }
}

impl Delivery for Stir {
    fn deliver(&self) {
        self.stir();
    }
}

#[cfg(all(test, not(loom)))]
#[path = "job_park_tests.rs"]
mod park_tests;

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Parks until `open` is set, counting its steps; then runs.
    struct Gated {
        open: Arc<std::sync::atomic::AtomicBool>,
        steps: Arc<AtomicUsize>,
        runs: Arc<AtomicUsize>,
        releases: Arc<AtomicUsize>,
    }

    impl JobBody for Gated {
        fn run(self: Box<Self>) {
            self.runs.fetch_add(1, Ordering::SeqCst);
        }
        fn step(self: Box<Self>, _waker: &Waker) -> Step {
            self.steps.fetch_add(1, Ordering::SeqCst);
            if self.open.load(Ordering::SeqCst) {
                self.run();
                Step::Done
            } else {
                Step::Parked(self)
            }
        }
        fn release(self: Box<Self>) {
            self.releases.fetch_add(1, Ordering::SeqCst);
        }
    }

    type GatedParts = (
        Arc<Job>,
        Arc<std::sync::atomic::AtomicBool>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
    );

    fn gated() -> GatedParts {
        let open = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (steps, runs, releases) = (
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );
        let job = Job::new(Box::new(Gated {
            open: Arc::clone(&open),
            steps: Arc::clone(&steps),
            runs: Arc::clone(&runs),
            releases: Arc::clone(&releases),
        }));
        (job, open, steps, runs, releases)
    }

    #[test]
    fn a_parked_job_gives_its_claim_back_and_runs_once_when_stepped_again() {
        let (job, open, steps, runs, _) = gated();
        assert!(job.claim());
        assert!(!job.step(Waker::noop()), "it parks");
        assert!(!job.is_done());
        assert!(job.is_runnable(), "the claim is free again");
        assert!(job.claim());
        assert!(!job.step(Waker::noop()), "still gated");
        open.store(true, Ordering::SeqCst);
        assert!(job.claim());
        assert!(job.step(Waker::noop()), "it runs and lands");
        assert!(job.is_done());
        assert!(!job.claim(), "a landed job is not claimed again");
        assert_eq!(steps.load(Ordering::SeqCst), 3);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_parked_job_is_settled_by_a_release_or_run_by_a_blocking_claim() {
        let (job, _, _, runs, releases) = gated();
        assert!(job.claim());
        assert!(!job.step(Waker::noop()));
        assert!(job.release(), "close settles a parked job");
        assert_eq!(
            (runs.load(Ordering::SeqCst), releases.load(Ordering::SeqCst)),
            (0, 1)
        );

        let (job, _, _, runs, releases) = gated();
        assert!(job.claim());
        assert!(!job.step(Waker::noop()));
        assert!(job.claim(), "a blocking caller claims it");
        job.run();
        assert!(job.is_done());
        assert_eq!(
            (runs.load(Ordering::SeqCst), releases.load(Ordering::SeqCst)),
            (1, 0)
        );
    }

    #[test]
    fn the_unpark_waker_pushes_the_job_back_to_its_queue() {
        let (job, _, _, _, _) = gated();
        let queue = Arc::new(QueueShared::new(super::super::shared::test_id(), 1));
        let waker = unpark_waker(&job, &queue);
        waker.wake_by_ref();
        let pushed: Vec<_> = queue.take_inbox().collect();
        assert!(
            matches!(&pushed[..], [Message::Ready(ready)] if Arc::ptr_eq(ready, &job)),
            "one Ready naming the job"
        );
        // One push per waker, however often it is woken.
        waker.wake_by_ref();
        assert_eq!(queue.take_inbox().count(), 0);
        // A job that is gone is woken to no effect.
        let fresh = unpark_waker(&job, &queue);
        drop((pushed, job));
        fresh.wake_by_ref();
        assert_eq!(queue.take_inbox().count(), 0);
    }

    struct Count(Arc<AtomicUsize>, Arc<AtomicUsize>);

    impl JobBody for Count {
        fn run(self: Box<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        fn release(self: Box<Self>) {
            self.1.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct Told(AtomicUsize);

    impl Delivery for Told {
        fn deliver(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn counted() -> (Arc<Job>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let runs = Arc::new(AtomicUsize::new(0));
        let releases = Arc::new(AtomicUsize::new(0));
        let job = Job::new(Box::new(Count(Arc::clone(&runs), Arc::clone(&releases))));
        (job, runs, releases)
    }

    #[test]
    fn one_claim_runs_the_body_once_and_tells_every_direct_waiter() {
        let (job, runs, releases) = counted();
        let told = Arc::new(Told(AtomicUsize::new(0)));
        assert!(job.listen(Arc::clone(&told) as Arc<dyn Delivery>));
        assert!(job.claim());
        assert!(!job.claim(), "a claimed job is not claimed again");
        job.run();
        assert!(job.is_done());
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(releases.load(Ordering::SeqCst), 0);
        assert_eq!(told.0.load(Ordering::SeqCst), 1);
        assert!(
            !job.listen(told as Arc<dyn Delivery>),
            "a landed job refuses"
        );
        assert!(!job.release(), "a landed job is not released");
    }

    #[test]
    fn a_release_settles_a_free_job_without_running_it() {
        let (job, runs, releases) = counted();
        assert!(job.release());
        assert!(job.is_done());
        assert_eq!(runs.load(Ordering::SeqCst), 0);
        assert_eq!(releases.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_passive_job_is_never_runnable_and_lands_only_by_release() {
        let job = Job::passive();
        assert!(!job.is_runnable());
        assert!(!job.claim());
        assert!(!job.is_done());
        assert!(job.release());
        assert!(job.is_done());
    }

    #[test]
    fn a_blocked_thread_wakes_when_the_job_lands() {
        let (job, _, _) = counted();
        std::thread::scope(|scope| {
            let waiting = Arc::clone(&job);
            let blocked = scope.spawn(move || waiting.wait_landed());
            assert!(job.claim());
            job.run();
            blocked.join().unwrap();
        });
        // Landed already: returns at once.
        job.wait_landed();
    }

    #[test]
    fn a_panicking_body_still_lands_the_job() {
        struct Boom;
        impl JobBody for Boom {
            fn run(self: Box<Self>) {
                panic!("the body panics");
            }
            fn release(self: Box<Self>) {}
        }
        let job = Job::new(Box::new(Boom));
        assert!(job.claim());
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job.run()));
        assert!(caught.is_err());
        assert!(job.is_done());
    }
}
