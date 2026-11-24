//! A caller's closure kept in place, so registering it allocates nothing.
//!
//! A transaction keeps the callbacks its caller registered until the commit
//! runs them, and each is a closure of a type of its own. Boxing every one
//! costs an allocation per callback. The types here keep a closure whose
//! captures fit [`INLINE_WORDS`] machine words in their own bytes, and box
//! only a larger or more strictly aligned one, so the few small callbacks a
//! transaction usually carries cost no allocation at all.
//!
//! Invariants this module holds:
//!
//! - The space holds exactly what `place` put there, a closure or the box of
//!   one, and `op` is the function made for that closure's type: nothing
//!   else ever reads the space.
//! - The closure is dropped once: by the `call` that consumes a run-once
//!   value, or when the value is dropped.
//! - A value is `Send` because its closure is, and not `Sync`: it is reached
//!   through `&mut` or by value only.

#![allow(unsafe_code)]

use std::mem::{ManuallyDrop, MaybeUninit, align_of, size_of};

use super::callbacks::AbortReason;
use super::{CommitReceipt, Transaction, TxResult};

/// Machine words of captures a closure may have and still be kept in place.
pub(crate) const INLINE_WORDS: usize = 4;

/// Where a closure lives, or the box of one too large for it.
type Space = MaybeUninit<[usize; INLINE_WORDS]>;

/// Whether an `F` is kept in the space itself.
const fn fits<F>() -> bool {
    size_of::<F>() <= size_of::<Space>() && align_of::<F>() <= align_of::<Space>()
}

/// Put `f` in a new space: itself when it fits, a box of it otherwise.
fn place<F>(f: F) -> Space {
    let mut space = Space::uninit();
    if fits::<F>() {
        // SAFETY: `fits` says an `F` has room and alignment in the space,
        // which is this function's own and holds nothing yet.
        unsafe { space.as_mut_ptr().cast::<F>().write(f) };
    } else {
        let boxed = Box::into_raw(Box::new(f));
        // SAFETY: a pointer has room and alignment in a space of words.
        unsafe { space.as_mut_ptr().cast::<*mut F>().write(boxed) };
    }
    space
}

/// The closure `place::<F>` put in `space`, where it lives now.
///
/// # Safety
///
/// `space` was filled by `place::<F>` with this same `F`, and its closure
/// has not been taken out.
unsafe fn at<F>(space: *mut Space) -> *mut F {
    if fits::<F>() {
        space.cast::<F>()
    } else {
        // SAFETY: by the contract the space holds the box's pointer.
        unsafe { space.cast::<*mut F>().read() }
    }
}

/// Take the closure `place::<F>` put in `space` out of it, freeing its box.
///
/// # Safety
///
/// As for [`at`], and nothing reads the space afterwards.
unsafe fn take<F>(space: *mut Space) -> F {
    if fits::<F>() {
        // SAFETY: by the contract the space holds an `F`, read this once.
        unsafe { space.cast::<F>().read() }
    } else {
        // SAFETY: by the contract the space holds the pointer `place` got
        // from `Box::into_raw`, made a box again this once.
        unsafe { *Box::from_raw(space.cast::<*mut F>().read()) }
    }
}

/// A closure that runs once, kept in place. Generated per signature: the
/// argument is a reference of any lifetime, which a type parameter cannot
/// name.
macro_rules! runs_once {
    ($(#[$doc:meta])* $name:ident, $arg:ty) => {
        $(#[$doc])*
        pub(crate) struct $name {
            space: Space,
            /// Takes the closure out of the space and runs it with the
            /// argument, or drops it unrun when handed none.
            op: unsafe fn(*mut Space, Option<$arg>),
        }

        impl $name {
            pub(crate) fn new<F>(f: F) -> Self
            where
                F: FnOnce($arg) + Send + 'static,
            {
                // SAFETY (of a call): as for `take` with this `F`.
                unsafe fn op<F: FnOnce($arg)>(space: *mut Space, arg: Option<$arg>) {
                    // SAFETY: this function's own contract.
                    let f = unsafe { take::<F>(space) };
                    if let Some(arg) = arg {
                        f(arg);
                    }
                }
                Self {
                    space: place(f),
                    op: op::<F>,
                }
            }

            /// Run the closure.
            pub(crate) fn call(self, arg: $arg) {
                let mut this = ManuallyDrop::new(self);
                let op = this.op;
                // SAFETY: `op` was made by `new` for the closure it placed
                // in `space`. The value is consumed and its `Drop` does not
                // run, so this is the one time the closure is taken out.
                unsafe { op(&mut this.space, Some(arg)) }
            }
        }

        impl Drop for $name {
            fn drop(&mut self) {
                let op = self.op;
                // SAFETY: as in `call`. A value that was called is never
                // dropped, so the closure is still in the space.
                unsafe { op(&mut self.space, None) }
            }
        }

        // SAFETY: the value owns one `F: Send` and nothing else.
        unsafe impl Send for $name {}
    };
}

/// Take the closure `place::<F>` put in `space` out of it and drop it.
///
/// # Safety
///
/// As for [`take`].
unsafe fn forget<F>(space: *mut Space) {
    // SAFETY: this function's own contract.
    drop(unsafe { take::<F>(space) });
}

/// A closure that may run more than once, kept in place. Generated per
/// signature, as [`runs_once`] is.
macro_rules! runs_again {
    ($(#[$doc:meta])* $name:ident, $arg:ty, $out:ty) => {
        $(#[$doc])*
        pub(crate) struct $name {
            space: Space,
            /// Runs the closure where it lives.
            run: unsafe fn(*mut Space, $arg) -> $out,
            /// Takes the closure out of the space and drops it.
            forget: unsafe fn(*mut Space),
        }

        impl $name {
            pub(crate) fn new<F>(f: F) -> Self
            where
                F: FnMut($arg) -> $out + Send + 'static,
            {
                // SAFETY (of a call): as for `at` with this `F`, with the
                // closure borrowed by nobody else.
                unsafe fn run<F: FnMut($arg) -> $out>(space: *mut Space, arg: $arg) -> $out {
                    // SAFETY: this function's own contract.
                    let f = unsafe { &mut *at::<F>(space) };
                    f(arg)
                }
                Self {
                    space: place(f),
                    run: run::<F>,
                    forget: forget::<F>,
                }
            }

            /// Run the closure.
            pub(crate) fn call(&mut self, arg: $arg) -> $out {
                let run = self.run;
                // SAFETY: `run` was made by `new` for the closure it placed
                // in `space`, which stays there until this value is
                // dropped, and `&mut self` is the only way to it.
                unsafe { run(&mut self.space, arg) }
            }
        }

        impl Drop for $name {
            fn drop(&mut self) {
                let forget = self.forget;
                // SAFETY: `forget` was made by `new` for the closure it
                // placed in `space`, and this is the one time the closure
                // is taken out.
                unsafe { forget(&mut self.space) };
            }
        }

        // SAFETY: the value owns one `F: Send` and nothing else.
        unsafe impl Send for $name {}
    };
}

runs_once!(
    /// An `on_commit` callback.
    OnCommit,
    &CommitReceipt
);
runs_once!(
    /// An `on_abort` callback.
    OnAbort,
    &AbortReason<'_>
);
runs_again!(
    /// A `before_commit` callback: one that returned an error runs again at
    /// the next `prepare`.
    BeforeCommit,
    &mut Transaction,
    TxResult<()>
);

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    runs_again!(
        /// A closure over a number, to run the run-again form without a
        /// transaction.
        Adds,
        &mut usize,
        usize
    );

    /// Counts its drops, so a test sees a closure's captures go exactly once.
    struct Dropped(Arc<AtomicUsize>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// More words than a space holds: a closure that captures it is boxed.
    type Ballast = [usize; INLINE_WORDS + 1];

    fn counter() -> Arc<AtomicUsize> {
        Arc::new(AtomicUsize::new(0))
    }

    #[test]
    fn a_small_closure_is_kept_in_place_and_a_large_or_aligned_one_is_boxed() {
        fn in_place<F>(_: &F) -> bool {
            fits::<F>()
        }
        let small = [0usize; INLINE_WORDS];
        let large: Ballast = [0; INLINE_WORDS + 1];
        #[repr(align(64))]
        struct Aligned(u8);
        let aligned = Aligned(1);
        assert_eq!(aligned.0, 1);
        assert!(in_place(&move |_: &CommitReceipt| {
            let _held = &small;
        }));
        assert!(in_place(&|_: &CommitReceipt| {}));
        assert!(!in_place(&move |_: &CommitReceipt| {
            let _held = &large;
        }));
        // The whole value is captured, with its alignment.
        assert!(!in_place(&move |_: &CommitReceipt| {
            let _held = &aligned;
        }));
    }

    /// An `on_commit` callback that adds the receipt's sequence to `ran` and
    /// holds `guard`, in place or boxed.
    fn on_commit(ran: &Arc<AtomicUsize>, guard: Dropped, boxed: bool) -> OnCommit {
        let ran = Arc::clone(ran);
        if boxed {
            let ballast: Ballast = [1; INLINE_WORDS + 1];
            OnCommit::new(move |receipt: &CommitReceipt| {
                let _held = (&guard, &ballast);
                ran.fetch_add(receipt.seq() as usize, Ordering::SeqCst);
            })
        } else {
            OnCommit::new(move |receipt: &CommitReceipt| {
                let _held = &guard;
                ran.fetch_add(receipt.seq() as usize, Ordering::SeqCst);
            })
        }
    }

    #[test]
    fn a_callback_runs_once_with_its_argument_and_drops_its_captures_once() {
        for boxed in [false, true] {
            let (ran, dropped) = (counter(), counter());
            let callback = on_commit(&ran, Dropped(Arc::clone(&dropped)), boxed);
            assert_eq!(dropped.load(Ordering::SeqCst), 0);
            callback.call(&CommitReceipt::new(7));
            assert_eq!(ran.load(Ordering::SeqCst), 7, "boxed: {boxed}");
            assert_eq!(dropped.load(Ordering::SeqCst), 1, "boxed: {boxed}");
        }
    }

    #[test]
    fn a_callback_dropped_unrun_runs_nothing_and_drops_its_captures_once() {
        for boxed in [false, true] {
            let (ran, dropped) = (counter(), counter());
            drop(on_commit(&ran, Dropped(Arc::clone(&dropped)), boxed));
            assert_eq!(ran.load(Ordering::SeqCst), 0, "boxed: {boxed}");
            assert_eq!(dropped.load(Ordering::SeqCst), 1, "boxed: {boxed}");
        }
    }

    #[test]
    fn a_callback_that_panics_drops_its_captures_once() {
        let dropped = counter();
        let guard = Dropped(Arc::clone(&dropped));
        let callback = OnAbort::new(move |_: &AbortReason<'_>| {
            let _held = &guard;
            panic!("a test's callback");
        });
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            callback.call(&AbortReason::Rollback);
        }));
        assert!(outcome.is_err());
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_callback_moves_to_another_thread() {
        let ran = counter();
        let seen = Arc::clone(&ran);
        let callback = OnCommit::new(move |_: &CommitReceipt| {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        std::thread::spawn(move || callback.call(&CommitReceipt::new(1)))
            .join()
            .unwrap();
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_run_again_closure_keeps_its_state_between_runs_and_drops_it_once() {
        for boxed in [false, true] {
            let dropped = counter();
            let guard = Dropped(Arc::clone(&dropped));
            let mut runs = 0usize;
            let mut adds = if boxed {
                let ballast: Ballast = [1; INLINE_WORDS + 1];
                Adds::new(move |n: &mut usize| {
                    let _held = (&guard, &ballast);
                    runs += 1;
                    *n += runs;
                    runs
                })
            } else {
                Adds::new(move |n: &mut usize| {
                    let _held = &guard;
                    runs += 1;
                    *n += runs;
                    runs
                })
            };
            let mut total = 0;
            assert_eq!(adds.call(&mut total), 1);
            assert_eq!(adds.call(&mut total), 2);
            assert_eq!(total, 3, "the closure kept its count: boxed {boxed}");
            assert_eq!(dropped.load(Ordering::SeqCst), 0);
            drop(adds);
            assert_eq!(dropped.load(Ordering::SeqCst), 1, "boxed: {boxed}");
        }
    }

    const fn assert_send<T: Send>() {}

    #[test]
    fn the_kept_callbacks_are_send() {
        assert_send::<OnCommit>();
        assert_send::<OnAbort>();
        assert_send::<BeforeCommit>();
    }
}
