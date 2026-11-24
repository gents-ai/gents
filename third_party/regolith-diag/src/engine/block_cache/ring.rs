//! The lock-free CLOCK ring behind one block-cache shard.
//!
//! A slot is one 64-bit state word plus the pointer of the entry it holds.
//! The word carries the slot's generation in its high half and four flags in
//! its low half, so every decision about a slot is one compare-and-swap of
//! one word, which `armv7` and `wasm32` do natively (D17):
//!
//! ```text
//!   free --publish--> OCCUPIED --hand clears REF--> OCCUPIED
//!                       |  ^
//!            claim (CAS)|  |release (put back)
//!                       v  |
//!                     OCCUPIED|BUSY --remove--> free (generation kept)
//! ```
//!
//! * **OCCUPIED**: an entry is published in the slot.
//! * **BUSY**: one thread holds the slot exclusively, by one CAS. Only the
//!   holder reads or changes the entry, so no reader of the ring ever
//!   touches an entry another thread may free. Nobody waits on a busy
//!   slot: the hand skips it, and a remover marks it `DOOMED` for the
//!   holder to finish.
//! * **DOOMED**: the entry must go whatever its reference bit or pins: a
//!   re-insert of its key, `evict_file` or `clear` asked for it. Whoever
//!   holds or next takes the slot removes it.
//! * **REF**: read since the hand last passed. Set by readers with one CAS
//!   on the slot whose generation they hold; advisory, so a lost update
//!   costs a miss and never correctness.
//!
//! The generation goes up by one at every publication, so a reader or a
//! remover that names `(slot, generation)` can never act on a later entry
//! that reused the slot.
//!
//! Slots are allocated in segments that double in size, on demand, and are
//! never moved or freed before the ring, so a slot reference stays valid
//! for the ring's life. Free slots form a Treiber stack of indices whose
//! head carries a tag against ABA, linked through the slot's pointer field.

#![allow(unsafe_code)]

use super::{CacheEntry, CacheKey};
use crate::portability::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};

/// An entry is published in the slot.
pub(super) const OCCUPIED: u64 = 1;
/// One thread holds the slot exclusively.
pub(super) const BUSY: u64 = 1 << 1;
/// The entry must be removed whatever its reference bit or pins.
pub(super) const DOOMED: u64 = 1 << 2;
/// Read since the hand last passed.
pub(super) const REF: u64 = 1 << 3;
/// Every flag bit.
#[cfg(test)]
const FLAGS: u64 = OCCUPIED | BUSY | DOOMED | REF;
/// Where the generation starts in the word.
const GEN_SHIFT: u32 = 32;

/// Slots in the first segment; each later one doubles.
const FIRST_SEGMENT: usize = 64;

/// The generation half of a state word.
#[inline]
pub(super) fn generation(word: u64) -> u32 {
    (word >> GEN_SHIFT) as u32
}

/// What a slot holds while it is occupied: the cache's strong reference to
/// the block, the key the map files it under, and the bytes it charged.
pub(super) struct Node {
    /// The cache's own reference. `None` only inside a removal, after the
    /// reference has been let go.
    pub(super) entry: Option<CacheEntry>,
    pub(super) key: CacheKey,
    /// What the insert reserved, so the removal returns exactly that.
    pub(super) charge: usize,
}

/// One slot of the ring.
pub(super) struct Slot {
    /// Generation in the high half, flags in the low half.
    word: AtomicU64,
    /// A `Box<Node>` pointer while occupied; while free, the next free
    /// index plus one (zero ends the stack).
    node: AtomicUsize,
}

impl Slot {
    fn new() -> Self {
        Self {
            word: AtomicU64::new(0),
            node: AtomicUsize::new(0),
        }
    }
}

/// Which segment holds `index`, and the offset in it.
#[inline]
fn locate(index: usize) -> (usize, usize) {
    let bucket = index / FIRST_SEGMENT + 1;
    let segment = (usize::BITS - 1 - bucket.leading_zeros()) as usize;
    (segment, index - FIRST_SEGMENT * ((1usize << segment) - 1))
}

/// Slots in `segment`.
#[inline]
fn segment_len(segment: usize) -> usize {
    FIRST_SEGMENT << segment
}

/// A thread's exclusive hold on an occupied slot: it owns the entry until
/// it puts it back ([`Claim::release`]) or takes it out
/// ([`Claim::remove`]).
pub(super) struct Claim<'r> {
    slot: &'r Slot,
    index: usize,
    generation: u32,
}

/// What one step of the hand found.
pub(super) enum Hand<'r> {
    /// Nothing to do at this position: empty, busy, or a reference bit the
    /// step just cleared.
    Passed,
    /// An entry this thread now holds.
    Claimed(Claim<'r>),
}

/// The ring of one shard. See the module docs.
pub(super) struct Ring {
    /// Segment `k` holds `FIRST_SEGMENT << k` slots; null until a slot in
    /// it is first handed out.
    segments: Box<[AtomicPtr<Slot>]>,
    /// The most slots this ring may hand out.
    max_slots: usize,
    /// Indices handed out at least once: the hand's range.
    fresh: AtomicUsize,
    /// Treiber stack of free indices: tag in the high half, index plus one
    /// in the low half (zero is empty).
    free: AtomicU64,
    /// Next position the hand inspects, modulo the ring's range.
    hand: AtomicUsize,
}

impl Ring {
    /// A ring that may grow to `max_slots` slots. Allocates only the
    /// segment directory, a few dozen words.
    pub(super) fn new(max_slots: usize) -> Self {
        let max_slots = max_slots.clamp(1, u32::MAX as usize - 1);
        let segments = locate(max_slots - 1).0 + 1;
        Self {
            segments: (0..segments)
                .map(|_| AtomicPtr::new(std::ptr::null_mut()))
                .collect(),
            max_slots,
            fresh: AtomicUsize::new(0),
            free: AtomicU64::new(0),
            hand: AtomicUsize::new(0),
        }
    }

    /// The slot at `index`, if its segment exists.
    #[inline]
    pub(super) fn slot(&self, index: usize) -> Option<&Slot> {
        let (segment, offset) = locate(index);
        let base = self.segments.get(segment)?.load(Ordering::Acquire);
        // SAFETY: a non-null segment pointer is a boxed slice of
        // `segment_len(segment)` slots that lives as long as the ring, and
        // `offset` is below that length by `locate`.
        (!base.is_null()).then(|| unsafe { &*base.add(offset) })
    }

    /// Indices handed out at least once.
    #[inline]
    pub(super) fn high_water(&self) -> usize {
        self.fresh.load(Ordering::Acquire).min(self.max_slots)
    }

    /// Bytes the ring's segments take, for the retention tests.
    #[cfg(test)]
    pub(super) fn allocated_bytes(&self) -> usize {
        self.segments
            .iter()
            .enumerate()
            .filter(|(_, base)| !base.load(Ordering::Acquire).is_null())
            .map(|(segment, _)| segment_len(segment) * std::mem::size_of::<Slot>())
            .sum()
    }

    /// Take a free slot index, or `None` when the ring is at its bound. The
    /// caller owns the slot until it publishes into it or gives it back.
    pub(super) fn take(&self) -> Option<usize> {
        let mut head = self.free.load(Ordering::Acquire);
        loop {
            let top = (head & u64::from(u32::MAX)) as usize;
            if top == 0 {
                break;
            }
            let index = top - 1;
            // A stale read here (the index taken and reused meanwhile) is
            // caught by the tag: the CAS below then fails.
            let next = self
                .slot(index)
                .map_or(0, |slot| slot.node.load(Ordering::Relaxed) as u64);
            let tag = (head >> 32).wrapping_add(1);
            match self.free.compare_exchange_weak(
                head,
                (tag << 32) | (next & u64::from(u32::MAX)),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(index),
                Err(actual) => head = actual,
            }
        }
        let index = self.fresh.fetch_add(1, Ordering::AcqRel);
        if index >= self.max_slots {
            // Never reached while every slot in use carries its charge (the
            // module invariant); refuse rather than index past the bound.
            return None;
        }
        self.ensure_segment(index)?;
        Some(index)
    }

    /// Give a slot index back. The slot must be free (unpublished) and
    /// owned by the caller.
    pub(super) fn give(&self, index: usize) {
        let Some(slot) = self.slot(index) else {
            return;
        };
        let mut head = self.free.load(Ordering::Acquire);
        loop {
            slot.node
                .store((head & u64::from(u32::MAX)) as usize, Ordering::Relaxed);
            let tag = (head >> 32).wrapping_add(1);
            match self.free.compare_exchange_weak(
                head,
                (tag << 32) | (index as u64 + 1),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => head = actual,
            }
        }
    }

    /// Make sure the segment holding `index` exists. Lock-free: racing
    /// allocators each build one and all but the first drop theirs.
    fn ensure_segment(&self, index: usize) -> Option<()> {
        let (segment, _) = locate(index);
        let cell = self.segments.get(segment)?;
        if !cell.load(Ordering::Acquire).is_null() {
            return Some(());
        }
        let slots: Box<[Slot]> = (0..segment_len(segment)).map(|_| Slot::new()).collect();
        let built = Box::into_raw(slots).cast::<Slot>();
        if cell
            .compare_exchange(
                std::ptr::null_mut(),
                built,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            // SAFETY: `built` came from `Box::into_raw` just above and was
            // never published, so this is its only owner.
            drop(unsafe {
                Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                    built,
                    segment_len(segment),
                ))
            });
        }
        Some(())
    }

    /// Publish `node` into slot `index`, which the caller took and still
    /// owns. Returns the generation readers and removers name it by.
    pub(super) fn publish(&self, index: usize, node: Box<Node>) -> Option<u32> {
        let slot = self.slot(index)?;
        slot.node
            .store(Box::into_raw(node) as usize, Ordering::Relaxed);
        // Only the owner of a free slot writes its word, so this load is
        // the slot's last generation.
        let generation = generation(slot.word.load(Ordering::Relaxed)).wrapping_add(1);
        slot.word.store(
            (u64::from(generation) << GEN_SHIFT) | OCCUPIED,
            Ordering::Release,
        );
        Some(generation)
    }

    /// Mark the entry `(index, generation)` read since the hand last
    /// passed. One load, and one CAS only when the bit was clear.
    #[inline]
    pub(super) fn touch(&self, index: usize, generation_seen: u32) {
        let Some(slot) = self.slot(index) else {
            return;
        };
        let word = slot.word.load(Ordering::Relaxed);
        if generation(word) == generation_seen && word & OCCUPIED != 0 && word & REF == 0 {
            let _ =
                slot.word
                    .compare_exchange(word, word | REF, Ordering::Relaxed, Ordering::Relaxed);
        }
    }

    /// The generation of the entry published at `index`, if any.
    pub(super) fn live_generation(&self, index: usize) -> Option<u32> {
        let word = self.slot(index)?.word.load(Ordering::Acquire);
        (word & OCCUPIED != 0).then(|| generation(word))
    }

    /// Whether `(index, generation)` is still published and not doomed.
    pub(super) fn is_live(&self, index: usize, generation_seen: u32) -> bool {
        self.slot(index).is_some_and(|slot| {
            let word = slot.word.load(Ordering::Acquire);
            generation(word) == generation_seen && word & OCCUPIED != 0 && word & DOOMED == 0
        })
    }

    /// One step of the hand. On the first revolution (`forced` false) a set
    /// reference bit is cleared and the entry passed over; from the second
    /// the hand takes whatever it lands on. A doomed entry is always taken,
    /// so a removal its holder left behind is finished here.
    pub(super) fn step(&self, forced: bool) -> Hand<'_> {
        let range = self.high_water();
        if range == 0 {
            return Hand::Passed;
        }
        let index = self.hand.fetch_add(1, Ordering::Relaxed) % range;
        let Some(slot) = self.slot(index) else {
            return Hand::Passed;
        };
        let word = slot.word.load(Ordering::Acquire);
        if word & OCCUPIED == 0 || word & BUSY != 0 {
            return Hand::Passed;
        }
        if !forced && word & REF != 0 && word & DOOMED == 0 {
            let _ =
                slot.word
                    .compare_exchange(word, word & !REF, Ordering::Relaxed, Ordering::Relaxed);
            return Hand::Passed;
        }
        match self.claim_word(index, slot, word) {
            Some(claim) => Hand::Claimed(claim),
            None => Hand::Passed,
        }
    }

    /// Take the slot from `word` to `word | BUSY` by one CAS.
    fn claim_word<'r>(&'r self, index: usize, slot: &'r Slot, word: u64) -> Option<Claim<'r>> {
        slot.word
            .compare_exchange(word, word | BUSY, Ordering::AcqRel, Ordering::Relaxed)
            .ok()
            .map(|_| Claim {
                slot,
                index,
                generation: generation(word),
            })
    }

    /// Take the occupied slot at `index`, whatever its generation and
    /// flags, unless another thread holds it. For the walks that visit
    /// every entry (`clear`, an oversized admission, the test invariants).
    pub(super) fn claim_any(&self, index: usize) -> Option<Claim<'_>> {
        let slot = self.slot(index)?;
        loop {
            let word = slot.word.load(Ordering::Acquire);
            if word & OCCUPIED == 0 || word & BUSY != 0 {
                return None;
            }
            if let Some(claim) = self.claim_word(index, slot, word) {
                return Some(claim);
            }
        }
    }

    /// Ask for the entry `(index, generation)` to be removed whatever its
    /// pins. Returns the claim when this call took the slot, for the
    /// caller to remove it; `None` when the entry is already gone or
    /// another thread holds the slot, which then removes it.
    pub(super) fn doom(&self, index: usize, generation_named: u32) -> Option<Claim<'_>> {
        let slot = self.slot(index)?;
        loop {
            let word = slot.word.load(Ordering::Acquire);
            if generation(word) != generation_named || word & OCCUPIED == 0 || word & DOOMED != 0 {
                return None;
            }
            let take = word & BUSY == 0;
            let next = word | DOOMED | if take { BUSY } else { 0 };
            if slot
                .word
                .compare_exchange(word, next, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return take.then_some(Claim {
                    slot,
                    index,
                    generation: generation_named,
                });
            }
        }
    }
}

impl Claim<'_> {
    /// The slot index held.
    pub(super) fn index(&self) -> usize {
        self.index
    }

    /// The generation of the entry held.
    pub(super) fn generation(&self) -> u32 {
        self.generation
    }

    fn slot(&self) -> &Slot {
        self.slot
    }

    /// The entry held. Exclusive: no other thread reads an entry while its
    /// slot is busy.
    pub(super) fn node(&mut self) -> &mut Node {
        let pointer = self.slot().node.load(Ordering::Relaxed) as *mut Node;
        // SAFETY: the slot is occupied, so `node` is the `Box<Node>`
        // pointer `publish` stored (made visible by the release that set
        // OCCUPIED, acquired by the CAS that made this claim), and BUSY
        // gives this claim exclusive access until it is released or
        // removed.
        unsafe { &mut *pointer }
    }

    /// Whether a remover has asked for this entry to go.
    pub(super) fn doomed(&self) -> bool {
        self.slot().word.load(Ordering::Acquire) & DOOMED != 0
    }

    /// Put the entry back. When a remover doomed it meanwhile the claim is
    /// handed back instead, for the caller to remove: nobody else will.
    pub(super) fn release(self) -> Option<Self> {
        let slot = self.slot();
        let mut word = slot.word.load(Ordering::Acquire);
        loop {
            if word & DOOMED != 0 {
                return Some(self);
            }
            match slot.word.compare_exchange_weak(
                word,
                word & !BUSY,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => return None,
                Err(actual) => word = actual,
            }
        }
    }

    /// Take the entry out of the ring and free the slot's word, keeping its
    /// generation. The slot index stays the caller's: it gives it back with
    /// [`Ring::give`] once the map no longer names it.
    pub(super) fn remove(self) -> Box<Node> {
        let slot = self.slot();
        let pointer = slot.node.load(Ordering::Relaxed) as *mut Node;
        slot.word
            .store(u64::from(self.generation) << GEN_SHIFT, Ordering::Release);
        // SAFETY: as in `node`; the word no longer says OCCUPIED, so no
        // other claim on this pointer can be made, and this is its only
        // reclaim.
        unsafe { Box::from_raw(pointer) }
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        for (segment, cell) in self.segments.iter().enumerate() {
            let base = cell.load(Ordering::Acquire);
            if base.is_null() {
                continue;
            }
            // SAFETY: `&mut self` excludes every other thread. The segment
            // is a boxed slice of `segment_len(segment)` slots, and an
            // occupied slot owns the `Box<Node>` its pointer names.
            let slots = unsafe {
                Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                    base,
                    segment_len(segment),
                ))
            };
            for slot in slots.iter() {
                if slot.word.load(Ordering::Acquire) & OCCUPIED != 0 {
                    // SAFETY: an occupied slot's pointer is the `Box<Node>`
                    // `publish` leaked, and nothing else reclaims it now.
                    drop(unsafe { Box::from_raw(slot.node.load(Ordering::Relaxed) as *mut Node) });
                }
            }
        }
    }
}

#[cfg(test)]
impl Ring {
    /// Every index on the free stack, in stack order. Quiescent use only.
    pub(super) fn free_indices(&self) -> Vec<usize> {
        let mut out = Vec::new();
        let mut top = (self.free.load(Ordering::Acquire) & u64::from(u32::MAX)) as usize;
        while top != 0 && out.len() <= self.max_slots {
            out.push(top - 1);
            top = self
                .slot(top - 1)
                .map_or(0, |slot| slot.node.load(Ordering::Relaxed));
        }
        out
    }

    /// The raw flags of slot `index`, for the tests.
    pub(super) fn flags(&self, index: usize) -> u64 {
        self.slot(index)
            .map_or(0, |slot| slot.word.load(Ordering::Acquire) & FLAGS)
    }
}
