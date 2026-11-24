//! The timed waits of one queue.
//!
//! A timed wait is ready at the first poll of its queue that finds the
//! queue's clock at its due time or later, and never before. Nothing else
//! ends one: no thread is woken for a timed wait and nothing sleeps for it,
//! so the queue tells its owner how long it may idle
//! ([`IoProgress::wake_in`](crate::IoProgress::wake_in)) and the owner polls
//! again by then.
//!
//! Held by the queue's owner alone. A wait is recorded through the queue's
//! inbox, like every other request, so it may be asked for on any thread.
//!
//! `proofs/tla/NonBlocking.tla` checks these rules with the rest of the
//! queue (`TimerNotEarly`, `TimerOnOwnQueue`, `DroppedHoldsNothing`, and
//! `AllDone` for an owner that polls again in time), and
//! `proofs/lean/Regolith/IoQueue.lean` proves them for any clock (Part 6).

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::Arc;
use std::time::Duration;

use crate::engine::io::shared::WaitSlot;

/// One timed wait: ready once the clock reads `due` microseconds or later.
struct Timer {
    due: u64,
    slot: Arc<WaitSlot>,
}

// Ordered by due time alone and reversed, so the heap's greatest is the wait
// that is due first.
impl Ord for Timer {
    fn cmp(&self, other: &Self) -> Ordering {
        other.due.cmp(&self.due)
    }
}

impl PartialOrd for Timer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Timer {
    fn eq(&self, other: &Self) -> bool {
        self.due == other.due
    }
}

impl Eq for Timer {}

/// The timed waits recorded on a queue and not yet due.
#[derive(Default)]
pub(super) struct Timers {
    waits: BinaryHeap<Timer>,
}

impl Timers {
    pub(super) fn is_empty(&self) -> bool {
        self.waits.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.waits.len()
    }

    /// Record a wait that is ready once the clock reads `due` or later.
    pub(super) fn record(&mut self, due: u64, slot: Arc<WaitSlot>) {
        self.waits.push(Timer { due, slot });
    }

    /// Complete every wait that is due at `now`, and say how many that was.
    /// `None` is an environment with no clock: no delay can be measured on
    /// it, so every wait is over.
    pub(super) fn fire(&mut self, now: Option<u64>) -> usize {
        let mut fired = 0;
        while self
            .waits
            .peek()
            .is_some_and(|next| now.is_none_or(|now| next.due <= now))
        {
            if let Some(timer) = self.waits.pop() {
                timer.slot.complete();
                fired += 1;
            }
        }
        fired
    }

    /// How long from `now` until the earliest wait is due: zero when one is
    /// due already or there is no clock to measure on, `None` when no wait
    /// is recorded.
    pub(super) fn wake_in(&self, now: Option<u64>) -> Option<Duration> {
        let next = self.waits.peek()?;
        Some(now.map_or(Duration::ZERO, |now| {
            Duration::from_micros(next.due.saturating_sub(now))
        }))
    }

    /// Complete every wait: the queue is going away, and nothing else would.
    pub(super) fn complete_all(&mut self) {
        for timer in self.waits.drain() {
            timer.slot.complete();
        }
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn slots(count: usize) -> Vec<Arc<WaitSlot>> {
        (0..count).map(|_| Arc::new(WaitSlot::new())).collect()
    }

    #[test]
    fn a_wait_is_ready_at_its_due_time_and_not_before() {
        let mut timers = Timers::default();
        let slot = Arc::new(WaitSlot::new());
        timers.record(100, Arc::clone(&slot));
        assert_eq!(timers.wake_in(Some(40)), Some(Duration::from_micros(60)));
        assert_eq!(timers.fire(Some(99)), 0);
        assert!(!slot.is_ready());
        assert_eq!(timers.fire(Some(100)), 1);
        assert!(slot.is_ready());
        assert!(timers.is_empty());
        assert_eq!(timers.wake_in(Some(100)), None);
    }

    #[test]
    fn with_no_clock_every_wait_is_over_at_once() {
        let mut timers = Timers::default();
        let waiting = slots(3);
        for (due, slot) in [u64::MAX, 7, 0].into_iter().zip(&waiting) {
            timers.record(due, Arc::clone(slot));
        }
        assert_eq!(timers.wake_in(None), Some(Duration::ZERO));
        assert_eq!(timers.fire(None), 3);
        assert!(waiting.iter().all(|slot| slot.is_ready()));
    }

    #[test]
    fn a_queue_going_away_completes_what_it_holds() {
        let mut timers = Timers::default();
        let waiting = slots(2);
        for slot in &waiting {
            timers.record(u64::MAX, Arc::clone(slot));
        }
        timers.complete_all();
        assert!(timers.is_empty());
        assert!(waiting.iter().all(|slot| slot.is_ready()));
    }

    proptest! {
        /// Against the plain rule: after a poll at `now`, exactly the waits
        /// due at or before `now` are ready, and the time to the next one is
        /// the earliest due time left, less `now`.
        #[test]
        fn each_poll_completes_exactly_the_waits_that_are_due(
            dues in proptest::collection::vec(0u64..1_000, 0..24),
            steps in proptest::collection::vec(0u64..200, 1..12),
        ) {
            let mut timers = Timers::default();
            let waiting = slots(dues.len());
            for (due, slot) in dues.iter().zip(&waiting) {
                timers.record(*due, Arc::clone(slot));
            }
            let (mut now, mut fired) = (0u64, 0usize);
            for step in steps {
                now += step;
                fired += timers.fire(Some(now));
                for (due, slot) in dues.iter().zip(&waiting) {
                    prop_assert_eq!(slot.is_ready(), *due <= now);
                }
                prop_assert_eq!(fired, dues.iter().filter(|due| **due <= now).count());
                let next = dues.iter().filter(|due| **due > now).min();
                prop_assert_eq!(
                    timers.wake_in(Some(now)),
                    next.map(|due| Duration::from_micros(due - now))
                );
                prop_assert_eq!(timers.len(), dues.len() - fired);
            }
        }
    }
}
