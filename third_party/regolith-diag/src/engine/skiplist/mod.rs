//! Insert-only concurrent skip list over a bump [`Arena`].
//!
//! One node is a single arena bump holding its header, tower, key bytes
//! and value bytes inline, so a steady-state write touches the global
//! allocator zero times. A general-purpose concurrent map would instead
//! cost three heap allocations per write (internal key, value, node)
//! plus the garbage its reclamation scheme defers.
//!
//! # Node layout
//!
//! One variable-length allocation, [`NODE_ALIGN`]-aligned:
//!
//! ```text
//! offset  0 : u32                    key_len     internal-key bytes
//! offset  4 : u32                    value_len
//! offset  8 : u8                     height      1 ..= MAX_HEIGHT
//! offset  9 : [u8; 7]                padding     keeps the tower aligned
//! offset 16 : [AtomicPtr<u8>; height] tower      level 0 first
//! offset 16 + PTR*height             key bytes
//! offset 16 + PTR*height + key_len   value bytes
//! ```
//!
//! # Concurrency contract
//!
//! Any number of threads insert at once, and any number read, with no lock.
//! The commit pipeline lets whichever thread reaches a decided commit apply
//! it (`engine::commit`, plan 4.7), so two threads can be inserting the very
//! same entries. An insert is therefore **idempotent per internal key**: the
//! key is the user key, the sequence and the value type, and the list holds
//! at most one node per key (S2). Inserting a key the list already holds
//! links nothing and changes nothing.
//!
//! [`ArenaSkipList::insert_with_hint`] is the same insert with a private
//! splice: it takes an [`InsertHint`] bound to one list and, for a key that
//! sorts after the hint's position, starts its descent there instead of at
//! the head. See [`InsertHint`] for the invariants (H1 to H3) that make a
//! hint safe to reuse across a whole run of inserts.
//!
//! The safety of the whole module rests on these invariants, named at
//! every `unsafe` site that relies on them. They are what the loom and
//! miri models exist to check.
//!
//! - **S1 (publication).** Every byte of a node - header, tower, key and
//!   value - is written before the compare-and-swap that links it into
//!   level 0. A reader that observes the node through an `Acquire` load
//!   therefore observes it fully initialised. A node is linked at level 0
//!   first and at each higher level after, so a node reachable at any level
//!   is reachable at level 0.
//! - **S2 (one node per key, linked by compare-and-swap).** A node is
//!   linked at a level by one compare-and-swap of its predecessor's link,
//!   from the successor the inserter saw to the node. An inserter that
//!   loses the swap walks forward from the same predecessor, whose key is
//!   still below its own (S3), and tries again. At level 0 the walk is also
//!   the duplicate check: an inserter that finds its own key there has lost
//!   to a thread inserting the same entry, and links nothing. Level 0 is a
//!   sorted list with no duplicates because every link into it is such a
//!   swap between a strictly smaller key and a strictly larger one.
//! - **S3 (no unlinking).** No node is ever removed. A `next` pointer
//!   observed once stays a valid node pointer until the whole arena dies.
//! - **S4 (provenance).** Every node pointer derives from the pointer
//!   `Arena::alloc` returned for that node; none is synthesised from an
//!   integer and none crosses chunks.
//! - **S5 (aliasing).** `NodeRef::key` and `NodeRef::value` form `&[u8]`
//!   over regions that are never written again after S1, and no `&mut`
//!   to any node byte exists once the node is published.
//! - **S6 (bounds).** `key_len` and `value_len` are written by the same
//!   code that sized the allocation, so both slices stay inside the
//!   node. `insert` refuses a key or value that does not fit a `u32`,
//!   which [`crate::Options::validate`] makes unreachable.
//! - **S7 (height in range).** `random_height` returns `1 ..= MAX_HEIGHT`,
//!   and every tower walk is bounded by the node's own height, so no
//!   read runs past the tower into the key bytes.
//! - **S8 (`Send` + `Sync`).** The only cross-thread communication is
//!   through the tower's `AtomicPtr`s, and the only mutation of a published
//!   node is a compare-and-swap of one of them: S1 plus S2 plus S3.
//! - **S9 (head sentinel).** The head is a standalone allocation owned
//!   by the list, so its address is stable for the list's life and an
//!   untouched memtable reserves no arena chunk at all.
//!
//! The arena's own invariants (A1 to A8), which these build on, are
//! documented in [`super::arena`].

#![allow(unsafe_code)]

use std::marker::PhantomData;
use std::ptr::NonNull;

use super::arena::Arena;
use super::internal_key::{INTERNAL_KEY_SUFFIX_LEN, compare_internal_keys, compare_internal_split};
use crate::sync::internal::{Arc, AtomicPtr, AtomicU64, AtomicUsize, Ordering};

#[cfg(test)]
mod tests;

/// Maximum tower height. With [`BRANCHING`] 4, height 12 indexes about
/// 16.7M entries, which covers a 64 MiB memtable of 64-byte entries with
/// room to spare.
const MAX_HEIGHT: usize = 12;

/// Expected fan-out between adjacent levels.
const BRANCHING: u64 = 4;

/// Bytes before the tower: two lengths, the height, and padding.
const NODE_HEADER: usize = 16;

/// One forward pointer.
type Link = AtomicPtr<u8>;

/// Size of one tower slot.
const PTR_SIZE: usize = size_of::<Link>();

/// Alignment every node is allocated at.
pub(crate) const NODE_ALIGN: usize = if align_of::<Link>() > 8 {
    align_of::<Link>()
} else {
    8
};

/// Size of the head sentinel: a full-height tower and nothing else.
const HEAD_SIZE: usize = NODE_HEADER + PTR_SIZE * MAX_HEIGHT;

/// Bytes one node occupies for the given shape.
fn node_size(key_len: usize, value_len: usize, height: usize) -> usize {
    (NODE_HEADER + PTR_SIZE * height + key_len + value_len).next_multiple_of(NODE_ALIGN)
}

/// Most arena bytes an entry of this shape can take.
///
/// Tower height is drawn at insert time, so the exact cost is not known
/// before the fact; this assumes the tallest tower. The commit leader
/// uses it to keep a group from carrying the active memtable past
/// `write_buffer_size`, where over-stating a cost only ends a group
/// early and under-stating one would break the bound.
pub(crate) fn max_node_size(internal_key_len: usize, value_len: usize) -> usize {
    node_size(internal_key_len, value_len, MAX_HEIGHT)
}

/// Read a node's `(key_len, value_len, height)` header.
///
/// # Safety
///
/// `node` must point at an initialised node or the head sentinel (S1).
unsafe fn header(node: *const u8) -> (usize, usize, usize) {
    unsafe {
        let key_len = node.cast::<u32>().read() as usize;
        let value_len = node.add(4).cast::<u32>().read() as usize;
        let height = node.add(8).read() as usize;
        (key_len, value_len, height)
    }
}

/// Borrow one tower slot.
///
/// # Safety
///
/// `node` must point at an initialised node whose height is greater than
/// `level` (S1, S7).
unsafe fn link(node: *const u8, level: usize) -> &'static Link {
    // SAFETY: the tower starts at NODE_HEADER and holds `height` slots,
    // each `PTR_SIZE` wide and `NODE_ALIGN`-aligned because the node is.
    // The `'static` lifetime is contained by the callers, which never
    // hand it out past their own node reference (S3, A5).
    unsafe { &*node.add(NODE_HEADER + level * PTR_SIZE).cast::<Link>() }
}

/// The node linked after `node` at `level`, or `None` at the end.
///
/// # Safety
///
/// Same as [`link`].
unsafe fn next_at(node: *const u8, level: usize) -> Option<NonNull<u8>> {
    // Acquire pairs with the Release store in `insert` that published the
    // node (S1): observing the pointer implies observing its bytes.
    NonNull::new(unsafe { link(node, level) }.load(Ordering::Acquire))
}

/// A node's internal key.
///
/// # Safety
///
/// `node` must point at an initialised node (S1); the head sentinel has
/// `key_len == 0` and must not be passed here.
unsafe fn node_key<'a>(node: *const u8) -> &'a [u8] {
    unsafe {
        let (key_len, _, height) = header(node);
        std::slice::from_raw_parts(node.add(NODE_HEADER + PTR_SIZE * height), key_len)
    }
}

/// Whether `prev`'s link at `level` still is `next`: the H3 re-check a
/// hinted insert runs before it trusts a level it did not just walk.
///
/// # Safety
///
/// `prev` is the head or a node of height greater than `level` (S1, S7).
unsafe fn adjacent(prev: NonNull<u8>, level: usize, next: *mut u8) -> bool {
    // SAFETY: the caller upholds the precondition above.
    unsafe { next_at(prev.as_ptr(), level) }.map_or(std::ptr::null_mut(), |n| n.as_ptr()) == next
}

/// Walk level `level` forward from `from` to the last node whose key is
/// below the target, returning it and its successor (null at the end).
///
/// # Safety
///
/// `from` is the head or a node of height greater than `level` whose key
/// is below the target (S1, S3, S7).
unsafe fn walk_level(
    from: NonNull<u8>,
    level: usize,
    user_key: &[u8],
    trailer: &[u8; INTERNAL_KEY_SUFFIX_LEN],
) -> (NonNull<u8>, *mut u8) {
    let mut cursor = from;
    loop {
        // SAFETY: `cursor` starts at `from` and only ever advances to a
        // node whose key the caller's precondition already bounds below
        // the target, so the precondition holds at every step (S1, S3,
        // S7).
        match unsafe { next_at(cursor.as_ptr(), level) } {
            Some(next) => {
                // SAFETY (S1, S3): `next` is a live node.
                let next_key = unsafe { node_key(next.as_ptr()) };
                if compare_internal_split(next_key, user_key, trailer).is_lt() {
                    cursor = next;
                } else {
                    return (cursor, next.as_ptr());
                }
            }
            None => return (cursor, std::ptr::null_mut()),
        }
    }
}

/// A borrowed view of one node.
///
/// The lifetime ties it to the skip list, which owns the `Arc<Arena>`
/// keeping the bytes alive (A5).
#[derive(Clone, Copy)]
pub(crate) struct NodeRef<'a> {
    ptr: NonNull<u8>,
    _marker: PhantomData<&'a ArenaSkipList>,
}

impl<'a> NodeRef<'a> {
    fn new(ptr: NonNull<u8>) -> Self {
        Self {
            ptr,
            _marker: PhantomData,
        }
    }

    /// The node's internal key.
    pub(crate) fn key(&self) -> &'a [u8] {
        // SAFETY (S1, S5): the node was fully written before it became
        // reachable and its bytes are never written again, so a shared
        // slice over them is sound for as long as the arena lives.
        unsafe { node_key(self.ptr.as_ptr()) }
    }

    /// The node's value bytes: empty for a deletion tombstone.
    pub(crate) fn value(&self) -> &'a [u8] {
        let (ptr, len) = self.value_span();
        match ptr {
            // SAFETY (S1, S5, S6): `value_span` derived the pointer from
            // this node's own allocation and the length from the header
            // that sized it.
            Some(ptr) => unsafe { std::slice::from_raw_parts(ptr.as_ptr(), len) },
            None => &[],
        }
    }

    /// Address and length of the internal-key bytes inside the arena,
    /// so a caller can build a zero-copy [`crate::DbSlice`] over them.
    pub(crate) fn key_span(&self) -> (Option<NonNull<u8>>, usize) {
        // SAFETY (S1, S6): the header was written before publication and
        // sized this very allocation, so the key region is in range.
        unsafe {
            let node = self.ptr.as_ptr();
            let (key_len, _, height) = header(node);
            if key_len == 0 {
                return (None, 0);
            }
            let ptr = node.add(NODE_HEADER + PTR_SIZE * height);
            (NonNull::new(ptr), key_len)
        }
    }

    /// Address and length of the value bytes inside the arena, so a
    /// caller can build a zero-copy [`crate::DbSlice`] over them.
    pub(crate) fn value_span(&self) -> (Option<NonNull<u8>>, usize) {
        // SAFETY (S1, S6): the header was written before publication and
        // sized this very allocation, so the value region is in range.
        unsafe {
            let node = self.ptr.as_ptr();
            let (key_len, value_len, height) = header(node);
            if value_len == 0 {
                return (None, 0);
            }
            let ptr = node.add(NODE_HEADER + PTR_SIZE * height + key_len);
            (NonNull::new(ptr), value_len)
        }
    }

    /// The next node in internal-key order.
    pub(crate) fn next(&self) -> Option<NodeRef<'a>> {
        // SAFETY (S1, S7): every node has at least one tower slot.
        unsafe { next_at(self.ptr.as_ptr(), 0) }.map(NodeRef::new)
    }
}

/// Where the previous insert through it landed: the predecessor and the
/// successor at every level, so an insert of a key that sorts after that
/// position starts its descent there instead of at the head.
///
/// A hint can only save work, never misplace a node: every level taken
/// from it is re-validated against the live links before it is used (see
/// [`ArenaSkipList::insert_with_hint`]). The lifetime ties it to the list
/// that produced it, so no node it points at can be freed while it
/// exists (S3, A5), and `head` ties it to that list's identity, so a
/// hint handed to a different list of the same lifetime restarts from
/// that list's head.
///
/// Invariants, maintained by every insert through the hint and relied on
/// by the next one (H1 to H3):
///
/// - **H1 (shape).** Once bound, `prev[l]` is the head or a node of
///   height greater than `l`, and `next[l]` is null or a node.
/// - **H2 (monotone).** Going up the levels, `prev` keys never increase
///   and `next` keys never decrease: `prev[l+1].key <= prev[l].key` (the
///   head counts as minus infinity) and `next[l+1].key >= next[l].key`
///   (null counts as plus infinity). This is what lets one bracket check
///   at level `L` vouch for every level above it.
/// - **H3 (adjacency is advisory).** `next[l]` was `prev[l]`'s successor
///   at level `l` when the hint was last written. Another insert, on this
///   thread or any other, may have linked a node there since; the hint is
///   never trusted on this, every level it is used at is re-read first, and
///   the compare-and-swap that links a node catches whatever changed after
///   the re-read.
///
/// Why H2 survives a write-back: after the descent and the repair loop
/// in `insert_inner`, every level below the new node's height holds the
/// exact splice for the new key `T` (`prev[l]` is the last node at level
/// `l` with key below `T`, `next[l]` the first at or above it), and
/// exact splices are monotone across levels because a node present at
/// level `l+1` is present at level `l`. Levels at or above
/// `max(recompute, height)` keep their old values (or a freshly walked
/// one, which is exactly as monotone), which were already monotone and
/// bracket `T` by the old H2 and the level-`recompute` bracket check;
/// the repair loop only moves a `prev` forward and a `next` backward, so
/// the boundary between the exact levels and the kept levels still
/// orders. Replacing `prev[l]` by the new node for `l` below its height
/// writes a single key `T` into levels whose neighbours above have keys
/// below `T` and whose `next` are at or above `T`.
pub(crate) struct InsertHint<'a> {
    /// Head sentinel of the list this hint belongs to; null until bound.
    head: *const u8,
    prev: [NonNull<u8>; MAX_HEIGHT],
    next: [*mut u8; MAX_HEIGHT],
    _marker: PhantomData<&'a ArenaSkipList>,
}

/// Insert-only concurrent skip list over a bump arena.
pub(crate) struct ArenaSkipList {
    arena: Arc<Arena>,
    /// Head sentinel: a full-height tower with no key or value. It is a
    /// standalone allocation rather than an arena one, so an untouched
    /// memtable reserves no arena chunk at all (S9).
    head: NonNull<u8>,
    /// xorshift64 state for the height draw. Two inserters may draw from
    /// the same state and so draw the same height; any height in range is a
    /// valid one (S7), so `Relaxed` suffices.
    rnd: AtomicU64,
    count: AtomicUsize,
}

// SAFETY (S8): the only cross-thread communication is through the tower's
// `AtomicPtr`s (S1), a published node is only ever changed by a
// compare-and-swap of one of them (S2), and no node is ever unlinked or
// freed while the list lives (S3, A5).
unsafe impl Send for ArenaSkipList {}
// SAFETY: see the `Send` impl above.
unsafe impl Sync for ArenaSkipList {}

impl ArenaSkipList {
    /// A new, empty list over `arena`.
    ///
    /// Returns `None` only when the global allocator refused the head
    /// sentinel, which the caller surfaces as an out-of-memory error
    /// rather than panicking.
    pub(crate) fn new(arena: Arc<Arena>) -> Option<Self> {
        let layout = std::alloc::Layout::from_size_align(HEAD_SIZE, NODE_ALIGN).ok()?;
        // SAFETY: `HEAD_SIZE` is nonzero and `NODE_ALIGN` is a power of
        // two, checked by `from_size_align` above.
        let head = NonNull::new(unsafe { std::alloc::alloc(layout) })?;
        // SAFETY (S9): `head` addresses `HEAD_SIZE` freshly allocated
        // bytes, which is exactly the header plus a `MAX_HEIGHT` tower.
        unsafe { init_node_header(head.as_ptr(), 0, 0, MAX_HEIGHT) };
        Some(Self {
            arena,
            head,
            // Any nonzero seed works; a fixed one keeps the height draw
            // reproducible across runs, which the tests rely on.
            rnd: AtomicU64::new(0x2545_F491_4F6C_DD1D),
            count: AtomicUsize::new(0),
        })
    }

    /// The arena backing every node.
    pub(crate) fn arena(&self) -> &Arc<Arena> {
        &self.arena
    }

    /// Whether the list holds no entries.
    pub(crate) fn is_empty(&self) -> bool {
        self.first().is_none()
    }

    /// Number of entries the list holds.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }

    fn random_height(&self) -> usize {
        let mut x = self.rnd.load(Ordering::Relaxed);
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rnd.store(x, Ordering::Relaxed);
        let mut height = 1;
        let mut draw = x;
        while height < MAX_HEIGHT && draw.is_multiple_of(BRANCHING) {
            height += 1;
            draw /= BRANCHING;
        }
        height
    }

    /// A fresh, unbound hint for a run of inserts into this list.
    pub(crate) fn insert_hint(&self) -> InsertHint<'_> {
        InsertHint {
            head: std::ptr::null(),
            prev: [NonNull::dangling(); MAX_HEIGHT],
            next: [std::ptr::null_mut(); MAX_HEIGHT],
            _marker: PhantomData,
        }
    }

    /// Insert as [`ArenaSkipList::insert`], starting the descent where
    /// `hint` says the previous insert landed when the new key sorts
    /// after the hinted position, and from the head otherwise. Leaves
    /// `hint` pointing at the new position.
    pub(crate) fn insert_with_hint<'a>(
        &'a self,
        hint: &mut InsertHint<'a>,
        user_key: &[u8],
        seq: u64,
        value_type: u8,
        value: &[u8],
    ) -> bool {
        self.insert_inner(user_key, seq, value_type, value, Some(hint))
    }

    /// Insert `user_key || !seq || value_type -> value`, unless the list
    /// already holds that internal key (S2): then nothing changes, and the
    /// value given is not looked at. Any number of threads may insert at
    /// once.
    ///
    /// The internal key is assembled directly inside the node, so no
    /// intermediate buffer is built and nothing is copied twice.
    ///
    /// Returns `false` without inserting when the entry cannot be
    /// represented (a key or value of 4 GiB or more), which
    /// [`crate::Options::validate`] makes unreachable.
    pub(crate) fn insert(&self, user_key: &[u8], seq: u64, value_type: u8, value: &[u8]) -> bool {
        self.insert_inner(user_key, seq, value_type, value, None)
    }

    /// Number of levels [`ArenaSkipList::insert_inner`] must recompute by
    /// walking forward from the hint's position, or [`MAX_HEIGHT`] (a
    /// full descent from the head) when the hint cannot help.
    ///
    /// Level 0 decides the direction. A key at or before the hinted
    /// predecessor restarts from the head: the hint was built for a
    /// later position and nothing above level 0 can bracket an earlier
    /// key more cheaply than the head does. A key after the hinted
    /// successor climbs, skipping the levels that share the successor it
    /// already compared against (H2 makes the compare result the same
    /// for all of them), until a level whose successor is at or past the
    /// key. By H2 the predecessor side needs no compare above level 0.
    fn hint_start_level(
        &self,
        hint: &InsertHint<'_>,
        user_key: &[u8],
        trailer: &[u8; INTERNAL_KEY_SUFFIX_LEN],
    ) -> usize {
        let head = self.head;
        if hint.prev[0] != head
            // SAFETY (H1): a bound hint's level-0 predecessor is the
            // head or a node, checked not to be the head above.
            && !compare_internal_split(unsafe { node_key(hint.prev[0].as_ptr()) }, user_key, trailer)
                .is_lt()
        {
            return MAX_HEIGHT;
        }
        let mut level = 0;
        while level < MAX_HEIGHT {
            let (prev, next) = (hint.prev[level], hint.next[level]);
            // SAFETY (H1, S3): a bound hint's `prev` is the head or a
            // node of height > level, valid for the list's life.
            if !unsafe { adjacent(prev, level, next) } {
                level += 1;
                continue;
            }
            if !next.is_null()
                // SAFETY (H1, S3): `next` is a live node when non-null.
                && compare_internal_split(unsafe { node_key(next) }, user_key, trailer).is_lt()
            {
                while level < MAX_HEIGHT && hint.next[level] == next {
                    level += 1;
                }
                continue;
            }
            return level;
        }
        MAX_HEIGHT
    }

    /// Shared body of [`ArenaSkipList::insert`] and
    /// [`ArenaSkipList::insert_with_hint`].
    fn insert_inner<'a>(
        &'a self,
        user_key: &[u8],
        seq: u64,
        value_type: u8,
        value: &[u8],
        hint: Option<&mut InsertHint<'a>>,
    ) -> bool {
        let key_len = user_key.len() + INTERNAL_KEY_SUFFIX_LEN;
        if u32::try_from(key_len).is_err() || u32::try_from(value.len()).is_err() {
            tracing::error!(
                key_len,
                value_len = value.len(),
                "memtable entry too large to encode; write dropped"
            );
            return false;
        }

        let trailer = internal_trailer(seq, value_type);
        let height = self.random_height();

        // Comparison always goes through the internal-key comparator: raw
        // byte order is wrong when one user key is a prefix of another.
        let mut prev = [self.head; MAX_HEIGHT];
        let mut next: [*mut u8; MAX_HEIGHT] = [std::ptr::null_mut(); MAX_HEIGHT];

        let recompute = match hint.as_deref() {
            Some(h) if h.head == self.head.as_ptr() => self.hint_start_level(h, user_key, &trailer),
            _ => MAX_HEIGHT,
        };

        let mut cursor = self.head;
        if recompute < MAX_HEIGHT {
            // The `Some` arm above only returns a level below `MAX_HEIGHT`
            // when the hint is bound to this list, so this always finds
            // one; the `if let` (rather than an `expect`) means that if
            // the two conditions ever drifted apart, this thread falls
            // back to the always-correct head descent below instead of
            // panicking on a write path.
            if let Some(h) = hint.as_deref() {
                prev[recompute..MAX_HEIGHT].copy_from_slice(&h.prev[recompute..MAX_HEIGHT]);
                next[recompute..MAX_HEIGHT].copy_from_slice(&h.next[recompute..MAX_HEIGHT]);
            }
            cursor = prev[recompute];
        }

        // Descend the levels the hint did not already bracket, recording
        // at every level the last node whose key is strictly less than
        // the new one and its successor.
        for level in (0..recompute).rev() {
            // SAFETY (H1, H2, S1, S3, S7): `cursor` is the head, or (when
            // `recompute < MAX_HEIGHT`) `prev[recompute]`, a node of
            // height greater than `recompute` (H1) whose key is below
            // the target by the level-0 check in `hint_start_level` plus
            // H2. Every node it reaches stays valid for the arena's life.
            let (p, n) = unsafe { walk_level(cursor, level, user_key, &trailer) };
            prev[level] = p;
            next[level] = n;
            cursor = p;
        }

        // Re-check every level the hint supplied but the descent above
        // did not touch: H3 only promises the link was current when the
        // hint was written, and a plain `insert` may have linked a node
        // there since. A level that is still adjacent is correct as
        // stored (H2: `prev[level].key <= prev[recompute].key < key <=
        // next[recompute].key <= next[level].key`); one that is not gets
        // walked forward, which finds the right pair because
        // `prev[level].key < key` still holds (keys are immutable and
        // nodes are never unlinked, S3).
        for level in recompute..height {
            // SAFETY (H1, S1, S3, S7): as in the loop above.
            if !unsafe { adjacent(prev[level], level, next[level]) } {
                // SAFETY: same as the descent above.
                let (p, n) = unsafe { walk_level(prev[level], level, user_key, &trailer) };
                prev[level] = p;
                next[level] = n;
            }
        }

        // S2: the key is already there, put by a thread applying the same
        // commit. Checked before the node is allocated, so the common
        // duplicate costs no arena bytes.
        // SAFETY (S1, S3): `next[0]` is null or a live node.
        if !next[0].is_null()
            && compare_internal_split(unsafe { node_key(next[0]) }, user_key, &trailer).is_eq()
        {
            return true;
        }

        let size = node_size(key_len, value.len(), height);
        let Some(node) = self.arena.alloc(size, NODE_ALIGN) else {
            // The global allocator refused. Aborting is what `Vec::push`
            // does, and it is the only alternative to silently losing an
            // acknowledged write.
            let layout = std::alloc::Layout::from_size_align(size, NODE_ALIGN)
                .unwrap_or_else(|_| std::alloc::Layout::new::<u8>());
            std::alloc::handle_alloc_error(layout)
        };

        // SAFETY (S1, S6): `node` addresses `size` bytes from this
        // arena, and `size` was computed from exactly these lengths, so
        // every write below stays inside the allocation. The node is not
        // reachable yet, so plain stores need no synchronisation.
        let raw = node.as_ptr();
        unsafe {
            init_node_header(raw, key_len, value.len(), height);
            let key_at = raw.add(NODE_HEADER + PTR_SIZE * height);
            std::ptr::copy_nonoverlapping(user_key.as_ptr(), key_at, user_key.len());
            std::ptr::copy_nonoverlapping(
                trailer.as_ptr(),
                key_at.add(user_key.len()),
                INTERNAL_KEY_SUFFIX_LEN,
            );
            std::ptr::copy_nonoverlapping(value.as_ptr(), key_at.add(key_len), value.len());
        }

        // Link the node, level 0 first, so a reader that reaches it from a
        // higher level always finds it linked at the bottom too (S1).
        for level in 0..height {
            loop {
                // SAFETY (S1, S7): `raw` is this thread's node, of height
                // greater than `level`, and nothing links to it at `level`
                // yet: no other thread reads or writes this slot, so a plain
                // store is enough. The swap below publishes it.
                unsafe { link(raw, level) }.store(next[level], Ordering::Relaxed);
                // SAFETY (H1, S1, S3, S7): `prev[level]` is the head or a
                // live node of height greater than `level`.
                let linked = unsafe { link(prev[level].as_ptr(), level) }.compare_exchange(
                    next[level],
                    raw,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                if linked.is_ok() {
                    break;
                }
                // S2: another insert linked a node after `prev[level]`. Its
                // key is still below this one's and nothing is ever
                // unlinked (S3), so the splice is found again by walking on
                // from it.
                // SAFETY: as in the descent above.
                let (p, n) = unsafe { walk_level(prev[level], level, user_key, &trailer) };
                prev[level] = p;
                next[level] = n;
                // SAFETY (S1, S3): `n` is null or a live node.
                if level == 0
                    && !n.is_null()
                    && compare_internal_split(unsafe { node_key(n) }, user_key, &trailer).is_eq()
                {
                    // Lost to a thread inserting this very entry. The node
                    // built here was never linked, so no reader can reach
                    // it; its bytes stay in the arena, unused.
                    return true;
                }
            }
        }

        self.count.fetch_add(1, Ordering::Release);

        if let Some(h) = hint {
            h.head = self.head.as_ptr();
            for level in 0..MAX_HEIGHT {
                h.prev[level] = if level < height { node } else { prev[level] };
                h.next[level] = next[level];
            }
        }

        true
    }

    /// Descend to level 0, returning the last node whose key is strictly
    /// less than `target` and the first whose key is greater or equal.
    ///
    /// Both come out of the **same** level-0 step, and that is load
    /// bearing: a writer may link a new node in between two reads, so a
    /// descent that stopped and then re-read `predecessor.next[0]` could
    /// hand back a node that was already inserted past `target`. A
    /// reader that then treats "first key not equal to mine" as "absent"
    /// misses a key that is present. The successor returned here was
    /// `>= target` at the instant it was observed.
    fn seek_pair(&self, target: &[u8]) -> (Option<NonNull<u8>>, Option<NonNull<u8>>) {
        let mut cursor = self.head;
        let mut successor = None;
        for level in (0..MAX_HEIGHT).rev() {
            loop {
                // SAFETY (S1, S3, S7): `cursor` is the head or a node
                // reached at this level, so its height exceeds `level`,
                // and every node it reaches stays valid for the arena's
                // life.
                let next = unsafe { next_at(cursor.as_ptr(), level) };
                match next {
                    Some(node)
                        if compare_internal_keys(unsafe { node_key(node.as_ptr()) }, target)
                            .is_lt() =>
                    {
                        cursor = node;
                    }
                    other => {
                        if level == 0 {
                            successor = other;
                        }
                        break;
                    }
                }
            }
        }
        ((cursor != self.head).then_some(cursor), successor)
    }

    /// The first node whose key is greater than or equal to `target`.
    pub(crate) fn seek_ge(&self, target: &[u8]) -> Option<NodeRef<'_>> {
        self.seek_pair(target).1.map(NodeRef::new)
    }

    /// The first node whose key is strictly greater than `target`.
    pub(crate) fn seek_gt(&self, target: &[u8]) -> Option<NodeRef<'_>> {
        let mut node = self.seek_ge(target);
        while let Some(current) = node {
            if compare_internal_keys(current.key(), target).is_gt() {
                return Some(current);
            }
            node = current.next();
        }
        None
    }

    /// The last node whose key is less than or equal to `target`.
    pub(crate) fn seek_le(&self, target: &[u8]) -> Option<NodeRef<'_>> {
        let (below, at_or_after) = self.seek_pair(target);
        if let Some(node) = at_or_after {
            let node = NodeRef::new(node);
            if compare_internal_keys(node.key(), target).is_le() {
                return Some(node);
            }
        }
        below.map(NodeRef::new)
    }

    /// The last node whose key is strictly less than `target`.
    pub(crate) fn seek_lt(&self, target: &[u8]) -> Option<NodeRef<'_>> {
        self.seek_pair(target).0.map(NodeRef::new)
    }

    /// The first node in internal-key order.
    pub(crate) fn first(&self) -> Option<NodeRef<'_>> {
        // SAFETY (S1, S9): the head always has a level-0 slot.
        unsafe { next_at(self.head.as_ptr(), 0) }.map(NodeRef::new)
    }

    /// The last node in internal-key order.
    pub(crate) fn last(&self) -> Option<NodeRef<'_>> {
        let mut cursor = self.head;
        for level in (0..MAX_HEIGHT).rev() {
            // SAFETY (S1, S3, S7): as in `insert`'s descent.
            while let Some(next) = unsafe { next_at(cursor.as_ptr(), level) } {
                cursor = next;
            }
        }
        (cursor != self.head).then(|| NodeRef::new(cursor))
    }
}

impl Drop for ArenaSkipList {
    fn drop(&mut self) {
        // SAFETY (S9): the head came from `std::alloc::alloc` with this
        // exact layout in `ArenaSkipList::new` and is freed once. Nodes
        // are not freed here: they belong to the arena, which returns its
        // chunks to the pool when its last reference dies (A5).
        unsafe {
            let layout = std::alloc::Layout::from_size_align_unchecked(HEAD_SIZE, NODE_ALIGN);
            std::alloc::dealloc(self.head.as_ptr(), layout);
        }
    }
}

/// The 9-byte `!seq || value_type` trailer of an internal key.
fn internal_trailer(seq: u64, value_type: u8) -> [u8; INTERNAL_KEY_SUFFIX_LEN] {
    let mut trailer = [0u8; INTERNAL_KEY_SUFFIX_LEN];
    trailer[..8].copy_from_slice(&(!seq).to_be_bytes());
    trailer[8] = value_type;
    trailer
}

/// Write a node's header and construct its tower as `height` null links.
///
/// # Safety
///
/// `node` must address at least `NODE_HEADER + PTR_SIZE * height` writable
/// bytes, aligned to [`NODE_ALIGN`], and no other thread may observe it
/// yet (S1).
unsafe fn init_node_header(node: *mut u8, key_len: usize, value_len: usize, height: usize) {
    debug_assert!((1..=MAX_HEIGHT).contains(&height));
    unsafe {
        node.cast::<u32>().write(key_len as u32);
        node.add(4).cast::<u32>().write(value_len as u32);
        node.add(8).write(height as u8);
        std::ptr::write_bytes(node.add(9), 0, NODE_HEADER - 9);
        for level in 0..height {
            node.add(NODE_HEADER + level * PTR_SIZE)
                .cast::<Link>()
                .write(Link::new(std::ptr::null_mut()));
        }
    }
}

impl<'a> NodeRef<'a> {
    /// This node's tower height. Test-only: production code never needs
    /// a node's height, only its key and value.
    #[cfg(test)]
    pub(crate) fn height(&self) -> usize {
        // SAFETY (S1): the node was fully written before it became
        // reachable.
        unsafe { header(self.ptr.as_ptr()).2 }
    }
}

#[cfg(test)]
impl ArenaSkipList {
    /// Check that every level above 0 is a subsequence of level 0: for
    /// every level `l >= 1`, the chain reachable from the head at level
    /// `l` must equal the level-0 chain filtered to nodes whose height
    /// is greater than `l`.
    ///
    /// Seeks stay correct even when one node is bypassed at a single
    /// level (they fall back to a lower level), so this is the cheapest
    /// observation that still catches a tower that skips a node it
    /// should not, or fails to skip one it should.
    pub(crate) fn assert_towers_consistent(&self) {
        let level0: Vec<NonNull<u8>> = {
            let mut nodes = Vec::new();
            let mut cursor = self.head;
            // SAFETY (S1, S3, S7): standard level-0 walk.
            while let Some(next) = unsafe { next_at(cursor.as_ptr(), 0) } {
                nodes.push(next);
                cursor = next;
            }
            nodes
        };
        for level in 1..MAX_HEIGHT {
            let want: Vec<NonNull<u8>> = level0
                .iter()
                .copied()
                // SAFETY (S1): every node in `level0` is initialised.
                .filter(|&n| unsafe { header(n.as_ptr()).2 } > level)
                .collect();
            let mut got = Vec::new();
            let mut cursor = self.head;
            // SAFETY (S1, S3, S7): standard tower walk at `level`.
            while let Some(next) = unsafe { next_at(cursor.as_ptr(), level) } {
                got.push(next);
                cursor = next;
            }
            assert_eq!(
                got, want,
                "level {level} diverges from the level-0 chain filtered to height > {level}"
            );
        }
    }
}

/// `(key, height)` for every node in the list, in level-0 order.
/// Test-only: compares the shape a hinted run produced against a plain
/// one.
#[cfg(test)]
pub(crate) fn tower_shape(list: &ArenaSkipList) -> Vec<(Vec<u8>, usize)> {
    let mut out = Vec::new();
    let mut node = list.first();
    while let Some(current) = node {
        out.push((current.key().to_vec(), current.height()));
        node = current.next();
    }
    out
}

/// An internal key ordered by [`compare_internal_keys`], for a
/// `BTreeMap` oracle in the proptest below. Test-only: production code
/// never needs internal keys to implement `Ord`, only the comparator.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OrdKey(pub(crate) Vec<u8>);

#[cfg(test)]
impl PartialOrd for OrdKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
impl Ord for OrdKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        compare_internal_keys(&self.0, &other.0)
    }
}
