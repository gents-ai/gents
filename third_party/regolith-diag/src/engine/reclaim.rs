//! Prompt reclamation for the rare kovan retirements that own large
//! resources: a replaced read view (memtables, a version and its table
//! descriptors) and a replaced range-tombstone index.
//!
//! kovan frees a retired node only once the batch holding it has been
//! handed to every reservation slot that might still reach it, and it hands
//! a batch out only when the batch has more nodes than there are such
//! slots. A data structure that retires a node on every update fills a
//! batch in no time. The read view retires one node per publication, a
//! handful a second at most, so a flushed batch of one node is refused as
//! soon as two other threads have read since it was born, and it then sits
//! in the publishing thread's local batch until that thread retires enough
//! to make up a full one. A replaced view in that batch keeps a flushed
//! memtable and every unlinked table it named alive for as long as that
//! takes.
//!
//! [`submit`] closes that: it fills the batch to kovan's own batch size with
//! empty nodes and flushes it, so the batch is placed as long as fewer than
//! [`PAD`] reservations can still reach it, which is the case kovan's own
//! batch size is built for. Past that many reading threads the batch stays
//! with this thread and the next [`submit`] adds another full batch, so it
//! is placed after at most `reading threads / PAD` more publications.
//!
//! The cost is [`PAD`] allocations of a few dozen bytes each per call, made
//! only on a publication path that has just written a manifest record or
//! sealed a memtable, never on a read.

#![allow(unsafe_code)]

use kovan::RetiredNode;

/// Empty nodes added per call: one short of kovan's batch size, so the
/// node the caller just retired completes a full batch.
pub(crate) const PAD: usize = 63;

/// An empty retired node. `#[repr(C)]` with the kovan header first, which
/// is what `kovan::retire` requires of every pointer it is handed.
#[repr(C)]
struct Pad {
    _node: RetiredNode,
}

/// Hand this thread's retired nodes to the reclaimer now, padded so the
/// batch can be placed. Wait-free: a fixed number of allocations, retires
/// and one flush, none of which waits for another thread.
///
/// A flush made while this thread holds a kovan guard still submits, but
/// cannot free anything that guard may reach; callers that can hold a guard
/// across a publication defer the call until it is released (see
/// `read_view::quiesce`).
pub(crate) fn submit() {
    for _ in 0..PAD {
        let pad = Box::into_raw(Box::new(Pad {
            _node: RetiredNode::new(),
        }));
        // SAFETY: `pad` is a fresh heap allocation from `Box::into_raw`,
        // retired exactly once, and `Pad` is `#[repr(C)]` with its
        // `RetiredNode` at offset 0, which is the layout `retire` requires.
        unsafe { kovan::retire(pad) };
    }
    kovan::flush();
}
