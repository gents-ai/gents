//! The commit ring: where every commit takes its place in the order, with
//! no lock (plan 4.7; `proofs/tla/CommitPipeline.tla`).
//!
//! A committer reserves a slot index with one atomic add on the ring's tail
//! and then installs its descriptor in the cell that index maps to. Every
//! later stage of the pipeline walks the indices in order. This module is
//! only the ring: who holds which index, which cell holds what, and the two
//! frontiers every thread advances together. What a slot means is
//! `pipeline.rs`.
//!
//! # A slot's life
//!
//! ```text
//! reserved --install--> READY --retire--> DONE
//!     \---void--------> VOID
//! ```
//!
//! - **Reserved.** The index is handed out and its cell still shows the
//!   index one lap earlier, finished. The owner has not installed yet.
//! - **READY.** The owner's descriptor is in the cell. Any thread may load
//!   it and work on the slot.
//! - **VOID.** A thread that reached the slot in ring order found it still
//!   only reserved, and voided it with one compare-and-swap of the cell's
//!   stamp. The owner's install then fails, and it reserves again. This is
//!   what stops a thread that stopped between its reservation and its
//!   install from holding every later commit behind it (model action
//!   `Void`).
//! - **DONE.** The slot is published and the cell may take the index one lap
//!   later.
//!
//! # Invariants
//!
//! - **R1, one fate per index.** A cell's stamp moves from "the lap before,
//!   finished" to `(index, READY)` or to `(index, VOID)` by one
//!   compare-and-swap, so an index is installed by its owner or voided by a
//!   helper, never both.
//! - **R2, the stamp fits one 64-bit word on every target** (D17): the slot
//!   index in the upper 62 bits and the phase in the lower two. Nothing in
//!   the ring needs a wider compare-and-swap, so armv7 and wasm32 take no
//!   emulation lock here.
//! - **R3, a loaded descriptor is alive.** A cell hands out its descriptor
//!   as a new `Arc`, taken while the loader is counted in the cell's `pins`.
//!   The cell's own reference is dropped only by a thread that moved the
//!   holder word to `LOCKED`, after the stamp said the slot is over
//!   (finished, voided or passed, none of which is undone), and then read
//!   `pins` as zero: a loader that validated the stamp earlier is still
//!   counted, and one that pins later fails the stamp check and never
//!   touches the pointer. A stamp that has not reached the slot yet is an
//!   install under way and is left alone. All four accesses are `SeqCst`,
//!   which is what makes "one of the two sees the other" hold
//!   (`sync::internal::seq_cst_handshake` says how loom is shown that).
//!   The pointer and the holder word are written only by the thread that
//!   locked the holder word, and read after an acquire of what that thread
//!   released (the stamp, or the holder word itself), so their stores are
//!   `Release` or `Relaxed`, not locked exchanges.
//! - **R4, nobody waits for a cell.** A thread that finds a cell's holder
//!   word busy (another thread is inside the few instructions of an install,
//!   a load or a clean, or stopped there) does not wait: an owner gives its
//!   index up, voiding it, and reserves another; a cleaner leaves the cell
//!   for the next thread that passes. A void never touches the holder word,
//!   so a stopped thread cannot stop a slot from being voided. A slot whose
//!   publisher stopped before retiring it is retired by the thread that
//!   needs its cell a lap later.
//! - **R5, the frontiers only grow.** `decided` and `published` advance by
//!   compare-and-swap from one index to the next, so every slot is passed
//!   exactly once and in ring order, whichever threads do the passing. A
//!   rollback freezes both with a bit of the same word, so a pass that was
//!   under way when it began fails, and moves them itself when it is done.
//!
//! - **R6, a void slot does not keep its cell.** A cell holding a void slot
//!   takes the index one lap later, voided or installed, whether or not the
//!   frontiers have passed the void one: nothing is held for it. A frontier
//!   that reaches an index whose cell has moved on therefore passes it
//!   ([`Loaded::Gone`]): only a void slot's cell moves on before the
//!   frontiers pass, since a cell holding a descriptor is finished by the
//!   publisher, after both did.
//!
//! # The tip
//!
//! [`ArcSlot`] holds one `Arc` that any thread loads and that is replaced by
//! compare-and-swap: the newest decision of the pipeline (`pipeline.rs`), and
//! the current sync round. It is the same pin protocol as a cell's (R3) with
//! a list for the values a replacement could not drop at once.

#![allow(unsafe_code)]

use std::ptr;

use crate::sync::internal::{
    Arc, AtomicPtr, AtomicU64, AtomicUsize, Ordering, seq_cst_handshake, take_ptr,
};

/// The slot is published; the cell is free for the index one lap later.
const DONE: u64 = 0;
/// The slot's descriptor is installed.
const READY: u64 = 1;
/// The slot was voided before its owner installed.
const VOID: u64 = 2;
/// Bits of a stamp holding the phase.
const PHASE_BITS: u32 = 2;

fn stamp(index: u64, phase: u64) -> u64 {
    (index << PHASE_BITS) | phase
}

fn index_of(stamp: u64) -> u64 {
    stamp >> PHASE_BITS
}

fn phase_of(stamp: u64) -> u64 {
    stamp & ((1 << PHASE_BITS) - 1)
}

/// The holder word of a cell with no descriptor in it.
const EMPTY: u64 = 0;
/// The holder word while one thread installs or removes the descriptor.
const LOCKED: u64 = u64::MAX;

/// The holder word of a cell holding the descriptor of slot `index`.
fn held(index: u64) -> u64 {
    (index << 1) | 1
}

/// The bit a rollback sets in a frontier while it discards the commits past
/// it: the frontier does not move meanwhile.
const FROZEN: u64 = 1 << 63;

/// What a cell holds for one slot index.
pub(super) enum Loaded<T> {
    /// The owner's descriptor.
    Ready(Arc<T>),
    /// The slot was voided: it holds nothing and decides nothing.
    Void,
    /// Reserved and not installed yet: a helper voids it.
    Empty,
    /// The cell still holds the slot one lap earlier, unpublished: the ring
    /// is full, and that slot must be published first.
    Full,
    /// The slot is finished, or was void, and its cell moved on (R6).
    Gone,
}

/// What an install did.
pub(super) enum Installed {
    /// The descriptor is in the cell and the slot is READY.
    Done,
    /// The index is lost: a helper voided it, or the cell was busy and this
    /// thread voided it itself (R4). Reserve another.
    Lost,
    /// The cell still holds the slot one lap earlier, unpublished.
    Full,
}

struct Cell<T> {
    /// `(index << 2) | phase`: the newest slot index this cell took, and
    /// how far that slot got (R1, R2).
    stamp: AtomicU64,
    /// [`EMPTY`], [`LOCKED`], or [`held`] for the slot whose descriptor
    /// `desc` points at.
    holder: AtomicU64,
    /// Loads between their count in and their count out (R3).
    pins: AtomicUsize,
    /// The cell's reference to the descriptor, as `Arc::into_raw` gave it.
    desc: AtomicPtr<T>,
}

/// A bounded ring of slots. `T` is the descriptor a slot holds.
pub(super) struct Ring<T> {
    cells: Box<[Cell<T>]>,
    /// The next index to hand out.
    tail: AtomicU64,
    /// Every slot at or below this index is decided; the top bit is
    /// [`FROZEN`].
    decided: AtomicU64,
    /// Every slot at or below this index is published; the top bit is
    /// [`FROZEN`].
    published: AtomicU64,
}

// SAFETY: a cell hands its descriptor out as `Arc<T>` clones (R3), so
// sharing the ring shares `T` between threads and may drop one on any of
// them: `T: Send + Sync` is what `Arc<T>` itself asks for.
unsafe impl<T: Send + Sync> Send for Ring<T> {}
// SAFETY: as above; every other field is an atomic.
unsafe impl<T: Send + Sync> Sync for Ring<T> {}

impl<T> Ring<T> {
    /// A ring of `capacity` cells, a power of two, with nothing in flight.
    /// Indices start at `capacity`, so every cell begins as "the lap before,
    /// finished" without a special case.
    pub(super) fn new(capacity: usize) -> Self {
        let capacity = capacity.next_power_of_two().max(2);
        let cells = (0..capacity as u64)
            .map(|index| Cell {
                stamp: AtomicU64::new(stamp(index, DONE)),
                holder: AtomicU64::new(EMPTY),
                pins: AtomicUsize::new(0),
                desc: AtomicPtr::new(ptr::null_mut()),
            })
            .collect();
        Self {
            cells,
            tail: AtomicU64::new(capacity as u64),
            decided: AtomicU64::new(capacity as u64 - 1),
            published: AtomicU64::new(capacity as u64 - 1),
        }
    }

    /// Slots the ring holds at once.
    pub(super) fn capacity(&self) -> u64 {
        self.cells.len() as u64
    }

    fn cell(&self, index: u64) -> &Cell<T> {
        &self.cells[(index & (self.capacity() - 1)) as usize]
    }

    /// Take the next slot index: one atomic add (model action `Reserve`).
    pub(super) fn reserve(&self) -> u64 {
        self.tail.fetch_add(1, Ordering::SeqCst)
    }

    /// One past the newest index handed out.
    pub(super) fn tail(&self) -> u64 {
        self.tail.load(Ordering::SeqCst)
    }

    /// Every slot at or below this index is decided.
    pub(super) fn decided(&self) -> u64 {
        self.decided.load(Ordering::SeqCst) & !FROZEN
    }

    /// Whether slot `index` is the one to decide now: the frontier stands
    /// just before it and no rollback froze it.
    pub(super) fn is_next_to_decide(&self, index: u64) -> bool {
        self.decided.load(Ordering::SeqCst) == index - 1
    }

    /// Whether a rollback froze the decided frontier.
    pub(super) fn is_deciding_frozen(&self) -> bool {
        self.decided.load(Ordering::SeqCst) & FROZEN != 0
    }

    /// Move the decided frontier from `index - 1` to `index` (R5). `false`
    /// when another thread already moved it or a rollback froze it.
    pub(super) fn pass_decided(&self, index: u64) -> bool {
        self.decided
            .compare_exchange(index - 1, index, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Stop the decided frontier where it stands and return that index.
    /// Only the thread running a rollback calls this; it then moves the
    /// frontier itself with [`Self::thaw_decided`].
    pub(super) fn freeze_decided(&self) -> u64 {
        self.decided.fetch_or(FROZEN, Ordering::SeqCst) & !FROZEN
    }

    /// Set the decided frontier to `index` and let deciding go on.
    pub(super) fn thaw_decided(&self, index: u64) {
        self.decided.store(index, Ordering::SeqCst);
    }

    /// Every slot at or below this index is published.
    pub(super) fn published(&self) -> u64 {
        self.published.load(Ordering::SeqCst) & !FROZEN
    }

    /// Whether a rollback froze publication.
    pub(super) fn is_frozen(&self) -> bool {
        self.published.load(Ordering::SeqCst) & FROZEN != 0
    }

    /// Move the published frontier from `index - 1` to `index` (R5). `false`
    /// when another thread already moved it or a rollback froze it.
    pub(super) fn pass_published(&self, index: u64) -> bool {
        self.published
            .compare_exchange(index - 1, index, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Stop publication where it stands and return that index. Only the
    /// thread running a rollback calls this; it then moves the frontier
    /// itself with [`Self::thaw`].
    pub(super) fn freeze(&self) -> u64 {
        self.published.fetch_or(FROZEN, Ordering::SeqCst) & !FROZEN
    }

    /// Set the published frontier to `index` and let publication go on.
    pub(super) fn thaw(&self, index: u64) {
        self.published.store(index, Ordering::SeqCst);
    }

    /// Set the frozen frontier to `index`, still frozen.
    pub(super) fn set_frozen(&self, index: u64) {
        self.published.store(index | FROZEN, Ordering::SeqCst);
    }

    /// Install `desc` as slot `index`, which this thread reserved (model
    /// action `Fill`).
    pub(super) fn install(&self, index: u64, desc: &Arc<T>) -> Installed {
        let cell = self.cell(index);
        let before = index - self.capacity();
        let seen = loop {
            let seen = cell.stamp.load(Ordering::SeqCst);
            if index_of(seen) >= index {
                // Only a void can have stamped this index before its owner;
                // a later index means the void happened a lap ago.
                return Installed::Lost;
            }
            if index_of(seen) == before && phase_of(seen) != READY {
                break seen;
            }
            if !self.retire_published(before, seen) {
                return Installed::Full;
            }
        };
        if cell
            .holder
            .compare_exchange(EMPTY, LOCKED, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            // The descriptor of an earlier lap is still here. Take it out if
            // nothing holds it; otherwise give the index up (R4).
            if !self.clean(cell) || !self.lock_empty(cell) {
                self.void_cell(cell, index, seen);
                return Installed::Lost;
            }
        }
        // The holder word is LOCKED by this thread, so nobody else writes
        // the pointer or the holder word until it is let go below. Neither
        // store is part of R3's handshake, and neither needs `SeqCst` (a
        // locked exchange each): a loader reads them only after it read the
        // stamp READY, which the compare-and-swap below releases, and a
        // cleaner only after its compare-and-swap of the holder word took
        // this `Release` store's value.
        let raw = Arc::into_raw(Arc::clone(desc)).cast_mut();
        cell.desc.store(raw, Ordering::Relaxed);
        cell.holder.store(held(index), Ordering::Release);
        if cell
            .stamp
            .compare_exchange(
                seen,
                stamp(index, READY),
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            return Installed::Done;
        }
        // A helper voided the slot while the descriptor went in (R1). The
        // cell's reference comes back out, by whichever thread gets to it.
        self.clean(cell);
        Installed::Lost
    }

    fn lock_empty(&self, cell: &Cell<T>) -> bool {
        cell.holder
            .compare_exchange(EMPTY, LOCKED, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Void slot `index`, reserved and not installed (model action `Void`).
    /// `true` when the slot is void afterwards; `false` when its owner
    /// installed first, or the cell is not at this slot.
    pub(super) fn void(&self, index: u64) -> bool {
        let cell = self.cell(index);
        let seen = cell.stamp.load(Ordering::SeqCst);
        if index_of(seen) == index {
            return phase_of(seen) == VOID;
        }
        if index_of(seen) != index - self.capacity() || phase_of(seen) == READY {
            return false;
        }
        self.void_cell(cell, index, seen)
    }

    fn void_cell(&self, cell: &Cell<T>, index: u64, seen: u64) -> bool {
        match cell.stamp.compare_exchange(
            seen,
            stamp(index, VOID),
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => true,
            Err(now) => now == stamp(index, VOID),
        }
    }

    /// Finish the slot a lap before, `before`, when the cell still shows it
    /// READY as `seen` and the published frontier is past it: the thread
    /// that published it stopped before it retired it, and nobody waits for
    /// that thread (R4). `true` when the cell moved on.
    fn retire_published(&self, before: u64, seen: u64) -> bool {
        if seen != stamp(before, READY) || self.published() < before {
            return false;
        }
        self.retire(before);
        true
    }

    /// What the ring holds for slot `index`.
    pub(super) fn load(&self, index: u64) -> Loaded<T> {
        loop {
            let (loaded, seen) = self.load_once(index);
            let stale = matches!(loaded, Loaded::Full)
                && index >= self.capacity()
                && self.retire_published(index - self.capacity(), seen);
            if !stale {
                return loaded;
            }
        }
    }

    /// One read of the cell of slot `index`: what it holds, and the stamp
    /// that says so.
    fn load_once(&self, index: u64) -> (Loaded<T>, u64) {
        let cell = self.cell(index);
        // R3: counted in before the stamp is read.
        cell.pins.fetch_add(1, Ordering::SeqCst);
        seq_cst_handshake();
        let seen = cell.stamp.load(Ordering::SeqCst);
        let loaded = if seen == stamp(index, READY) {
            if cell.holder.load(Ordering::SeqCst) == held(index) {
                let raw = cell.desc.load(Ordering::SeqCst).cast_const();
                // SAFETY (R3): the stamp names this slot as READY and the
                // holder word names its descriptor, both read while this
                // thread is counted in `pins`, so the cell still owns the
                // reference `raw` came from; a new reference is taken for
                // the caller.
                Loaded::Ready(unsafe {
                    Arc::increment_strong_count(raw);
                    Arc::from_raw(raw)
                })
            } else {
                // The stamp moved on between the two reads.
                Loaded::Gone
            }
        } else if index_of(seen) == index {
            if phase_of(seen) == VOID {
                Loaded::Void
            } else {
                Loaded::Gone
            }
        } else if index_of(seen) > index {
            Loaded::Gone
        } else if phase_of(seen) == READY {
            Loaded::Full
        } else {
            Loaded::Empty
        };
        cell.pins.fetch_sub(1, Ordering::SeqCst);
        (loaded, seen)
    }

    /// Mark slot `index` finished, once it is published, and drop the cell's
    /// reference to its descriptor if nothing is loading it now. Any thread
    /// may, any number of times.
    pub(super) fn retire(&self, index: u64) {
        let cell = self.cell(index);
        if cell
            .stamp
            .compare_exchange(
                stamp(index, READY),
                stamp(index, DONE),
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            self.clean(cell);
        }
    }

    /// Take the descriptor of a finished or voided slot out of `cell` and
    /// drop the cell's reference to it. `true` when the cell holds none
    /// afterwards; `false` when a load is under way or another thread is in
    /// the cell (R4): the next thread to pass cleans it.
    fn clean(&self, cell: &Cell<T>) -> bool {
        let holder = cell.holder.load(Ordering::SeqCst);
        if holder == EMPTY {
            return true;
        }
        if holder == LOCKED {
            return false;
        }
        let of = holder >> 1;
        let seen = cell.stamp.load(Ordering::SeqCst);
        // The descriptor may go only once the stamp says its slot is over:
        // finished, voided under its owner's install, or passed. None of
        // those is ever undone, so the answer still holds once the holder
        // word is locked below. A stamp still at the lap before is an
        // install under way: the descriptor is in the cell and the stamp
        // not yet moved, and whether the slot becomes READY is the owner's
        // compare-and-swap to decide, so the descriptor stays (R3).
        let over = index_of(seen) > of || (index_of(seen) == of && phase_of(seen) != READY);
        if !over {
            return false;
        }
        if cell
            .holder
            .compare_exchange(holder, LOCKED, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        // R3: the stamp said the slot is over before the pins are read.
        seq_cst_handshake();
        if cell.pins.load(Ordering::SeqCst) != 0 {
            cell.holder.store(holder, Ordering::Release);
            return false;
        }
        // The holder word is LOCKED by this thread and no load is counted
        // in, so nobody else reads or writes the pointer: a plain read and
        // write, not a locked exchange. The next install reads the pointer
        // only after its compare-and-swap of the holder word took the
        // `Release` store of EMPTY, and overwrites it.
        let raw = cell.desc.load(Ordering::Relaxed);
        cell.desc.store(ptr::null_mut(), Ordering::Relaxed);
        cell.holder.store(EMPTY, Ordering::Release);
        if !raw.is_null() {
            // SAFETY (R3): the holder word was LOCKED by this thread with no
            // load counted in, so `raw` is the cell's own reference, taken
            // out exactly once.
            drop(unsafe { Arc::from_raw(raw.cast_const()) });
        }
        true
    }
}

impl<T> Drop for Ring<T> {
    fn drop(&mut self) {
        // A plain take per cell, not a locked swap: a ring is dropped with
        // every engine, and its hundreds of cells made that a cost of a close.
        for cell in self.cells.iter_mut() {
            let raw = take_ptr(&mut cell.desc);
            if !raw.is_null() {
                // SAFETY: `&mut self` excludes every other thread, and a
                // non-null pointer is a reference the cell still owns.
                drop(unsafe { Arc::from_raw(raw.cast_const()) });
            }
        }
    }
}

/// A value an [`ArcSlot`] can hold: it carries the link of the list a
/// replaced value waits on.
pub(super) trait Linked: Sized {
    /// The next replaced value waiting to be dropped. Touched only by the
    /// slot.
    fn link(&self) -> &AtomicPtr<Self>;

    /// Let go of the slot's reference to a replaced value. A kind whose
    /// allocation a thread keeps for its next value (`spare.rs`) says so
    /// here.
    fn retired(value: Arc<Self>) {
        drop(value);
    }
}

/// One `Arc<T>` that any thread loads and that is replaced by
/// compare-and-swap. See the module docs.
///
/// A replaced value is dropped at once when no load is under way, and
/// otherwise at a later replacement that finds none. A thread stopped inside
/// a load (a few instructions) therefore keeps the values replaced meanwhile
/// alive, one per replacement, until it runs again; it never stops another
/// thread.
pub(super) struct ArcSlot<T: Linked> {
    current: AtomicPtr<T>,
    pins: AtomicUsize,
    /// Replaced values not yet dropped, linked through [`Linked::link`].
    limbo: AtomicPtr<T>,
}

// SAFETY: the slot hands out `Arc<T>` clones and may drop a `T` on any
// thread, which is what `Arc<T>: Send + Sync` asks of `T`.
unsafe impl<T: Linked + Send + Sync> Send for ArcSlot<T> {}
// SAFETY: as above; every field is an atomic.
unsafe impl<T: Linked + Send + Sync> Sync for ArcSlot<T> {}

impl<T: Linked> ArcSlot<T> {
    pub(super) fn new(value: Arc<T>) -> Self {
        Self {
            current: AtomicPtr::new(Arc::into_raw(value).cast_mut()),
            pins: AtomicUsize::new(0),
            limbo: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// The value the slot holds now.
    pub(super) fn load(&self) -> Arc<T> {
        self.pins.fetch_add(1, Ordering::SeqCst);
        seq_cst_handshake();
        let raw = self.current.load(Ordering::SeqCst).cast_const();
        // SAFETY: `raw` was the slot's value while this thread was counted
        // in `pins`, and a replaced value is dropped only after `pins` was
        // read as zero later than its replacement, so the slot's reference
        // to it is still held; a new one is taken for the caller.
        let value = unsafe {
            Arc::increment_strong_count(raw);
            Arc::from_raw(raw)
        };
        self.pins.fetch_sub(1, Ordering::SeqCst);
        value
    }

    /// Whether `value` is the value the slot holds now: what a [`Self::load`]
    /// made now would return, with no pin and no new reference. The caller
    /// holds `value`, so its address cannot have been reused by another value
    /// in between.
    pub(super) fn holds(&self, value: &Arc<T>) -> bool {
        ptr::eq(self.current.load(Ordering::SeqCst), Arc::as_ptr(value))
    }

    /// Replace `expected` with `next`, when `expected` is still the value
    /// the slot holds. The caller holds `expected`, so its address cannot
    /// have been reused by another value in between.
    pub(super) fn replace(&self, expected: &Arc<T>, next: Arc<T>) -> bool {
        let next = Arc::into_raw(next).cast_mut();
        match self.current.compare_exchange(
            Arc::as_ptr(expected).cast_mut(),
            next,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(old) => {
                // The common case: no load under way and nothing waiting on
                // the list. A load that could hold `old`'s address without a
                // reference of its own counted itself in `pins` before it
                // read `current`, so before the swap above, and is still
                // counted when `pins` is read after it (the argument
                // `collect` makes). Read as zero, `old` goes at once, with
                // no trip through the list: three locked instructions fewer
                // on every commit.
                seq_cst_handshake();
                if self.limbo.load(Ordering::SeqCst).is_null()
                    && self.pins.load(Ordering::SeqCst) == 0
                {
                    // SAFETY: the slot's own reference to the value it held,
                    // swapped out above, on no list, with no load that could
                    // hold its address.
                    T::retired(unsafe { Arc::from_raw(old.cast_const()) });
                } else {
                    self.retire(old);
                    self.collect();
                }
                true
            }
            Err(_) => {
                // SAFETY: `next` came from `Arc::into_raw` above and was not
                // published.
                drop(unsafe { Arc::from_raw(next.cast_const()) });
                false
            }
        }
    }

    /// Put a replaced value on the list of those waiting to be dropped.
    fn retire(&self, old: *mut T) {
        let mut head = self.limbo.load(Ordering::SeqCst);
        loop {
            // SAFETY: the slot still owns its reference to `old`, and only
            // the thread retiring a value writes its link.
            unsafe { (*old).link() }.store(head, Ordering::SeqCst);
            match self
                .limbo
                .compare_exchange(head, old, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return,
                Err(now) => head = now,
            }
        }
    }

    /// Drop every replaced value, if no load is under way.
    fn collect(&self) {
        let mut taken = self.limbo.swap(ptr::null_mut(), Ordering::SeqCst);
        if taken.is_null() {
            return;
        }
        // Every value in `taken` was replaced before this read. A load that
        // could still hold one of their addresses without a reference of
        // its own read `current` before that replacement, so it is still
        // counted here.
        seq_cst_handshake();
        if self.pins.load(Ordering::SeqCst) != 0 {
            while !taken.is_null() {
                // SAFETY: as in `retire`: owned by the slot, link written by
                // the thread holding the value off the list.
                let next = unsafe { (*taken).link() }.load(Ordering::SeqCst);
                self.retire(taken);
                taken = next;
            }
            return;
        }
        while !taken.is_null() {
            // SAFETY: as above; read before the value is dropped.
            let next = unsafe { (*taken).link() }.load(Ordering::SeqCst);
            // SAFETY: the slot's own reference, off every list, with no
            // load that could hold its address (see above).
            T::retired(unsafe { Arc::from_raw(taken.cast_const()) });
            taken = next;
        }
    }
}

impl<T: Linked> Drop for ArcSlot<T> {
    fn drop(&mut self) {
        // `&mut self`: no load is under way.
        self.collect();
        let raw = self.current.swap(ptr::null_mut(), Ordering::SeqCst);
        if !raw.is_null() {
            // SAFETY: the slot's own reference to its last value.
            drop(unsafe { Arc::from_raw(raw.cast_const()) });
        }
    }
}

#[cfg(all(test, not(loom)))]
#[path = "ring_tests.rs"]
mod tests;
