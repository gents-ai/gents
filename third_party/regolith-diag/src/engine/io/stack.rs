//! A lock-free stack whose head word carries two flag bits.
//!
//! The per-thread queues need three kinds of list that many threads push to
//! and one party takes whole: a queue's inbox, the queues waiting on a unit,
//! and the tasks waiting on one read. Each is this stack. A push is one
//! compare-and-swap on the head word; a take is one swap that empties the
//! stack and sets the flags the taker chooses, so changing a flag and taking
//! the list it governs are a single step.
//!
//! - An inbox uses [`IDLE`]: its owner sets the flag on an empty inbox before
//!   it sleeps, and the one push that clears it is the push that wakes the
//!   owner. A dropped queue's inbox is taken with [`SHUT`], so every later
//!   push is refused.
//! - The waiters of a unit and the wakers of a read use [`SHUT`] as "done":
//!   the take that closes the list refuses every later push. A push either
//!   lands before the take, and the taker serves it, or is refused, and the
//!   pusher sees the outcome for itself. Nothing is served twice and nothing
//!   is lost.
//!
//! Nodes leave only in whole lists, never one at a time, so the push
//! compare-and-swap cannot suffer ABA: a node address that reappears at the
//! head is a new push onto a list nobody else is walking.

#![allow(unsafe_code)]

use core::marker::PhantomData;
use core::ptr;

use crate::sync::internal::{AtomicPtr, Ordering, take_ptr};

/// The inbox's owner is asleep and waits for the next push to wake it.
pub(crate) const IDLE: usize = 0b01;
/// The list is closed: pushes that refuse it are turned away.
pub(crate) const SHUT: usize = 0b10;
/// Every flag bit; a node address never has these bits set.
const FLAGS: usize = IDLE | SHUT;

struct Node<T> {
    value: T,
    next: *mut Node<T>,
}

// Two flag bits ride in the low bits of a node address.
const _: () = assert!(core::mem::align_of::<Node<u8>>() > FLAGS);

/// The flag bits alone, as a head word.
fn flags_only<T>(flags: usize) -> *mut Node<T> {
    ptr::without_provenance_mut(flags)
}

/// A head word split into its node address and its flags.
fn split<T>(word: *mut Node<T>) -> (*mut Node<T>, usize) {
    (word.map_addr(|a| a & !FLAGS), word.addr() & FLAGS)
}

/// A multi-producer stack taken whole by one consumer at a time. See the
/// module documentation for the flags.
pub(crate) struct Stack<T> {
    head: AtomicPtr<Node<T>>,
    /// The stack owns the values it holds and hands them across threads.
    _owns: PhantomData<*mut T>,
}

// SAFETY: a value enters on one thread and leaves on another, which needs
// `T: Send` and nothing more; no value is ever shared by reference.
unsafe impl<T: Send> Send for Stack<T> {}
// SAFETY: as above. Every access to the head is atomic, and a node is only
// read by the one party that took it out.
unsafe impl<T: Send> Sync for Stack<T> {}

/// What [`Stack::rest_if_empty`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rest {
    /// The stack was empty; the flag is now set.
    Set,
    /// The flag was already set on an empty stack.
    Already,
    /// The stack holds nodes, or is shut: the flag was not set.
    Busy,
}

impl<T> Stack<T> {
    pub(crate) fn new() -> Self {
        Self {
            head: AtomicPtr::new(ptr::null_mut()),
            _owns: PhantomData,
        }
    }

    /// Push `value`, unless the head carries one of the `refuse` flags, in
    /// which case `value` comes back as `Err`. A push clears [`IDLE`] and
    /// keeps [`SHUT`]. On success, the flags the head carried before the
    /// push: a push that finds [`IDLE`] is the one that must wake the owner.
    pub(crate) fn push(&self, value: T, refuse: usize) -> Result<usize, T> {
        let node = Box::into_raw(Box::new(Node {
            value,
            next: ptr::null_mut(),
        }));
        let mut word = self.head.load(Ordering::Acquire);
        loop {
            let (next, flags) = split(word);
            if flags & refuse != 0 {
                // SAFETY: the node was never published, so this call still
                // owns it.
                let node = unsafe { Box::from_raw(node) };
                return Err(node.value);
            }
            // SAFETY: the node is not published yet; nobody else sees it.
            unsafe { (*node).next = next };
            let new = node.map_addr(|a| a | (flags & SHUT));
            // AcqRel: Release publishes the node; Acquire pairs with the
            // Release of whoever set the flags, so a pusher that finds
            // `IDLE` sees what the owner stored before setting it.
            match self
                .head
                .compare_exchange(word, new, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return Ok(flags),
                Err(actual) => word = actual,
            }
        }
    }

    /// Take every value, oldest first, and leave the stack empty with
    /// exactly the flags `set`.
    pub(crate) fn take(&self, set: usize) -> Taken<T> {
        debug_assert_eq!(set & !FLAGS, 0);
        // AcqRel: Acquire takes the nodes the pushes released; Release
        // publishes what the taker did before it (a unit's outcome) to every
        // pusher the new flags refuse.
        let word = self.head.swap(flags_only(set), Ordering::AcqRel);
        // SAFETY: the swap handed this call every node on the list.
        unsafe { Taken::oldest_first(split(word).0) }
    }

    /// Take every value, oldest first, and leave the flags as they are: for
    /// a taker that neither closes the list nor may reopen one another
    /// thread closed meanwhile.
    ///
    /// The list still leaves whole, so a push cannot suffer ABA; and a head
    /// address that came back between this call's load and its
    /// compare-and-swap names the list that is there now, which is the one
    /// it then walks.
    pub(crate) fn take_open(&self) -> Taken<T> {
        let mut word = self.head.load(Ordering::Acquire);
        loop {
            let (node, flags) = split(word);
            if node.is_null() {
                return Taken {
                    next: ptr::null_mut(),
                };
            }
            match self.head.compare_exchange(
                word,
                flags_only(flags),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                // SAFETY: the compare-and-swap handed this call every node
                // on the list.
                Ok(_) => return unsafe { Taken::oldest_first(node) },
                Err(actual) => word = actual,
            }
        }
    }

    /// Set [`IDLE`] when the stack is empty and open, the owner's last step
    /// before it sleeps.
    pub(crate) fn rest_if_empty(&self) -> Rest {
        // Release: the waker stored before this is visible to the push that
        // clears the flag.
        match self.head.compare_exchange(
            ptr::null_mut(),
            flags_only(IDLE),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Rest::Set,
            Err(actual) if actual.addr() == IDLE => Rest::Already,
            Err(_) => Rest::Busy,
        }
    }

    /// Set [`SHUT`] when the stack is empty and carries no flag: a taker that
    /// served every value pushed so far closes the list in the same step that
    /// finds it empty, so a push either lands before and is served by it, or
    /// is refused after. `false` when a value came in first: take it, and try
    /// again.
    pub(crate) fn shut_if_empty(&self) -> bool {
        // Release: what the taker did before closing (the values it served)
        // is visible to every push the flag refuses.
        self.head
            .compare_exchange(
                ptr::null_mut(),
                flags_only(SHUT),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Clear [`IDLE`] on an empty stack: the owner is awake again. `true`
    /// when this call cleared it, so no push will.
    pub(crate) fn wake_if_resting(&self) -> bool {
        self.head
            .compare_exchange(
                flags_only(IDLE),
                ptr::null_mut(),
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_ok()
    }

    /// Whether the stack holds no value, whatever its flags.
    pub(crate) fn is_empty(&self) -> bool {
        split(self.head.load(Ordering::Acquire)).0.is_null()
    }

    /// The flags the head carries now.
    pub(crate) fn flags(&self) -> usize {
        self.head.load(Ordering::Acquire).addr() & FLAGS
    }
}

impl<T> Drop for Stack<T> {
    fn drop(&mut self) {
        // SAFETY: `&mut self` means no other thread can push or take, so
        // every node on the list is this call's.
        drop(unsafe { Taken::oldest_first(split(take_ptr(&mut self.head)).0) });
    }
}

/// The values one [`Stack::take`] took, oldest first. Values not iterated
/// are dropped with it.
pub(crate) struct Taken<T> {
    next: *mut Node<T>,
}

impl<T> Taken<T> {
    /// The list whose newest node is `node`, turned oldest first: pushes
    /// stacked newest first.
    ///
    /// # Safety
    ///
    /// The caller owns every node reachable from `node`, and nobody else
    /// reads them.
    unsafe fn oldest_first(mut node: *mut Node<T>) -> Self {
        let mut oldest_first: *mut Node<T> = ptr::null_mut();
        while !node.is_null() {
            // SAFETY: the caller owns the list.
            let next = unsafe { (*node).next };
            unsafe { (*node).next = oldest_first };
            oldest_first = node;
            node = next;
        }
        Self { next: oldest_first }
    }
}

impl<T> Iterator for Taken<T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        if self.next.is_null() {
            return None;
        }
        // SAFETY: `take` handed this list over whole, and each node is
        // unlinked here exactly once.
        let node = unsafe { Box::from_raw(self.next) };
        self.next = node.next;
        Some(node.value)
    }
}

impl<T> Drop for Taken<T> {
    fn drop(&mut self) {
        for value in self.by_ref() {
            drop(value);
        }
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[test]
    fn a_take_hands_values_back_oldest_first() {
        let stack = Stack::new();
        for i in 0..5 {
            assert_eq!(stack.push(i, SHUT), Ok(0));
        }
        assert_eq!(stack.take(0).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4]);
        assert!(stack.is_empty());
        assert_eq!(stack.take(0).count(), 0);
    }

    #[test]
    fn a_shut_stack_refuses_the_pushes_that_refuse_it() {
        let stack = Stack::new();
        stack.push(1, SHUT).unwrap();
        assert_eq!(stack.take(SHUT).collect::<Vec<_>>(), vec![1]);
        assert_eq!(stack.push(2, SHUT), Err(2));
        assert_eq!(stack.flags(), SHUT);
        // A push that does not refuse it lands and keeps it.
        assert_eq!(stack.push(3, 0), Ok(SHUT));
        assert_eq!(stack.flags(), SHUT);
        assert_eq!(stack.push(4, SHUT), Err(4));
    }

    #[test]
    fn an_open_take_empties_the_list_and_keeps_its_flags() {
        let stack = Stack::new();
        assert_eq!(stack.take_open().count(), 0);
        for i in 0..3 {
            stack.push(i, SHUT).unwrap();
        }
        assert_eq!(stack.take_open().collect::<Vec<_>>(), vec![0, 1, 2]);
        assert!(stack.is_empty());
        assert_eq!(stack.flags(), 0);
        // A list another thread closed stays closed.
        drop(stack.take(SHUT));
        assert_eq!(stack.take_open().count(), 0);
        assert_eq!(stack.flags(), SHUT);
        assert_eq!(stack.push(4, 0), Ok(SHUT));
        assert_eq!(stack.take_open().collect::<Vec<_>>(), vec![4]);
        assert_eq!(stack.flags(), SHUT);
        assert_eq!(stack.push(5, SHUT), Err(5));
    }

    #[test]
    fn shut_if_empty_closes_only_an_empty_list() {
        let stack = Stack::new();
        stack.push(1, SHUT).unwrap();
        assert!(!stack.shut_if_empty(), "a value came in first");
        assert_eq!(stack.take_open().collect::<Vec<_>>(), vec![1]);
        assert!(stack.shut_if_empty());
        assert_eq!(stack.flags(), SHUT);
        assert_eq!(stack.push(2, SHUT), Err(2));
        assert!(!stack.shut_if_empty(), "closed already");
    }

    #[test]
    fn the_push_that_finds_idle_is_told_so_and_clears_it() {
        let stack = Stack::new();
        assert_eq!(stack.rest_if_empty(), Rest::Set);
        assert_eq!(stack.rest_if_empty(), Rest::Already);
        assert_eq!(stack.push(1, SHUT), Ok(IDLE));
        assert_eq!(stack.flags(), 0);
        assert_eq!(stack.push(2, SHUT), Ok(0));
        assert_eq!(stack.rest_if_empty(), Rest::Busy);
        assert_eq!(stack.take(0).count(), 2);
        assert_eq!(stack.rest_if_empty(), Rest::Set);
        assert!(stack.wake_if_resting());
        assert!(!stack.wake_if_resting());
        assert_eq!(stack.flags(), 0);
    }

    #[test]
    fn dropping_frees_what_was_never_taken() {
        let witness = std::sync::Arc::new(());
        {
            let stack = Stack::new();
            for _ in 0..3 {
                stack.push(std::sync::Arc::clone(&witness), SHUT).unwrap();
            }
            let mut taken = stack.take(0);
            assert!(taken.next().is_some());
            stack.push(std::sync::Arc::clone(&witness), SHUT).unwrap();
        }
        assert_eq!(std::sync::Arc::strong_count(&witness), 1);
    }

    #[test]
    fn many_pushers_lose_nothing() {
        let stack = std::sync::Arc::new(Stack::new());
        std::thread::scope(|scope| {
            for t in 0..4u32 {
                let stack = std::sync::Arc::clone(&stack);
                scope.spawn(move || {
                    for i in 0..1000u32 {
                        stack.push(t * 1000 + i, SHUT).unwrap();
                    }
                });
            }
        });
        let mut got: Vec<u32> = stack.take(0).collect();
        got.sort_unstable();
        assert_eq!(got, (0..4000).collect::<Vec<_>>());
    }
}
