//! The slot table: every descriptor [`super::OpenFileLimit`] holds lives in
//! one of `capacity` slots, so it never holds more, and a descriptor is
//! closed only by the thread that took its slot with one compare-and-swap,
//! which succeeds only while no reader uses it.
//!
//! # A slot's state word
//!
//! ```text
//! bits 0..32   readers using the descriptor right now
//! bit  32      REF, the CLOCK reference bit, set by every read
//! bit  33      DRAIN, set by a thread that found no free slot: no new reader
//!              joins, and that thread takes the slot once the readers leave
//! bits 34..36  the phase: EMPTY, CLAIMED (one thread owns the slot and its
//!              descriptor exclusively) or OPEN (readers may join)
//! bit  36      WANTED, set by a reopen that may not wait and found the slot
//!              busy: the step that makes the slot claimable again clears it
//!              and wakes every parked waiter
//! ```
//!
//! # Invariants
//!
//! - **At most `capacity` descriptors.** A descriptor exists only inside a
//!   slot, put there by the slot's claimer, and the claimer closes the old
//!   one before it opens the new one.
//! - **Never closed in use.** A claim is one CAS from `OPEN` with zero
//!   readers (or from `EMPTY`) to `CLAIMED`, and a reader joins by one CAS
//!   that requires `OPEN`; the two CAS the same word, so exactly one of
//!   them wins, and a claimed slot takes no reader until it is `OPEN` again.
//! - **A reader reads its own file.** A slot's owner (the record whose file
//!   it holds) changes only while `CLAIMED`. A reader that joined checks the
//!   owner again after its CAS: joined, the owner cannot change under it.
//! - **A starving `Blocking` reader is served.** A thread that sweeps every
//!   slot twice without a claim (every slot is mid-read) sets `DRAIN` on
//!   one: the slot takes no new reader, and its current readers each finish
//!   one read. This is the only wait, it is on reads already running, and
//!   only a `Blocking` read takes it (D60).
//! - **A reopen that may not wait never waits** (D60). The unit a `CacheOnly`
//!   read queued sweeps once ([`SlotTable::load_or_park`]); finding no slot,
//!   it parks ([`SlotTable::park`]) and returns.
//! - **No parked waiter is forgotten.** A park first pushes its waiter on
//!   `parked`, then looks at every slot again: one it may claim, it claims;
//!   on every busy one it sets `WANTED`. Every write to a state word keeps
//!   `WANTED`, except the one step that makes a wanted slot claimable (a
//!   reader's leave, an unwinding claim, a closing owner): that step clears
//!   it and wakes every waiter on `parked`. The push comes before the look
//!   and the clear before the take, so a slot that frees after the park
//!   looked at it either shows its waiter on `parked` or was seen free by
//!   the park itself.
//!
//! `OpenFileTable.tla` and `Regolith/OpenFileTable.lean` model this protocol
//! (`Regolith/OpenFileNoWait.lean` the park and the wake);
//! `open_file_limit::loom_model` checks it under every interleaving loom permits.

#![allow(unsafe_code)]

use std::io;
use std::sync::Arc;

use crate::engine::io::stack::Stack;
use crate::env::ReadFile;
use crate::sync::internal::{AtomicU64, AtomicUsize, Ordering, UnsafeCell};

/// Readers using a slot's descriptor.
const READERS: u64 = (1 << 32) - 1;
/// The CLOCK reference bit.
const REF: u64 = 1 << 32;
/// A starving claimer reserved the slot; no reader may join.
const DRAIN: u64 = 1 << 33;
const PHASE: u64 = 0b11 << 34;
const EMPTY: u64 = 0;
const CLAIMED: u64 = 1 << 34;
const OPEN: u64 = 2 << 34;
/// A parked waiter needs this slot: the step that makes it claimable wakes
/// `SlotTable::parked`.
const WANTED: u64 = 1 << 36;
/// What `fill` adds to a `CLAIMED` word to publish it `OPEN`, referenced and
/// held by its one reader, keeping `WANTED`. While `CLAIMED` the readers,
/// `REF` and `DRAIN` are zero (the claim CAS wrote them so, and only the
/// claimer writes them until it publishes), so the add touches nothing else.
const PUBLISH: u64 = (OPEN - CLAIMED) + REF + 1;

/// No record owns a slot.
pub(crate) const NO_OWNER: u64 = 0;

/// A reopen that may not wait and found every slot busy: told when a slot
/// it marked wanted is free to claim (D60).
pub(crate) trait SlotWaiter: Send + Sync {
    /// A slot this waiter wanted can be claimed now. Runs on the thread that
    /// freed the slot, in the middle of its read's return, so it must be
    /// quick and must never wait.
    fn slot_freed(&self);
}

/// Whether `state` is a slot a claim may take: empty, or open with no
/// reader and no drain. The CLOCK bit only orders claims, so it is ignored.
fn claimable(state: u64) -> bool {
    match state & PHASE {
        EMPTY => true,
        OPEN => state & (READERS | DRAIN) == 0,
        _ => false,
    }
}

/// One descriptor's place in the table.
pub(super) struct Slot {
    state: AtomicU64,
    /// The record whose file the descriptor is. Written only by the thread
    /// that holds the slot `CLAIMED`.
    owner: AtomicU64,
    /// The descriptor. Written only by the `CLAIMED` holder; read only by
    /// readers counted in `state` while `OPEN`.
    file: UnsafeCell<Option<Arc<dyn ReadFile>>>,
}

// SAFETY: `file` is written only by the one thread that won the slot's
// CAS to `CLAIMED`, and read only by threads counted as readers in `state`,
// which no claim can win while non-zero. The publishing add to `OPEN` is
// Release and every join is an Acquire CAS, so a reader sees the write.
unsafe impl Sync for Slot {}

impl Slot {
    fn new() -> Self {
        Self {
            state: AtomicU64::new(EMPTY),
            owner: AtomicU64::new(NO_OWNER),
            file: UnsafeCell::new(None),
        }
    }
}

/// `capacity` slots and the CLOCK hand that sweeps them.
pub(crate) struct SlotTable {
    slots: Box<[Slot]>,
    hand: AtomicUsize,
    /// Descriptors held right now: one per slot holding one.
    open: AtomicUsize,
    /// Waiters that parked since a wanted slot last freed, woken whole by
    /// the next one that does.
    // vertexia: one node per park, freed at the next wake; it holds at most
    // the reopens parked since a wanted slot last freed, so only a read that
    // never returns lets it grow.
    parked: Stack<Arc<dyn SlotWaiter>>,
    /// The one step a loom calibration plants wrong.
    #[cfg(loom)]
    mutant: Mutant,
}

/// The steps a loom calibration can plant wrong, one at a time, so the
/// models check the production protocol and its calibrations fail on it.
#[cfg(loom)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mutant {
    /// The production protocol.
    None,
    /// A claim takes an open slot whatever its readers.
    IgnoreReaders,
    /// A join trusts its owner check from before the CAS.
    NoOwnerRecheck,
    /// A park marks the busy slots wanted before it puts its waiter on the
    /// parked list.
    MarkBeforeRegister,
    /// Publishing a loaded slot stores its word whole, dropping `WANTED`.
    PublishDropsWanted,
}

/// What [`SlotTable::park`] did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Parking {
    /// Every slot was busy; each is marked wanted, and the waiter is told
    /// when one frees.
    Parked,
    /// A slot freed meanwhile, and this park claimed it: fill it.
    Claimed(usize),
}

/// A reader's hold on one slot's descriptor. Leaving is one decrement.
pub(crate) struct Held<'a> {
    table: &'a SlotTable,
    index: usize,
}

impl Held<'_> {
    /// The descriptor this hold keeps open.
    pub(crate) fn file(&self) -> &dyn ReadFile {
        let slot = &self.table.slots[self.index];
        // SAFETY: this hold counts as a reader of an `OPEN` slot, so no
        // claim can win and nobody writes `file` until it is dropped.
        let file = slot.file.with(|file| unsafe { (*file).as_ref() });
        match file {
            Some(file) => &**file,
            // An `OPEN` slot always holds its descriptor: `install` stores it
            // before it publishes `OPEN`.
            None => unreachable!("an open slot without its descriptor"),
        }
    }

    /// The slot this hold is on, which the record remembers as its hint.
    pub(crate) fn index(&self) -> usize {
        self.index
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        // Release: this reader's use of the descriptor happens before the
        // CAS that may then claim the slot and close it.
        let before = self.table.slots[self.index]
            .state
            .fetch_sub(1, Ordering::Release);
        // The last reader of an open, undrained, wanted slot made it
        // claimable: wake the parked. A drained slot goes to its drainer,
        // whose own read wakes them when it leaves. The acquire this needs
        // (the park pushed its waiter before marking) is `freed`'s own
        // read-modify-write of the word, so a read that frees an unwanted
        // slot pays nothing more than it did before.
        if before & (READERS | DRAIN | PHASE | WANTED) == OPEN | WANTED | 1 {
            self.table.freed(self.index);
        }
    }
}

/// Puts a slot back to `EMPTY` if its claimer unwinds before publishing.
struct Claimed<'a> {
    table: &'a SlotTable,
    index: usize,
    armed: bool,
}

impl Drop for Claimed<'_> {
    fn drop(&mut self) {
        if self.armed {
            let slot = &self.table.slots[self.index];
            slot.owner.store(NO_OWNER, Ordering::Relaxed);
            self.table.settle(slot, EMPTY);
        }
    }
}

impl SlotTable {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            slots: (0..capacity.max(1)).map(|_| Slot::new()).collect(),
            hand: AtomicUsize::new(0),
            open: AtomicUsize::new(0),
            parked: Stack::new(),
            #[cfg(loom)]
            mutant: Mutant::None,
        }
    }

    /// A table with one step planted wrong, for a loom calibration.
    #[cfg(loom)]
    pub(crate) fn with_mutant(capacity: usize, mutant: Mutant) -> Self {
        Self {
            mutant,
            ..Self::new(capacity)
        }
    }

    pub(super) fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Descriptors held right now. Never above [`Self::capacity`].
    pub(crate) fn open_count(&self) -> usize {
        self.open.load(Ordering::Relaxed)
    }

    /// Whether slot `index` is marked wanted by a parked waiter.
    #[cfg(test)]
    pub(crate) fn wanted(&self, index: usize) -> bool {
        self.slots[index].state.load(Ordering::Acquire) & WANTED != 0
    }

    /// Whether a `Blocking` claimer is draining slot `index`.
    #[cfg(test)]
    pub(crate) fn drained(&self, index: usize) -> bool {
        self.slots[index].state.load(Ordering::Acquire) & DRAIN != 0
    }

    /// Join the descriptor slot `index` holds for `owner`, if it still does.
    pub(crate) fn join(&self, index: usize, owner: u64) -> Option<Held<'_>> {
        let slot = self.slots.get(index)?;
        let mut state = slot.state.load(Ordering::Acquire);
        loop {
            if state & PHASE != OPEN
                || state & DRAIN != 0
                || state & READERS == READERS
                || slot.owner.load(Ordering::Acquire) != owner
            {
                return None;
            }
            match slot.state.compare_exchange_weak(
                state,
                (state + 1) | REF,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(now) => state = now,
            }
        }
        let held = Held { table: self, index };
        #[cfg(loom)]
        if self.mutant == Mutant::NoOwnerRecheck {
            return Some(held);
        }
        // Counted as a reader, the owner is fixed: it changes only while
        // `CLAIMED`, which no claim reaches while this count stands. A slot
        // reloaded for another record between the check above and the CAS
        // is caught here, and the hold is given back on drop.
        (slot.owner.load(Ordering::Acquire) == owner).then_some(held)
    }

    /// Take a slot for `owner`: open its file with `open` there, closing
    /// whatever the slot held first, and return the slot held by this one
    /// reader. Waits only when every slot is mid-read, and then only for
    /// the reads already running on the one it drains. For a `Blocking`
    /// read, which chose to block (D60).
    pub(crate) fn load(
        &self,
        owner: u64,
        open: impl FnOnce() -> io::Result<Arc<dyn ReadFile>>,
    ) -> io::Result<Held<'_>> {
        let index = self.claim();
        self.fill(index, owner, open)
    }

    /// [`Self::load`] for a reopen that may not wait (D60): one sweep, and
    /// when it finds nothing, a [`park`](Self::park) of `waiter`. `None`
    /// when parked: `waiter` is told once a slot frees, and the caller
    /// returns at once.
    pub(crate) fn load_or_park(
        &self,
        owner: u64,
        waiter: Arc<dyn SlotWaiter>,
        open: impl FnOnce() -> io::Result<Arc<dyn ReadFile>>,
    ) -> io::Result<Option<Held<'_>>> {
        let index = match self.sweep() {
            Some(index) => index,
            None => match self.park(waiter) {
                Parking::Claimed(index) => index,
                Parking::Parked => return Ok(None),
            },
        };
        self.fill(index, owner, open).map(Some)
    }

    /// Open `owner`'s file in slot `index`, which this thread holds
    /// `CLAIMED`, after closing what it held, and publish it with this
    /// thread as its one reader.
    pub(crate) fn fill(
        &self,
        index: usize,
        owner: u64,
        open: impl FnOnce() -> io::Result<Arc<dyn ReadFile>>,
    ) -> io::Result<Held<'_>> {
        let slot = &self.slots[index];
        let mut claimed = Claimed {
            table: self,
            index,
            armed: true,
        };
        // SAFETY: this thread holds the slot `CLAIMED`: no reader is counted
        // and none can join, so nobody else touches `file`.
        let old = slot.file.with_mut(|file| unsafe { (*file).take() });
        if old.is_some() {
            self.open.fetch_sub(1, Ordering::Relaxed);
        }
        // Closed before the new one opens, so the table never holds more
        // than its capacity even for an instant.
        drop(old);
        slot.owner.store(owner, Ordering::Relaxed);
        let file = open()?;
        // SAFETY: still `CLAIMED`, as above.
        slot.file.with_mut(|cell| unsafe { *cell = Some(file) });
        self.open.fetch_add(1, Ordering::Relaxed);
        claimed.armed = false;
        // Release: the owner and the descriptor are visible to every reader
        // whose join CAS reads this word. One reader: this thread. An add,
        // not a store, because a park may have marked the slot wanted while
        // it was claimed, and that mark must survive until the slot frees.
        #[cfg(loom)]
        if self.mutant == Mutant::PublishDropsWanted {
            slot.state.store(OPEN | REF | 1, Ordering::Release);
            return Ok(Held { table: self, index });
        }
        slot.state.fetch_add(PUBLISH, Ordering::AcqRel);
        Ok(Held { table: self, index })
    }

    /// Put `waiter` on the parked list and look at every slot again: claim
    /// one that freed since the sweep, or mark every busy one wanted, so
    /// the step that frees any of them wakes `waiter` (the module docs).
    /// Never waits on another thread: each slot is one compare-and-swap,
    /// retried only when another thread changed its word.
    pub(crate) fn park(&self, waiter: Arc<dyn SlotWaiter>) -> Parking {
        #[cfg(loom)]
        if self.mutant == Mutant::MarkBeforeRegister {
            let parking = self.mark_or_claim();
            if parking == Parking::Parked {
                self.register(waiter);
            }
            return parking;
        }
        self.register(waiter);
        let parking = self.mark_or_claim();
        if let Parking::Claimed(_) = parking {
            // A slot freed between the sweep and this look, and this park
            // took it. Whoever freed it may have found no slot wanted and
            // woken nobody, so wake the list now: the waiters on it get a
            // try at the next free slot, and this waiter's own node goes
            // instead of staying behind until some later wake.
            self.wake();
        }
        parking
    }

    fn register(&self, waiter: Arc<dyn SlotWaiter>) {
        // The list is never shut, so the push always lands. AcqRel: the
        // marks that follow are ordered after the push for every waker
        // that reads them.
        let _ = self.parked.push(waiter, 0);
    }

    fn mark_or_claim(&self) -> Parking {
        for (index, slot) in self.slots.iter().enumerate() {
            let mut state = slot.state.load(Ordering::Acquire);
            loop {
                let claim = claimable(state);
                let next = if claim {
                    CLAIMED | (state & WANTED)
                } else if state & WANTED == 0 {
                    state | WANTED
                } else {
                    break;
                };
                // AcqRel: Release orders the push before this mark for the
                // waker that reads it; Acquire orders a claim after the
                // readers' leave, as `try_claim`'s does.
                match slot
                    .state
                    .compare_exchange(state, next, Ordering::AcqRel, Ordering::Acquire)
                {
                    Ok(_) if claim => return Parking::Claimed(index),
                    Ok(_) => break,
                    Err(now) => state = now,
                }
            }
        }
        Parking::Parked
    }

    /// Slot `index` became claimable while wanted: clear the mark, then
    /// wake every parked waiter. Clearing first is what makes a park that
    /// saw the mark still set (and so did not mark again) one this wake
    /// finds on the list.
    fn freed(&self, index: usize) {
        // Acquire: this reads the word in the release sequence of the CAS
        // that marked it, so the marking park's push onto the list happens
        // before the take below.
        self.slots[index].state.fetch_and(!WANTED, Ordering::AcqRel);
        self.wake();
    }

    /// Take the parked list whole and tell each waiter a slot is free.
    fn wake(&self) {
        // AcqRel: Acquire takes the pushes; Release orders the clear above
        // before any later push that reads this take.
        for waiter in self.parked.take(0) {
            waiter.slot_freed();
        }
    }

    /// Store `state`, a claimable word, into `slot`, which this thread holds
    /// `CLAIMED`; a park that marked it wanted meanwhile is woken.
    fn settle(&self, slot: &Slot, state: u64) {
        if slot.state.swap(state & !WANTED, Ordering::AcqRel) & WANTED != 0 {
            self.wake();
        }
    }

    /// Close every descriptor `owner` holds that no reader is using. Called
    /// when the record goes away; a slot a reader holds right now (one that
    /// joined, saw another owner and is about to leave) is left to CLOCK.
    pub(crate) fn release_owner(&self, owner: u64) {
        // vertexia: one pass over every slot per dropped table, O(capacity);
        // a per-record count of the slots it holds if a very large
        // max_open_files ever shows this in a profile.
        for slot in self.slots.iter() {
            if slot.owner.load(Ordering::Acquire) != owner {
                continue;
            }
            let state = slot.state.load(Ordering::Acquire);
            if state & PHASE != OPEN || state & (READERS | DRAIN) != 0 {
                continue;
            }
            if slot
                .state
                .compare_exchange(
                    state,
                    CLAIMED | (state & WANTED),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
            {
                continue;
            }
            // Claimed: is it still this owner's? A claim between the check
            // and the CAS reloaded it for someone else; put it back.
            if slot.owner.load(Ordering::Acquire) != owner {
                self.settle(slot, state);
                continue;
            }
            // SAFETY: `CLAIMED` by this thread; no reader is counted.
            let old = slot.file.with_mut(|file| unsafe { (*file).take() });
            if old.is_some() {
                self.open.fetch_sub(1, Ordering::Relaxed);
            }
            drop(old);
            slot.owner.store(NO_OWNER, Ordering::Relaxed);
            self.settle(slot, EMPTY);
        }
    }

    /// CLOCK: sweep from the hand for an empty slot or an open one nobody is
    /// reading whose reference bit is clear, clearing set bits on the way.
    /// After two fruitless sweeps every slot is mid-read: drain one.
    fn claim(&self) -> usize {
        loop {
            if let Some(index) = self.sweep() {
                return index;
            }
            if let Some(index) = self.drain() {
                return index;
            }
            // Every slot is being loaded or drained by another thread.
            pause();
        }
    }

    /// Two turns of the CLOCK hand over the table: the claimed slot, or
    /// `None` when every slot it reached was mid-read, being loaded or
    /// drained, or referenced twice over. Never waits.
    fn sweep(&self) -> Option<usize> {
        let n = self.slots.len();
        (0..2 * n).find_map(|_| {
            let index = self.hand.fetch_add(1, Ordering::Relaxed) % n;
            self.try_claim(index).then_some(index)
        })
    }

    fn try_claim(&self, index: usize) -> bool {
        let slot = &self.slots[index];
        let state = slot.state.load(Ordering::Acquire);
        #[cfg(loom)]
        let busy = if self.mutant == Mutant::IgnoreReaders {
            DRAIN
        } else {
            READERS | DRAIN
        };
        #[cfg(not(loom))]
        let busy = READERS | DRAIN;
        let claimable = match state & PHASE {
            EMPTY => true,
            OPEN if state & busy == 0 => {
                if state & REF != 0 {
                    // Second chance: clear the bit and move on. A failed CAS
                    // means a reader or a claimer got there first.
                    let _ = slot.state.compare_exchange(
                        state,
                        state & !REF,
                        Ordering::AcqRel,
                        Ordering::Relaxed,
                    );
                    false
                } else {
                    true
                }
            }
            _ => false,
        };
        // Acquire: every reader's leave (Release) happens before the close
        // this claim leads to. `WANTED` stays: the slot is busy again, and a
        // parked waiter still needs its next free.
        claimable
            && slot
                .state
                .compare_exchange(
                    state,
                    CLAIMED | (state & WANTED),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
    }

    /// Reserve the slot under the hand against new readers and take it once
    /// the readers already on it leave. `None` when another thread drains or
    /// holds it; the caller sweeps again.
    fn drain(&self) -> Option<usize> {
        let index = self.hand.fetch_add(1, Ordering::Relaxed) % self.slots.len();
        let slot = &self.slots[index];
        let mut state = slot.state.load(Ordering::Acquire);
        loop {
            match state & PHASE {
                EMPTY => return self.try_claim(index).then_some(index),
                OPEN if state & DRAIN == 0 => {
                    match slot.state.compare_exchange_weak(
                        state,
                        state | DRAIN,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => break,
                        Err(now) => state = now,
                    }
                }
                _ => return None,
            }
        }
        // Only this thread takes a drained slot. Its readers each finish one
        // read and leave; no new one joins. A `Blocking` read only: a reopen
        // that may not wait never comes here (D60).
        loop {
            let state = slot.state.load(Ordering::Acquire);
            if state & READERS == 0
                && slot
                    .state
                    .compare_exchange(
                        state,
                        CLAIMED | (state & WANTED),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
            {
                return Some(index);
            }
            pause();
        }
    }
}

/// One step of a wait on reads already running.
fn pause() {
    #[cfg(loom)]
    loom::thread::yield_now();
    #[cfg(not(loom))]
    std::thread::yield_now();
}
