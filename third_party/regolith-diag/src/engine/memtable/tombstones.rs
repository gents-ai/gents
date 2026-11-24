//! A memtable's range tombstones: an append-only log that readers walk
//! without a lock, and that an append never blocks.
//!
//! # Invariants
//!
//! * **T1, any number of writers, each slot written once.** An append takes
//!   the next position with one `fetch_add`, so no two appends share a slot,
//!   and none waits for another. The commit pipeline lets whichever thread
//!   reaches a decided commit apply it (`engine::commit`), so two threads
//!   can append the same tombstone; an append that finds its tombstone
//!   already in the log adds nothing, and the rare pair that both get in is
//!   harmless: a tombstone listed twice deletes what it deletes once, and
//!   every listing drops exact duplicates.
//! * **T2, a slot is whole before it is ready.** An append writes its
//!   tombstone into its slot first and only then sets the slot's `ready`
//!   flag with a release store. A reader acquires the number of positions
//!   handed out and reads only the slots below it whose flag it acquires as
//!   set, so every tombstone it reads is whole. A slot handed out and not
//!   yet ready is skipped: its append has not returned, so the commit it
//!   belongs to is not applied, and no reader's snapshot includes it.
//! * **T3, never moved, never removed.** A ready slot is never written
//!   again and is freed only when the memtable is dropped, so a reader needs
//!   no reclamation for the log itself.
//! * **T4, the index is an optimization over a prefix of ready slots.**
//!   Every [`REBUILD`] appends, or once after a batch of them
//!   ([`TombstoneLog::extend`]), a writer publishes, through kovan, a sorted
//!   view of the first `covered` positions, every one of them ready, with a
//!   running maximum of their ends. A reader binary-searches that view and
//!   scans the positions past it, so a point lookup costs `O(log n)` rather
//!   than `O(n)`. The view stops at the first slot not ready yet, since a
//!   slot it passed would never be read again; a slot that stays unready
//!   (its thread was descheduled) only lengthens the scan past the view. A
//!   loom build has no index and scans every position, which is the
//!   protocol T2 is about.
//!
//! Why a tombstone a commit published is seen by every read after it: the
//! commit's apply returns only after the append set `ready` (T2's release),
//! and the commit's sequence is published through the read horizon, itself
//! a release read-modify-write, only after its apply. A reader whose
//! snapshot includes the tombstone acquired a horizon at or past it, so it
//! acquires the position count and the flag as the append left them. A
//! validation in the pipeline's ordered step runs after the apply of every
//! commit before it.

#![allow(unsafe_code)]

use std::cmp::Ordering as CmpOrdering;
use std::mem::{MaybeUninit, size_of};
use std::ptr;

use super::super::range_tombstone::RangeTombstone;
use crate::sync::internal::{AtomicPtr, AtomicUsize, Ordering, UnsafeCell};

/// Slots in the first segment; each later segment doubles.
const FIRST_SEGMENT: usize = 8;

/// Segments a log can have: `FIRST_SEGMENT * (2^SEGMENTS - 1)` slots, past
/// what a `u32` position (the index's element) can name.
const SEGMENTS: usize = 30;

/// Appends between two index rebuilds, and so the most appends a reader
/// scans past the index.
#[cfg(not(loom))]
pub(crate) const REBUILD: usize = 16;

#[cfg(all(test, not(loom)))]
thread_local! {
    /// Test seam: how many indexes this thread has published.
    static INDEXES_BUILT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };

    /// Test seam: how many slots this thread has looked at for a
    /// duplicate, one at a time ([`TombstoneLog::holds`]).
    static DUPLICATE_LOOKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Test seam: what this thread's appends have cost since the last call, as
/// the indexes it published and the slots it looked at for a duplicate. A
/// test pins the cost of an append by these counts, not by a time.
#[cfg(all(test, not(loom)))]
pub(crate) fn work_since_last_call() -> (usize, usize) {
    (
        INDEXES_BUILT.with(|built| built.replace(0)),
        DUPLICATE_LOOKS.with(|looks| looks.replace(0)),
    )
}

/// One slot: a tombstone once `ready` is set.
struct Slot {
    /// `1` once `value` is written and never written again (T2, T3).
    ready: AtomicUsize,
    value: UnsafeCell<MaybeUninit<RangeTombstone>>,
}

impl Slot {
    fn empty() -> Self {
        Self {
            ready: AtomicUsize::new(0),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

/// Which segment `position` lives in, and its offset there.
#[inline]
fn locate(position: usize) -> (usize, usize) {
    let bucket = position / FIRST_SEGMENT + 1;
    let segment = (usize::BITS - 1 - bucket.leading_zeros()) as usize;
    let start = FIRST_SEGMENT * ((1usize << segment) - 1);
    (segment, position - start)
}

/// Slots in `segment`.
#[inline]
fn segment_len(segment: usize) -> usize {
    FIRST_SEGMENT << segment
}

/// The order a memtable has always reported its tombstones in: by start,
/// then end, then newest first. `clone_range_tombstones` and
/// `newer_range_tombstone` answer in it.
fn listing_order(a: &RangeTombstone, b: &RangeTombstone) -> CmpOrdering {
    a.start
        .cmp(&b.start)
        .then_with(|| a.end.cmp(&b.end))
        .then_with(|| b.seq.cmp(&a.seq))
}

/// A sorted view over the first `covered` appends. A loom build never
/// makes one (T4).
#[cfg_attr(loom, allow(dead_code))]
struct TombstoneIndex {
    /// How many appends, from the first, the view covers.
    covered: usize,
    /// Positions of those appends in [`listing_order`].
    order: Box<[u32]>,
    /// `reach[j]` is the position, among `order[..=j]`, of the tombstone
    /// with the greatest end: a walk down `order` from a key's insertion
    /// point stops once no earlier tombstone reaches the key.
    reach: Box<[u32]>,
}

/// The log. See the module docs for its invariants.
pub(crate) struct TombstoneLog {
    /// Segment `k` holds `FIRST_SEGMENT << k` slots; null until an append
    /// first needs it, then installed by one compare-and-swap.
    segments: [AtomicPtr<Slot>; SEGMENTS],
    /// Positions handed out to appends (T1). A reader's scan ends here.
    claimed: AtomicUsize,
    /// Heap bytes the ready tombstones own, for the memtable's size.
    bytes: AtomicUsize,
    /// The sorted view of a prefix (T4); empty until the first rebuild.
    #[cfg(not(loom))]
    index: kovan::AtomOption<TombstoneIndex>,
    /// One rebuild of the index at a time: a second thread that would
    /// rebuild leaves it to the first rather than build the same view.
    #[cfg(not(loom))]
    rebuilding: AtomicUsize,
}

// SAFETY: a slot is written only by the one append its position was handed
// to (T1), before the release that marks it ready, and read only after the
// acquire that observes that release (T2); a `RangeTombstone` is
// `Send + Sync`. Segment pointers and counters are atomics.
unsafe impl Send for TombstoneLog {}
// SAFETY: as above.
unsafe impl Sync for TombstoneLog {}

#[cfg(test)]
thread_local! {
    /// Test seam: runs once, on this thread, right after the next rebuild
    /// has looked for the first position not ready. Thread-local so a
    /// parallel test never fires another test's step.
    static AFTER_READY_SCAN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run the step a test set for this instant, if any.
#[cfg(test)]
fn after_ready_scan() {
    if let Some(step) = AFTER_READY_SCAN.with(|hook| hook.borrow_mut().take()) {
        step();
    }
}

/// Test-only: run `step` once, on this thread, right after the next
/// rebuild of the index has looked for the first position not ready: the
/// instant another append's claim lands past that bound.
#[cfg(test)]
fn after_next_ready_scan(step: impl FnOnce() + 'static) {
    AFTER_READY_SCAN.with(|hook| *hook.borrow_mut() = Some(Box::new(step)));
}

impl TombstoneLog {
    /// An empty log. Allocates nothing until the first append.
    pub(crate) fn new() -> Self {
        Self {
            segments: std::array::from_fn(|_| AtomicPtr::new(ptr::null_mut())),
            claimed: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
            #[cfg(not(loom))]
            index: kovan::AtomOption::none(),
            #[cfg(not(loom))]
            rebuilding: AtomicUsize::new(0),
        }
    }

    /// Positions handed out so far, acquired: a scan of the log ends here.
    /// A position below it may not be ready yet; [`Self::get`] says.
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.claimed.load(Ordering::Acquire)
    }

    /// Heap bytes the ready tombstones own.
    pub(crate) fn bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }

    /// The slot at `position`, or `None` when its segment is not installed
    /// yet (its append has not got that far).
    #[inline]
    fn slot(&self, position: usize) -> Option<&Slot> {
        let (segment, offset) = locate(position);
        let base = self.segments[segment].load(Ordering::Acquire);
        if base.is_null() {
            return None;
        }
        // SAFETY: a segment holds `segment_len(segment)` slots and `locate`
        // returns an offset below that; a segment installed is never freed
        // or replaced while the log lives (T3).
        Some(unsafe { &*base.add(offset) })
    }

    /// The tombstone at `position`, or `None` while its append has not
    /// marked it ready (T2).
    #[inline]
    fn get(&self, position: usize) -> Option<&RangeTombstone> {
        let slot = self.slot(position)?;
        if slot.ready.load(Ordering::Acquire) == 0 {
            return None;
        }
        // SAFETY: the flag was acquired as set, so the append wrote this
        // slot before the release this thread observed (T2), and nothing
        // writes it again (T3). The reference lives no longer than `self`,
        // which owns the slot.
        Some(unsafe { slot.value.with(|value| (*value).assume_init_ref()) })
    }

    /// The tombstone at a position the index covers: ready by T4.
    #[inline]
    fn indexed(&self, position: u32) -> &RangeTombstone {
        static NEVER: std::sync::OnceLock<RangeTombstone> = std::sync::OnceLock::new();
        self.get(position as usize)
            // T4 makes this unreachable; an empty range deletes nothing.
            .unwrap_or_else(|| NEVER.get_or_init(|| RangeTombstone::new(Vec::new(), Vec::new(), 0)))
    }

    /// Whether the log already holds `tombstone`. A commit's applies run in
    /// sequence order, so its tombstone, when present, sits above every
    /// ready tombstone of a lower sequence: the walk from the end stops at
    /// the first of those.
    fn holds(&self, tombstone: &RangeTombstone) -> bool {
        for position in (0..self.len()).rev() {
            #[cfg(all(test, not(loom)))]
            DUPLICATE_LOOKS.with(|looks| looks.set(looks.get() + 1));
            let Some(held) = self.get(position) else {
                continue;
            };
            if held.seq < tombstone.seq {
                return false;
            }
            if held.seq == tombstone.seq
                && held.start == tombstone.start
                && held.end == tombstone.end
            {
                return true;
            }
        }
        false
    }

    /// The segment `segment`, installed by this call when no append has
    /// needed it yet.
    fn segment(&self, segment: usize) -> *mut Slot {
        let installed = self.segments[segment].load(Ordering::Acquire);
        if !installed.is_null() {
            return installed;
        }
        let slots: Box<[Slot]> = (0..segment_len(segment)).map(|_| Slot::empty()).collect();
        let fresh = Box::into_raw(slots).cast::<Slot>();
        match self.segments[segment].compare_exchange(
            ptr::null_mut(),
            fresh,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => fresh,
            Err(winner) => {
                // SAFETY: `fresh` came from `Box::into_raw` of a boxed slice
                // of exactly this many slots just above, and no other thread
                // has seen it.
                drop(unsafe {
                    Box::from_raw(ptr::slice_from_raw_parts_mut(fresh, segment_len(segment)))
                });
                winner
            }
        }
    }

    /// Append `tombstone`, unless the log already holds it (T1). Any number
    /// of threads may append at once.
    pub(crate) fn push(&self, tombstone: RangeTombstone) {
        if self.holds(&tombstone) {
            return;
        }
        self.append(tombstone);
        #[cfg(not(loom))]
        self.maybe_rebuild();
    }

    /// Append every tombstone of `batch` the log does not hold yet, then
    /// publish one index over them all (T4).
    ///
    /// What [`Self::push`] does for each, at a cost linear in the batch
    /// (one sort of it aside): `push` looks through the log for its
    /// tombstone and rebuilds the index every [`REBUILD`] appends, so `n`
    /// pushes of tombstones that share a sequence cost on the order of
    /// `n * n` slot reads. Here the log is looked at once, before the first
    /// append, and the index is built once, after the last. For recovery,
    /// which can bring thousands at a time.
    ///
    /// Each append is the one `push` makes (T1, T2), so any number of
    /// threads may run this beside appends and reads. A tombstone another
    /// thread appends after the one look can end up listed twice, which is
    /// harmless (T1), and a rebuild another thread has under way leaves
    /// this batch to the next one, as it leaves any append. Recovery calls
    /// it before the memtable is shared, where neither can happen.
    pub(crate) fn extend(&self, mut batch: Vec<RangeTombstone>) {
        let same = |a: &RangeTombstone, b: &RangeTombstone| {
            a.seq == b.seq && a.start == b.start && a.end == b.end
        };
        // In listing order: the batch's own duplicates fall side by side,
        // and the index's sort of these appends finds them sorted.
        batch.sort_by(listing_order);
        batch.dedup_by(|a, b| same(a, b));
        let held = self.len();
        if held > 0 {
            let mut existing: Vec<&RangeTombstone> = (0..held)
                .filter_map(|position| self.get(position))
                .collect();
            existing.sort_by(|a, b| listing_order(a, b));
            batch.retain(|tombstone| {
                existing
                    .binary_search_by(|held| listing_order(held, tombstone))
                    .is_err()
            });
        }
        for tombstone in batch {
            self.append(tombstone);
        }
        #[cfg(not(loom))]
        self.maybe_rebuild();
    }

    /// Write `tombstone` into the next position and mark it ready (T1, T2).
    fn append(&self, tombstone: RangeTombstone) {
        // T1: the position is this append's alone.
        let position = self.claimed.fetch_add(1, Ordering::AcqRel);
        // SAFETY: the add above handed `position` to this append alone.
        unsafe { self.fill(position, tombstone) };
    }

    /// The rest of an append once it claimed `position`: write the slot,
    /// then mark it ready (T2).
    ///
    /// # Safety
    ///
    /// `position` was handed to the caller by the add on `claimed`, and no
    /// other call fills it (T1): the slot is the caller's alone.
    #[inline]
    unsafe fn fill(&self, position: usize, tombstone: RangeTombstone) {
        let heap = tombstone.start.len() + tombstone.end.len() + size_of::<RangeTombstone>();
        let (segment, offset) = locate(position);
        let base = self.segment(segment);
        // SAFETY: `offset` is within the segment (see `slot`), and the slot
        // is this append's alone until the release below (T1, T2).
        let slot = unsafe { &*base.add(offset) };
        // SAFETY: as above; no reader touches `value` before `ready` is set.
        unsafe {
            slot.value.with_mut(|value| {
                (*value).write(tombstone);
            });
        }
        self.bytes.fetch_add(heap, Ordering::Relaxed);
        // T2: the slot is whole before the flag that says so.
        slot.ready.store(1, Ordering::Release);
    }

    /// Publish a sorted view of the leading ready positions once [`REBUILD`]
    /// of them have gathered past the current view (T4). One thread at a
    /// time: another that would rebuild leaves it to the one that is.
    #[cfg(not(loom))]
    fn maybe_rebuild(&self) {
        let covered = self.index.load().map_or(0, |index| index.covered);
        if self.len() - covered < REBUILD {
            return;
        }
        if self.rebuilding.swap(1, Ordering::AcqRel) != 0 {
            return;
        }
        self.rebuild();
        self.rebuilding.store(0, Ordering::Release);
    }

    /// [`Self::maybe_rebuild`]'s work, with the rebuild flag held.
    #[cfg(not(loom))]
    fn rebuild(&self) {
        let (covered, old_order) = match self.index.load() {
            Some(index) => (index.covered, index.order.to_vec()),
            None => (0, Vec::new()),
        };
        // T4: the view covers ready positions only, so it stops at the
        // first one that is not. The count is read once: a position claimed
        // after the scan passed its bound is not ready, and a second read
        // would let it into the view.
        let len = self.len();
        let published = (covered..len)
            .find(|position| self.get(*position).is_none())
            .unwrap_or(len);
        #[cfg(test)]
        after_ready_scan();
        if published - covered < REBUILD {
            return;
        }
        // Positions past `u32::MAX` are never indexed; a log that long is
        // read by its tail scan instead, which is slower and still exact.
        let Ok(last) = u32::try_from(published) else {
            return;
        };
        let Ok(first_new) = u32::try_from(covered) else {
            return;
        };
        let by_listing = |a: &u32, b: &u32| listing_order(self.indexed(*a), self.indexed(*b));
        let mut fresh: Vec<u32> = (first_new..last).collect();
        fresh.sort_by(by_listing);
        let mut order = Vec::with_capacity(published);
        let (mut old, mut new) = (
            old_order.into_iter().peekable(),
            fresh.into_iter().peekable(),
        );
        loop {
            let take_old = match (old.peek(), new.peek()) {
                (Some(a), Some(b)) => by_listing(a, b) != CmpOrdering::Greater,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            let next = if take_old { old.next() } else { new.next() };
            order.extend(next);
        }
        let mut reach = Vec::with_capacity(order.len());
        for &position in &order {
            let farthest = match reach.last() {
                Some(&prev) if self.indexed(prev).end >= self.indexed(position).end => prev,
                _ => position,
            };
            reach.push(farthest);
        }
        #[cfg(test)]
        INDEXES_BUILT.with(|built| built.set(built.get() + 1));
        self.index.store_some(TombstoneIndex {
            covered: published,
            order: order.into_boxed_slice(),
            reach: reach.into_boxed_slice(),
        });
        // The view this replaced can be as large as the log; hand it to
        // the reclaimer now rather than when this thread next fills a
        // batch.
        super::super::reclaim::submit();
    }

    /// The largest sequence of a tombstone covering `user_key` that a
    /// snapshot at `snapshot_seq` sees, or `0` when there is none.
    pub(crate) fn covering_seq(&self, user_key: &[u8], snapshot_seq: u64) -> u64 {
        let mut best = 0;
        let covered = self.scan_index(|index| {
            let mut j = index
                .order
                .partition_point(|&p| self.indexed(p).start.as_slice() <= user_key);
            while j > 0 {
                j -= 1;
                if self.indexed(index.reach[j]).end.as_slice() <= user_key {
                    break;
                }
                let tombstone = self.indexed(index.order[j]);
                if tombstone.seq <= snapshot_seq && tombstone.covers(user_key) {
                    best = best.max(tombstone.seq);
                }
            }
        });
        // The index is loaded before `len`, so `len` is never below what it
        // covers and the two together read every position once.
        for tombstone in (covered..self.len()).filter_map(|position| self.get(position)) {
            if tombstone.seq <= snapshot_seq && tombstone.covers(user_key) {
                best = best.max(tombstone.seq);
            }
        }
        best
    }

    /// The first tombstone in listing order with a sequence above `floor`
    /// and at or below `ceil` that overlaps `[lo, hi)`. `u64::MAX` as
    /// `ceil` bounds nothing.
    pub(crate) fn first_newer_overlap(
        &self,
        lo: &[u8],
        hi: &[u8],
        floor: u64,
        ceil: u64,
    ) -> Option<&RangeTombstone> {
        let matches = |t: &RangeTombstone| t.seq > floor && t.seq <= ceil && t.overlaps(lo, hi);
        let mut found: Option<&RangeTombstone> = None;
        let covered = self.scan_index(|index| {
            found = index
                .order
                .iter()
                .map(|&p| self.indexed(p))
                .find(|t| matches(t));
        });
        for tombstone in (covered..self.len()).filter_map(|position| self.get(position)) {
            if matches(tombstone)
                && found.is_none_or(|best| listing_order(tombstone, best) == CmpOrdering::Less)
            {
                found = Some(tombstone);
            }
        }
        found
    }

    /// Whether any ready tombstone satisfies `pred`.
    pub(crate) fn any(&self, mut pred: impl FnMut(&RangeTombstone) -> bool) -> bool {
        (0..self.len())
            .filter_map(|position| self.get(position))
            .any(&mut pred)
    }

    /// Every ready tombstone, sorted and deduplicated as the memtable has
    /// always listed them.
    ///
    /// The index is already in listing order, so only the tail past it is
    /// sorted and merged in: `O(n + t log t)` for `t < REBUILD`, not a sort
    /// of the whole log on every iterator built over the memtable.
    pub(crate) fn to_sorted_vec(&self) -> Vec<RangeTombstone> {
        let mut indexed: Vec<RangeTombstone> = Vec::new();
        let covered = self.scan_index(|index| {
            indexed = index
                .order
                .iter()
                .map(|&position| self.indexed(position).clone())
                .collect();
        });
        let mut tail: Vec<RangeTombstone> = (covered..self.len())
            .filter_map(|position| self.get(position).cloned())
            .collect();
        tail.sort_by(listing_order);
        let mut merged = Vec::with_capacity(indexed.len() + tail.len());
        let (mut left, mut right) = (indexed.into_iter().peekable(), tail.into_iter().peekable());
        loop {
            let take_left = match (left.peek(), right.peek()) {
                (Some(a), Some(b)) => listing_order(a, b) != CmpOrdering::Greater,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            merged.extend(if take_left { left.next() } else { right.next() });
        }
        merged.dedup_by(|a, b| a.start == b.start && a.end == b.end && a.seq == b.seq);
        merged
    }

    /// Run `walk` over the index when there is one; returns how many
    /// appends it covers, from which the caller's tail scan starts.
    #[cfg(not(loom))]
    fn scan_index(&self, walk: impl FnOnce(&TombstoneIndex)) -> usize {
        match self.index.load() {
            Some(index) => {
                walk(&index);
                index.covered
            }
            None => 0,
        }
    }

    /// Without an index the whole prefix is the tail.
    #[cfg(loom)]
    fn scan_index(&self, _walk: impl FnOnce(&TombstoneIndex)) -> usize {
        0
    }
}

impl Drop for TombstoneLog {
    fn drop(&mut self) {
        // `&mut self`: no other thread can touch the log, so relaxed loads
        // read the final values.
        let claimed = self.claimed.load(Ordering::Relaxed);
        for position in 0..claimed {
            let (segment, offset) = locate(position);
            let base = self.segments[segment].load(Ordering::Relaxed);
            if base.is_null() {
                continue;
            }
            // SAFETY: `&mut self` excludes every reader and writer; a slot
            // whose flag is set was written exactly once (T2, T3), and one
            // whose flag is clear holds nothing to drop.
            unsafe {
                let slot = &*base.add(offset);
                if slot.ready.load(Ordering::Relaxed) != 0 {
                    slot.value.with_mut(|value| (*value).assume_init_drop());
                }
            }
        }
        for (segment, base) in self.segments.iter().enumerate() {
            let base = base.load(Ordering::Relaxed);
            if !base.is_null() {
                // SAFETY: installed in `segment` as a boxed slice of exactly
                // this many slots, and freed once, here.
                drop(unsafe {
                    Box::from_raw(ptr::slice_from_raw_parts_mut(base, segment_len(segment)))
                });
            }
        }
    }
}

#[cfg(all(test, not(loom)))]
#[path = "tombstones_tests.rs"]
mod tests;
