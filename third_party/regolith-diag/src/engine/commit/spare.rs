//! The allocations a thread keeps between commits, so that a commit
//! allocates nothing for the pipeline's own objects.
//!
//! A commit needs a descriptor in the ring and a decision in the chain, and a
//! sync round needs the round and its job. Each is shared between threads
//! through an `Arc`, so each would be one allocation a commit, and the write
//! path had none of them before the pipeline. Instead, when the last
//! reference to one of them turns out to be this thread's, the thread moves
//! the value out and keeps the allocation, a *shell*, and its next commit
//! fills the shell instead of allocating. One thread committing on its own
//! reuses the same few allocations for ever; under contention a thread that
//! finds no shell allocates, as it would have.
//!
//! # Invariants
//!
//! - **S1, a shell has one owner.** It is made only from an `Arc` whose
//!   counts say it is unique: one strong reference, read before an acquire
//!   fence, and no weak one. Nobody else can reach the allocation, then or
//!   later: a weak reference is made only from a strong one, and the last
//!   other strong reference was let go of before the fence (its release
//!   decrement is what the read saw), so all its use of the value happened
//!   first. That is what [`Arc::get_mut`] asks too, with a locked
//!   compare-and-swap of the weak count it needs only against a weak
//!   reference being made at the same time, which takes a strong one.
//! - **S2, a shell holds no value.** The value was moved out when the shell
//!   was made, and the allocation is typed `MaybeUninit` from then on, so it
//!   is never read or dropped as a value again and pins nothing: no log, no
//!   memtable, no key.
//! - **S3, a thread keeps one shell of each kind.** A second one is freed, so
//!   what a thread holds back is bounded by four small allocations, and one
//!   record buffer: the buffer of the last descriptor it let go of that
//!   nobody else held, trimmed as a stage is (`trim_stage`).
//!
//! A loom build keeps no shell: its `Arc` is the model's, and what is reused
//! here is memory, not a protocol.

#![allow(unsafe_code)]

use super::super::io::job::{Job, JobBody};
use super::descriptor::{Decision, Descriptor, Work};
use super::round::Round;
use crate::sync::internal::Arc as RingArc;

#[cfg(not(loom))]
mod shell {
    use std::mem::MaybeUninit;
    use std::ptr;
    use std::sync::Arc;
    use std::sync::atomic::{Ordering, fence};

    /// The allocation of an `Arc<T>` whose value was moved out (S1, S2).
    pub(super) struct Shell<T>(Arc<MaybeUninit<T>>);

    /// Whether `arc` is the only reference to its allocation, strong or
    /// weak (S1): two plain loads and a fence, where [`Arc::get_mut`] takes a
    /// locked compare-and-swap and a store even to say no.
    pub(super) fn unique<T>(arc: &Arc<T>) -> bool {
        if Arc::strong_count(arc) != 1 {
            return false;
        }
        // Pairs with the release decrement of whichever thread let go of
        // the last other strong reference: its use of the value, and any
        // weak reference it made, happened before what follows.
        fence(Ordering::Acquire);
        Arc::weak_count(arc) == 0
    }

    impl<T> Shell<T> {
        /// Move the value out of `arc` and keep its allocation, when `arc`
        /// is the last reference; otherwise hand `arc` back.
        pub(super) fn take(arc: Arc<T>) -> Result<(Self, T), Arc<T>> {
            if !unique(&arc) {
                return Err(arc);
            }
            let raw = Arc::into_raw(arc);
            // SAFETY (S1): `unique` said this is the only reference, strong
            // or weak, so no other thread reads the value, and it is moved
            // out exactly once, here.
            let value = unsafe { ptr::read(raw) };
            // SAFETY (S2): `raw` came from `Arc::into_raw` just above, and
            // `MaybeUninit<T>` has the size and alignment of `T`, which is
            // what `Arc::from_raw` asks for when the type changes. The
            // allocation is only seen as uninitialized from here on, so the
            // value moved out above is never dropped a second time.
            let shell = unsafe { Arc::from_raw(raw.cast::<MaybeUninit<T>>()) };
            Ok((Self(shell), value))
        }

        /// Drop the value of `arc` where it lies and keep its allocation,
        /// when `arc` is the last reference; otherwise let `arc` go. What
        /// [`Self::take`] does for a value nobody needs, with no copy of it
        /// out of the allocation first: a descriptor or a decision is a few
        /// hundred bytes. `before` sees the value alone first, to take out
        /// what is kept beyond it.
        pub(super) fn recycle(arc: Arc<T>, before: impl FnOnce(&mut T)) -> Option<Self> {
            if !unique(&arc) {
                return None;
            }
            let raw = Arc::into_raw(arc).cast_mut();
            // SAFETY (S1): `unique` said this is the only reference, so this
            // borrow is the only access to the value, and it ends before the
            // value is dropped below. A `before` that unwinds leaks the
            // allocation and the value, which frees nothing twice.
            before(unsafe { &mut *raw });
            // SAFETY (S2): as in `take`; from here on the allocation is only
            // seen as uninitialized, so the shell never drops the value.
            let shell = Self(unsafe { Arc::from_raw(raw.cast_const().cast::<MaybeUninit<T>>()) });
            // SAFETY (S1): `unique` said this is the only reference, so no
            // other thread reads the value, and it is dropped exactly once,
            // here. A drop that unwinds leaves the shell to free the
            // allocation, dropping nothing.
            unsafe { ptr::drop_in_place(raw) };
            Some(shell)
        }

        /// Put `value` in the allocation and hand it out as an `Arc<T>`.
        ///
        /// No uniqueness check here: it would ask again what S1 already
        /// says.
        pub(super) fn fill(self, value: T) -> Arc<T> {
            let raw = Arc::into_raw(self.0).cast_mut();
            // SAFETY (S1): the shell's `Arc` is the only reference to the
            // allocation, strong or weak, so nothing else reads or writes it
            // while the value goes in. `raw` came from `Arc::into_raw`, and
            // `T` has the layout of `MaybeUninit<T>`, so `Arc::from_raw`
            // takes it back as an `Arc<T>` once the value is written.
            unsafe {
                (*raw).write(value);
                Arc::from_raw(raw.cast_const().cast::<T>())
            }
        }
    }
}

#[cfg(not(loom))]
mod kept {
    use std::cell::Cell;
    use std::sync::Arc;
    use std::thread::LocalKey;

    use super::shell::Shell;
    use super::{Decision, Descriptor, Job, Round};

    /// Where a thread keeps its shell of one kind (S3).
    pub(super) type Slot<T> = Cell<Option<Shell<T>>>;

    std::thread_local! {
        pub(super) static DESCRIPTOR: Slot<Descriptor> = const { Cell::new(None) };
        pub(super) static DECISION: Slot<Decision> = const { Cell::new(None) };
        pub(super) static ROUND: Slot<Round> = const { Cell::new(None) };
        pub(super) static JOB: Slot<Job> = const { Cell::new(None) };
        /// The record buffer of the last descriptor this thread kept, and
        /// the length of the record before it (`trim_stage`'s hysteresis).
        pub(super) static RECORD: Cell<(Vec<u8>, usize)> = const { Cell::new((Vec::new(), 0)) };
    }

    /// `value` in this thread's shell, or in a new allocation when the
    /// thread has none (or is past the end of its thread-local storage).
    pub(super) fn make<T>(slot: &'static LocalKey<Slot<T>>, value: T) -> Arc<T> {
        match slot.try_with(Cell::take) {
            Ok(Some(shell)) => shell.fill(value),
            _ => Arc::new(value),
        }
    }

    /// Keep the allocation of `arc` for this thread's next value of the
    /// kind, when `arc` is the last reference, and hand back the value it
    /// held. `None` when it is still shared: it is then only let go of.
    pub(super) fn keep<T>(slot: &'static LocalKey<Slot<T>>, arc: Arc<T>) -> Option<T> {
        let (shell, value) = Shell::take(arc).ok()?;
        // S3: a shell already there is freed by the `set`.
        let _ = slot.try_with(|slot| slot.set(Some(shell)));
        Some(value)
    }

    /// [`keep`] for a value nobody needs: dropped where it lies, and its
    /// allocation kept when `arc` was the last reference. `before` sees the
    /// value alone first.
    pub(super) fn recycle<T>(
        slot: &'static LocalKey<Slot<T>>,
        arc: Arc<T>,
        before: impl FnOnce(&mut T),
    ) {
        if let Some(shell) = Shell::recycle(arc, before) {
            // S3: a shell already there is freed by the `set`.
            let _ = slot.try_with(|slot| slot.set(Some(shell)));
        }
    }
}

/// A descriptor for `work`, in a kept allocation when this thread has one,
/// with the record buffer it kept. `blocking`: see
/// [`Descriptor::is_blocking`].
pub(super) fn descriptor(work: Work, blocking: bool) -> RingArc<Descriptor> {
    #[cfg(not(loom))]
    {
        let record = kept::RECORD
            .try_with(|kept| {
                let (record, prev) = kept.take();
                kept.set((Vec::new(), prev));
                record
            })
            .unwrap_or_default();
        kept::make(
            &kept::DESCRIPTOR,
            Descriptor::with_record(work, blocking, record),
        )
    }
    #[cfg(loom)]
    {
        RingArc::new(Descriptor::new(work, blocking))
    }
}

/// Test-only: the capacity and the address of the record buffer this thread
/// kept for its next descriptor.
#[cfg(all(test, not(loom)))]
pub(super) fn record_buffer() -> (usize, *const u8) {
    kept::RECORD.with(|kept| {
        let (record, prev) = kept.take();
        let seen = (record.capacity(), record.as_ptr());
        kept.set((record, prev));
        seen
    })
}

/// Let go of `desc`, a commit's descriptor its owner is done with, keeping
/// its allocation and its record buffer when nobody else holds it.
pub(super) fn keep_descriptor(desc: RingArc<Descriptor>) {
    #[cfg(not(loom))]
    kept::recycle(&kept::DESCRIPTOR, desc, |desc| {
        let mut record = desc.take_record();
        let _ = kept::RECORD.try_with(|kept| {
            let (_, prev) = kept.take();
            let used = record.len();
            super::trim_stage(&mut record, prev, used);
            record.clear();
            kept.set((record, used));
        });
    });
    #[cfg(loom)]
    drop(desc);
}

/// `decision` as the chain holds it, in a kept allocation when this thread
/// has one.
pub(super) fn decision(decision: Decision) -> RingArc<Decision> {
    #[cfg(not(loom))]
    {
        kept::make(&kept::DECISION, decision)
    }
    #[cfg(loom)]
    {
        RingArc::new(decision)
    }
}

/// Let go of `decision`: one that lost its install, or that the tip moved
/// past. Its allocation is kept when nobody else holds it.
pub(super) fn keep_decision(decision: RingArc<Decision>) {
    #[cfg(not(loom))]
    kept::recycle(&kept::DECISION, decision, |_| {});
    #[cfg(loom)]
    drop(decision);
}

/// A round whose job runs `body`, both in kept allocations when this thread
/// has them.
pub(super) fn round(body: Box<dyn JobBody>) -> RingArc<Round> {
    #[cfg(not(loom))]
    {
        let job = kept::make(&kept::JOB, Job::fresh(body));
        kept::make(&kept::ROUND, Round::fresh(job))
    }
    #[cfg(loom)]
    {
        RingArc::new(Round::fresh(Job::new(body)))
    }
}

/// Let go of `round`, one that was run and replaced, keeping its allocation
/// and its job's when nobody else holds them.
pub(super) fn keep_round(round: RingArc<Round>) {
    #[cfg(not(loom))]
    if let Some(round) = kept::keep(&kept::ROUND, round) {
        drop(kept::keep(&kept::JOB, round.into_job()));
    }
    #[cfg(loom)]
    drop(round);
}

#[cfg(all(test, not(loom)))]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::shell::Shell;

    /// Counts its drops.
    struct Counted(Arc<AtomicUsize>, u64);

    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_shared_value_is_handed_back_untouched() {
        let drops = Arc::new(AtomicUsize::new(0));
        let arc = Arc::new(Counted(Arc::clone(&drops), 7));
        let other = Arc::clone(&arc);
        let Err(back) = Shell::take(arc) else {
            panic!("a shared value was taken");
        };
        assert_eq!(back.1, 7);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(back);
        drop(other);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_weak_reference_keeps_a_value_from_being_taken() {
        let arc = Arc::new(5u64);
        let weak = Arc::downgrade(&arc);
        assert!(Shell::take(arc).is_err());
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn the_last_reference_gives_its_value_once_and_its_allocation_again() {
        let drops = Arc::new(AtomicUsize::new(0));
        let arc = Arc::new(Counted(Arc::clone(&drops), 1));
        let address = Arc::as_ptr(&arc) as usize;
        let Ok((shell, value)) = Shell::take(arc) else {
            panic!("the last reference was not taken");
        };
        assert_eq!(value.1, 1);
        assert_eq!(drops.load(Ordering::SeqCst), 0, "moved out, not dropped");
        drop(value);
        assert_eq!(drops.load(Ordering::SeqCst), 1);

        let again = shell.fill(Counted(Arc::clone(&drops), 2));
        assert_eq!(again.1, 2);
        assert_eq!(Arc::as_ptr(&again) as usize, address, "the same allocation");
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        drop(again);
        assert_eq!(drops.load(Ordering::SeqCst), 2, "each value dropped once");
    }

    #[test]
    fn a_recycled_value_is_dropped_where_it_lies_and_its_allocation_serves_again() {
        let drops = Arc::new(AtomicUsize::new(0));
        let arc = Arc::new(Counted(Arc::clone(&drops), 1));
        let address = Arc::as_ptr(&arc) as usize;
        let shared = Arc::clone(&arc);
        assert!(
            Shell::recycle(arc, |_| {}).is_none(),
            "a shared value was recycled"
        );
        assert_eq!(
            drops.load(Ordering::SeqCst),
            0,
            "a shared value was dropped"
        );
        let Some(shell) = Shell::recycle(shared, |_| {}) else {
            panic!("the last reference was not recycled");
        };
        assert_eq!(drops.load(Ordering::SeqCst), 1, "dropped once, in place");
        let again = shell.fill(Counted(Arc::clone(&drops), 2));
        assert_eq!(Arc::as_ptr(&again) as usize, address, "the same allocation");
        drop(again);
        assert_eq!(drops.load(Ordering::SeqCst), 2, "each value dropped once");
    }

    /// A value written on another thread, which then let go of its
    /// reference: the counts say the last reference is unique only with what
    /// that thread did before letting go visible here. Under miri a fence
    /// that does not pair with the release decrement is a data race on the
    /// value.
    #[test]
    fn a_value_let_go_of_on_another_thread_is_taken_with_its_writes() {
        struct Plain(std::cell::UnsafeCell<u64>);
        // SAFETY: the test writes the cell on one thread and reads it on
        // another only after the counts say the writer let go.
        unsafe impl Sync for Plain {}
        let arc = Arc::new(Plain(std::cell::UnsafeCell::new(0)));
        let other = Arc::clone(&arc);
        assert!(!super::shell::unique(&arc), "two strong references");
        let writer = std::thread::spawn(move || {
            // SAFETY: nobody reads the cell until this reference is gone.
            unsafe { *other.0.get() = 42 };
            drop(other);
        });
        while !super::shell::unique(&arc) {
            std::thread::yield_now();
        }
        let Ok((_shell, value)) = Shell::take(arc) else {
            panic!("the last reference was not taken");
        };
        assert_eq!(value.0.into_inner(), 42, "the writer's write was lost");
        writer.join().unwrap();
    }

    #[test]
    fn a_weak_reference_keeps_a_value_from_being_recycled() {
        let arc = Arc::new(5u64);
        let weak = Arc::downgrade(&arc);
        assert!(!super::shell::unique(&arc), "a weak reference is out");
        assert!(Shell::recycle(arc, |_| {}).is_none());
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn a_shell_let_go_drops_no_value() {
        let drops = Arc::new(AtomicUsize::new(0));
        let arc = Arc::new(Counted(Arc::clone(&drops), 1));
        let Ok((shell, value)) = Shell::take(arc) else {
            panic!("the last reference was not taken");
        };
        drop(shell);
        assert_eq!(drops.load(Ordering::SeqCst), 0, "a shell holds no value");
        drop(value);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_thread_keeps_one_shell_and_uses_it_for_its_next_value() {
        use std::cell::Cell;

        std::thread_local! {
            static SLOT: super::kept::Slot<u64> = const { Cell::new(None) };
        }
        let first = super::kept::make(&SLOT, 1);
        let second = super::kept::make(&SLOT, 2);
        let kept_address = Arc::as_ptr(&second) as usize;
        assert_eq!(super::kept::keep(&SLOT, first), Some(1));
        // S3: the second shell takes the first one's place.
        assert_eq!(super::kept::keep(&SLOT, second), Some(2));
        let third = super::kept::make(&SLOT, 3);
        assert_eq!(*third, 3);
        assert_eq!(Arc::as_ptr(&third) as usize, kept_address);
        let fourth = super::kept::make(&SLOT, 4);
        assert_ne!(Arc::as_ptr(&fourth) as usize, kept_address, "no shell left");

        let shared = Arc::clone(&third);
        assert_eq!(super::kept::keep(&SLOT, third), None, "still shared");
        assert_eq!(*shared, 3);
    }
}
