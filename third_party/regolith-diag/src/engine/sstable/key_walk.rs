//! The walk over one key's entries across an SSTable's data blocks, shared by
//! the merge-chain read and the commit check's terminator walk, so a chain
//! longer than one block is read whole by both.

use std::io;
use std::ops::ControlFlow;
use std::sync::Arc;

use super::super::merge_chain::VersionSink;
use super::super::source_walk::{Above, Skip};
use super::{
    Block, BlockCache, DbSlice, LookupKey, SsTableReader, VALUE_TYPE_MERGE, decode_internal_key,
    invalid_data,
};

/// Visits one entry of a key in a data block: the block, the entry's
/// sequence and value type, and its value's offset and length in the block.
pub(crate) trait VisitBlockEntry<R>:
    FnMut(&Arc<Block>, u64, u8, usize, usize) -> ControlFlow<R>
{
}

impl<R, F> VisitBlockEntry<R> for F where
    F: FnMut(&Arc<Block>, u64, u8, usize, usize) -> ControlFlow<R>
{
}

impl SsTableReader {
    /// Walk every visible entry for `user_key` at `snapshot_seq` in
    /// newest-seq-first order, appending `(seq, value_type, value)`
    /// tuples onto `out` and stopping at (and including) the first
    /// terminator (`VALUE_TYPE_VALUE` or `VALUE_TYPE_DELETION`).
    /// Returns `true` if a terminator was reached.
    ///
    /// The chain continues into the following data blocks for as long as
    /// the key does: a key with more operands than one block holds, which a
    /// pinned snapshot makes routine by keeping compaction from folding
    /// them, would otherwise lose the rest of its chain and its base.
    pub(crate) fn collect_merge_chain<S>(
        &self,
        lk: &LookupKey,
        key_buf: &mut Vec<u8>,
        cache: &BlockCache,
        out: &mut S,
    ) -> io::Result<bool>
    where
        S: VersionSink,
    {
        let terminated = self.scan_key(lk, key_buf, cache, |block, seq, vt, offset, len| {
            let Some(value) = DbSlice::from_block(Arc::clone(block), offset, len) else {
                return ControlFlow::Break(Err(invalid_data("block value extends past block")));
            };
            out.push_version((seq, vt, value));
            if vt != VALUE_TYPE_MERGE {
                ControlFlow::Break(Ok(()))
            } else {
                ControlFlow::Continue(())
            }
        })?;
        Ok(terminated.transpose()?.is_some())
    }

    /// [`MemTable::skip_merges_above`](crate::engine::memtable::MemTable::skip_merges_above)
    /// for one table. The skip continues into the following data blocks for
    /// as long as the key's operands above `floor` do, like
    /// [`SsTableReader::collect_merge_chain`], and stops at the first block
    /// that settles it.
    ///
    /// Copies nothing: each block is scanned in place and no value is sliced.
    pub(crate) fn skip_merges_above(
        &self,
        lk: &LookupKey,
        floor: u64,
        key_buf: &mut Vec<u8>,
        cache: &BlockCache,
    ) -> io::Result<Skip> {
        let mut passed = false;
        let stop = self.scan_key(lk, key_buf, cache, |_, seq, vt, _, _| {
            if seq <= floor || vt != VALUE_TYPE_MERGE {
                ControlFlow::Break((seq, vt))
            } else {
                passed = true;
                ControlFlow::Continue(())
            }
        })?;
        Ok(Skip {
            passed,
            stop: stop.map(|(seq, _)| seq),
            stop_type: stop.map_or(0, |(_, vt)| vt),
        })
    }

    /// [`MemTable::newer_in_range`](crate::engine::memtable::MemTable::newer_in_range)
    /// for one table: every data block that holds a key of `[lo, hi)` is read,
    /// since a table keeps no sequence range of its own to rule it out.
    pub(crate) fn newer_in_range(
        &self,
        lo: &[u8],
        hi: &[u8],
        floor: u64,
        key_buf: &mut Vec<u8>,
        cache: &BlockCache,
    ) -> io::Result<Option<(Vec<u8>, u64, u8)>> {
        let start = LookupKey::from_prefixed(lo, u64::MAX);
        let search_key = start.internal();
        let mut cursor = self.seek_block_cursor(search_key, cache)?;
        while let Some(at) = cursor {
            let block = self.load_block_at_cursor(&at, cache)?;
            let ended = block.scan_from(search_key, key_buf, |ik, _, _| {
                let (user_key, seq, vt) = decode_internal_key(ik);
                if user_key >= hi {
                    return ControlFlow::Break(None);
                }
                if seq > floor {
                    return ControlFlow::Break(Some((user_key.to_vec(), seq, vt)));
                }
                ControlFlow::Continue(())
            });
            match ended {
                Some(found) => return Ok(found),
                None => cursor = self.next_block_cursor(&at, cache)?,
            }
        }
        Ok(None)
    }

    /// [`MemTable::holds_key_in`](crate::engine::memtable::MemTable::holds_key_in)
    /// for one table: whether it holds an entry whose user key lies in
    /// `[first, last]`, or a range tombstone reaching into it, at any
    /// sequence. Reads the one or two data blocks the first such key could
    /// be in.
    pub(crate) fn holds_key_in(
        &self,
        first: &[u8],
        last: &[u8],
        key_buf: &mut Vec<u8>,
        cache: &BlockCache,
    ) -> io::Result<bool> {
        if self
            .range_tombstones
            .iter()
            .any(|rt| rt.start.as_slice() <= last && first < rt.end.as_slice())
        {
            return Ok(true);
        }
        let start = LookupKey::from_prefixed(first, u64::MAX);
        let search_key = start.internal();
        let mut cursor = self.seek_block_cursor(search_key, cache)?;
        while let Some(at) = cursor {
            let block = self.load_block_at_cursor(&at, cache)?;
            let found = block.scan_from(search_key, key_buf, |ik, _, _| {
                ControlFlow::Break(decode_internal_key(ik).0 <= last)
            });
            match found {
                Some(holds) => return Ok(holds),
                // Only a block emptied by the seek moves on to the next.
                None => cursor = self.next_block_cursor(&at, cache)?,
            }
        }
        Ok(false)
    }

    /// [`MemTable::visit_above`](crate::engine::memtable::MemTable::visit_above)
    /// for one table: `lk`'s entries newest first among those at or below the
    /// lookup's snapshot and above `floor`, carrying on into the following
    /// data blocks for as long as the key does, until `visit` breaks.
    pub(crate) fn visit_above<R>(
        &self,
        lk: &LookupKey,
        floor: u64,
        key_buf: &mut Vec<u8>,
        cache: &BlockCache,
        mut visit: impl FnMut(u64, u8, DbSlice) -> ControlFlow<R>,
    ) -> io::Result<Above<R>> {
        let ended = self.scan_key(lk, key_buf, cache, |block, seq, vt, offset, len| {
            if seq <= floor {
                return ControlFlow::Break(Ok(None));
            }
            let Some(value) = DbSlice::from_block(Arc::clone(block), offset, len) else {
                return ControlFlow::Break(Err(invalid_data("block value extends past block")));
            };
            visit(seq, vt, value).map_break(|result| Ok(Some(result)))
        })?;
        Ok(match ended.transpose()? {
            None => Above::Exhausted,
            Some(None) => Above::Floor,
            Some(Some(result)) => Above::Broke(result),
        })
    }

    /// Visits `lk`'s entries newest first from the lookup's snapshot, carrying
    /// on into the following data blocks for as long as the key does, until
    /// `visit` breaks. `Ok(None)` when the key ends first or the bloom filter
    /// rules it out. Each block is scanned in place; `visit` sees the block, the
    /// entry's sequence and value type, and where its value lies in the block.
    fn scan_key<R>(
        &self,
        lk: &LookupKey,
        key_buf: &mut Vec<u8>,
        cache: &BlockCache,
        mut visit: impl VisitBlockEntry<R>,
    ) -> io::Result<Option<R>> {
        let user_key = lk.prefixed_user_key();
        if self.hidden_at(lk.snapshot_seq()) || !self.filter(cache)?.may_contain(user_key) {
            return Ok(None);
        }

        let search_key = lk.internal();
        let mut cursor = self.seek_block_cursor(search_key, cache)?;
        while let Some(at) = cursor {
            let block = self.load_block_at_cursor(&at, cache)?;
            // Every entry of a later block sorts after `search_key`, so
            // seeking to it there starts at the block's first entry.
            let ended = block.scan_from(search_key, key_buf, |ik, value_offset, value_len| {
                let (uk, seq, vt) = decode_internal_key(ik);
                if uk != user_key {
                    return ControlFlow::Break(None);
                }
                visit(&block, seq, vt, value_offset, value_len).map_break(Some)
            });
            match ended {
                Some(result) => return Ok(result),
                // The block ran out while still on this key.
                None => cursor = self.next_block_cursor(&at, cache)?,
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::internal_key::{VALUE_TYPE_DELETION, VALUE_TYPE_VALUE, encode_internal_key};
    use crate::engine::sstable::SsTableWriter;
    use crate::options::CompressionType;
    use proptest::prelude::*;
    use std::cmp::Reverse;
    use std::ops::RangeInclusive;
    use tempfile::TempDir;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Kind {
        Put,
        Delete,
        Merge,
    }

    impl Kind {
        /// The value type an entry of this kind carries in its internal key.
        fn value_type(self) -> u8 {
            match self {
                Self::Put => VALUE_TYPE_VALUE,
                Self::Delete => VALUE_TYPE_DELETION,
                Self::Merge => VALUE_TYPE_MERGE,
            }
        }

        /// The stored value for this kind; a deletion carries none.
        fn value(self) -> &'static [u8] {
            match self {
                Self::Put => b"base",
                Self::Delete => b"",
                Self::Merge => b"operand",
            }
        }
    }

    /// A user key, a sequence and what the write was.
    type Entry = (&'static [u8], u64, Kind);

    /// What `collect_merge_chain` reports: the `(seq, value_type, value)`
    /// entries it pushed, and whether it reached a terminator.
    type Chain = (Vec<(u64, u8, Vec<u8>)>, bool);

    const BLOCK_SIZE: usize = 64;

    /// A table of `entries`, written in internal-key order, with data blocks
    /// and index leaves a few entries wide so one key's chain spans many of
    /// both. The index is flat or partitioned as asked.
    fn table(dir: &TempDir, partitioned: bool, entries: &[Entry]) -> SsTableReader {
        let mut sorted = entries.to_vec();
        sorted.sort_by(|a, b| a.0.cmp(b.0).then(b.1.cmp(&a.1)));
        let path = dir.path().join("key_walk.sst");
        let mut writer = SsTableWriter::new(
            &path,
            BLOCK_SIZE,
            10,
            CompressionType::None,
            None,
            partitioned,
            BLOCK_SIZE,
        )
        .unwrap();
        for (key, seq, kind) in sorted {
            writer
                .add(
                    &encode_internal_key(key, seq, kind.value_type()),
                    kind.value(),
                )
                .unwrap();
        }
        writer.finish().unwrap().unwrap();
        SsTableReader::open(&path, 1).unwrap()
    }

    /// One merge operand of `key` at each sequence in `seqs`.
    fn operands(key: &'static [u8], seqs: RangeInclusive<u64>) -> impl Iterator<Item = Entry> {
        seqs.map(move |seq| (key, seq, Kind::Merge))
    }

    /// Data blocks in the table, counted through the block cursor.
    fn block_count(reader: &SsTableReader, cache: &BlockCache) -> usize {
        let first = LookupKey::from_prefixed(b"a", u64::MAX);
        let mut cursor = reader.seek_block_cursor(first.internal(), cache).unwrap();
        let mut count = 0;
        while let Some(at) = cursor {
            count += 1;
            cursor = reader.next_block_cursor(&at, cache).unwrap();
        }
        count
    }

    /// `skip_merges_above` for `key` read at `snapshot_seq`, as reported.
    fn skip_typed(
        reader: &SsTableReader,
        cache: &BlockCache,
        key: &[u8],
        snapshot_seq: u64,
        floor: u64,
    ) -> Skip {
        let lk = LookupKey::from_prefixed(key, snapshot_seq);
        reader
            .skip_merges_above(&lk, floor, &mut Vec::new(), cache)
            .unwrap()
    }

    /// [`skip_typed`] without the stop's value type, which the model test and
    /// `the_stop_names_the_value_type_of_the_entry_it_ended_at` check apart, so
    /// the cases below stay about where the skip ended.
    fn skip(
        reader: &SsTableReader,
        cache: &BlockCache,
        key: &[u8],
        snapshot_seq: u64,
        floor: u64,
    ) -> Skip {
        Skip {
            stop_type: 0,
            ..skip_typed(reader, cache, key, snapshot_seq, floor)
        }
    }

    /// A skip that passed an operand or not, and stopped at `stop`.
    fn ended(passed: bool, stop: Option<u64>) -> Skip {
        Skip {
            passed,
            stop,
            stop_type: 0,
        }
    }

    #[test]
    fn the_stop_names_the_value_type_of_the_entry_it_ended_at() {
        let mut entries = vec![(b"k".as_slice(), 1, Kind::Put)];
        entries.extend(operands(b"k", 2..=3));
        entries.push((b"k", 4, Kind::Delete));
        entries.extend(operands(b"k", 5..=6));
        entries.push((b"l", 7, Kind::Put));
        for_both_indexes(&entries, |reader, cache| {
            let stopped = skip_typed(reader, cache, b"k", u64::MAX, 0);
            assert_eq!(
                (stopped.passed, stopped.stop, stopped.stop_type),
                (true, Some(4), VALUE_TYPE_DELETION)
            );
            let stopped = skip_typed(reader, cache, b"l", u64::MAX, 0);
            assert_eq!(
                (stopped.stop, stopped.stop_type),
                (Some(7), VALUE_TYPE_VALUE)
            );
            // At the floor the skip stops on whatever entry is there, an
            // operand included.
            let stopped = skip_typed(reader, cache, b"k", u64::MAX, 6);
            assert_eq!(
                (stopped.stop, stopped.stop_type),
                (Some(6), VALUE_TYPE_MERGE)
            );
        });
    }

    /// `collect_merge_chain` for `key` read at `snapshot_seq`.
    fn chain_of(
        reader: &SsTableReader,
        cache: &BlockCache,
        key: &[u8],
        snapshot_seq: u64,
    ) -> Chain {
        let lk = LookupKey::from_prefixed(key, snapshot_seq);
        let mut out = Vec::new();
        let terminated = reader
            .collect_merge_chain(&lk, &mut Vec::new(), cache, &mut out)
            .unwrap();
        let found = out
            .into_iter()
            .map(|(seq, vt, value)| (seq, vt, value.as_slice().to_vec()))
            .collect();
        (found, terminated)
    }

    /// `key`'s entries among `entries` at or below `snapshot_seq`, newest first.
    fn visible(entries: &[Entry], key: &[u8], snapshot_seq: u64) -> Vec<Entry> {
        let mut found: Vec<Entry> = entries
            .iter()
            .copied()
            .filter(|&(k, seq, _)| k == key && seq <= snapshot_seq)
            .collect();
        found.sort_by_key(|&(_, seq, _)| Reverse(seq));
        found
    }

    /// What `skip_merges_above` should report, by walking `visible` entry by
    /// entry.
    fn model_skip(entries: &[Entry], key: &[u8], snapshot_seq: u64, floor: u64) -> Skip {
        let mut skip = ended(false, None);
        for (_, seq, kind) in visible(entries, key, snapshot_seq) {
            if seq <= floor || kind != Kind::Merge {
                skip.stop = Some(seq);
                skip.stop_type = kind.value_type();
                break;
            }
            skip.passed = true;
        }
        skip
    }

    /// The chain `collect_merge_chain` should report: `visible`, through the
    /// first terminator.
    fn model_chain(entries: &[Entry], key: &[u8], snapshot_seq: u64) -> Chain {
        let mut found = Vec::new();
        for (_, seq, kind) in visible(entries, key, snapshot_seq) {
            found.push((seq, kind.value_type(), kind.value().to_vec()));
            if kind != Kind::Merge {
                return (found, true);
            }
        }
        (found, false)
    }

    /// Run `check` against `entries` under both index shapes.
    fn for_both_indexes(entries: &[Entry], check: impl Fn(&SsTableReader, &BlockCache)) {
        for partitioned in [false, true] {
            let dir = TempDir::new().unwrap();
            let reader = table(&dir, partitioned, entries);
            check(&reader, &BlockCache::new(1024 * 1024));
        }
    }

    /// `k` is a put at 3 under 300 operands, between neighbours that would
    /// answer for it if the walk ever left `k`.
    fn chain() -> Vec<Entry> {
        let mut entries = vec![(b"j".as_slice(), 1, Kind::Put), (b"j", 2, Kind::Merge)];
        entries.push((b"k", 3, Kind::Put));
        entries.extend(operands(b"k", 4..=303));
        entries.extend([(b"l".as_slice(), 304, Kind::Put), (b"l", 305, Kind::Merge)]);
        entries
    }

    /// `k` holds only operands 200 through 300, no base: last in the table
    /// in the second shape, and followed by a put of `l` in the first.
    fn operands_only() -> [Vec<Entry>; 2] {
        let mut at_the_end = vec![(b"j".as_slice(), 1, Kind::Put)];
        at_the_end.extend(operands(b"k", 200..=300));
        let mut before_l = at_the_end.clone();
        before_l.push((b"l", 400, Kind::Put));
        [before_l, at_the_end]
    }

    #[test]
    fn a_chain_of_hundreds_of_operands_spans_many_blocks_and_leaves() {
        let entries = chain();
        for partitioned in [false, true] {
            let dir = TempDir::new().unwrap();
            let reader = table(&dir, partitioned, &entries);
            let cache = BlockCache::new(1024 * 1024);
            assert!(block_count(&reader, &cache) > 60);
            if partitioned {
                assert!(reader.index(&cache).unwrap().len() > 30);
            }
        }
    }

    #[test]
    fn the_skip_crosses_blocks_down_to_the_base() {
        for_both_indexes(&chain(), |reader, cache| {
            assert_eq!(skip(reader, cache, b"k", u64::MAX, 0), ended(true, Some(3)));
            assert_eq!(skip(reader, cache, b"k", u64::MAX, 2), ended(true, Some(3)));
        });
    }

    #[test]
    fn the_skip_stops_at_the_floor_in_the_middle_of_the_chain() {
        for_both_indexes(&chain(), |reader, cache| {
            assert_eq!(
                skip(reader, cache, b"k", u64::MAX, 150),
                ended(true, Some(150))
            );
            assert_eq!(skip(reader, cache, b"k", u64::MAX, 3), ended(true, Some(3)));
            assert_eq!(
                skip(reader, cache, b"k", u64::MAX, 303),
                ended(false, Some(303))
            );
            assert_eq!(
                skip(reader, cache, b"k", u64::MAX, 400),
                ended(false, Some(303))
            );
        });
    }

    #[test]
    fn a_skip_reports_whether_it_passed_an_operand() {
        for_both_indexes(&chain(), |reader, cache| {
            // The newest operand is the stop itself: nothing was passed.
            assert_eq!(
                skip(reader, cache, b"k", u64::MAX, 303),
                ended(false, Some(303))
            );
            // One operand above the floor is passed over.
            assert_eq!(
                skip(reader, cache, b"k", u64::MAX, 302),
                ended(true, Some(302))
            );
            // The only visible entry is a base: there is nothing to pass.
            assert_eq!(skip(reader, cache, b"j", 1, 0), ended(false, Some(1)));
        });
    }

    #[test]
    fn the_skip_starts_below_the_lookup_snapshot() {
        for_both_indexes(&chain(), |reader, cache| {
            assert_eq!(skip(reader, cache, b"k", 100, 0), ended(true, Some(3)));
            assert_eq!(skip(reader, cache, b"k", 100, 50), ended(true, Some(50)));
            assert_eq!(skip(reader, cache, b"k", 100, 120), ended(false, Some(100)));
            assert_eq!(skip(reader, cache, b"k", 3, 0), ended(false, Some(3)));
            assert_eq!(skip(reader, cache, b"k", 2, 0), ended(false, None));
        });
    }

    #[test]
    fn a_deletion_in_the_middle_of_the_chain_ends_the_skip() {
        let mut entries = vec![(b"j".as_slice(), 1, Kind::Put), (b"k", 3, Kind::Put)];
        entries.extend(operands(b"k", 4..=100));
        entries.push((b"k", 101, Kind::Delete));
        entries.extend(operands(b"k", 102..=303));
        entries.push((b"l", 304, Kind::Put));
        for_both_indexes(&entries, |reader, cache| {
            assert_eq!(
                skip(reader, cache, b"k", u64::MAX, 0),
                ended(true, Some(101))
            );
            assert_eq!(
                skip(reader, cache, b"k", u64::MAX, 50),
                ended(true, Some(101))
            );
            assert_eq!(
                skip(reader, cache, b"k", u64::MAX, 100),
                ended(true, Some(101))
            );
            assert_eq!(
                skip(reader, cache, b"k", u64::MAX, 101),
                ended(true, Some(101))
            );
            assert_eq!(
                skip(reader, cache, b"k", u64::MAX, 150),
                ended(true, Some(150))
            );
        });
    }

    #[test]
    fn operands_above_the_floor_find_nothing_before_or_at_the_end_of_the_table() {
        for entries in operands_only() {
            for_both_indexes(&entries, |reader, cache| {
                assert_eq!(skip(reader, cache, b"k", u64::MAX, 100), ended(true, None));
                assert_eq!(skip(reader, cache, b"k", u64::MAX, 199), ended(true, None));
                assert_eq!(
                    skip(reader, cache, b"k", u64::MAX, 200),
                    ended(true, Some(200))
                );
            });
        }
    }

    #[test]
    fn a_key_the_table_lacks_finds_nothing() {
        for_both_indexes(&chain(), |reader, cache| {
            for key in [b"a".as_slice(), b"i", b"ka", b"m", b"zz"] {
                assert_eq!(
                    skip(reader, cache, key, u64::MAX, 0),
                    ended(false, None),
                    "{key:?}"
                );
            }
        });
    }

    #[test]
    fn the_chain_runs_across_blocks_newest_first_through_the_base() {
        for_both_indexes(&chain(), |reader, cache| {
            let (found, terminated) = chain_of(reader, cache, b"k", u64::MAX);
            assert!(terminated);
            let seqs: Vec<u64> = found.iter().map(|&(seq, _, _)| seq).collect();
            assert_eq!(seqs, (3..=303).rev().collect::<Vec<_>>());
            let (base, operands) = found.split_last().unwrap();
            assert_eq!(*base, (3, VALUE_TYPE_VALUE, b"base".to_vec()));
            assert!(
                operands
                    .iter()
                    .all(|(_, vt, value)| *vt == VALUE_TYPE_MERGE && value == b"operand")
            );
        });
    }

    #[test]
    fn the_chain_starts_below_the_lookup_snapshot() {
        for_both_indexes(&chain(), |reader, cache| {
            let (found, terminated) = chain_of(reader, cache, b"k", 100);
            assert!(terminated);
            let seqs: Vec<u64> = found.iter().map(|&(seq, _, _)| seq).collect();
            assert_eq!(seqs, (3..=100).rev().collect::<Vec<_>>());
            let (found, terminated) = chain_of(reader, cache, b"k", 3);
            assert!(terminated);
            assert_eq!(found, [(3, VALUE_TYPE_VALUE, b"base".to_vec())]);
            assert_eq!(chain_of(reader, cache, b"k", 2), (Vec::new(), false));
        });
    }

    #[test]
    fn a_deletion_ends_the_chain_without_the_base_beneath_it() {
        let mut entries = vec![(b"k".as_slice(), 3, Kind::Put)];
        entries.extend(operands(b"k", 4..=100));
        entries.push((b"k", 101, Kind::Delete));
        entries.extend(operands(b"k", 102..=303));
        entries.push((b"l", 304, Kind::Put));
        for_both_indexes(&entries, |reader, cache| {
            let (found, terminated) = chain_of(reader, cache, b"k", u64::MAX);
            assert!(terminated);
            let seqs: Vec<u64> = found.iter().map(|&(seq, _, _)| seq).collect();
            assert_eq!(seqs, (101..=303).rev().collect::<Vec<_>>());
            assert_eq!(found.last(), Some(&(101, VALUE_TYPE_DELETION, Vec::new())));
        });
    }

    #[test]
    fn operands_that_run_to_the_end_of_the_table_have_no_terminator() {
        let [_, at_the_end] = operands_only();
        for_both_indexes(&at_the_end, |reader, cache| {
            let (found, terminated) = chain_of(reader, cache, b"k", u64::MAX);
            assert!(!terminated);
            let seqs: Vec<u64> = found.iter().map(|&(seq, _, _)| seq).collect();
            assert_eq!(seqs, (200..=300).rev().collect::<Vec<_>>());
            assert!(
                found
                    .iter()
                    .all(|(_, vt, value)| *vt == VALUE_TYPE_MERGE && value == b"operand")
            );
        });
    }

    #[test]
    fn neighbouring_keys_never_leak_into_a_chain() {
        let [before_l, _] = operands_only();
        for_both_indexes(&before_l, |reader, cache| {
            let (found, terminated) = chain_of(reader, cache, b"k", u64::MAX);
            assert!(!terminated, "the put of `l` ended the chain of `k`");
            assert_eq!(found.len(), 101);
            assert_eq!(
                chain_of(reader, cache, b"j", u64::MAX),
                (vec![(1, VALUE_TYPE_VALUE, b"base".to_vec())], true)
            );
            assert_eq!(
                chain_of(reader, cache, b"l", u64::MAX),
                (vec![(400, VALUE_TYPE_VALUE, b"base".to_vec())], true)
            );
        });
    }

    #[test]
    fn an_absent_key_has_an_empty_chain() {
        for_both_indexes(&chain(), |reader, cache| {
            for key in [b"a".as_slice(), b"i", b"ka", b"m", b"zz"] {
                assert_eq!(
                    chain_of(reader, cache, key, u64::MAX),
                    (Vec::new(), false),
                    "{key:?}"
                );
            }
        });
    }

    const KEYS: [&[u8]; 3] = [b"j", b"k", b"l"];

    /// Writes as `(key index, kind)` in sequence order from 1, a floor and
    /// a lookup snapshot, both drawn from just past the last sequence, the
    /// snapshot sometimes unbounded.
    fn scenario() -> impl Strategy<Value = (Vec<(usize, Kind)>, u64, u64)> {
        let kind = prop_oneof![Just(Kind::Put), Just(Kind::Delete), Just(Kind::Merge)];
        proptest::collection::vec((0..KEYS.len(), kind), 1..160).prop_flat_map(|writes| {
            let past_last = writes.len() as u64 + 1;
            (
                Just(writes),
                0..=past_last,
                prop_oneof![0..=past_last, Just(u64::MAX)],
            )
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn both_walks_match_a_linear_model(
            (writes, floor, snapshot_seq) in scenario(),
        ) {
            let entries: Vec<Entry> = writes
                .iter()
                .enumerate()
                .map(|(index, &(key, kind))| (KEYS[key], index as u64 + 1, kind))
                .collect();
            for partitioned in [false, true] {
                let dir = TempDir::new().unwrap();
                let reader = table(&dir, partitioned, &entries);
                let cache = BlockCache::new(1024 * 1024);
                for key in KEYS.into_iter().chain([b"absent".as_slice()]) {
                    prop_assert_eq!(
                        skip_typed(&reader, &cache, key, snapshot_seq, floor),
                        model_skip(&entries, key, snapshot_seq, floor),
                        "skip, partitioned={}", partitioned
                    );
                    prop_assert_eq!(
                        chain_of(&reader, &cache, key, snapshot_seq),
                        model_chain(&entries, key, snapshot_seq),
                        "chain, partitioned={}", partitioned
                    );
                }
            }
        }
    }
}
