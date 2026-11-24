//! The claim word and waiter list every single-flight unit shares.
//!
//! A block read (`unit.rs`) and a job (`job.rs`) are both run once however
//! many queues wait on them. Both move through the same states:
//!
//! ```text
//! Free --claim (one CAS)--> Claimed --land--> Done
//!   ^   ^                      |  |
//!   |   +--------- park -------+  +--park_closed (a block read whose
//!   |                                 reopen found no open-file slot, D60)
//!   +--unpark (one CAS)-- Parked <----+
//! ```
//!
//! The claim is the only way to `Claimed`, so the work runs on one thread
//! at a time and lands at most once. `land` publishes `Done` and then closes
//! the list of waiting queues with one swap, handing the caller every queue
//! that registered. A queue that registers after that swap is refused and
//! reads the outcome itself. So every queue that registered is handed to the
//! lander exactly once, and none is lost.
//!
//! A claim holder that cannot go on without waiting for another thread
//! gives the claim up without landing, in one of two ways:
//!
//! - **A job parks open** ([`Flight::park`]): back to `Free`, before any
//!   of its gated work ran, so whoever is to go on with it claims it again:
//!   its owner's queue once the job's waker says it may go on, or a blocking
//!   caller that chose to wait for it on its own thread (`job.rs`). A job
//!   several callers wait on tells the ones that saw it claimed when it
//!   parks, and every queue registered on it when its wait ends, so none of
//!   them is left waiting on the thread that parked it.
//! - **A block read parks closed** ([`Flight::park_closed`]): to `Parked`,
//!   which no claim takes. Every queue that registered on the read would
//!   otherwise claim it at its next poll and find the same slots busy. It
//!   is neither run nor landed until it is unparked (a slot freed) or
//!   released (close, a dropped queue).
//!
//! Either way the unit is still in flight, so queues go on registering on
//! it, and a parked unit ran nothing, so the work still runs at most once.

use std::sync::Arc;

use super::shared::QueueShared;
use super::stack::{SHUT, Stack, Taken};
use crate::sync::internal::{AtomicUsize, Ordering};

const FREE: usize = 0;
const CLAIMED: usize = 1;
const DONE: usize = 2;
const PARKED: usize = 3;

pub(crate) struct Flight {
    state: AtomicUsize,
    /// The queues waiting on this unit, closed with [`SHUT`] by `land`.
    waiters: Stack<Arc<QueueShared>>,
}

impl Flight {
    pub(crate) fn new() -> Self {
        Self {
            state: AtomicUsize::new(FREE),
            waiters: Stack::new(),
        }
    }

    pub(crate) fn is_done(&self) -> bool {
        self.state.load(Ordering::Acquire) == DONE
    }

    /// Nobody has claimed the unit yet.
    pub(crate) fn is_free(&self) -> bool {
        self.state.load(Ordering::Acquire) == FREE
    }

    /// Whether the unit is parked.
    #[cfg(test)]
    pub(crate) fn is_parked(&self) -> bool {
        self.state.load(Ordering::Acquire) == PARKED
    }

    /// Take the unit for this thread with one compare-and-swap. `true` for
    /// one caller at a time (exactly one over the unit's life unless its
    /// holder parks it); that caller must then land or park it.
    pub(crate) fn claim(&self) -> bool {
        self.state
            .compare_exchange(FREE, CLAIMED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Hand the claim back without landing: the unit goes on later, under a
    /// new claim. Only by the claim holder, after it put back whatever it
    /// took from the unit: the `Release` store orders that before the next
    /// claim's `Acquire`.
    pub(crate) fn park(&self) {
        debug_assert_eq!(self.state.load(Ordering::Relaxed), CLAIMED);
        self.state.store(FREE, Ordering::Release);
    }

    /// Take the unit to close it without running it, whether nobody has
    /// claimed it or its holder parked it. `false` when a thread holds it
    /// claimed or it has landed.
    pub(crate) fn claim_to_release(&self) -> bool {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if state != FREE && state != PARKED {
                return false;
            }
            match self
                .state
                .compare_exchange(state, CLAIMED, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(now) => state = now,
            }
        }
    }

    /// Give the claim up without landing and close the unit to claims: it
    /// is neither run nor landed until [`unpark`](Self::unpark) or
    /// [`claim_to_release`](Self::claim_to_release). Only the claim holder
    /// writes a claimed unit's state, so a store suffices; `Release` hands
    /// what the holder left behind to whoever claims it next.
    pub(crate) fn park_closed(&self) {
        debug_assert_eq!(self.state.load(Ordering::Relaxed), CLAIMED);
        self.state.store(PARKED, Ordering::Release);
    }

    /// Make a parked unit claimable again. `true` for the one call that
    /// did; `false` when it was not parked (unparked already, released, or
    /// landed).
    pub(crate) fn unpark(&self) -> bool {
        self.state
            .compare_exchange(PARKED, FREE, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Record `queue` as waiting. `false` when the unit already landed: the
    /// caller then reads the outcome itself.
    pub(crate) fn register(&self, queue: Arc<QueueShared>) -> bool {
        self.waiters.push(queue, SHUT).is_ok()
    }

    /// Take the queues registered so far out of the list, which stays as
    /// open or as closed as it was. The caller registers each of them again
    /// once it has told it what it has to, and tells a queue the list then
    /// refuses that the unit landed: the lander did not see that queue.
    pub(crate) fn registered(&self) -> Taken<Arc<QueueShared>> {
        self.waiters.take_open()
    }

    /// Publish `Done` and close the waiter list, returning every queue that
    /// registered. Only by the claim holder, once, after it wrote the
    /// outcome: the `Release` store orders that write before `Done`.
    pub(crate) fn land(&self) -> Taken<Arc<QueueShared>> {
        debug_assert_eq!(self.state.load(Ordering::Relaxed), CLAIMED);
        self.state.store(DONE, Ordering::Release);
        self.waiters.take(SHUT)
    }
}
