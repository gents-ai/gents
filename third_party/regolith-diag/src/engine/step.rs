//! A step: work that runs one section at a time, on whichever thread holds
//! the step, in place of a mutex (plan 4.6, 4.7).
//!
//! A caller hands [`Step::run`] a *section*, a closure. Sections never
//! overlap and run in the order they were handed over, as critical sections
//! of one mutex would. What differs is who runs them and what a caller
//! waits for:
//!
//! - **The step is taken, not waited for.** A section is pushed on a
//!   lock-free list. The push that finds nobody holding the step takes it,
//!   with the same compare-and-swap that pushed, and runs every section
//!   queued, its own and other threads', oldest first, until the list is
//!   empty. So a section never waits for its own thread to be scheduled:
//!   whichever thread holds the step runs it.
//! - **A caller waits only for the work under way.** A caller whose push
//!   found the step held parks until its section has run, on the thread
//!   that holds the step, which is doing the sections queued before it.
//! - **A caller that may not wait holds the step instead.** A job a queue's
//!   poll steps never waits for another thread, so before it starts work
//!   that hands sections over it takes the step for the whole of that work
//!   ([`Step::hold_or_park`]), with the same compare-and-swap a push takes
//!   it with, and parks when another thread has it. While it keeps the
//!   [`Hold`], every section handed over, its own and other threads', runs
//!   on its thread: at once when it hands one over, and when the hold ends.
//!
//! # Invariants
//!
//! - **ST1, one thread runs sections**, one section at a time: the thread
//!   that holds the step.
//! - **ST2, the step is held exactly while a thread runs sections or keeps
//!   a [`Hold`].** The list's head word carries `IDLE` when it is empty and
//!   nobody holds the step. The push that clears `IDLE`, or the hold that
//!   does, holds the step from then on; the holder gives it up with one
//!   compare-and-swap that succeeds only on an empty list. So a section
//!   pushed at any moment is either taken by the holder's next sweep or is
//!   the push that takes the step: none is left behind with nobody to run
//!   it.
//! - **ST3, sections run in the order they were pushed**, each to its end
//!   before the next begins.
//! - **ST4, a section has finished before its caller returns.** The caller
//!   parks until its section's own job lands, which the running thread does
//!   after the section returned or unwound. That is what lets a section
//!   borrow from its caller's stack though another thread may run it.
//! - **ST5, a panic in a section is its caller's.** It is caught on the
//!   thread that ran the section and resumed on the thread that asked for
//!   it, so one caller's panic never unwinds the thread serving the rest.
//! - **ST6, a hold never waits, and nobody is left parked for one.** Taking
//!   a hold is one compare-and-swap. A section the holder hands over runs
//!   on the holder's thread before the call returns. A job that parked for
//!   the step is woken by the thread that next gives the step up
//!   (`io::parked`, PK1).

#![allow(unsafe_code)]

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::task::Waker;

use super::io::job::Job;
use super::io::parked::Parked;
use super::io::shared::thread_token;
use super::io::stack::{IDLE, Rest, Stack};
use super::take::Take;
use crate::sync::internal::{AtomicUsize, Ordering};

/// One section waiting for the step.
struct Request {
    /// Runs the section and keeps what it returned. Taken by the thread
    /// that runs it.
    body: Option<Box<dyn FnOnce() + Send>>,
    /// Landed once the section has run: what its caller parks on (ST4).
    done: Arc<Job>,
}

impl Drop for Request {
    fn drop(&mut self) {
        // ST4: after the body ran, or with a list dropped unrun.
        self.done.release();
    }
}

/// See the module docs.
pub(crate) struct Step {
    /// The sections waiting for the step, and whether it is held (ST2).
    waiting: Stack<Request>,
    /// The thread that keeps a [`Hold`], as its thread token; zero when no
    /// hold is out. A thread only ever compares it with its own token.
    holder: AtomicUsize,
    /// The jobs that found the step held and parked, woken when the step
    /// is next given up (ST6).
    parked: Parked,
}

/// The step, kept by a caller that may not wait for it. See the module docs.
/// Dropping it runs the sections queued meanwhile and gives the step up.
#[must_use = "the step is held only while the hold is alive"]
pub(crate) struct Hold<'a> {
    step: &'a Step,
}

impl Drop for Hold<'_> {
    fn drop(&mut self) {
        self.step.holder.store(0, Ordering::Release);
        self.step.sweep();
    }
}

impl Step {
    pub(crate) fn new() -> Self {
        let waiting = Stack::new();
        // Nobody holds the step.
        let rested = waiting.rest_if_empty();
        debug_assert_eq!(rested, Rest::Set);
        Self {
            waiting,
            holder: AtomicUsize::new(0),
            parked: Parked::new(),
        }
    }

    /// Take the step and keep it, if nobody holds it. Never waits.
    fn try_hold(&self) -> Option<Hold<'_>> {
        // ST2: clearing `IDLE` on the empty list takes the step, as the
        // push that finds `IDLE` does.
        if !self.waiting.wake_if_resting() {
            return None;
        }
        self.holder.store(thread_token(), Ordering::Release);
        Some(Hold { step: self })
    }

    /// [`Self::try_hold`] for a job a queue's poll steps: when another
    /// thread holds the step, `waker` is woken once that thread gives it
    /// up, and `None` tells the job to park (ST6).
    pub(crate) fn hold_or_park(&self, waker: &Waker) -> Option<Hold<'_>> {
        self.parked.take_or_park(waker, || self.try_hold())
    }

    /// Run `section` alone and after every section pushed before it, and
    /// return what it returns.
    ///
    /// The section may run on another thread, the one that holds the step,
    /// which is why it is `Send`. It has finished when this returns (ST4).
    pub(crate) fn run<T: Send>(&self, section: impl FnOnce() -> T + Send) -> T {
        let out: Take<std::thread::Result<T>> = Take::empty();
        let done = Job::passive();
        {
            let out = &out;
            let body: Box<dyn FnOnce() + Send + '_> = Box::new(move || {
                // ST5: the panic is this section's caller's.
                out.put(catch_unwind(AssertUnwindSafe(section)));
            });
            // SAFETY (ST4): the lifetime of what `section` and `out` borrow
            // is erased so the body can wait on the list. This call does
            // not return before `done` has landed, and the thread that
            // lands it has by then run the body to its end, or dropped it
            // with the list. So the body never outlives its borrows.
            let body: Box<dyn FnOnce() + Send> = unsafe { std::mem::transmute(body) };
            let request = Request {
                body: Some(body),
                done: Arc::clone(&done),
            };
            // No flag refuses a push here, so `Err` cannot come back.
            if let Ok(flags) = self.waiting.push(request, 0) {
                if flags & IDLE != 0 {
                    // ST2: this push took the step.
                    self.sweep();
                } else if self.holder.load(Ordering::Acquire) == thread_token() {
                    // This thread keeps the step (ST6): it runs what is
                    // queued, this section last.
                    self.drain();
                }
            }
        }
        done.wait_landed();
        match out.take() {
            Some(Ok(value)) => value,
            Some(Err(panic)) => resume_unwind(panic),
            None => unreachable!("a section's job landed before the section ran"),
        }
    }

    /// Run every queued section, as the thread that holds the step, and
    /// give the step up once the list is empty (ST2, ST3). The jobs that
    /// parked for the step are woken then (ST6).
    fn sweep(&self) {
        loop {
            self.drain();
            if self.waiting.rest_if_empty() != Rest::Busy {
                break;
            }
        }
        self.parked.released();
    }

    /// Run the sections queued now, oldest first, as the thread that holds
    /// the step, and keep it.
    fn drain(&self) {
        for mut request in self.waiting.take(0) {
            if let Some(body) = request.body.take() {
                body();
            }
            // Dropped here: its caller goes on.
        }
    }

    /// Whether this thread keeps a [`Hold`]: a section it hands over then
    /// runs before the call returns, and waits for nobody (ST6).
    pub(crate) fn held_here(&self) -> bool {
        self.holder.load(Ordering::Acquire) == thread_token()
    }

    /// Test-only: whether a section is waiting on the list.
    #[cfg(test)]
    pub(crate) fn has_waiting(&self) -> bool {
        !self.waiting.is_empty()
    }
}
