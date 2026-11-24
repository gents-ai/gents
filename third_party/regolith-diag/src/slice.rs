//! A refcounted view into bytes the database already owns.

#![allow(unsafe_code)]

use std::sync::Arc;

use crate::engine::arena::Arena;
use crate::engine::block::Block;
/// The arena's reference count, which is a `loom::sync::Arc` in a
/// `--cfg loom` build so the model checker can see the handoff that
/// keeps an arena alive under a live slice (invariant A5).
use crate::sync::internal::Arc as ArenaArc;

/// A borrowed, refcounted view of a value.
///
/// Returned by [`crate::Db::get_slice`] and friends. The bytes are not
/// copied: a `DbSlice` holds a reference count on whatever already owns
/// them, and keeps that owner alive for as long as the slice exists.
///
/// Holding a `DbSlice` pins its owner. A slice over an SSTable block
/// keeps that block resident even after the block cache evicts it, and a
/// slice taken from a memtable keeps the memtable's value bytes alive
/// across a flush. Both are cheap to hold briefly and expensive to hold
/// forever; call [`DbSlice::to_vec`] if the value must outlive the read.
///
/// # Values, not keys
///
/// There is no `key_slice`, and there is no way to add one that would
/// mean anything. A data block stores keys prefix-compressed against
/// their restart point, so the key a cursor reports is reassembled into
/// a buffer the iterator owns rather than addressed in place: the bytes
/// the caller wants are not contiguous anywhere in the block. A
/// `key_slice` would therefore copy, which [`crate::Iter::key`] already
/// does at the point of use and more cheaply. Values are stored whole,
/// which is why they can be handed out by reference.
///
/// # Comparing against `Option`
///
/// `DbSlice` compares against `[u8]`, `&[u8]`, `[u8; N]` and `Vec<u8>`
/// in both directions, but `Option<DbSlice> == Option<Vec<u8>>` does
/// **not** compile: `core`'s `impl<T: PartialEq> PartialEq for Option<T>`
/// is homogeneous and `Option` is a foreign type, so no `PartialEq` impl
/// on `DbSlice` can bridge it. That is why [`crate::Db::get_slice`] is
/// additive and [`crate::Db::get`] keeps returning `Option<Vec<u8>>`.
pub struct DbSlice {
    owner: SliceOwner,
    ptr: std::ptr::NonNull<u8>,
    len: usize,
}

/// What keeps a [`DbSlice`]'s bytes alive.
///
/// Every variant satisfies the same contract: the bytes at
/// `DbSlice::ptr[..DbSlice::len]` live inside this owner's allocation,
/// and the owner never mutates or frees them while it is held.
#[derive(Clone)]
enum SliceOwner {
    /// A decompressed SSTable data block. Immutable after
    /// `Block::decode`, so the bytes are stable for the `Arc`'s life.
    /// The payload is a liveness anchor, never read through: the slice
    /// addresses the block's bytes directly.
    Block(#[allow(dead_code)] Arc<Block>),
    /// A memtable arena chunk. Immutable once the skip-list node that
    /// owns it is published, and the chunk does not return to the
    /// recycling pool until the last reference - including this one -
    /// is dropped.
    Arena(#[allow(dead_code)] ArenaArc<Arena>),
    /// Bytes the engine already had on the heap: a merge-operator
    /// result, or a TTL value awaiting its header strip.
    Heap(Arc<Vec<u8>>),
    /// The empty slice. No owner needed.
    Empty,
}

// SAFETY (D3): every owner variant is `Send + Sync` (`Block`, `Arena`,
// `Vec<u8>` and `Arc` all are), and the bytes `ptr` addresses are immutable for as
// long as the owner is held (D2), so sharing a `DbSlice` across threads
// only ever shares read-only bytes plus an atomic refcount.
unsafe impl Send for DbSlice {}
// SAFETY: see the `Send` impl above.
unsafe impl Sync for DbSlice {}

impl DbSlice {
    /// The empty slice.
    pub(crate) fn empty() -> Self {
        Self {
            owner: SliceOwner::Empty,
            ptr: std::ptr::NonNull::dangling(),
            len: 0,
        }
    }

    /// View `offset..offset + len` of a decoded SSTable block.
    ///
    /// Returns `None` when the range falls outside the block's entry
    /// region, so a corrupt handle can never produce a dangling view.
    pub(crate) fn from_block(block: Arc<Block>, offset: usize, len: usize) -> Option<Self> {
        let ptr = {
            let bytes = block.entry_bytes(offset, len)?;
            // SAFETY (D1, D4): `entry_bytes` validated the range against
            // the block's own buffer, so `bytes` points into memory the
            // `Arc<Block>` owns and `bytes.len() == len`.
            std::ptr::NonNull::new(bytes.as_ptr().cast_mut())?
        };
        Some(Self {
            owner: SliceOwner::Block(block),
            ptr,
            len,
        })
    }

    /// View `len` bytes at `ptr` inside a memtable arena.
    ///
    /// The caller must have taken `ptr` from a published skip-list node
    /// in `arena` (invariants S1 and A5 in
    /// [`crate::engine::skiplist`]): the bytes are then immutable and
    /// stay valid for as long as this `Arc<Arena>` is held.
    pub(crate) fn from_arena(
        arena: ArenaArc<Arena>,
        ptr: std::ptr::NonNull<u8>,
        len: usize,
    ) -> Self {
        if len == 0 {
            return Self::empty();
        }
        Self {
            owner: SliceOwner::Arena(arena),
            ptr,
            len,
        }
    }

    /// Take ownership of a heap buffer without copying its payload.
    /// `Arc::new` boxes the 24-byte `Vec` header; the bytes stay put.
    pub(crate) fn from_vec(bytes: Vec<u8>) -> Self {
        if bytes.is_empty() {
            return Self::empty();
        }
        Self::from_arc_vec(Arc::new(bytes))
    }

    /// View the whole of an already-shared heap buffer.
    pub(crate) fn from_arc_vec(bytes: Arc<Vec<u8>>) -> Self {
        let len = bytes.len();
        let Some(ptr) = std::ptr::NonNull::new(bytes.as_ptr().cast_mut()) else {
            return Self::empty();
        };
        Self {
            owner: SliceOwner::Heap(bytes),
            ptr,
            len,
        }
    }

    /// The bytes.
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY (D1, D2): `ptr[..len]` lies inside the owner's
        // allocation, which this `DbSlice` holds a reference count on,
        // and those bytes are never written again while it is held. The
        // empty case uses a dangling-but-aligned pointer with `len == 0`,
        // which `from_raw_parts` explicitly permits.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    /// Copy into an owned vector. This is what [`crate::Db::get`] does
    /// with the slice it gets back.
    pub fn to_vec(&self) -> Vec<u8> {
        self.as_slice().to_vec()
    }

    /// Consume the slice, reusing the owner's buffer when this slice is
    /// its sole owner and spans all of it. Otherwise this is
    /// [`DbSlice::to_vec`].
    pub fn into_vec(self) -> Vec<u8> {
        let (ptr, len) = (self.ptr, self.len);
        if let SliceOwner::Heap(arc) = self.owner {
            let spans_whole_buffer = arc.len() == len && std::ptr::eq(arc.as_ptr(), ptr.as_ptr());
            if spans_whole_buffer {
                return match Arc::try_unwrap(arc) {
                    Ok(bytes) => bytes,
                    Err(shared) => shared.as_slice().to_vec(),
                };
            }
            // SAFETY: same as `as_slice`; `arc` is still alive here.
            return unsafe { std::slice::from_raw_parts(ptr.as_ptr(), len) }.to_vec();
        }
        // SAFETY: same as `as_slice`; `self.owner` was moved out only in
        // the `Heap` arm above, so the owner is still alive.
        unsafe { std::slice::from_raw_parts(ptr.as_ptr(), len) }.to_vec()
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the slice is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// A narrower view over the same bytes, sharing the same owner and
    /// costing one reference-count increment.
    ///
    /// Returns `None` if the range is out of bounds or inverted; there
    /// is no panicking variant.
    pub fn try_subslice(&self, range: core::ops::Range<usize>) -> Option<DbSlice> {
        if range.start > range.end || range.end > self.len {
            return None;
        }
        let len = range.end - range.start;
        if len == 0 {
            return Some(Self::empty());
        }
        // SAFETY (D4): the range was validated against `self.len` above,
        // so `start` is within the owner's allocation.
        let ptr = unsafe { std::ptr::NonNull::new_unchecked(self.ptr.as_ptr().add(range.start)) };
        Some(Self {
            owner: self.owner.clone(),
            ptr,
            len,
        })
    }
}

impl Clone for DbSlice {
    fn clone(&self) -> Self {
        Self {
            owner: self.owner.clone(),
            ptr: self.ptr,
            len: self.len,
        }
    }
}

impl core::ops::Deref for DbSlice {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl AsRef<[u8]> for DbSlice {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

/// Prints the length plus the payload, escaped. Assertion failures are
/// unreadable without the bytes; payloads longer than 64 bytes are
/// truncated to the first 32. Structured logs must record `len` and an
/// id instead of ever formatting a `DbSlice`.
impl core::fmt::Debug for DbSlice {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let bytes = self.as_slice();
        let (shown, elided) = if bytes.len() <= 64 {
            (bytes, false)
        } else {
            (&bytes[..32], true)
        };
        write!(f, "DbSlice {{ len: {}, bytes: \"", self.len)?;
        for &byte in shown {
            for ch in std::ascii::escape_default(byte) {
                write!(f, "{}", ch as char)?;
            }
        }
        if elided {
            write!(f, "...")?;
        }
        write!(f, "\" }}")
    }
}

impl PartialEq for DbSlice {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for DbSlice {}

impl PartialEq<[u8]> for DbSlice {
    fn eq(&self, other: &[u8]) -> bool {
        self.as_slice() == other
    }
}

impl PartialEq<&[u8]> for DbSlice {
    fn eq(&self, other: &&[u8]) -> bool {
        self.as_slice() == *other
    }
}

impl PartialEq<Vec<u8>> for DbSlice {
    fn eq(&self, other: &Vec<u8>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<const N: usize> PartialEq<[u8; N]> for DbSlice {
    fn eq(&self, other: &[u8; N]) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl PartialEq<DbSlice> for [u8] {
    fn eq(&self, other: &DbSlice) -> bool {
        self == other.as_slice()
    }
}

impl PartialEq<DbSlice> for &[u8] {
    fn eq(&self, other: &DbSlice) -> bool {
        *self == other.as_slice()
    }
}

impl PartialEq<DbSlice> for Vec<u8> {
    fn eq(&self, other: &DbSlice) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<const N: usize> PartialEq<DbSlice> for [u8; N] {
    fn eq(&self, other: &DbSlice) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl PartialOrd for DbSlice {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Plain byte order, so a collection of slices sorts the way the keys
/// they came from do.
impl Ord for DbSlice {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

/// Hashes the bytes, and only the bytes. An owner may carry interior
/// mutability - a memtable arena guards its chunk list with a mutex -
/// but none of it is reachable through the payload, so a `DbSlice` is a
/// sound hash-map key even though `clippy::mutable_key_type` cannot see
/// that.
impl core::hash::Hash for DbSlice {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state);
    }
}

/// Adopt an owned buffer as a slice, without copying it.
///
/// Lets a caller funnel bytes the database produced and bytes it
/// assembled itself (a decrypted value, a reassembled chunk) through one
/// value type instead of an enum that means the same thing.
impl From<Vec<u8>> for DbSlice {
    fn from(bytes: Vec<u8>) -> Self {
        DbSlice::from_vec(bytes)
    }
}

impl From<DbSlice> for Vec<u8> {
    fn from(slice: DbSlice) -> Vec<u8> {
        slice.into_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::block::{BlockBuilder, RESTART_INTERVAL};
    use std::collections::HashSet;

    fn heap(bytes: &[u8]) -> DbSlice {
        DbSlice::from_vec(bytes.to_vec())
    }

    fn block_slice(pairs: &[(&[u8], &[u8])], want: &[u8]) -> DbSlice {
        let mut builder = BlockBuilder::new(RESTART_INTERVAL);
        for (k, v) in pairs {
            builder.add(k, v);
        }
        let block = Arc::new(Block::decode(builder.finish()).expect("valid block"));
        let mut key_buf = Vec::new();
        let found = block
            .scan_from(pairs[0].0, &mut key_buf, |key, off, len| {
                if key == want {
                    core::ops::ControlFlow::Break((off, len))
                } else {
                    core::ops::ControlFlow::Continue(())
                }
            })
            .expect("key present");
        DbSlice::from_block(block, found.0, found.1).expect("in-range value")
    }

    #[test]
    fn empty_slice_is_usable() {
        let s = DbSlice::empty();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert_eq!(s.as_slice(), b"");
        assert_eq!(s.to_vec(), Vec::<u8>::new());
    }

    #[test]
    fn from_vec_round_trips() {
        let s = heap(b"hello");
        assert_eq!(s.as_slice(), b"hello");
        assert_eq!(s.len(), 5);
        assert_eq!(&*s, b"hello");
        assert_eq!(s.as_ref(), b"hello");
    }

    #[test]
    fn borrows_a_block_without_copying() {
        let s = block_slice(&[(b"a", b"alpha"), (b"b", b"beta")], b"b");
        assert_eq!(s.as_slice(), b"beta");
    }

    #[test]
    fn block_slice_outlives_every_other_reference() {
        let s = block_slice(&[(b"a", b"alpha")], b"a");
        // The `Arc<Block>` created inside the helper is gone; the slice
        // still owns the block.
        assert_eq!(s.as_slice(), b"alpha");
        let cloned = s.clone();
        drop(s);
        assert_eq!(cloned.as_slice(), b"alpha");
    }

    #[test]
    fn try_subslice_narrows_and_shares() {
        let s = heap(b"0123456789");
        let mid = s.try_subslice(2..5).expect("in range");
        assert_eq!(mid.as_slice(), b"234");
        assert_eq!(s.try_subslice(0..0).expect("empty").len(), 0);
        assert_eq!(s.try_subslice(10..10).expect("empty tail").len(), 0);
    }

    #[test]
    fn try_subslice_rejects_out_of_range() {
        let s = heap(b"abc");
        assert!(s.try_subslice(0..4).is_none(), "end past the slice");
        let (start, end) = (2usize, 1usize);
        assert!(s.try_subslice(start..end).is_none(), "inverted range");
        assert!(s.try_subslice(4..4).is_none(), "empty range past the end");
        assert!(s.try_subslice(usize::MAX..usize::MAX).is_none());
    }

    #[test]
    fn into_vec_reuses_a_solely_owned_heap_buffer() {
        let original = b"reuse me".to_vec();
        let addr = original.as_ptr();
        let s = DbSlice::from_vec(original);
        let back = s.into_vec();
        assert_eq!(back, b"reuse me");
        assert_eq!(back.as_ptr(), addr, "sole owner must not re-copy");
    }

    #[test]
    fn into_vec_copies_a_shared_or_narrowed_slice() {
        let s = heap(b"0123456789");
        let clone = s.clone();
        assert_eq!(s.into_vec(), b"0123456789");
        assert_eq!(
            clone.try_subslice(1..3).expect("in range").into_vec(),
            b"12"
        );
    }

    #[test]
    fn comparisons_work_in_both_directions() {
        let s = heap(b"abc");
        assert_eq!(s, *b"abc".as_slice());
        assert_eq!(s, b"abc".as_slice());
        assert_eq!(s, b"abc".to_vec());
        assert_eq!(s, *b"abc");
        assert!(*b"abc".as_slice() == s);
        assert!(b"abc".as_slice() == s.as_slice());
        assert!(b"abc".to_vec() == s);
        assert!(*b"abc" == s);
        assert_eq!(s, s.clone());
    }

    #[test]
    // The bytes a `DbSlice` hashes are immutable; see the `Hash` impl.
    #[allow(clippy::mutable_key_type)]
    fn ord_and_hash_follow_byte_order() {
        let a = heap(b"aaa");
        let b = heap(b"aab");
        assert!(a < b);
        let mut sorted = [b.clone(), a.clone()];
        sorted.sort();
        assert_eq!(sorted[0], a);

        let mut set = HashSet::new();
        set.insert(a.clone());
        assert!(set.contains(&heap(b"aaa")));
        assert!(!set.contains(&b));
    }

    #[test]
    fn from_dbslice_for_vec() {
        let v: Vec<u8> = heap(b"xyz").into();
        assert_eq!(v, b"xyz");
    }

    #[test]
    fn debug_shows_bytes_and_truncates_long_payloads() {
        let short = format!("{:?}", heap(b"a\nb"));
        assert_eq!(short, r#"DbSlice { len: 3, bytes: "a\nb" }"#);
        let long = format!("{:?}", heap(&[b'z'; 100]));
        assert!(long.starts_with("DbSlice { len: 100, bytes: \"zzz"));
        assert!(long.ends_with("...\" }"));
        assert_eq!(long.matches('z').count(), 32);
    }

    #[test]
    fn slices_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DbSlice>();
        let s = heap(b"threaded");
        let handle = std::thread::spawn(move || s.to_vec());
        assert_eq!(handle.join().expect("thread"), b"threaded");
    }
}
