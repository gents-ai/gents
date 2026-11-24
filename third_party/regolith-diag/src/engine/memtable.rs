//! In-memory sorted table: an arena-backed skip list plus the range
//! tombstones recorded alongside it.
//!
//! Keys and values live inline in the memtable's [`Arena`], so a write
//! copies each byte exactly once and a read hands back a [`DbSlice`] that
//! borrows those bytes instead of cloning them. The arena is what makes
//! [`MemTable::approximate_size`] mean something: it counts the whole
//! node (header, tower, internal key, value, rounded to alignment)
//! rather than only the key and value payload the old counter measured.

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::arena::{Arena, ArenaProfile, ChunkPool};
use super::internal_key::{
    INTERNAL_KEY_SUFFIX_LEN, VALUE_TYPE_DELETION, VALUE_TYPE_MERGE, VALUE_TYPE_VALUE,
    compare_internal_keys, decode_internal_key,
};
use super::lookup_key::LookupKey;
use super::range_tombstone::RangeTombstone;
use super::skiplist::{ArenaSkipList, InsertHint, NodeRef};
use crate::DbSlice;
use crate::sync::internal::Arc;
use tombstones::TombstoneLog;
/// Test seam: what this thread's appends of range tombstones have cost
/// since the last call, as (indexes built, slots looked at for a duplicate).
#[cfg(all(test, not(loom)))]
pub(crate) use tombstones::work_since_last_call as range_tombstone_work;

mod key_walk;
mod tombstones;

/// Everything a memtable needs to build its arena: the engine-wide chunk
/// pool, the per-memtable byte budget, and the chunk sizing policy.
///
/// Held by the engine and handed to every [`MemTable::new`], so all of an
/// engine's memtables share one bounded pool and recycle each other's
/// chunks.
#[derive(Clone)]
pub(crate) struct MemTableConfig {
    pool: Arc<ChunkPool>,
    budget: usize,
    profile: ArenaProfile,
}

impl MemTableConfig {
    /// Build the config, and with it the engine's one chunk pool.
    pub(crate) fn new(
        profile: ArenaProfile,
        write_buffer_size: usize,
        max_write_buffer_number: usize,
    ) -> Self {
        Self {
            pool: Arc::new(ChunkPool::new(
                profile,
                write_buffer_size,
                max_write_buffer_number,
            )),
            budget: write_buffer_size,
            profile,
        }
    }

    /// Bytes currently parked in the recycling pool, and the bound it
    /// will not exceed.
    pub(crate) fn pool_bytes(&self) -> (usize, usize) {
        (self.pool.parked_bytes(), self.pool.budget())
    }
}

impl Default for MemTableConfig {
    fn default() -> Self {
        Self::new(ArenaProfile::SERVER, 64 * 1024 * 1024, 2)
    }
}

/// Concurrent in-memory sorted table backed by an arena skip list.
///
/// Supports many concurrent readers and many concurrent writers, with no
/// lock. Every write is idempotent: the same entry (key, sequence and kind)
/// written twice, by one thread or by two at once, is held once. That is
/// what lets whichever thread reaches a decided commit apply it
/// (`engine::commit`, plan 4.7).
///
/// Range tombstones live beside the skip list in an append-only log
/// (`tombstones::TombstoneLog`) rather than interleaved with point
/// entries. Range deletes are orders of magnitude rarer than point writes,
/// and keeping them apart lets a point lookup skip them with one acquire
/// load while the memtable holds none. Once it holds some, a lookup reads
/// the ready ones without any lock, and an append blocks neither a reader
/// nor another append (T1 to T4 in that module).
pub(crate) struct MemTable {
    list: ArenaSkipList,
    /// The range tombstones. Their published length is also the gate a
    /// lookup checks first: zero means none was ever recorded. Why a reader
    /// can never miss a tombstone visible at its snapshot is argued on the
    /// log's module docs; a sealed memtable takes no further range deletes,
    /// so the argument covers frozen memtables as well.
    range_tombstones: TombstoneLog,
    /// The write-ahead log that backs this memtable's contents, recorded
    /// when the memtable is sealed and the log rotated away from it.
    ///
    /// A flush unlinks this log once an SSTable holding the same records
    /// is in the published version, so the memtable has to carry it
    /// rather than the flush being handed one: the caller that seals a
    /// memtable and the flush that persists it are not necessarily
    /// looking at the same memtable, and a flush that unlinked the
    /// caller's log would delete the only durable copy of a memtable
    /// nobody has flushed yet. A crash then loses every write in it.
    sealed_wal: OnceLock<PathBuf>,
    /// The highest sequence number this memtable can contain, recorded
    /// when it is sealed.
    ///
    /// Exact, and free. Sealing happens in the commit pipeline's ordered
    /// step with every earlier commit applied and published, so every
    /// write decided before the seal is already in this memtable and no
    /// later write can enter it: the last sequence decided at that point
    /// is precisely this memtable's ceiling. A flush stamps it into the manifest, which is
    /// what makes `last_seq` mean "durable in an SSTable through here"
    /// rather than "the newest sequence the engine had handed out when
    /// the flush happened to finish".
    sealed_seq: OnceLock<u64>,
}

impl MemTable {
    /// A new, empty memtable over a fresh arena.
    ///
    /// The arena reserves nothing until the first write, so an untouched
    /// memtable costs one small head-sentinel allocation and nothing else.
    pub(crate) fn new(config: &MemTableConfig) -> std::io::Result<Self> {
        let arena = Arc::new(Arena::new(
            Arc::clone(&config.pool),
            config.budget,
            config.profile,
        ));
        let list = ArenaSkipList::new(arena).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::OutOfMemory,
                "could not allocate the memtable skip-list head",
            )
        })?;
        Ok(Self {
            list,
            range_tombstones: TombstoneLog::new(),
            sealed_wal: OnceLock::new(),
            sealed_seq: OnceLock::new(),
        })
    }

    /// The backing skip list.
    ///
    /// Only the loom models reach for it: they drive seek shapes the
    /// memtable's own read path deliberately does not use, to prove the
    /// model explores the interleaving those shapes get wrong.
    #[cfg(loom)]
    pub(crate) fn list(&self) -> &ArenaSkipList {
        &self.list
    }

    /// Record the log that backs this memtable, at the moment the
    /// memtable is sealed and the log rotated away from it.
    ///
    /// Set once. A memtable is sealed exactly once, and the log it was
    /// written through never changes after that.
    pub(crate) fn seal_wal(&self, path: PathBuf) {
        let _ = self.sealed_wal.set(path);
    }

    /// Record this memtable's sequence ceiling, at the moment it is
    /// sealed. Set once, like the log.
    pub(crate) fn seal_seq(&self, seq: u64) {
        let _ = self.sealed_seq.set(seq);
    }

    /// The highest sequence this memtable can hold, if it has been
    /// sealed.
    pub(crate) fn sealed_seq(&self) -> Option<u64> {
        self.sealed_seq.get().copied()
    }

    /// The log this memtable's records were written through, if it has
    /// been sealed. `None` for the active memtable, whose log is still
    /// the one writers are appending to.
    pub(crate) fn sealed_wal(&self) -> Option<&Path> {
        self.sealed_wal.get().map(PathBuf::as_path)
    }

    /// Insert a key-value pair with the given sequence number.
    pub(crate) fn put(&self, key: &[u8], value: &[u8], seq: u64) {
        self.list.insert(key, seq, VALUE_TYPE_VALUE, value);
    }

    /// Insert a deletion tombstone for the given key.
    pub(crate) fn delete(&self, key: &[u8], seq: u64) {
        self.list.insert(key, seq, VALUE_TYPE_DELETION, &[]);
    }

    /// Insert a merge operand for the given key. The operand will
    /// be combined with any older base value (or other operands) at
    /// read time via the configured [`crate::MergeOperator`].
    pub(crate) fn merge(&self, key: &[u8], operand: &[u8], seq: u64) {
        self.list.insert(key, seq, VALUE_TYPE_MERGE, operand);
    }

    /// A hint for a run of writes in key order; see
    /// [`ArenaSkipList::insert_hint`].
    pub(crate) fn insert_hint(&self) -> InsertHint<'_> {
        self.list.insert_hint()
    }

    /// [`MemTable::put`] starting from `hint`, which the call updates to
    /// point at the new entry.
    pub(crate) fn put_hinted<'a>(
        &'a self,
        hint: &mut InsertHint<'a>,
        key: &[u8],
        value: &[u8],
        seq: u64,
    ) {
        self.list
            .insert_with_hint(hint, key, seq, VALUE_TYPE_VALUE, value);
    }

    /// [`MemTable::delete`] starting from `hint`.
    pub(crate) fn delete_hinted<'a>(&'a self, hint: &mut InsertHint<'a>, key: &[u8], seq: u64) {
        self.list
            .insert_with_hint(hint, key, seq, VALUE_TYPE_DELETION, &[]);
    }

    /// [`MemTable::merge`] starting from `hint`.
    pub(crate) fn merge_hinted<'a>(
        &'a self,
        hint: &mut InsertHint<'a>,
        key: &[u8],
        operand: &[u8],
        seq: u64,
    ) {
        self.list
            .insert_with_hint(hint, key, seq, VALUE_TYPE_MERGE, operand);
    }

    /// Record a range tombstone - every user key in `[start, end)`
    /// is considered deleted as of `seq`. Idempotent and callable by any
    /// number of threads at once, as the point writes are.
    pub(crate) fn delete_range(&self, start: &[u8], end: &[u8], seq: u64) {
        self.range_tombstones
            .push(RangeTombstone::new(start.to_vec(), end.to_vec(), seq));
    }

    /// Record every tombstone of `batch`: what [`Self::delete_range`] does
    /// for each, at a cost linear in the batch, where one call a tombstone
    /// is quadratic for tombstones that share a sequence
    /// (`TombstoneLog::extend`). Recovery uses it, for the range tombstones a
    /// log holds and for the ones an open copies out of the tables it moved.
    pub(crate) fn delete_ranges(&self, batch: Vec<RangeTombstone>) {
        self.range_tombstones.extend(batch);
    }

    /// Whether this memtable holds any range tombstone. One acquire load.
    pub(crate) fn has_range_tombstones(&self) -> bool {
        self.range_tombstones.len() != 0
    }

    /// Return a snapshot of every range tombstone currently held, sorted by
    /// start, then end, then newest first, with exact duplicates dropped.
    /// Used by flush (to persist them into the produced SSTable)
    /// and by the iterator / scan paths to query cover info.
    pub(crate) fn clone_range_tombstones(&self) -> Vec<RangeTombstone> {
        self.range_tombstones.to_sorted_vec()
    }

    /// A range tombstone overlapping `[lo, hi)` with a sequence above
    /// `floor` and at or below `ceil`, as the first key of the range it
    /// deletes and its sequence. The first such tombstone in the order
    /// `clone_range_tombstones` lists them. `u64::MAX` as `ceil` bounds
    /// nothing.
    pub(crate) fn newer_range_tombstone(
        &self,
        lo: &[u8],
        hi: &[u8],
        floor: u64,
        ceil: u64,
    ) -> Option<(Vec<u8>, u64)> {
        if !self.has_range_tombstones() {
            return None;
        }
        self.range_tombstones
            .first_newer_overlap(lo, hi, floor, ceil)
            .map(|t| (t.start.as_slice().max(lo).to_vec(), t.seq))
    }

    /// Largest seq of any range tombstone covering `user_key` that is
    /// visible at `snapshot_seq`. Returns `0` if no such tombstone
    /// exists - `0` is a safe sentinel because real seqs start at 1.
    ///
    /// Takes no lock: the gate is one acquire load, and past it the log
    /// is read as a published prefix.
    pub(crate) fn covering_range_tombstone_seq(&self, user_key: &[u8], snapshot_seq: u64) -> u64 {
        if !self.has_range_tombstones() {
            return 0;
        }
        self.range_tombstones.covering_seq(user_key, snapshot_seq)
    }

    /// The newest entry for `lk`'s user key at or below its snapshot, with the
    /// sequence and value type already decoded. Merge operands and point
    /// tombstones count as entries; the caller decides what to make of the
    /// type.
    fn newest_visible(&self, lk: &LookupKey) -> Option<(NodeRef<'_>, u64, u8)> {
        self.scan_key(lk, |node, seq, value_type| {
            ControlFlow::Break((node, seq, value_type))
        })
    }

    /// Look up the newest point entry for `key` visible at
    /// `snapshot_seq`. Returns `Some((seq, value_opt))` - `value_opt`
    /// is `Some(..)` for a live value and `None` for a tombstone -
    /// or `None` if the memtable has no entry for `key` at or below
    /// `snapshot_seq`.
    ///
    /// The returned slice borrows the arena; no value bytes are copied.
    ///
    /// This method intentionally ignores range tombstones; the caller
    /// is responsible for merging range-tombstone coverage across
    /// sources and comparing seqs.
    pub(crate) fn get(&self, lk: &LookupKey) -> Option<(u64, Option<DbSlice>)> {
        let (node, seq, value_type) = self.newest_visible(lk)?;
        let value = (value_type != VALUE_TYPE_DELETION).then(|| self.value_slice(&node));
        Some((seq, value))
    }

    /// [`MemTable::get`] without the value: the sequence and value type of
    /// the newest entry of any kind (value, tombstone or merge operand) for
    /// `lk`'s key at or below its snapshot. No arena `Arc` is cloned and no
    /// byte is copied, which is what commit validation wants: it asks
    /// whether and how the key was written again, never what it holds.
    pub(crate) fn latest_version(&self, lk: &LookupKey) -> Option<(u64, u8)> {
        self.newest_visible(lk)
            .map(|(_, seq, value_type)| (seq, value_type))
    }

    /// A zero-copy view of one node's value, keeping the arena alive for
    /// as long as the slice does (A5).
    fn value_slice(&self, node: &NodeRef<'_>) -> DbSlice {
        match node.value_span() {
            (Some(ptr), len) => DbSlice::from_arena(Arc::clone(self.list.arena()), ptr, len),
            _ => DbSlice::empty(),
        }
    }

    /// A zero-copy view of one node's internal key, on the same terms
    /// as [`MemTable::value_slice`].
    fn key_slice(&self, node: &NodeRef<'_>) -> DbSlice {
        match node.key_span() {
            (Some(ptr), len) => DbSlice::from_arena(Arc::clone(self.list.arena()), ptr, len),
            _ => DbSlice::empty(),
        }
    }

    /// Visit every raw entry in internal-key order, preserving every
    /// version and tombstone, without copying anything.
    ///
    /// The callback sees `(internal_key, value_bytes)` borrowed straight
    /// from the arena and may fail, which stops the walk and propagates.
    /// This is what lets a flush write an SSTable while holding one
    /// entry plus the block builder, never a second copy of the whole
    /// memtable.
    pub(crate) fn try_for_each_entry<F>(&self, mut f: F) -> std::io::Result<()>
    where
        F: FnMut(&[u8], &[u8]) -> std::io::Result<()>,
    {
        let mut cursor = self.list.first();
        while let Some(current) = cursor {
            f(current.key(), current.value())?;
            cursor = current.next();
        }
        Ok(())
    }

    /// Iterate **all** raw entries in internal-key order, preserving
    /// every version and tombstone, into an owned vector.
    ///
    /// The engine streams with [`MemTable::try_for_each_entry`] instead;
    /// this materializing form is kept as the reference the tests
    /// compare against.
    #[cfg(any(test, loom))]
    pub(crate) fn iter_internal(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut out = Vec::new();
        let _ = self.try_for_each_entry(|key, value| {
            out.push((key.to_vec(), value.to_vec()));
            Ok(())
        });
        out
    }

    /// Walk every raw entry whose user key falls in `[start, end)` and
    /// return the count and approximate total size (sum of internal-key
    /// length + value length). Every version and every tombstone is
    /// counted - this is a raw-entry stat, not a distinct-user-key stat.
    ///
    /// Used by [`crate::Db::get_approximate_memtable_stats`] to give
    /// callers a cheap-ish estimate of how big a range is inside the
    /// active memtable without doing a full visible scan.
    pub(crate) fn approximate_stats_for_range(&self, start: &[u8], end: &[u8]) -> (u64, u64) {
        if start >= end {
            return (0, 0);
        }
        // Walk from the smallest possible internal key for `start`
        // (seq=MAX, value_type=0) to the smallest for `end`. Every
        // entry in between has user key in `[start, end)`.
        let lo = LookupKey::from_prefixed(start, u64::MAX);
        let hi = LookupKey::from_prefixed(end, u64::MAX);
        let mut count: u64 = 0;
        let mut size: u64 = 0;
        let mut node = self.list.seek_ge(lo.internal());
        while let Some(current) = node {
            if compare_internal_keys(current.key(), hi.internal()).is_ge() {
                break;
            }
            count += 1;
            size += (current.key().len() + current.value_span().1) as u64;
            node = current.next();
        }
        (count, size)
    }

    /// Return the first `(internal_key, value)` pair whose key is in the
    /// half-open range `[lower, ..)`.
    ///
    /// Copies both halves. The streaming iterator uses
    /// [`MemTable::first_slice_from`] instead, which borrows; this is
    /// retained as the reference implementation that variant is checked
    /// against.
    #[cfg(test)]
    pub(crate) fn first_entry_from(
        &self,
        lower: std::ops::Bound<&[u8]>,
    ) -> Option<(Vec<u8>, Vec<u8>)> {
        let node = match lower {
            std::ops::Bound::Included(bound) => self.list.seek_ge(bound),
            std::ops::Bound::Excluded(bound) => self.list.seek_gt(bound),
            std::ops::Bound::Unbounded => self.list.first(),
        };
        node.map(|n| (n.key().to_vec(), n.value().to_vec()))
    }

    /// [`MemTable::first_entry_from`] handing back arena-backed views
    /// instead of copies.
    ///
    /// This is what the streaming iterator steps with: both halves are
    /// reference counts on the arena the bytes already live in, so a
    /// forward step copies nothing.
    pub(crate) fn first_slice_from(
        &self,
        lower: std::ops::Bound<&[u8]>,
    ) -> Option<(DbSlice, DbSlice)> {
        let node = match lower {
            std::ops::Bound::Included(bound) => self.list.seek_ge(bound),
            std::ops::Bound::Excluded(bound) => self.list.seek_gt(bound),
            std::ops::Bound::Unbounded => self.list.first(),
        };
        node.map(|n| (self.key_slice(&n), self.value_slice(&n)))
    }

    /// [`MemTable::last_entry_before`] handing back arena-backed views
    /// instead of copies. See [`MemTable::first_slice_from`].
    pub(crate) fn last_slice_before(
        &self,
        upper: std::ops::Bound<&[u8]>,
    ) -> Option<(DbSlice, DbSlice)> {
        let node = match upper {
            std::ops::Bound::Included(bound) => self.list.seek_le(bound),
            std::ops::Bound::Excluded(bound) => self.list.seek_lt(bound),
            std::ops::Bound::Unbounded => self.list.last(),
        };
        node.map(|n| (self.key_slice(&n), self.value_slice(&n)))
    }

    /// Return the last `(internal_key, value)` pair whose key is in the
    /// half-open range `(.., upper]`. The copying companion of
    /// [`MemTable::first_entry_from`], and the reference implementation
    /// [`MemTable::last_slice_before`] is checked against.
    ///
    /// The skip list has no back pointers, so a reverse step is an
    /// `O(log N)` re-seek.
    #[cfg(test)]
    pub(crate) fn last_entry_before(
        &self,
        upper: std::ops::Bound<&[u8]>,
    ) -> Option<(Vec<u8>, Vec<u8>)> {
        let node = match upper {
            std::ops::Bound::Included(bound) => self.list.seek_le(bound),
            std::ops::Bound::Excluded(bound) => self.list.seek_lt(bound),
            std::ops::Bound::Unbounded => self.list.last(),
        };
        node.map(|n| (n.key().to_vec(), n.value().to_vec()))
    }

    /// Bytes this memtable holds, counted as the arena bytes handed out
    /// (node header, tower, internal key and value, rounded to
    /// alignment) plus the heap the range tombstones own.
    ///
    /// This is what `write_buffer_size` bounds. It differs from the
    /// memtable's true resident cost only by the unused tail of the
    /// newest chunk; [`MemTable::reserved_size`] is that exact figure.
    pub(crate) fn approximate_size(&self) -> usize {
        self.list.arena().used_bytes() + self.range_tombstones.bytes()
    }

    /// Most arena bytes one `(key, value)` entry can add to a memtable.
    ///
    /// `key` is the column-family-prefixed user key, as
    /// [`MemTable::put`] takes it; the internal key adds the sequence
    /// and value-type suffix on top.
    pub(crate) fn max_entry_size(key_len: usize, value_len: usize) -> usize {
        super::skiplist::max_node_size(key_len + INTERNAL_KEY_SUFFIX_LEN, value_len)
    }

    /// Bytes this memtable actually took from the global allocator: the
    /// sum of its arena chunk sizes, plus the range-tombstone heap.
    pub(crate) fn reserved_size(&self) -> usize {
        self.list.arena().reserved_bytes() + self.range_tombstones.bytes()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::internal_key::encode_internal_key;

    fn memtable() -> MemTable {
        MemTable::new(&MemTableConfig::default()).expect("memtable")
    }

    fn small_memtable(budget: usize) -> MemTable {
        MemTable::new(&MemTableConfig::new(ArenaProfile::EMBEDDED, budget, 2)).expect("memtable")
    }

    fn probe(key: &[u8], snapshot_seq: u64) -> LookupKey {
        LookupKey::from_prefixed(key, snapshot_seq)
    }

    /// Materialize `MemTable::get` so the assertions below can compare
    /// against plain byte vectors.
    fn get_owned(mt: &MemTable, key: &[u8], snapshot_seq: u64) -> Option<(u64, Option<Vec<u8>>)> {
        mt.get(&probe(key, snapshot_seq))
            .map(|(seq, value)| (seq, value.map(|v| v.to_vec())))
    }

    #[test]
    fn test_put_get() {
        let mt = memtable();
        mt.put(b"key1", b"value1", 1);
        assert_eq!(
            get_owned(&mt, b"key1", 1),
            Some((1, Some(b"value1".to_vec())))
        );
        assert_eq!(get_owned(&mt, b"key1", 0), None);
    }

    #[test]
    fn test_delete() {
        let mt = memtable();
        mt.put(b"key1", b"value1", 1);
        mt.delete(b"key1", 2);

        assert_eq!(get_owned(&mt, b"key1", 2), Some((2, None)));
        assert_eq!(
            get_owned(&mt, b"key1", 1),
            Some((1, Some(b"value1".to_vec())))
        );
    }

    #[test]
    fn test_overwrite() {
        let mt = memtable();
        mt.put(b"key1", b"v1", 1);
        mt.put(b"key1", b"v2", 2);

        assert_eq!(get_owned(&mt, b"key1", 2), Some((2, Some(b"v2".to_vec()))));
        assert_eq!(get_owned(&mt, b"key1", 1), Some((1, Some(b"v1".to_vec()))));
    }

    #[test]
    fn test_iter_internal_preserves_versions() {
        let mt = memtable();
        mt.put(b"a", b"v1", 1);
        mt.put(b"a", b"v2", 2);
        mt.delete(b"a", 3);
        let items = mt.iter_internal();
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn try_for_each_entry_matches_iter_internal() {
        let mt = memtable();
        mt.put(b"a", b"1", 1);
        mt.merge(b"b", b"op", 2);
        mt.delete(b"c", 3);
        let mut streamed = Vec::new();
        mt.try_for_each_entry(|key, value| {
            streamed.push((key.to_vec(), value.to_vec()));
            Ok(())
        })
        .expect("walk");
        assert_eq!(streamed, mt.iter_internal());
    }

    #[test]
    fn try_for_each_entry_stops_on_the_first_error() {
        let mt = memtable();
        for i in 0..8u64 {
            mt.put(format!("k{i}").as_bytes(), b"v", i + 1);
        }
        let mut seen = 0usize;
        let err = mt
            .try_for_each_entry(|_, _| {
                seen += 1;
                if seen == 3 {
                    Err(std::io::Error::other("stop"))
                } else {
                    Ok(())
                }
            })
            .expect_err("callback failed");
        assert_eq!(seen, 3, "the walk stops at the failing entry");
        assert_eq!(err.to_string(), "stop");
    }

    #[test]
    fn test_range_tombstone_basic() {
        let mt = memtable();
        mt.delete_range(b"b", b"d", 5);
        assert_eq!(mt.covering_range_tombstone_seq(b"a", 10), 0);
        assert_eq!(mt.covering_range_tombstone_seq(b"b", 10), 5);
        assert_eq!(mt.covering_range_tombstone_seq(b"c", 10), 5);
        assert_eq!(mt.covering_range_tombstone_seq(b"d", 10), 0); // end exclusive
        // Invisible to snapshot older than the tombstone.
        assert_eq!(mt.covering_range_tombstone_seq(b"c", 4), 0);
    }

    #[test]
    fn test_range_tombstone_clone() {
        let mt = memtable();
        mt.delete_range(b"a", b"c", 1);
        mt.delete_range(b"e", b"g", 2);
        let rts = mt.clone_range_tombstones();
        assert_eq!(rts.len(), 2);
    }

    #[test]
    fn empty_memtable_reports_empty_and_zero_size() {
        let mt = memtable();
        assert!(mt.is_empty());
        assert_eq!(mt.approximate_size(), 0);
        assert_eq!(mt.reserved_size(), 0, "an untouched arena reserves nothing");
        assert_eq!(get_owned(&mt, b"k", u64::MAX), None);
        assert!(mt.iter_internal().is_empty());
    }

    #[test]
    fn approximate_size_grows_monotonically() {
        let mt = memtable();
        let s0 = mt.approximate_size();
        mt.put(b"k", b"v", 1);
        let s1 = mt.approximate_size();
        mt.put(b"k2", b"vv", 2);
        let s2 = mt.approximate_size();
        mt.delete(b"k3", 3);
        let s3 = mt.approximate_size();
        mt.delete_range(b"a", b"z", 4);
        let s4 = mt.approximate_size();
        assert!(s1 > s0 && s2 > s1 && s3 > s2 && s4 > s3);
    }

    #[test]
    fn approximate_size_counts_per_entry_overhead() {
        // The old counter measured `internal_key.len() + value.len()`
        // and ignored the node header, the tower and alignment padding
        // entirely. The arena counter charges all of it, so the
        // configured budget can no longer under-count reality.
        let mt = memtable();
        let payload = 200usize;
        let entries = 64usize;
        for i in 0..entries {
            mt.put(
                format!("key{i:08}").as_bytes(),
                &vec![7u8; payload],
                i as u64 + 1,
            );
        }
        let payload_only = entries * (11 + INTERNAL_KEY_SUFFIX_LEN + payload);
        assert!(
            mt.approximate_size() > payload_only,
            "arena accounting {} must exceed payload-only accounting {payload_only}",
            mt.approximate_size()
        );
        assert!(mt.reserved_size() >= mt.approximate_size());
    }

    #[test]
    fn reserved_size_tracks_the_configured_budget() {
        let budget = 64 * 1024;
        let mt = small_memtable(budget);
        let mut seq = 0u64;
        while mt.approximate_size() < budget {
            seq += 1;
            mt.put(format!("key{seq:08}").as_bytes(), &[3u8; 64], seq);
        }
        // This is where the engine rotates. The arena has reserved the
        // budget and at most one further chunk beyond it.
        assert!(mt.reserved_size() <= budget + ArenaProfile::EMBEDDED.max_chunk_size);
        assert!(
            mt.reserved_size() * 10 >= budget * 9,
            "chunks must be nearly full: reserved {} for budget {budget}",
            mt.reserved_size()
        );
    }

    #[test]
    fn merge_and_get_returns_operand_as_value() {
        // `get` returns the newest entry regardless of value_type; merge
        // operands appear as `Some(bytes)` just like values. The merge
        // resolution itself happens one level up.
        let mt = memtable();
        mt.merge(b"k", b"op1", 1);
        let (seq, val) = get_owned(&mt, b"k", 1).expect("should find operand");
        assert_eq!(seq, 1);
        assert_eq!(val, Some(b"op1".to_vec()));
    }

    #[test]
    fn approximate_stats_for_range_counts_every_version() {
        let mt = memtable();
        mt.put(b"a", b"1", 1);
        mt.put(b"a", b"2", 2);
        mt.put(b"b", b"x", 3);
        mt.put(b"z", b"outside", 4);

        let (count, _size) = mt.approximate_stats_for_range(b"a", b"c");
        assert_eq!(count, 3, "two versions of 'a' + one 'b'");

        let (count_empty, _) = mt.approximate_stats_for_range(b"x", b"a");
        assert_eq!(count_empty, 0, "reversed range yields zero");
    }

    #[test]
    fn first_and_last_entry_bracket_the_memtable() {
        let mt = memtable();
        mt.put(b"b", b"1", 1);
        mt.put(b"m", b"2", 2);
        mt.put(b"y", b"3", 3);

        let first = mt
            .first_entry_from(std::ops::Bound::Unbounded)
            .expect("has first");
        let last = mt
            .last_entry_before(std::ops::Bound::Unbounded)
            .expect("has last");
        assert_eq!(user_key_of_v(&first.0), b"b");
        assert_eq!(user_key_of_v(&last.0), b"y");

        // Bounded from above "k" - first >= k is "m".
        let m_first = mt
            .first_entry_from(std::ops::Bound::Included(&encode_internal_key(
                b"k",
                u64::MAX,
                VALUE_TYPE_VALUE,
            )))
            .expect("has entry");
        assert_eq!(user_key_of_v(&m_first.0), b"m");
    }

    #[test]
    fn excluded_bounds_step_past_the_current_entry() {
        let mt = memtable();
        mt.put(b"b", b"1", 1);
        mt.put(b"m", b"2", 2);

        let first = mt
            .first_entry_from(std::ops::Bound::Unbounded)
            .expect("has first");
        let next = mt
            .first_entry_from(std::ops::Bound::Excluded(first.0.as_slice()))
            .expect("has next");
        assert_eq!(user_key_of_v(&next.0), b"m");
        assert!(
            mt.first_entry_from(std::ops::Bound::Excluded(next.0.as_slice()))
                .is_none()
        );

        let back = mt
            .last_entry_before(std::ops::Bound::Excluded(next.0.as_slice()))
            .expect("has previous");
        assert_eq!(user_key_of_v(&back.0), b"b");
        assert!(
            mt.last_entry_before(std::ops::Bound::Excluded(back.0.as_slice()))
                .is_none()
        );
    }

    #[test]
    fn values_survive_the_memtable_being_dropped() {
        // A `DbSlice` pins the arena, so the chunk holding its bytes
        // cannot return to the pool while the slice is alive (A5).
        let mt = memtable();
        mt.put(b"k", b"pinned bytes", 1);
        let (_, value) = mt.get(&probe(b"k", 1)).expect("present");
        let value = value.expect("live value");
        drop(mt);
        assert_eq!(value.as_slice(), b"pinned bytes");
    }

    #[test]
    fn chunks_return_to_the_shared_pool_when_a_memtable_dies() {
        let config = MemTableConfig::new(ArenaProfile::EMBEDDED, 32 * 1024, 2);
        let mt = MemTable::new(&config).expect("memtable");
        for i in 0..64u64 {
            mt.put(format!("k{i:04}").as_bytes(), &[1u8; 128], i + 1);
        }
        let reserved = mt.reserved_size();
        assert!(reserved > 0);
        assert_eq!(config.pool_bytes().0, 0, "chunks are still live");
        drop(mt);
        let (parked, bound) = config.pool_bytes();
        assert_eq!(parked, reserved, "every chunk came back");
        assert!(parked <= bound);
    }

    #[test]
    fn a_pinned_slice_keeps_its_chunk_out_of_the_recycling_pool() {
        // A5 at its sharpest. Dropping a memtable normally hands every
        // chunk to the shared pool, and the next memtable writes its own
        // bytes into them. A live `DbSlice` holds an `Arc<Arena>`, so
        // that cannot happen while a reader is still pointing in: the
        // pool stays empty, the next memtable takes fresh chunks, and
        // the slice reads what it always read.
        let config = MemTableConfig::new(ArenaProfile::EMBEDDED, 32 * 1024, 2);
        let mt = MemTable::new(&config).expect("memtable");
        mt.put(b"k", b"pinned bytes", 1);
        let (_, value) = mt.get(&probe(b"k", 1)).expect("present");
        let value = value.expect("live value");
        let reserved = mt.reserved_size();
        assert!(reserved > 0);

        drop(mt);
        assert_eq!(
            config.pool_bytes().0,
            0,
            "a live slice holds every chunk back"
        );

        let next = MemTable::new(&config).expect("memtable");
        for i in 0..64u64 {
            next.put(format!("k{i:04}").as_bytes(), &[0xab; 128], i + 1);
        }
        assert_eq!(value.as_slice(), b"pinned bytes");

        drop(value);
        assert_eq!(
            config.pool_bytes().0,
            reserved,
            "the arena parks its chunks once the last slice is gone"
        );
        drop(next);
    }

    fn user_key_of_v(ik: &[u8]) -> &[u8] {
        &ik[..ik.len() - 9]
    }

    /// The borrowing seeks the streaming iterator steps with must
    /// visit exactly what the copying seeks do, with the same bytes,
    /// for every bound kind. A divergence here is a wrong scan result.
    #[test]
    fn slice_seeks_agree_with_copying_seeks() {
        use std::ops::Bound;

        let mt = memtable();
        for i in 0..64u32 {
            mt.put(
                format!("key{i:04}").as_bytes(),
                format!("v{i}").as_bytes(),
                1,
            );
        }

        let pair =
            |slices: Option<(DbSlice, DbSlice)>| slices.map(|(k, v)| (k.to_vec(), v.to_vec()));

        assert_eq!(
            pair(mt.first_slice_from(Bound::Unbounded)),
            mt.first_entry_from(Bound::Unbounded)
        );
        assert_eq!(
            pair(mt.last_slice_before(Bound::Unbounded)),
            mt.last_entry_before(Bound::Unbounded)
        );

        // Walk the whole table forward and back through both APIs.
        let mut cursor = mt.first_entry_from(Bound::Unbounded);
        let mut steps = 0usize;
        while let Some((key, _)) = cursor.clone() {
            let bound = Bound::Excluded(key.as_slice());
            assert_eq!(pair(mt.first_slice_from(bound)), mt.first_entry_from(bound));
            let bound = Bound::Included(key.as_slice());
            assert_eq!(pair(mt.first_slice_from(bound)), mt.first_entry_from(bound));
            assert_eq!(
                pair(mt.last_slice_before(bound)),
                mt.last_entry_before(bound)
            );
            let bound = Bound::Excluded(key.as_slice());
            assert_eq!(
                pair(mt.last_slice_before(bound)),
                mt.last_entry_before(bound)
            );
            cursor = mt.first_entry_from(Bound::Excluded(key.as_slice()));
            steps += 1;
        }
        assert_eq!(steps, 64);

        // Probes that fall outside the populated range.
        for probe in [b"".as_ref(), b"a", b"key", b"key9999", b"zzz"] {
            assert_eq!(
                pair(mt.first_slice_from(Bound::Included(probe))),
                mt.first_entry_from(Bound::Included(probe))
            );
            assert_eq!(
                pair(mt.last_slice_before(Bound::Included(probe))),
                mt.last_entry_before(Bound::Included(probe))
            );
        }
    }

    /// An empty memtable has nothing to hand back from either API.
    #[test]
    fn slice_seeks_on_an_empty_memtable_yield_nothing() {
        use std::ops::Bound;
        let mt = memtable();
        assert!(mt.first_slice_from(Bound::Unbounded).is_none());
        assert!(mt.last_slice_before(Bound::Unbounded).is_none());
        assert!(mt.first_slice_from(Bound::Included(b"k")).is_none());
        assert!(mt.last_slice_before(Bound::Excluded(b"k")).is_none());
    }
}

#[cfg(test)]
mod probe_tests;
