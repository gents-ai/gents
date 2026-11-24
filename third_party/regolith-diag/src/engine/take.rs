//! A value put in by one thread and taken out by another, with no lock.

#![allow(unsafe_code)]

use crate::sync::internal::{AtomicPtr, Ordering, take_ptr};

/// A cell holding at most one `T`: [`put`](Take::put) stores a value,
/// [`take`](Take::take) removes it. Each value put is handed to exactly one
/// taker or dropped with the cell; a put over a value still there drops the
/// older one.
pub(crate) struct Take<T>(AtomicPtr<T>);

// SAFETY: the value moves from the putting thread to the taking one and is
// never shared by reference, so `T: Send` is all that is needed.
unsafe impl<T: Send> Send for Take<T> {}
// SAFETY: as above; every access goes through the atomic pointer.
unsafe impl<T: Send> Sync for Take<T> {}

impl<T> Take<T> {
    pub(crate) fn empty() -> Self {
        Self(AtomicPtr::new(std::ptr::null_mut()))
    }

    /// Put `value` in. A value already there is dropped.
    pub(crate) fn put(&self, value: T) {
        let old = self
            .0
            .swap(Box::into_raw(Box::new(value)), Ordering::AcqRel);
        if !old.is_null() {
            // SAFETY: a non-null pointer here came from `Box::into_raw` above
            // and was swapped out, so this call owns it.
            drop(unsafe { Box::from_raw(old) });
        }
    }

    /// Take the value out, if one is there.
    pub(crate) fn take(&self) -> Option<T> {
        let value = self.0.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if value.is_null() {
            return None;
        }
        // SAFETY: as in `put`: swapped out, so owned here.
        Some(*unsafe { Box::from_raw(value) })
    }

    /// Whether a value is there now.
    #[cfg(test)]
    pub(crate) fn is_some(&self) -> bool {
        !self.0.load(Ordering::Acquire).is_null()
    }
}

impl<T> Drop for Take<T> {
    fn drop(&mut self) {
        let value = take_ptr(&mut self.0);
        if !value.is_null() {
            // SAFETY: as in `put`: the cell's own box, and `&mut self` means
            // no other thread can take it.
            drop(unsafe { Box::from_raw(value) });
        }
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    struct Counted(Arc<AtomicUsize>);

    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_value_is_taken_once() {
        let cell = Take::empty();
        assert!(!cell.is_some());
        cell.put(7u32);
        assert!(cell.is_some());
        assert_eq!(cell.take(), Some(7));
        assert_eq!(cell.take(), None);
    }

    #[test]
    fn a_second_put_and_the_cells_drop_each_drop_what_they_replace() {
        let drops = Arc::new(AtomicUsize::new(0));
        let cell = Take::empty();
        cell.put(Counted(Arc::clone(&drops)));
        cell.put(Counted(Arc::clone(&drops)));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        drop(cell);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }
}
