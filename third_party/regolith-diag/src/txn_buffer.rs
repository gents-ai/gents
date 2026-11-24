//! The buffer a transaction accumulates its own keys in.
//!
//! Buffering takes `&self` so one transaction can be shared across
//! threads, which rules out a plain `BTreeMap`. A general concurrent map
//! is the obvious substitute and the wrong one here: it allocates a table
//! the moment it exists, and a transaction holds three of these buffers
//! and lives for microseconds. Measured on the transaction benchmark,
//! paying for those tables cost 61% to 67% of uncontended commit
//! throughput.
//!
//! This is sized for what a transaction actually holds. It allocates
//! nothing until the first entry, then one node per entry, and answers a
//! lookup by walking the list. A transaction touching a handful of keys
//! beats a hash table on both counts. Past
//! [`crate::Options::transaction_keys_inline`] entries the walk would stop
//! being cheap, so the buffer indexes itself with a `HopscotchMap`.
//!
//! The index holds pointers into the list, not copies of what the list
//! holds. Copying would put a second key and a second value on the heap
//! for every entry, which on a large transaction is the memory the caller
//! was trying not to spend.
//!
//! # Why the reclamation problem does not arise
//!
//! Nodes are never unlinked while the buffer is shared. An overwrite
//! prepends, and the walk returns the first match, so the newest value for a
//! key is the one found. The list is freed only in `Drop`, `drain` and
//! `truncate`, which take `&mut self` and therefore cannot run beside a
//! reader. That is what makes a plain `AtomicPtr` list sound here without
//! epochs or hazard pointers, and it is a property of how a transaction is
//! used rather than of this type: it holds because a transaction is resolved
//! (or rolled back to a savepoint) exclusively.
//!
//! # Reading one key's writes
//!
//! A node also points at the next older node with its key, so the writes
//! of one key since it was last replaced are read in the time they take,
//! however many unrelated entries the buffer holds. The index names the
//! newest node of a key and the back-links lead from there. A node
//! pushed before the index was built carries no back-link and finds its
//! older node by walking the list, which is short by construction: the
//! index is built once the list outgrows
//! [`crate::Options::transaction_keys_inline`]. Until it is built a lookup
//! walks the list.

#![allow(unsafe_code)]

use core::hash::Hash;
use std::ops::ControlFlow;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

use kovan_map::HopscotchMap as ConcurrentMap;

struct Node<K, V> {
    key: K,
    value: V,
    /// The next older node of the whole list.
    next: *mut Node<K, V>,
    /// Position in the list: one more than `next`'s, so of two nodes the
    /// larger is the newer. Fixed before the node is published.
    order: usize,
    /// The next older node with this key, or null when there is none.
    /// [`unlinked`] until the index threads the node into its key's chain,
    /// when the older node is found by walking `next` instead. Only ever
    /// names a listed node.
    prev: AtomicPtr<Node<K, V>>,
}

/// The back-link of a node no key chain has threaded yet. It points at
/// nothing and is never dereferenced.
fn unlinked<K, V>() -> *mut Node<K, V> {
    core::ptr::dangling_mut()
}

/// A pointer to a listed node, for the index to hold instead of a copy.
struct NodeRef<K: 'static, V: 'static>(*mut Node<K, V>);

impl<K, V> Clone for NodeRef<K, V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V> Copy for NodeRef<K, V> {}

// SAFETY: the pointer reaches a node this buffer allocated, which is
// immutable once published (its back-link aside, an atomic) and freed only
// under `&mut self`. Sharing a `NodeRef` shares read-only bytes for as long
// as the buffer lives, and the index never outlives the buffer.
unsafe impl<K: Send + Sync, V: Send + Sync> Send for NodeRef<K, V> {}
// SAFETY: see the `Send` impl above.
unsafe impl<K: Send + Sync, V: Send + Sync> Sync for NodeRef<K, V> {}

/// A transaction's buffered entries, keyed and lock-free.
pub(crate) struct TxnBuffer<K: 'static, V: 'static> {
    /// Newest first. Null while the buffer is empty, which is the state
    /// that has to cost nothing.
    head: AtomicPtr<Node<K, V>>,
    len: AtomicUsize,
    /// Entries `truncate` and `drain` have ever taken off the list, so that
    /// [`Self::generation`] never falls. Only those two write it, and both
    /// hold `&mut self`.
    removed: u64,
    /// An index over the list, built once it grows past `spill_at`.
    /// Values are pointers into the list rather than copies of it. Boxed: a
    /// map is a few hundred bytes of cache-padded fields, and a buffer that
    /// never grows that far (most do not) would otherwise carry them, in
    /// every transaction that is built, moved and dropped.
    spill: OnceLock<Box<ConcurrentMap<K, NodeRef<K, V>>>>,
    /// Entries buffered before the index is built. `0` never indexes.
    spill_at: usize,
    /// Set once `spill` is published. An insert reads it after its push and
    /// the thread that builds the index sets it before it walks the list;
    /// both are sequentially consistent, so one of them always sees the
    /// other and no node is left out of the index.
    spill_published: AtomicBool,
    /// Set once the spill map has absorbed the entries that predate it,
    /// after which a lookup can trust the map alone.
    spill_seeded: AtomicBool,
}

// SAFETY: the pointers reach nodes this buffer allocated and never hands
// out, and nothing is freed until `Drop` takes `&mut self`. Sharing the
// buffer therefore shares immutable nodes plus atomics (the back-links
// included), so it is `Send` and `Sync` exactly when the data it holds is.
unsafe impl<K: Send + Sync + 'static, V: Send + Sync + 'static> Send for TxnBuffer<K, V> {}
// SAFETY: see the `Send` impl above.
unsafe impl<K: Send + Sync + 'static, V: Send + Sync + 'static> Sync for TxnBuffer<K, V> {}

impl<K: 'static, V: 'static> Default for TxnBuffer<K, V> {
    fn default() -> Self {
        Self {
            head: AtomicPtr::new(core::ptr::null_mut()),
            len: AtomicUsize::new(0),
            removed: 0,
            spill: OnceLock::new(),
            spill_at: 0,
            spill_published: AtomicBool::new(false),
            spill_seeded: AtomicBool::new(false),
        }
    }
}

impl<K, V> TxnBuffer<K, V>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// A buffer that indexes itself past `spill_at` entries. `0` never
    /// indexes and always walks.
    pub(crate) fn new(spill_at: usize) -> Self {
        Self {
            head: AtomicPtr::new(core::ptr::null_mut()),
            len: AtomicUsize::new(0),
            removed: 0,
            spill: OnceLock::new(),
            spill_at,
            spill_published: AtomicBool::new(false),
            spill_seeded: AtomicBool::new(false),
        }
    }

    /// Entries buffered, counting an overwritten key once per write.
    pub(crate) fn len(&self) -> usize {
        self.len.load(Ordering::Acquire)
    }

    /// A number that changes whenever the buffer does and never falls: it
    /// grows by one for each entry inserted and by at least one for each
    /// `truncate` or `drain` that takes entries off. So two reads that agree
    /// saw the same entries, which a length cannot say once a truncation can
    /// bring the length back to an earlier value.
    pub(crate) fn generation(&self) -> u64 {
        // A removal counts twice: once for the length it took off and once for
        // the entries gone, so the sum rises by what was removed.
        self.len() as u64 + 2 * self.removed
    }

    /// Buffer `key` at `value`, replacing whatever this buffer held for
    /// it. O(1): the node goes on the front and the walk finds it first.
    pub(crate) fn insert(&self, key: K, value: V) {
        let node = self.push(key, value);
        // Read after the push, so a map published while this insert ran
        // either walks over the node when it is seeded or is seen here and
        // links it.
        if self.spill_published.load(Ordering::SeqCst)
            && let Some(spill) = self.spill.get()
        {
            self.link(spill, node);
        }
        let len = self.len.fetch_add(1, Ordering::AcqRel) + 1;
        if self.spill_at > 0 && len > self.spill_at && self.spill.get().is_none() {
            self.start_spilling();
        }
    }

    /// Put a node for `key` at the front of the list.
    fn push(&self, key: K, value: V) -> *mut Node<K, V> {
        let node = Box::into_raw(Box::new(Node {
            key,
            value,
            next: core::ptr::null_mut(),
            order: 0,
            prev: AtomicPtr::new(unlinked()),
        }));
        let mut head = self.head.load(Ordering::Acquire);
        loop {
            // SAFETY: `node` is unpublished until the CAS, and `head` is null or listed.
            unsafe {
                (*node).next = head;
                (*node).order = head.as_ref().map_or(0, |head| head.order + 1);
            }
            // Sequentially consistent, to pair with `spill_published`.
            match self
                .head
                .compare_exchange_weak(head, node, Ordering::SeqCst, Ordering::Acquire)
            {
                Ok(_) => return node,
                Err(current) => head = current,
            }
        }
    }

    /// Record the listed `node` in `spill` as its key's newest node when it
    /// is, and thread it into the key's back-links.
    ///
    /// Inserts of one key reach here in any order, so a node newer than the
    /// one the map holds takes its place, and an older one is spliced into
    /// the chain beneath the first node newer than it. Runs under the key's
    /// guard in the map, which is what keeps two such splices of one key
    /// apart.
    ///
    /// Until seeding is done no back-link is written. The map lags the list
    /// then, so a link written now could skip a node the seed has not
    /// reached, and a lookup walks the list instead.
    fn link(&self, spill: &ConcurrentMap<K, NodeRef<K, V>>, node: *mut Node<K, V>) {
        // SAFETY: `node` is listed, so it lives until `Drop` takes `&mut self`.
        let this = unsafe { &*node };
        spill.compute(this.key.clone(), |present| {
            let threaded = self.spill_seeded.load(Ordering::Acquire);
            let Some(&NodeRef(newest)) = present else {
                if threaded {
                    this.prev.store(core::ptr::null_mut(), Ordering::Release);
                }
                return Some(NodeRef(node));
            };
            if newest == node {
                return Some(NodeRef(node));
            }
            // SAFETY: the map holds only listed nodes.
            let newest_ref = unsafe { &*newest };
            if newest_ref.order < this.order {
                if threaded {
                    this.prev.store(newest, Ordering::Release);
                }
                return Some(NodeRef(node));
            }
            if threaded {
                Self::splice(newest_ref, this, node);
            }
            Some(NodeRef(newest))
        });
    }

    /// Thread `node`, older than the chain's newest node `above`, into the
    /// chain beneath the first node newer than it.
    fn splice(mut above: &Node<K, V>, this: &Node<K, V>, node: *mut Node<K, V>) {
        loop {
            let below = Self::older(above);
            if below == node {
                return;
            }
            // SAFETY: `older` answers null or a listed node.
            match unsafe { below.as_ref() } {
                Some(below_ref) if below_ref.order > this.order => above = below_ref,
                _ => {
                    this.prev.store(below, Ordering::Release);
                    above.prev.store(node, Ordering::Release);
                    return;
                }
            }
        }
    }

    /// The next older node with `node`'s key, or null: its back-link when
    /// the index threaded one, else the first node below it in the list
    /// with the same key.
    // vertexia: a node pushed before the index was seeded finds its older
    // node by walking, at most as far as the list was long then (never
    // indexing, every step walks); thread them while seeding if a profile
    // shows it.
    fn older(node: &Node<K, V>) -> *mut Node<K, V> {
        let prev = node.prev.load(Ordering::Acquire);
        if prev != unlinked() {
            return prev;
        }
        let mut cursor = node.next;
        // SAFETY: `next` is null or a listed node, which lives until `Drop` takes `&mut self`.
        while let Some(below) = unsafe { cursor.as_ref() } {
            if below.key == node.key {
                return cursor;
            }
            cursor = below.next;
        }
        core::ptr::null_mut()
    }

    /// The newest node listed for `key`: the index's once seeding is done,
    /// and until then the first match walking the list, since the map can
    /// lag it.
    fn newest<Q>(&self, key: &Q) -> Option<&Node<K, V>>
    where
        K: core::borrow::Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        if self.spill_seeded.load(Ordering::Acquire)
            && let Some(spill) = self.spill.get()
        {
            // SAFETY: the index only holds listed nodes, which live until `Drop`.
            return spill.get(key).map(|NodeRef(node)| unsafe { &*node });
        }
        self.nodes().find(|node| node.key.borrow() == key)
    }

    /// Value buffered for `key`, or `None`.
    pub(crate) fn get<Q>(&self, key: &Q) -> Option<V>
    where
        K: core::borrow::Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.newest(key).map(|node| node.value.clone())
    }

    /// Whether anything is buffered for `key`, cloning nothing.
    pub(crate) fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: core::borrow::Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.newest(key).is_some()
    }

    /// Build the index now if it does not exist yet, regardless of
    /// [`Self::spill_at`].
    ///
    /// For a caller that knows a burst of lookups is about to walk this
    /// buffer repeatedly and would rather pay one index build than one
    /// list walk per lookup, even below the entry count that would
    /// otherwise trigger it. Cheap to call again: past the first time, it
    /// is a single `OnceLock::get` check.
    pub(crate) fn ensure_indexed(&self) {
        if self.spill.get().is_none() {
            self.start_spilling();
        }
    }

    /// Whether the index is built, so a test can tell a lookup that used
    /// it apart from one that walked the list.
    #[cfg(test)]
    pub(crate) fn is_indexed(&self) -> bool {
        self.spill.get().is_some()
    }

    /// Publish the spill map, then link in everything already listed.
    ///
    /// Order matters: publishing first means every insert that follows
    /// links itself, and seeding second means nothing already listed is
    /// missed. A node both paths reach is linked twice, which changes
    /// nothing the second time.
    fn start_spilling(&self) {
        if let Some(spill) = self.publish() {
            self.seed(spill);
        }
    }

    /// Create the index and open it to inserts. `None` when another thread
    /// already did, and is seeding it.
    fn publish(&self) -> Option<&ConcurrentMap<K, NodeRef<K, V>>> {
        self.spill.set(Box::new(ConcurrentMap::new())).ok()?;
        self.spill_published.store(true, Ordering::SeqCst);
        self.spill.get().map(|spill| &**spill)
    }

    /// Link every node already listed into `spill`, newest first, then let
    /// lookups trust it.
    // vertexia: `compute` rewrites the entry even when `link` keeps it, so a
    // node older than its key's entry costs an allocation here where
    // `insert_if_absent` cost none; skip such nodes with a lookup if seeding
    // a long list shows in a profile.
    fn seed(&self, spill: &ConcurrentMap<K, NodeRef<K, V>>) {
        let mut cursor = self.head.load(Ordering::SeqCst);
        while !cursor.is_null() {
            // SAFETY: listed nodes live until `Drop` takes `&mut self`.
            let next = unsafe { (*cursor).next };
            self.link(spill, cursor);
            cursor = next;
        }
        self.spill_seeded.store(true, Ordering::Release);
    }

    /// The value for `key`, inserting `value` and returning it if the
    /// buffer has none. Linearizable: concurrent callers for one key all
    /// receive the same value.
    pub(crate) fn get_or_insert(&self, key: K, value: V) -> V {
        if let Some(existing) = self.get(&key) {
            return existing;
        }
        // A racing insert for the same key can land between the lookup
        // and the CAS, and both callers have to come away with the same
        // value. Re-reading settles it: the list is newest-first and both
        // nodes are on it, so every caller walks to the same one.
        self.insert(key.clone(), value);
        self.get(&key)
            .expect("the entry just inserted is on the list")
    }

    /// The listed nodes, newest first.
    fn nodes(&self) -> impl Iterator<Item = &Node<K, V>> {
        // SAFETY: the head and each `next` after it are null or listed nodes,
        // which live until `Drop` takes `&mut self`.
        std::iter::successors(
            unsafe { self.head.load(Ordering::Acquire).as_ref() },
            |node| unsafe { node.next.as_ref() },
        )
    }

    /// Visit the values buffered for `key`, newest first, until `visit`
    /// breaks.
    ///
    /// Follows the key's back-links, so it costs the writes walked and not
    /// the size of the buffer, and hands out references, so it copies
    /// nothing. A write racing the walk in another thread may or may not be
    /// seen; one that finished before the walk began always is.
    pub(crate) fn walk_chain<'a, Q>(
        &'a self,
        key: &Q,
        mut visit: impl FnMut(&'a V) -> ControlFlow<()>,
    ) where
        K: core::borrow::Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let mut node = self.newest(key);
        while let Some(current) = node {
            if visit(&current.value).is_break() {
                return;
            }
            // SAFETY: `older` answers null or a listed node, which outlives this borrow.
            node = unsafe { Self::older(current).as_ref() };
        }
    }

    /// The entries of the keys `include` accepts, newest first, each key's
    /// through the first value `done` accepts, without consuming the buffer.
    ///
    /// Where `done` accepts every value this is the newest write of each
    /// key. Where it accepts only a write that replaces the key outright, it
    /// is the key's writes since it was last replaced: all a reader of an
    /// append-only log or a savepoint has to keep.
    ///
    /// Bounded by what this transaction has written, never by what the
    /// database holds, which is why materializing it is affordable where
    /// materializing the database side would not be. `include` runs before
    /// anything is copied, so a narrow scan does not clone unrelated
    /// buffered values, especially during a large history replay.
    pub(crate) fn chains_matching(
        &self,
        include: impl Fn(&K) -> bool,
        done: impl Fn(&V) -> bool,
    ) -> Vec<(K, V)> {
        let mut out = Vec::new();
        let mut finished = std::collections::HashSet::new();
        for node in self.nodes() {
            if include(&node.key) && !finished.contains(&node.key) {
                if done(&node.value) {
                    finished.insert(node.key.clone());
                }
                out.push((node.key.clone(), node.value.clone()));
            }
        }
        out
    }

    /// Drop the newest entries until `len` remain, so [`Self::len`] reads
    /// `len` afterwards. A `len` at or above the current one drops nothing.
    ///
    /// The cost is the entries dropped, never the entries kept: they are the
    /// front of the list, and the index, once built, is pointed back at the
    /// next older node of each key they carried. This is how a savepoint
    /// rolls back, with the buffer's entry count as the mark.
    ///
    /// Takes `&mut self` for the reason `drain` does: freeing a node is sound
    /// only when no reader can be walking the list, and a mutable borrow says
    /// so.
    pub(crate) fn truncate(&mut self, len: usize) {
        self.truncate_into(len, drop);
    }

    /// [`Self::truncate`], handing each entry it takes off to `taken`, newest
    /// first, instead of dropping it: what a rollback inside a transaction
    /// callback keeps, so the callback failing can put it back.
    pub(crate) fn truncate_into(&mut self, len: usize, mut taken: impl FnMut((K, V))) {
        let before = *self.len.get_mut();
        let mut remaining = before;
        let mut head = *self.head.get_mut();
        while remaining > len && !head.is_null() {
            // SAFETY: `&mut self` makes this thread the only one that can reach
            // the list, and `head` is a node `push` allocated by `Box::into_raw`.
            let node = unsafe { Box::from_raw(head) };
            if let Some(spill) = self.spill.get() {
                // The head is the newest node of its key, so the index holds
                // it. Its older node, if any, is the key's newest now.
                let older = Self::older(&node);
                spill.compute(node.key.clone(), |_| {
                    (!older.is_null()).then_some(NodeRef(older))
                });
            }
            head = node.next;
            remaining -= 1;
            let Node { key, value, .. } = *node;
            taken((key, value));
        }
        *self.head.get_mut() = head;
        *self.len.get_mut() = remaining;
        self.removed += (before - remaining) as u64;
    }

    /// Rule out repeated keys without allocating; large unindexed buffers stay conservative.
    pub(crate) fn may_have_repeated_keys(&mut self) -> bool {
        if let Some(index) = self.spill.get() {
            return index.len() < *self.len.get_mut();
        }
        if *self.len.get_mut() > 32 {
            return true;
        }
        self.nodes().any(|node| !Self::older(node).is_null())
    }

    /// Every buffered entry, newest write of each key only.
    ///
    /// Takes `&mut self` because it consumes the buffer: draining is what
    /// a resolved transaction does, and a resolved transaction is
    /// exclusive.
    /// Newest write first, duplicates included. Deduplicating here would
    /// mean cloning every key to track what was seen; the caller collects
    /// into a keyed structure anyway and gets it for free by keeping the
    /// first value it sees for a key.
    pub(crate) fn drain(&mut self) -> Vec<(K, V)> {
        let mut drained = Vec::with_capacity(*self.len.get_mut());
        self.drain_with(|key, value| drained.push((key, value)));
        drained
    }

    /// [`Self::drain`] handing each entry to `f`, newest first, instead of
    /// collecting them: for a caller that only walks them once, so nothing
    /// is allocated to hold them.
    ///
    /// Each node leaves the list before `f` sees it, so a panic in `f` leaves
    /// the rest listed, and the buffer's `Drop` frees them.
    pub(crate) fn drain_with(&mut self, mut f: impl FnMut(K, V)) {
        // The index points into the list, so it goes first.
        self.spill = OnceLock::new();
        *self.spill_published.get_mut() = false;
        *self.spill_seeded.get_mut() = false;
        let taken = *self.len.get_mut();
        *self.len.get_mut() = 0;
        self.removed += taken as u64;
        while !self.head.get_mut().is_null() {
            // SAFETY: `&mut self` makes this thread the only one that can reach
            // the list, and the head is a node `push` allocated by `Box::into_raw`.
            let node = unsafe { Box::from_raw(*self.head.get_mut()) };
            *self.head.get_mut() = node.next;
            let Node { key, value, .. } = *node;
            f(key, value);
        }
    }
}

impl<K: 'static, V: 'static> Drop for TxnBuffer<K, V> {
    fn drop(&mut self) {
        // `&mut self`: a plain read, not a locked exchange.
        let mut cursor = core::mem::replace(self.head.get_mut(), core::ptr::null_mut());
        while !cursor.is_null() {
            // SAFETY: `&mut self` means no reader can be walking the
            // list, and each node was allocated by `Box::into_raw`.
            let node = unsafe { Box::from_raw(cursor) };
            cursor = node.next;
        }
    }
}

#[cfg(test)]
mod scan_tests;

#[cfg(test)]
mod truncate_tests;

#[cfg(test)]
mod repeated_key_tests {
    use super::TxnBuffer;

    #[test]
    fn unindexed_detection_is_bounded_and_conservative() {
        let mut buffer = TxnBuffer::new(0);
        for key in 0..32 {
            buffer.insert(key, key);
        }
        assert!(!buffer.may_have_repeated_keys());
        buffer.insert(32, 32);
        assert!(buffer.may_have_repeated_keys());
    }

    #[test]
    fn unique_keys_and_rollback_use_the_current_index() {
        for inline in [4, 32] {
            let mut buffer = TxnBuffer::new(inline);
            for key in 0..64 {
                buffer.insert(key, key);
            }
            assert!(!buffer.may_have_repeated_keys());
            buffer.insert(17, 100);
            assert!(buffer.may_have_repeated_keys());
            buffer.truncate(64);
            assert!(!buffer.may_have_repeated_keys());
            buffer.drain();
            assert!(!buffer.may_have_repeated_keys());
        }
    }
}
