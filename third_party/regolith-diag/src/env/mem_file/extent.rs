//! The bytes of one extent of a [`super::MemFile`]: a chain of segments that
//! an append in place lengthens, so a growing file never copies itself.
//!
//! An extent starts as one segment. An append in place that runs out of room
//! links one more segment at the end of the chain, as large as everything
//! before it (or as the append needs), while it holds `WRITER`, and before it
//! publishes the length that reaches into it. A file of `n` bytes is then
//! held in about `log2(n)` segments, written once each and never moved; a
//! copy (an overwrite, a shrink, a lost race) builds a new extent of one
//! segment.
//!
//! The room above the published length is left uninitialized: a write fills
//! what it publishes, and an extension zeroes what it exposes, so a file
//! holds in memory the bytes it has, not the room it grew into.
//!
//! # Invariants
//!
//! - Every segment that holds a byte below the published length was linked
//!   before that length was published (the link is a Release store, the
//!   publication a Release CAS, a reader's load of the length Acquire), so a
//!   reader that walks the chain below a length it loaded finds every link.
//! - Only the thread holding `WRITER` links a segment, and only at the end
//!   of the chain: a link is written once and never changed while the
//!   extent can be reached.

use std::cell::UnsafeCell;
use std::io;
use std::mem::MaybeUninit;
use std::ptr;

use crate::portability::{AtomicPtr, AtomicU64, Ordering};

/// The smallest segment a file grows by, so a file of a few small appends
/// does not allocate on each.
pub(super) const MIN_CAPACITY: usize = 64;

/// A byte of a segment: written through a shared reference, and never read
/// before it is written.
type Cell = UnsafeCell<MaybeUninit<u8>>;

/// One buffer of an extent's bytes, and the next one once it is linked.
struct Segment {
    bytes: Box<[Cell]>,
    next: AtomicPtr<Segment>,
}

impl Segment {
    fn with_capacity(capacity: usize) -> io::Result<Self> {
        let mut bytes: Vec<Cell> = Vec::new();
        bytes.try_reserve_exact(capacity).map_err(|_| {
            io::Error::new(
                io::ErrorKind::OutOfMemory,
                format!("cannot hold a {capacity} byte file in memory"),
            )
        })?;
        // SAFETY: the capacity is reserved, and a `MaybeUninit` cell needs no
        // initialization.
        unsafe { bytes.set_len(capacity) };
        Ok(Self::from_cells(bytes.into_boxed_slice()))
    }

    fn from_cells(bytes: Box<[Cell]>) -> Self {
        Self {
            bytes,
            next: AtomicPtr::new(ptr::null_mut()),
        }
    }

    fn base(&self) -> *mut u8 {
        UnsafeCell::raw_get(self.bytes.as_ptr()).cast()
    }

    /// The segment linked after this one, if any.
    fn next(&self) -> Option<&Segment> {
        // SAFETY: a link points to a segment the chain owns, freed only when
        // the extent is dropped, which no `&self` outlives.
        unsafe { self.next.load(Ordering::Acquire).as_ref() }
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        // Free the rest of the chain in a loop rather than by recursion.
        let mut next = std::mem::replace(self.next.get_mut(), ptr::null_mut());
        while !next.is_null() {
            // SAFETY: each link was made from `Box::into_raw` and is owned by
            // the chain alone; taking it out of the link frees it once.
            let mut segment = unsafe { Box::from_raw(next) };
            next = std::mem::replace(segment.next.get_mut(), ptr::null_mut());
        }
    }
}

/// One chain of segments and its published length. See the module docs.
pub(super) struct Extent {
    first: Segment,
    pub(super) state: AtomicU64,
}

// SAFETY: bytes below the published length are never written while the
// extent can be reached (only read), and bytes at or above it, and the links
// that reach them, are written only by the one thread holding `WRITER`, which
// no reader reads: a reader copies only below a length it loaded with
// Acquire, which the writer's publishing CAS (Release) made visible after the
// bytes and the links.
unsafe impl Sync for Extent {}

impl Extent {
    /// An extent of `capacity` bytes of room and no published length.
    pub(super) fn with_capacity(capacity: usize) -> io::Result<Self> {
        Ok(Self {
            first: Segment::with_capacity(capacity)?,
            state: AtomicU64::new(0),
        })
    }

    /// An extent with no room and no length.
    pub(super) fn empty() -> Self {
        Self {
            first: Segment::from_cells(Box::new([])),
            state: AtomicU64::new(0),
        }
    }

    /// An extent holding `bytes`, all published.
    #[cfg(any(test, all(target_arch = "wasm32", target_os = "unknown")))]
    pub(super) fn holding(bytes: Vec<u8>) -> Self {
        let len = bytes.len() as u64;
        let bytes = bytes.into_boxed_slice();
        // SAFETY: `UnsafeCell` and `MaybeUninit` are both `repr(transparent)`
        // over `u8`, so the slice has the same layout, and the box keeps its
        // allocation.
        let cells = unsafe { Box::from_raw(Box::into_raw(bytes) as *mut [Cell]) };
        Self {
            first: Segment::from_cells(cells),
            state: AtomicU64::new(len),
        }
    }

    fn segments(&self) -> impl Iterator<Item = &Segment> {
        std::iter::successors(Some(&self.first), |segment| segment.next())
    }

    /// The room of every segment linked so far.
    pub(super) fn capacity(&self) -> usize {
        self.segments().map(|segment| segment.bytes.len()).sum()
    }

    /// How many segments hold the room.
    #[cfg(test)]
    pub(super) fn segment_count(&self) -> usize {
        self.segments().count()
    }

    /// Link segments until the room reaches `end` bytes. Each new segment is
    /// at least as large as the room before it, so the chain stays short.
    ///
    /// # Safety
    ///
    /// This thread holds `WRITER`, or the extent is not yet published.
    pub(super) unsafe fn reserve(&self, end: usize) -> io::Result<()> {
        let mut room = 0;
        let mut tail = &self.first;
        loop {
            room += tail.bytes.len();
            match tail.next() {
                Some(next) => tail = next,
                None => break,
            }
        }
        if end <= room {
            return Ok(());
        }
        let grow = (end - room).max(room).max(MIN_CAPACITY);
        let segment = Box::into_raw(Box::new(Segment::with_capacity(grow)?));
        // Release: a reader that loads a length reaching into the segment
        // through the publishing CAS sees the link and the segment's bytes.
        tail.next.store(segment, Ordering::Release);
        Ok(())
    }

    /// Run `f` over the pieces of `[at, at + n)`, one per segment it spans,
    /// as each piece's start, the offset into the range it covers, and its
    /// length.
    ///
    /// # Safety
    ///
    /// Every segment holding a byte of the range is linked.
    unsafe fn pieces<F>(&self, mut at: usize, n: usize, mut f: F)
    where
        F: FnMut(*mut u8, usize, usize),
    {
        let mut done = 0;
        let mut segment = &self.first;
        while done < n {
            let len = segment.bytes.len();
            if at < len {
                let take = (n - done).min(len - at);
                // SAFETY: `at + take <= len`, within this segment.
                f(unsafe { segment.base().add(at) }, done, take);
                done += take;
                at = 0;
            } else {
                at -= len;
            }
            if done < n {
                match segment.next() {
                    Some(next) => segment = next,
                    // Unreachable under the caller's contract; stopping short
                    // is the one answer that touches no memory it must not.
                    None => return,
                }
            }
        }
    }

    /// Copy `out.len()` published bytes starting at `at`.
    ///
    /// # Safety
    ///
    /// `at + out.len()` is at most a length this thread loaded from `state`
    /// with Acquire ordering.
    pub(super) unsafe fn copy_out(&self, at: usize, out: &mut [u8]) {
        let dst = out.as_mut_ptr();
        // SAFETY: the range is published, so every segment of it is linked
        // and nothing writes it (see the `Sync` impl); `out` is this thread's.
        unsafe {
            self.pieces(at, out.len(), |src, offset, n| {
                ptr::copy_nonoverlapping(src, dst.add(offset), n)
            })
        }
    }

    /// Copy `src` into the extent at `at`.
    ///
    /// # Safety
    ///
    /// The range is at or above every length published on this extent and
    /// this thread holds `WRITER`, or the extent is not yet published; and
    /// the room reaches its end ([`Self::reserve`]).
    pub(super) unsafe fn copy_in(&self, at: usize, src: &[u8]) {
        let from = src.as_ptr();
        // SAFETY: the caller owns the range exclusively, and it is linked.
        unsafe {
            self.pieces(at, src.len(), |dst, offset, n| {
                ptr::copy_nonoverlapping(from.add(offset), dst, n)
            })
        }
    }

    /// Zero `n` bytes at `at`.
    ///
    /// # Safety
    ///
    /// As [`Self::copy_in`].
    pub(super) unsafe fn zero(&self, at: usize, n: usize) {
        // SAFETY: as `copy_in`.
        unsafe { self.pieces(at, n, |dst, _, n| ptr::write_bytes(dst, 0, n)) }
    }

    /// Copy the first `n` bytes of `src` to the start of this extent.
    ///
    /// # Safety
    ///
    /// `n` is at most a length loaded from `src.state` with Acquire, this
    /// extent is not yet published, and its room reaches `n`.
    pub(super) unsafe fn copy_prefix(&self, src: &Extent, n: usize) {
        // SAFETY: the source range is published and never written; the
        // destination is this thread's alone. Two allocations, no overlap.
        unsafe {
            src.pieces(0, n, |from, offset, n| {
                self.copy_in(offset, std::slice::from_raw_parts(from, n))
            })
        }
    }
}
