//! A list any thread pushes to and any thread takes whole, with no lock.
//!
//! A transaction's `on_abort` callbacks have two possible runners: its owner,
//! and `close` on another thread. Whoever wins the transaction's abort claim
//! (see `super::claim`) takes everything registered so far in one step and
//! runs it; a callback registered after that is simply left in the list, and
//! the owner takes it at the end of the transaction.
//!
//! The list is a Treiber stack that only ever grows at the head and is only
//! ever emptied by swapping the head out for null. No node is popped alone,
//! so a push never dereferences the head it read and the ABA problem has
//! nothing to attack. A taker owns the whole chain after the swap, reverses
//! it into push order, and frees each node as it hands the value out.

#![allow(unsafe_code)]

use std::mem;
use std::ptr;

// Loom's under `--cfg loom`, so the models of `super::first_four` see the
// overflow's head too.
use crate::sync::internal::{AtomicPtr, Ordering};

struct Node<T> {
    value: T,
    next: *mut Node<T>,
}

/// Entries pushed by any thread and taken, all at once and oldest first, by
/// any thread. Pushing and taking never wait for each other.
pub(super) struct Handoff<T> {
    head: AtomicPtr<Node<T>>,
}

// SAFETY: a value is moved into the list by one thread and out of it by
// another, and is never shared between them, so `T: Send` is all that is
// needed. The head is an atomic pointer.
unsafe impl<T: Send> Send for Handoff<T> {}
// SAFETY: as above; every access to the nodes goes through the atomic head.
unsafe impl<T: Send> Sync for Handoff<T> {}

impl<T> Default for Handoff<T> {
    fn default() -> Self {
        Self {
            head: AtomicPtr::new(ptr::null_mut()),
        }
    }
}

impl<T> Handoff<T> {
    pub(super) fn push(&self, value: T) {
        let node = Box::into_raw(Box::new(Node {
            value,
            next: ptr::null_mut(),
        }));
        let mut head = self.head.load(Ordering::Relaxed);
        loop {
            // SAFETY: `node` is ours alone until the exchange below links it.
            unsafe { (*node).next = head };
            match self
                .head
                .compare_exchange_weak(head, node, Ordering::Release, Ordering::Relaxed)
            {
                Ok(_) => return,
                Err(seen) => head = seen,
            }
        }
    }

    /// Everything pushed so far, oldest first. What is pushed after this
    /// call stays in the list for the next one.
    pub(super) fn take(&self) -> Taken<T> {
        let mut newest = self.head.swap(ptr::null_mut(), Ordering::Acquire);
        let mut oldest = ptr::null_mut();
        while !newest.is_null() {
            // SAFETY: the swap made the whole chain ours alone, and every
            // node on it came from `Box::into_raw` in `push`.
            let next = unsafe { mem::replace(&mut (*newest).next, oldest) };
            oldest = newest;
            newest = next;
        }
        Taken { next: oldest }
    }
}

impl<T> Drop for Handoff<T> {
    fn drop(&mut self) {
        drop(self.take());
    }
}

/// The entries one [`Handoff::take`] removed. Dropping it drops the ones not
/// yet handed out.
pub(super) struct Taken<T> {
    next: *mut Node<T>,
}

impl<T> Iterator for Taken<T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        if self.next.is_null() {
            return None;
        }
        // SAFETY: the chain is this iterator's alone, and `next` is a node
        // `push` allocated and nothing has freed.
        let Node { value, next } = *unsafe { Box::from_raw(self.next) };
        self.next = next;
        Some(value)
    }
}

impl<T> Drop for Taken<T> {
    fn drop(&mut self) {
        self.for_each(drop);
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn a_take_yields_the_entries_oldest_first_and_leaves_the_list_empty() {
        let list = Handoff::default();
        for i in 0..10 {
            list.push(i);
        }
        assert_eq!(list.take().collect::<Vec<_>>(), (0..10).collect::<Vec<_>>());
        assert_eq!(list.take().count(), 0);
    }

    #[test]
    fn an_entry_pushed_after_a_take_waits_for_the_next_one() {
        let list = Handoff::default();
        list.push(1);
        let first = list.take();
        list.push(2);
        assert_eq!(first.collect::<Vec<_>>(), [1]);
        assert_eq!(list.take().collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn dropping_the_list_or_a_half_read_take_drops_what_is_left() {
        let live = Arc::new(());
        let list = Handoff::default();
        for _ in 0..5 {
            list.push(Arc::clone(&live));
        }
        let mut taken = list.take();
        list.push(Arc::clone(&live));
        drop(taken.next());
        assert_eq!(Arc::strong_count(&live), 1 + 4 + 1);
        drop(taken);
        assert_eq!(Arc::strong_count(&live), 1 + 1);
        drop(list);
        assert_eq!(Arc::strong_count(&live), 1);
    }

    proptest! {
        /// The list agrees with a plain vector, whatever the mix of pushes
        /// and takes.
        #[test]
        fn the_list_matches_a_vector(
            ops in prop::collection::vec(prop::option::of(any::<u32>()), 0..200)
        ) {
            let list = Handoff::default();
            let mut model = Vec::new();
            for op in ops {
                match op {
                    Some(x) => {
                        list.push(x);
                        model.push(x);
                    }
                    None => prop_assert_eq!(list.take().collect::<Vec<_>>(), mem::take(&mut model)),
                }
            }
            prop_assert_eq!(list.take().collect::<Vec<_>>(), model);
        }
    }

    /// Producers push while a taker takes: every entry comes out exactly once,
    /// and each producer's entries come out in the order it pushed them,
    /// across takes as well as within one.
    #[test]
    fn concurrent_pushes_and_takes_lose_nothing_and_keep_each_producers_order() {
        const PRODUCERS: u32 = 4;
        const EACH: u32 = 5_000;
        let list = Arc::new(Handoff::default());
        let done = Arc::new(AtomicBool::new(false));
        let taker = {
            let (list, done) = (Arc::clone(&list), Arc::clone(&done));
            std::thread::spawn(move || {
                let mut got = Vec::new();
                loop {
                    let finished = done.load(Ordering::Acquire);
                    got.extend(list.take());
                    if finished {
                        return got;
                    }
                    std::thread::yield_now();
                }
            })
        };
        std::thread::scope(|scope| {
            for producer in 0..PRODUCERS {
                let list = &list;
                scope.spawn(move || {
                    for i in 0..EACH {
                        list.push((producer, i));
                    }
                });
            }
        });
        done.store(true, Ordering::Release);
        let got = taker.join().unwrap();
        assert_eq!(got.len() as u32, PRODUCERS * EACH);
        for producer in 0..PRODUCERS {
            let mine: Vec<u32> = got
                .iter()
                .filter(|(p, _)| *p == producer)
                .map(|(_, i)| *i)
                .collect();
            assert_eq!(mine, (0..EACH).collect::<Vec<_>>(), "producer {producer}");
        }
    }
}
