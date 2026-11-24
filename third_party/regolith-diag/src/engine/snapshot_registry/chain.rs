//! The entries snapshot pins are announced in: per slot, a chain of chunks.
//!
//! A [`Slot`] belongs to one thread number (`crate::per_thread`). It holds
//! a chain of [`Chunk`]s, allocated the first time a pin needs one and
//! appended when every entry of the chain is taken. A chunk holds
//! [`ENTRIES`] entries and one `taken` word: bit `i` says entry `i` is in
//! use, and the top bit, [`WATCHED`], says a drain waiter has looked at the
//! chunk. An [`Entry`] announces one sequence and counts the pins on it.
//!
//! # Entry states
//!
//! | `taken` bit | `pins` | state |
//! |---|---|---|
//! | clear | 0 | free |
//! | set | 0 | claimed: the claimer is writing `seq`; or freeing: the last pin left and its releaser clears the bit next |
//! | set | n >= 1 | announced: `n` pins hold `seq` |
//!
//! # Invariants
//!
//! - **E1.** `seq` and `since` are written only by the thread whose
//!   compare-and-swap set the entry's bit, before it stores `pins = 1` with
//!   `Release`. Nobody writes them while `pins >= 1`, so an entry announces
//!   one sequence from its first pin until its last pin leaves.
//! - **E2.** `pins` rises from 0 only by the claimer's store. Every other
//!   rise is from `n >= 1`: a join's compare-and-swap, or a clone's
//!   `fetch_add` while the pin it copies holds a count. So a pin never
//!   lands on an entry that is free, being claimed or being freed.
//! - **E3.** Chunks are appended, never unlinked, and freed only when the
//!   slot drops (`&mut self`). A chunk reached through `&self` stays valid
//!   for as long as that borrow, which is the whole safety argument for the
//!   `unsafe` blocks below.

#![allow(unsafe_code)]

use std::ptr;

use crate::sync::internal::{AtomicPtr, AtomicU64, Ordering};

/// Entries per chunk: one bit each in the chunk's `taken` word, below
/// [`WATCHED`].
#[cfg(not(loom))]
pub(super) const ENTRIES: usize = 63;

/// Three under loom, so a model fills a chunk and grows the chain in a few
/// steps. Nothing in the protocol depends on the number.
#[cfg(loom)]
pub(super) const ENTRIES: usize = 3;

/// The bits of `taken` that name entries.
const ENTRY_BITS: u64 = (1 << ENTRIES) - 1;

/// Set in `taken` by a drain waiter before it reads the chunk's bits, and
/// never cleared. A release that frees an entry reads it in the same
/// read-modify-write that clears the entry's bit, so a waiter that saw the
/// entry taken is always told when it goes free.
pub(super) const WATCHED: u64 = 1 << 63;

/// `since` of an entry claimed where the environment has no wall clock.
pub(super) const NO_TIME: u64 = u64::MAX;

/// `Slot::hint` before the slot's first announce.
const NO_HINT: u64 = u64::MAX;

/// One announced sequence and the pins on it.
pub(super) struct Entry {
    /// How many pins hold this entry (E2).
    pins: AtomicU64,
    /// The sequence the entry announces (E1).
    seq: AtomicU64,
    /// Unix seconds when the entry was claimed, or [`NO_TIME`].
    since: AtomicU64,
}

impl Entry {
    fn new() -> Self {
        Self {
            pins: AtomicU64::new(0),
            seq: AtomicU64::new(0),
            since: AtomicU64::new(NO_TIME),
        }
    }
}

/// One announced entry, as a scan read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Announced {
    pub(crate) seq: u64,
    pub(crate) pins: u64,
    pub(crate) since: u64,
}

/// Where in its slot a pin is announced: the chunk's depth in the chain and
/// the entry's index in the chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct At {
    pub(crate) depth: u32,
    pub(crate) index: u32,
}

impl At {
    fn pack(self) -> u64 {
        (u64::from(self.depth) << 32) | u64::from(self.index)
    }

    fn unpack(word: u64) -> Self {
        Self {
            depth: (word >> 32) as u32,
            index: word as u32,
        }
    }
}

/// A run of entries, the unit a slot's chain grows by.
#[repr(align(128))]
pub(super) struct Chunk {
    /// Bit `i < ENTRIES`: entry `i` is in use. Top bit: [`WATCHED`].
    taken: AtomicU64,
    /// The next chunk of this slot's chain, or null (E3).
    next: AtomicPtr<Chunk>,
    entries: [Entry; ENTRIES],
}

/// What a join attempt did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Join {
    /// The entry announces the sequence and now counts one more pin.
    Joined,
    /// The entry does not announce the sequence; nothing changed.
    Missed,
    /// The entry was recycled for another sequence between the check and
    /// the count, so the count was taken back. `true` when taking it back
    /// freed the entry in a chunk a drain waiter watches.
    Undone(bool),
}

impl Chunk {
    fn new() -> Self {
        Self {
            taken: AtomicU64::new(0),
            next: AtomicPtr::new(ptr::null_mut()),
            entries: std::array::from_fn(|_| Entry::new()),
        }
    }

    /// Takes a free entry, if any, writes `seq` and `since` in it and
    /// announces it with one pin.
    fn claim(&self, seq: u64, since: &mut impl FnMut() -> u64) -> Option<u32> {
        let mut bits = self.taken.load(Ordering::Relaxed);
        loop {
            let free = !bits & ENTRY_BITS;
            if free == 0 {
                return None;
            }
            let index = free.trailing_zeros();
            match self.taken.compare_exchange_weak(
                bits,
                bits | (1 << index),
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    let entry = &self.entries[index as usize];
                    entry.seq.store(seq, Ordering::Relaxed);
                    entry.since.store(since(), Ordering::Relaxed);
                    entry.pins.store(1, Ordering::Release);
                    return Some(index);
                }
                Err(now) => bits = now,
            }
        }
    }

    /// Adds one pin to entry `index` if it announces `seq`.
    pub(super) fn join(&self, index: u32, seq: u64) -> Join {
        let entry = &self.entries[index as usize];
        let mut pins = entry.pins.load(Ordering::Acquire);
        loop {
            // `pins >= 1` synchronised with the claimer's store, so `seq` is
            // that claimer's; a recycle in between is caught below.
            if pins == 0 || entry.seq.load(Ordering::Relaxed) != seq {
                return Join::Missed;
            }
            match entry.pins.compare_exchange_weak(
                pins,
                pins + 1,
                Ordering::Acquire,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(now) => pins = now,
            }
        }
        // The count is ours, so `seq` cannot change now (E1): what it reads
        // is the sequence of the incarnation joined.
        if entry.seq.load(Ordering::Relaxed) == seq {
            Join::Joined
        } else {
            Join::Undone(self.unpin(index))
        }
    }

    /// Adds one pin to entry `index`, which a live pin already holds (E2).
    pub(super) fn add_pin(&self, index: u32) {
        self.entries[index as usize]
            .pins
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Removes one pin from entry `index`, freeing the entry when it was
    /// the last. Returns `true` when that freed it in a chunk a drain waiter
    /// watches, so the caller must wake the waiters.
    pub(super) fn unpin(&self, index: u32) -> bool {
        let before = self.entries[index as usize]
            .pins
            .fetch_sub(1, Ordering::Release);
        debug_assert!(before >= 1, "an entry released more often than pinned");
        if before != 1 {
            return false;
        }
        let bits = self.taken.fetch_and(!(1 << index), Ordering::AcqRel);
        bits & WATCHED != 0
    }

    /// Calls `each` for every announced entry. An entry claimed or freed
    /// while the scan runs may be seen or missed.
    pub(super) fn each_announced(&self, each: &mut impl FnMut(Announced)) {
        self.each_announced_in(self.taken.load(Ordering::Acquire), each);
    }

    /// Marks the chunk watched, then calls `each` for every announced entry,
    /// and returns whether any entry is taken. The mark and the read of the
    /// taken bits are one read-modify-write: a release that frees an entry
    /// after it sees the mark (see [`WATCHED`]).
    pub(super) fn watch(&self, each: &mut impl FnMut(Announced)) -> bool {
        let bits = self.taken.fetch_or(WATCHED, Ordering::AcqRel);
        self.each_announced_in(bits, each);
        bits & ENTRY_BITS != 0
    }

    /// Whether a drain waiter has marked the chunk. Test use only.
    #[cfg(test)]
    pub(super) fn is_watched(&self) -> bool {
        self.taken.load(Ordering::Acquire) & WATCHED != 0
    }

    fn each_announced_in(&self, bits: u64, each: &mut impl FnMut(Announced)) {
        let mut taken = bits & ENTRY_BITS;
        while taken != 0 {
            let index = taken.trailing_zeros() as usize;
            taken &= taken - 1;
            let entry = &self.entries[index];
            let pins = entry.pins.load(Ordering::Acquire);
            if pins >= 1 {
                each(Announced {
                    seq: entry.seq.load(Ordering::Relaxed),
                    pins,
                    since: entry.since.load(Ordering::Relaxed),
                });
            }
        }
    }
}

/// One thread number's entries. See the module documentation.
pub(super) struct Slot {
    /// The first chunk, or null until the slot's first pin.
    first: AtomicPtr<Chunk>,
    /// Where this slot's last announce landed, packed by [`At::pack`], or
    /// [`NO_HINT`]: the entry a pin at the same sequence joins.
    hint: AtomicU64,
}

impl Slot {
    pub(super) fn new() -> Self {
        Self {
            first: AtomicPtr::new(ptr::null_mut()),
            hint: AtomicU64::new(NO_HINT),
        }
    }

    /// The chunk a link points to.
    fn load<'a>(&'a self, link: &'a AtomicPtr<Chunk>) -> Option<&'a Chunk> {
        let chunk = link.load(Ordering::Acquire);
        // SAFETY: a link holds null or a pointer `load_or_grow` made with
        // `Box::into_raw`, freed only in `Drop` with `&mut self` (E3), so it
        // is valid for as long as `&'a self`.
        unsafe { chunk.as_ref() }
    }

    /// The chunk a link points to, appending a fresh one when the link is
    /// null. Two threads growing at once keep the winner's chunk.
    fn load_or_grow<'a>(&'a self, link: &'a AtomicPtr<Chunk>) -> &'a Chunk {
        if let Some(chunk) = self.load(link) {
            return chunk;
        }
        let fresh = Box::into_raw(Box::new(Chunk::new()));
        let chunk = match link.compare_exchange(
            ptr::null_mut(),
            fresh,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => fresh,
            Err(winner) => {
                // SAFETY: `fresh` came from `Box::into_raw` above and was
                // never published, so this is its only owner.
                drop(unsafe { Box::from_raw(fresh) });
                winner
            }
        };
        // SAFETY: `chunk` is non-null and now linked into the chain, so E3
        // keeps it alive for `&'a self`.
        unsafe { &*chunk }
    }

    /// The chunk `depth` links down the chain, if the chain is that long.
    pub(super) fn chunk_at(&self, depth: u32) -> Option<&Chunk> {
        let mut chunk = self.load(&self.first)?;
        for _ in 0..depth {
            chunk = self.load(&chunk.next)?;
        }
        Some(chunk)
    }

    /// Every chunk of the chain, in order.
    pub(super) fn chunks(&self) -> impl Iterator<Item = &Chunk> {
        std::iter::successors(self.load(&self.first), |chunk| self.load(&chunk.next))
    }

    /// The entry this slot's last announce landed in, if any.
    pub(super) fn hint(&self) -> Option<At> {
        let hint = self.hint.load(Ordering::Relaxed);
        (hint != NO_HINT).then(|| At::unpack(hint))
    }

    /// Claims a free entry for `seq`, appending a chunk when the chain is
    /// full, and remembers it as the slot's hint. Never fails: the chain
    /// grows until a claim succeeds.
    pub(super) fn claim(&self, seq: u64, mut since: impl FnMut() -> u64) -> At {
        let mut link = &self.first;
        let mut depth = 0u32;
        loop {
            let chunk = self.load_or_grow(link);
            if let Some(index) = chunk.claim(seq, &mut since) {
                let at = At { depth, index };
                self.remember(at);
                return at;
            }
            link = &chunk.next;
            depth += 1;
        }
    }

    /// Makes `at` the entry the next announce at the same sequence joins.
    pub(super) fn remember(&self, at: At) {
        self.hint.store(at.pack(), Ordering::Relaxed);
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut next = self.first.load(Ordering::Acquire);
        while !next.is_null() {
            // SAFETY: every link was made by `Box::into_raw` in `load_or_grow`, and
            // `&mut self` means no pin, scan or growth can reach the chain.
            let chunk = unsafe { Box::from_raw(next) };
            next = chunk.next.load(Ordering::Acquire);
        }
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    fn announced(chunk: &Chunk) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        chunk.each_announced(&mut |a: Announced| out.push((a.seq, a.pins)));
        out
    }

    #[test]
    fn a_claim_takes_the_lowest_free_entry_and_announces_one_pin() {
        let slot = Slot::new();
        assert!(slot.chunks().next().is_none(), "no chunk before a pin");
        let a = slot.claim(7, || 100);
        let b = slot.claim(9, || NO_TIME);
        assert_eq!(
            (a, b),
            (At { depth: 0, index: 0 }, At { depth: 0, index: 1 })
        );
        assert_eq!(slot.hint(), Some(b));
        let chunk = slot.chunk_at(0).unwrap();
        assert_eq!(announced(chunk), vec![(7, 1), (9, 1)]);
    }

    #[test]
    fn a_full_chain_grows_by_one_chunk() {
        let slot = Slot::new();
        for seq in 0..ENTRIES as u64 {
            assert_eq!(slot.claim(seq, || 0).depth, 0);
        }
        let spilled = slot.claim(99, || 0);
        assert_eq!(spilled, At { depth: 1, index: 0 });
        assert_eq!(slot.chunks().count(), 2);
        // Freeing one in the first chunk is reused before the second fills.
        assert!(!slot.chunk_at(0).unwrap().unpin(3));
        assert_eq!(slot.claim(100, || 0), At { depth: 0, index: 3 });
    }

    #[test]
    fn a_join_counts_only_on_the_same_sequence() {
        let slot = Slot::new();
        let at = slot.claim(5, || 0);
        let chunk = slot.chunk_at(0).unwrap();
        assert_eq!(chunk.join(at.index, 6), Join::Missed);
        assert_eq!(chunk.join(at.index, 5), Join::Joined);
        assert_eq!(announced(chunk), vec![(5, 2)]);
        assert!(!chunk.unpin(at.index));
        assert!(!chunk.unpin(at.index));
        assert_eq!(announced(chunk), vec![], "the last pin frees it");
        assert_eq!(
            chunk.join(at.index, 5),
            Join::Missed,
            "a free entry is never joined"
        );
    }

    #[test]
    fn the_last_unpin_reports_a_watched_chunk() {
        let slot = Slot::new();
        let at = slot.claim(1, || 0);
        let chunk = slot.chunk_at(0).unwrap();
        chunk.add_pin(at.index);
        let mut seen = Vec::new();
        assert!(chunk.watch(&mut |a: Announced| seen.push(a.pins)), "taken");
        assert_eq!(seen, vec![2]);
        assert!(!chunk.unpin(at.index), "a pin remains");
        assert!(chunk.unpin(at.index), "the last pin frees a watched entry");
        assert!(!chunk.watch(&mut |_| unreachable!("nothing announced")));
    }
}
