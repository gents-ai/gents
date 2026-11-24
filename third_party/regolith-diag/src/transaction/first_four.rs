//! A hand-off list whose first four entries live in place.
//!
//! A transaction's `on_abort` callbacks sit in its claim, shared with `close`
//! (`super::claim`). One thread, the transaction's owner, pushes them; the
//! thread that ends the transaction takes them, and the owner takes what
//! was pushed after that at the end. The first [`IN_PLACE`] pushes go to
//! slots inside the list itself, so a transaction with a few callbacks
//! allocates nothing for them beyond the claim; later ones go to a
//! [`Handoff`].
//!
//! Invariants this module holds:
//!
//! - One thread pushes. Slots fill in order, and the overflow is used only
//!   once every slot was filled.
//! - A slot is taken once: the taker moves it from full to taken with one
//!   compare-and-swap, and nothing fills it again.
//! - A take stops at the first slot not filled yet, and reaches the
//!   overflow only past four filled slots, so entries come out oldest first
//!   across takes as well as within one.

#![allow(unsafe_code)]

use std::mem::MaybeUninit;

use super::handoff::{Handoff, Taken};
// Loom's under `--cfg loom`: `super::lock_loom` races a push against a take.
use crate::sync::internal::{AtomicU8, AtomicUsize, Ordering, UnsafeCell};

/// Entries the list holds in place before it uses the overflow.
pub(super) const IN_PLACE: usize = 4;

/// Not filled yet.
const EMPTY: u8 = 0;
/// Filled, not taken.
const FULL: u8 = 1;
/// Taken; never filled again.
const TAKEN: u8 = 2;

struct Slot<T> {
    state: AtomicU8,
    value: UnsafeCell<MaybeUninit<T>>,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self {
            state: AtomicU8::new(EMPTY),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

/// See the module docs.
pub(super) struct FirstFour<T> {
    slots: [Slot<T>; IN_PLACE],
    /// Slots filled so far. Written by the one pushing thread.
    pushed: AtomicUsize,
    rest: Handoff<T>,
}

// SAFETY: a value is moved in by the pushing thread and out by the one
// taker whose compare-and-swap won its slot, never shared between them, so
// `T: Send` is all that is needed; the overflow is a `Handoff`, which is
// `Send` and `Sync` for the same reason.
unsafe impl<T: Send> Send for FirstFour<T> {}
// SAFETY: as above; every access to a slot's value is ordered by its state.
unsafe impl<T: Send> Sync for FirstFour<T> {}

impl<T> Default for FirstFour<T> {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| Slot::default()),
            pushed: AtomicUsize::new(0),
            rest: Handoff::default(),
        }
    }
}

impl<T> FirstFour<T> {
    /// Push `value`. Called by one thread only: the module docs.
    pub(super) fn push(&self, value: T) {
        let at = self.pushed.load(Ordering::Relaxed);
        let Some(slot) = self.slots.get(at) else {
            return self.rest.push(value);
        };
        // SAFETY: slot `at` is EMPTY, and only this thread fills slots, so
        // nothing reads or writes its value until the store below.
        slot.value.with_mut(|cell| unsafe { (*cell).write(value) });
        slot.state.store(FULL, Ordering::Release);
        self.pushed.store(at + 1, Ordering::Relaxed);
    }

    /// What is pushed and not taken yet, oldest first, taken one entry at a
    /// time as the iterator reaches it.
    pub(super) fn take(&self) -> TakeFirst<'_, T> {
        TakeFirst {
            list: self,
            at: 0,
            stopped: false,
            rest: None,
            #[cfg(loom)]
            past_empty: false,
        }
    }

    /// The take a loom calibration breaks: it runs on past a slot not filled
    /// yet instead of stopping there.
    #[cfg(loom)]
    pub(super) fn take_past_empty(&self) -> TakeFirst<'_, T> {
        TakeFirst {
            past_empty: true,
            ..self.take()
        }
    }
}

impl<T> Drop for FirstFour<T> {
    fn drop(&mut self) {
        for slot in &self.slots {
            // `&mut self` excludes every other access, so the load sees the
            // last state stored.
            if slot.state.load(Ordering::Relaxed) == FULL {
                // SAFETY: a FULL slot holds a value nobody took, and `&mut
                // self` excludes every other access.
                slot.value
                    .with_mut(|cell| unsafe { (*cell).assume_init_drop() });
            }
        }
    }
}

/// The entries one [`FirstFour::take`] hands out.
pub(super) struct TakeFirst<'a, T> {
    list: &'a FirstFour<T>,
    at: usize,
    /// A slot was found not filled: nothing past it is handed out.
    stopped: bool,
    rest: Option<Taken<T>>,
    /// The calibration's take ([`FirstFour::take_past_empty`]).
    #[cfg(loom)]
    past_empty: bool,
}

impl<T> TakeFirst<'_, T> {
    /// Whether a slot not filled yet ends this take: always.
    #[cfg(not(loom))]
    fn stops_at_empty(&self) -> bool {
        true
    }

    /// Whether a slot not filled yet ends this take: always, but in a loom
    /// calibration's.
    #[cfg(loom)]
    fn stops_at_empty(&self) -> bool {
        !self.past_empty
    }
}

impl<T> Iterator for TakeFirst<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        while let Some(slot) = self.list.slots.get(self.at) {
            self.at += 1;
            match slot
                .state
                .compare_exchange(FULL, TAKEN, Ordering::Acquire, Ordering::Relaxed)
            {
                Ok(_) => {
                    // SAFETY: the exchange made the filled value this taker's
                    // alone, and the Acquire pairs with the push's Release.
                    let value = slot
                        .value
                        .with(|cell| unsafe { (*cell).assume_init_read() });
                    return Some(value);
                }
                Err(EMPTY) if self.stops_at_empty() => {
                    self.stopped = true;
                    self.at = IN_PLACE;
                    return None;
                }
                Err(_) => {}
            }
        }
        if self.stopped {
            return None;
        }
        self.rest
            .get_or_insert_with(|| self.list.rest.take())
            .next()
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn entries_come_out_oldest_first_across_the_in_place_edge() {
        let list = FirstFour::default();
        for i in 0..10 {
            list.push(i);
        }
        assert_eq!(list.take().collect::<Vec<_>>(), (0..10).collect::<Vec<_>>());
        assert_eq!(list.take().count(), 0);
    }

    #[test]
    fn an_entry_pushed_after_a_take_comes_out_at_the_next_one() {
        let list = FirstFour::default();
        list.push(1);
        assert_eq!(list.take().collect::<Vec<_>>(), [1]);
        list.push(2);
        for i in 3..8 {
            list.push(i);
        }
        assert_eq!(list.take().collect::<Vec<_>>(), [2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn a_take_stops_at_a_slot_not_filled_yet() {
        let list = FirstFour::default();
        list.push(1);
        let mut taking = list.take();
        assert_eq!(taking.next(), Some(1));
        assert_eq!(taking.next(), None, "slot 2 is not filled");
        list.push(2);
        assert_eq!(taking.next(), None, "a take that stopped stays stopped");
        assert_eq!(list.take().collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn what_nobody_took_is_dropped_with_the_list_once() {
        let live = Arc::new(());
        let list = FirstFour::default();
        for _ in 0..6 {
            list.push(Arc::clone(&live));
        }
        drop(list.take().next());
        assert_eq!(Arc::strong_count(&live), 1 + 5);
        drop(list);
        assert_eq!(Arc::strong_count(&live), 1);
    }

    /// One thread pushes while another takes: every entry comes out
    /// exactly once, in the order it was pushed.
    #[test]
    fn a_push_racing_a_take_loses_nothing_and_keeps_the_order() {
        // Under miri each thread costs much more: a few rounds there.
        let rounds = if cfg!(miri) { 4 } else { 200 };
        for round in 0..rounds {
            let list = Arc::new(FirstFour::default());
            let pushes = 2 + round % 8;
            let taker = {
                let list = Arc::clone(&list);
                std::thread::spawn(move || {
                    let mut got = Vec::new();
                    while got.len() < pushes {
                        got.extend(list.take());
                        std::thread::yield_now();
                    }
                    got
                })
            };
            for i in 0..pushes {
                list.push(i);
            }
            let got = taker.join().unwrap();
            assert_eq!(got, (0..pushes).collect::<Vec<_>>());
        }
    }
}
