//! One waker slot that one party registers into and any thread wakes.
//!
//! The per-thread queues keep two: the owner's idle waker, which a thread
//! that pushes to the idle inbox wakes, and each pending read's waker, which
//! the owner's `poll` wakes. Registering and waking can race, so the slot is
//! guarded by a three-state word rather than a lock:
//!
//! | state | who holds the slot |
//! |---|---|
//! | `WAITING` | nobody; a waker may be stored |
//! | `REGISTERING` | the registrar, replacing the waker |
//! | `WAKING` | a waker, taking the waker out |
//!
//! A wake that lands while a registration is in progress sets `WAKING` on top
//! of `REGISTERING`; the registrar sees it when it gives the slot back and
//! wakes the waker it just stored. So a wake is never lost to a concurrent
//! registration, and nobody waits for anybody. Only one party registers at a
//! time: the owner for the idle waker, the one `IoWait` that owns a read's
//! slot for that one.

#![allow(unsafe_code)]

use core::task::Waker;

use crate::sync::internal::{AtomicUsize, Ordering, UnsafeCell};

const WAITING: usize = 0;
const REGISTERING: usize = 0b01;
const WAKING: usize = 0b10;

pub(crate) struct AtomicWaker {
    state: AtomicUsize,
    waker: UnsafeCell<Option<Waker>>,
}

// SAFETY: the state word gives the slot to one party at a time (see the
// module documentation), and a `Waker` is `Send + Sync`.
unsafe impl Send for AtomicWaker {}
// SAFETY: as above.
unsafe impl Sync for AtomicWaker {}

impl AtomicWaker {
    pub(crate) fn new() -> Self {
        Self {
            state: AtomicUsize::new(WAITING),
            waker: UnsafeCell::new(None),
        }
    }

    /// Store `waker` as the one to wake next, replacing any earlier one. If
    /// a wake raced this call, `waker` is woken before it returns.
    pub(crate) fn register(&self, waker: &Waker) {
        match self
            .state
            .compare_exchange(WAITING, REGISTERING, Ordering::Acquire, Ordering::Acquire)
            .unwrap_or_else(|actual| actual)
        {
            WAITING => {
                // SAFETY: `REGISTERING` gives this call the slot.
                self.waker.with_mut(|slot| unsafe {
                    match &*slot {
                        Some(old) if old.will_wake(waker) => {}
                        _ => *slot = Some(waker.clone()),
                    }
                });
                // AcqRel: Release publishes the stored waker to the next
                // waker-taker; Acquire sees a wake that arrived meanwhile.
                if self
                    .state
                    .compare_exchange(REGISTERING, WAITING, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    // A wake came in while the slot was held: it left the
                    // waking to this call.
                    // SAFETY: the state is still `REGISTERING | WAKING`, so
                    // the slot is still this call's.
                    let woken = self.waker.with_mut(|slot| unsafe { (*slot).take() });
                    self.state.swap(WAITING, Ordering::AcqRel);
                    if let Some(woken) = woken {
                        woken.wake();
                    }
                }
            }
            // A wake is taking the old waker out right now: it would wake the
            // old one, so wake the new one too.
            WAKING => waker.wake_by_ref(),
            // Only one party registers into a slot, so a registration never
            // meets another; if it did, the other one's waker stays.
            _ => {}
        }
    }

    /// Wake the stored waker, if there is one, and empty the slot.
    pub(crate) fn wake(&self) {
        if let Some(waker) = self.take() {
            waker.wake();
        }
    }

    /// Take the stored waker out. `None` when there is none, or when a
    /// registration is in progress, which then wakes it itself.
    fn take(&self) -> Option<Waker> {
        match self.state.fetch_or(WAKING, Ordering::AcqRel) {
            WAITING => {
                // SAFETY: moving the state from `WAITING` to `WAKING` gives
                // this call the slot.
                let waker = self.waker.with_mut(|slot| unsafe { (*slot).take() });
                self.state.fetch_and(!WAKING, Ordering::Release);
                waker
            }
            _ => None,
        }
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize as Count, Ordering as CountOrdering};
    use std::task::Wake;

    struct Counter(Count);

    impl Wake for Counter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, CountOrdering::SeqCst);
        }
    }

    fn counting() -> (Arc<Counter>, Waker) {
        let counter = Arc::new(Counter(Count::new(0)));
        (Arc::clone(&counter), Waker::from(counter))
    }

    #[test]
    fn a_wake_reaches_the_last_registered_waker_once() {
        let slot = AtomicWaker::new();
        let (first, first_waker) = counting();
        let (second, second_waker) = counting();
        slot.register(&first_waker);
        slot.register(&second_waker);
        slot.wake();
        slot.wake();
        assert_eq!(first.0.load(CountOrdering::SeqCst), 0);
        assert_eq!(second.0.load(CountOrdering::SeqCst), 1);
    }

    #[test]
    fn a_wake_with_nothing_registered_does_nothing() {
        let slot = AtomicWaker::new();
        slot.wake();
        let (counter, waker) = counting();
        slot.register(&waker);
        assert_eq!(counter.0.load(CountOrdering::SeqCst), 0);
        slot.wake();
        assert_eq!(counter.0.load(CountOrdering::SeqCst), 1);
    }
}
