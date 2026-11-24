//! The shared half of a ticket: an outcome decided once, then delivered
//! once, on the thread that owns the ticket's queue.
//!
//! A ticket ([`CommitTicket`](crate::CommitTicket),
//! [`JobTicket`](crate::JobTicket)) moves through four states:
//!
//! ```text
//! Pending --decide--> Decided --deliver--> Delivering --> Ready
//! ```
//!
//! - **Decide** stores the outcome. Whoever learns it does this, on any
//!   thread: a worker that ran a foreground job, or, for a commit that
//!   was still owed its fsync, the delivering thread itself, which resolves
//!   the outcome from the commit's slot the moment it is told the commit
//!   landed.
//! - **Deliver** is the one step that makes the outcome visible to the
//!   ticket's holder. It runs on the thread that owns the ticket's queue, at
//!   the `poll` that takes the completion in (or on the deciding thread, for
//!   a ticket with no queue). One compare-and-swap from `Decided` to
//!   `Delivering` makes it run once however many parties reach for it. It
//!   runs, in order, what the outcome owes first (a commit's own `on_commit`
//!   or `on_abort` callbacks, then the database's hooks), then every
//!   `on_complete` callback, then marks the ticket ready and wakes the task
//!   awaiting it and every thread blocked on it.
//!
//! **Ready** is the `on_complete` list closed. Delivery takes the list, runs
//! what it took, and takes it again until it finds it empty, and closes it
//! in the same compare-and-swap that finds it so. An `on_complete` registered
//! while delivery is under way therefore lands on the list and runs on the
//! delivering thread before the ticket is ready, as one registered before
//! does; one the closed list refuses finds the ticket ready, and runs at
//! once on the registering thread. Each callback runs exactly once, and the
//! registering thread runs one only when the ticket is ready. Nothing here
//! takes a lock.

use std::sync::{Arc, OnceLock, Weak};
use std::task::{Context, Poll};
use std::thread::{self, Thread};

use crate::engine::RegolithEngine;
use crate::engine::io::atomic_waker::AtomicWaker;
use crate::engine::io::job::Delivery;
use crate::engine::io::stack::{SHUT, Stack};
use crate::sync::internal::{AtomicUsize, Ordering};

const PENDING: usize = 0;
const DECIDED: usize = 1;
/// Delivery began. Ready is not a state of its own: it is the `on_complete`
/// list closed with [`SHUT`] (module docs).
const DELIVERING: usize = 2;

/// A callback that runs once with the outcome.
pub(crate) type Callback<T> = Box<dyn FnOnce(&T) + Send>;

/// What computes an outcome once the work it waits on has landed.
pub(crate) type Resolve<T> = Box<dyn FnOnce() -> T + Send>;

pub(crate) struct Completion<T> {
    outcome: OnceLock<T>,
    state: AtomicUsize,
    /// The task awaiting the ticket.
    waker: AtomicWaker,
    /// Computes the outcome at delivery, for a completion not decided when it
    /// was handed out. Taken whole, once, with [`SHUT`].
    resolve: Stack<Resolve<T>>,
    /// What the outcome owes before anything else: a commit's own callbacks
    /// and the database hooks. Closed with [`SHUT`] at delivery.
    first: Stack<Callback<T>>,
    /// The ticket's `on_complete` callbacks, closed with [`SHUT`] at the end
    /// of delivery: closed is ready.
    on_complete: Stack<Callback<T>>,
    /// Threads blocked in [`Completion::wait_decided`], closed with [`SHUT`]
    /// once the outcome is decided.
    sleepers: Stack<Thread>,
    /// Told when an `on_complete` callback panics.
    report: Weak<RegolithEngine>,
    /// What a panic in an `on_complete` callback is reported as.
    name: &'static str,
}

impl<T: Send + Sync + 'static> Completion<T> {
    /// A pending completion whose `on_complete` panics are reported to
    /// `report` under `name`.
    pub(crate) fn new(report: Weak<RegolithEngine>, name: &'static str) -> Arc<Self> {
        Arc::new(Self {
            outcome: OnceLock::new(),
            state: AtomicUsize::new(PENDING),
            waker: AtomicWaker::new(),
            resolve: Stack::new(),
            first: Stack::new(),
            on_complete: Stack::new(),
            sleepers: Stack::new(),
            report,
            name,
        })
    }

    /// The database a callback's panic is reported to.
    pub(crate) fn report(&self) -> &Weak<RegolithEngine> {
        &self.report
    }

    /// Compute the outcome with `f` at delivery, unless it was decided by
    /// then. Set before the completion is handed out.
    pub(crate) fn resolve_with(&self, f: Resolve<T>) {
        // Refused only after delivery began, which the caller's order rules
        // out; nothing is left to resolve then.
        let _ = self.resolve.push(f, SHUT);
    }

    /// Owe `f` first at delivery, before any `on_complete` callback. Made
    /// before the completion is handed out.
    pub(crate) fn first(&self, f: Callback<T>) {
        // Refused only after delivery began, which the caller's order rules
        // out; run it then rather than drop it.
        if let Err(f) = self.first.push(f, SHUT)
            && let Some(outcome) = self.outcome.get()
        {
            f(outcome);
        }
    }

    /// Store the outcome. `false` (and `outcome` dropped) when one was
    /// already decided: the first decision stands.
    pub(crate) fn decide(&self, outcome: T) -> bool {
        if self.outcome.set(outcome).is_err() {
            return false;
        }
        let decided = self
            .state
            .compare_exchange(PENDING, DECIDED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        for sleeper in self.sleepers.take(SHUT) {
            sleeper.unpark();
        }
        decided
    }

    /// Whether an outcome has been decided.
    fn is_decided(&self) -> bool {
        self.state.load(Ordering::Acquire) != PENDING
    }

    /// Whether the ticket's holder may see the outcome: delivery ran every
    /// callback and closed the list.
    pub(crate) fn is_ready(&self) -> bool {
        self.on_complete.flags() & SHUT != 0
    }

    /// Deliver the decided outcome on this thread: what it owes first, then
    /// each `on_complete` callback, the ones registered while it runs
    /// included, then readiness and the wakes. Runs once; a call before the
    /// decision, or after another delivery began, does nothing.
    pub(crate) fn deliver_decided(&self) {
        if self
            .state
            .compare_exchange(DECIDED, DELIVERING, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let Some(outcome) = self.outcome.get() else {
            return;
        };
        for f in self.first.take(SHUT) {
            f(outcome);
        }
        // A registration racing this lands on the list and is served by the
        // next take; the close that finds the list empty is the step that
        // makes the ticket ready, so a registration it refuses sees it ready.
        loop {
            for f in self.on_complete.take_open() {
                crate::engine::callback::survive(
                    || self.report.upgrade(),
                    self.name,
                    || f(outcome),
                );
            }
            if self.on_complete.shut_if_empty() {
                break;
            }
        }
        self.waker.wake();
    }

    /// Run `f` with the outcome once: at delivery, on the delivering thread,
    /// or now, on this thread, if the ticket is ready.
    pub(crate) fn on_complete(&self, f: Callback<T>) {
        if self.is_ready() {
            self.run_now(f);
        } else if let Err(f) = self.on_complete.push(f, SHUT) {
            // Delivery closed the list after the check: the ticket is ready.
            self.run_now(f);
        }
    }

    fn run_now(&self, f: Callback<T>) {
        if let Some(outcome) = self.outcome.get() {
            crate::engine::callback::survive(|| self.report.upgrade(), self.name, || f(outcome));
        }
    }

    /// The outcome when ready, else register `cx`'s waker to be woken at
    /// delivery.
    pub(crate) fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<&T> {
        if !self.is_ready() {
            self.waker.register(cx.waker());
            // Delivery may have finished between the check and the
            // registration, and then woke no waker or an old one.
            if !self.is_ready() {
                return Poll::Pending;
            }
        }
        match self.outcome.get() {
            Some(outcome) => Poll::Ready(outcome),
            None => Poll::Pending,
        }
    }

    /// Block this thread until the outcome is decided, and return it. Its
    /// delivery (the callbacks, the ticket's readiness) still happens where
    /// it always does.
    pub(crate) fn wait_decided(&self) -> &T {
        if let Some(outcome) = self.decided_outcome() {
            return outcome;
        }
        // A wait for another thread: release what this thread's last read
        // holds back first, and what the views retired meanwhile placed on
        // it once the wait is over (`read_view::idle`).
        crate::engine::read_view::idle();
        let outcome = loop {
            if let Some(outcome) = self.decided_outcome() {
                break outcome;
            }
            // Refused once the decision took the sleepers: decided now.
            if self.sleepers.push(thread::current(), SHUT).is_ok() {
                while !self.is_decided() {
                    thread::park();
                }
            }
        };
        crate::engine::read_view::idle();
        outcome
    }

    fn decided_outcome(&self) -> Option<&T> {
        if self.is_decided() {
            self.outcome.get()
        } else {
            None
        }
    }
}

impl<T: Send + Sync + 'static> Delivery for Completion<T> {
    /// Resolve the outcome if nobody decided it yet, then deliver it.
    fn deliver(&self) {
        if let Some(resolve) = self.resolve.take(SHUT).next() {
            self.decide(resolve());
        }
        self.deliver_decided();
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn completion() -> Arc<Completion<u32>> {
        Completion::new(Weak::new(), "on_complete")
    }

    #[test]
    fn nothing_is_ready_before_delivery_and_everything_runs_once_at_it() {
        let done = completion();
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = Arc::clone(&order);
        done.first(Box::new(move |v| seen.lock().unwrap().push(("first", *v))));
        let seen = Arc::clone(&order);
        done.on_complete(Box::new(move |v| {
            seen.lock().unwrap().push(("complete", *v))
        }));
        assert!(done.decide(7));
        assert!(!done.decide(8), "the first decision stands");
        assert!(!done.is_ready(), "decided is not delivered");
        done.deliver_decided();
        done.deliver_decided();
        assert!(done.is_ready());
        assert_eq!(*done.wait_decided(), 7);
        assert_eq!(*order.lock().unwrap(), vec![("first", 7), ("complete", 7)]);
        // Registered after delivery: runs at once, once.
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&runs);
        done.on_complete(Box::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
        }));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    /// Another thread registers while delivery is under way (the delivering
    /// thread is inside an earlier callback): the callback still runs on the
    /// delivering thread, before the ticket is ready, as the public
    /// `on_complete` contract promises for a ticket not ready yet.
    #[test]
    fn a_callback_registered_during_delivery_runs_on_the_delivering_thread() {
        let done = completion();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (racing, saw) = (Arc::clone(&done), Arc::clone(&seen));
        done.on_complete(Box::new(move |_| {
            std::thread::spawn(move || {
                let ticket = Arc::clone(&racing);
                racing.on_complete(Box::new(move |_| {
                    let ready = ticket.is_ready();
                    saw.lock()
                        .unwrap()
                        .push((std::thread::current().id(), ready));
                }));
            })
            .join()
            .unwrap();
        }));
        assert!(done.decide(1));
        done.deliver_decided();
        assert!(done.is_ready());
        assert_eq!(
            *seen.lock().unwrap(),
            [(std::thread::current().id(), false)],
            "ran on the delivering thread, before the ticket was ready"
        );
    }

    #[test]
    fn delivery_before_a_decision_does_nothing() {
        let done = completion();
        done.deliver_decided();
        assert!(!done.is_ready());
    }

    #[test]
    fn a_blocked_thread_wakes_at_the_decision() {
        let done = completion();
        std::thread::scope(|scope| {
            let waiting = Arc::clone(&done);
            let blocked = scope.spawn(move || *waiting.wait_decided());
            assert!(done.decide(3));
            assert_eq!(blocked.join().unwrap(), 3);
            assert!(!done.is_ready(), "decided is not delivered");
        });
    }

    #[test]
    fn a_panicking_on_complete_does_not_stop_the_others() {
        let done = completion();
        let runs = Arc::new(AtomicUsize::new(0));
        done.on_complete(Box::new(|_| panic!("callback panics")));
        let counted = Arc::clone(&runs);
        done.on_complete(Box::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
        }));
        done.decide(1);
        done.deliver_decided();
        assert!(done.is_ready());
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }
}
