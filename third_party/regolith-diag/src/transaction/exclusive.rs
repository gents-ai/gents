//! A `Send` value made `Sync` by never lending it out by shared reference.
//!
//! A transaction holds the closures its caller registered, which are `Send`
//! and not `Sync`, and a `Transaction` must be `Sync`. They are only ever
//! reached through `&mut Transaction` or by value, so no two threads can touch
//! them at once; this type states that in the type system instead of paying
//! for a lock nobody takes. It is the stable form of `std::sync::Exclusive`.

#![allow(unsafe_code)]

#[derive(Default)]
pub(super) struct Exclusive<T>(T);

// SAFETY: the only ways in are `&mut self` and by value, so a shared
// reference to an `Exclusive` lends out nothing and `T` needs to be `Send`
// alone.
unsafe impl<T: Send> Sync for Exclusive<T> {}

impl<T> Exclusive<T> {
    pub(super) fn get_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const fn assert_sync<T: Sync>() {}

    #[test]
    fn a_send_value_that_is_not_sync_becomes_sync() {
        assert_sync::<Exclusive<Cell<u8>>>();
        assert_sync::<Exclusive<Box<dyn FnMut() + Send>>>();
    }

    #[test]
    fn the_value_is_reached_through_exclusive_access_only() {
        let mut value = Exclusive(Cell::new(1));
        value.get_mut().set(2);
        assert_eq!(value.0.get(), 2);
    }
}
