//! The jobs parked until something another thread holds is given up: the
//! version store's step, the flush exclusion.
//!
//! A job a queue's poll steps never waits for another thread
//! (`engine::commit::park`). When it finds what it needs held, it leaves
//! its waker here and parks; whoever gives the thing up wakes every waker
//! left. The two sides race, so each looks at the other's word after
//! writing its own:
//!
//! ```text
//! the job:     register its waker;  fence;  try to take the thing again
//! the holder:  give the thing up;   fence;  take the wakers and wake them
//! ```
//!
//! # Invariant
//!
//! - **PK1, a job that parks is woken.** One of the two fences comes first.
//!   If it is the job's, the holder's take, after its own fence, sees the
//!   waker. If it is the holder's, the job's second try, after its own
//!   fence, sees the thing free. So a job either takes the thing or is
//!   woken after it was given up, and never parks on a thing nobody holds.

use std::task::Waker;

use super::stack::Stack;
use crate::sync::internal::{Ordering, fence};

/// See the module docs.
pub(crate) struct Parked {
    wakers: Stack<Waker>,
}

impl Parked {
    pub(crate) fn new() -> Self {
        Self {
            wakers: Stack::new(),
        }
    }

    /// Take the thing with `take`, which never waits, for a job that may
    /// not wait either. `None` when another thread holds it: `waker` is
    /// then woken once that thread gave it up (PK1), and the job parks.
    pub(crate) fn take_or_park<T>(&self, waker: &Waker, take: impl Fn() -> Option<T>) -> Option<T> {
        if let Some(taken) = take() {
            return Some(taken);
        }
        // No flag refuses a push here.
        let _ = self.wakers.push(waker.clone(), 0);
        fence(Ordering::SeqCst);
        // A waker left behind by a second try that succeeds is woken at the
        // next release, to no effect: its job has gone on.
        take()
    }

    /// The thing was given up: wake every job parked for it. Called after
    /// the release, by the thread that made it.
    pub(crate) fn released(&self) {
        fence(Ordering::SeqCst);
        for waker in self.wakers.take(0) {
            waker.wake();
        }
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;

    struct Count(AtomicUsize);

    impl Wake for Count {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_free_thing_is_taken_and_nobody_is_registered() {
        let parked = Parked::new();
        let count = Arc::new(Count(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&count));
        assert_eq!(parked.take_or_park(&waker, || Some(7)), Some(7));
        parked.released();
        assert_eq!(count.0.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_held_thing_parks_the_job_until_it_is_released() {
        let parked = Parked::new();
        let count = Arc::new(Count(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&count));
        assert_eq!(parked.take_or_park(&waker, || None::<()>), None);
        assert_eq!(count.0.load(Ordering::SeqCst), 0, "not before the release");
        parked.released();
        assert_eq!(count.0.load(Ordering::SeqCst), 1);
        parked.released();
        assert_eq!(count.0.load(Ordering::SeqCst), 1, "woken once");
    }

    #[test]
    fn a_thing_given_up_between_the_two_tries_is_taken_by_the_second() {
        let parked = Parked::new();
        let count = Arc::new(Count(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&count));
        let tries = Cell::new(0);
        let taken = parked.take_or_park(&waker, || {
            tries.set(tries.get() + 1);
            (tries.get() == 2).then_some(())
        });
        assert_eq!(taken, Some(()));
        assert_eq!(tries.get(), 2);
    }
}
