//! What a table holds that a read at the newest sequence walks and never
//! returns: its deletions and the entries they hide.
//!
//! The counts are exact. They are taken while the table's entries stream by,
//! where every entry is already in hand: as a flush or a compaction writes
//! it ([`super::SsTableWriter`]), as an ingest validates it, or, for a table
//! recorded before the counts existed, block by block in the background
//! (`compaction::census`). A table whose counts are not known yet carries
//! `None` and is never guessed at.
//!
//! They feed one decision, which compaction pass is worth running for its
//! deletions (`compaction::deletion`), and no read. A count can therefore
//! pick a pass that turns out not to be worth its cost; it cannot change
//! what any read returns.

use std::io;
use std::sync::Arc;

use super::super::block::decode_entry_at;
use super::super::block_cache::BlockCache;
use super::super::internal_key::{VALUE_TYPE_DELETION, decode_internal_key};
use super::super::range_tombstone::RangeTombstoneSet;
use super::{SsTableBlockCursor, SsTableReader};

/// The deletions of one table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Deletions {
    /// Point deletion entries.
    pub(crate) points: u64,
    /// Entries a deletion of this same table hides from a read at the newest
    /// sequence: every point deletion, every older entry of a key under one,
    /// and every entry one of the table's range tombstones covers with a
    /// larger sequence. At least `points`, at most the table's entries.
    pub(crate) dead: u64,
    /// The largest sequence of a point deletion in the table; 0 with none.
    pub(crate) newest_seq: u64,
}

impl Deletions {
    /// Whether these can be the counts of a table of `entries` entries. A
    /// record that says otherwise is not believed, and the table is counted
    /// again.
    pub(crate) fn fits(&self, entries: u64) -> bool {
        self.points <= self.dead && self.dead <= entries
    }
}

/// Counts a table's [`Deletions`] as its entries go by in table order: by
/// user key, and newest first within a key.
#[derive(Default)]
pub(crate) struct DeletionCounter {
    /// The range tombstones the same table carries.
    covering: Option<Arc<RangeTombstoneSet>>,
    /// A point deletion of the key now going by was seen: what follows of
    /// that key is older, and hidden by it.
    under_deletion: bool,
    counted: Deletions,
}

impl DeletionCounter {
    /// Count what `tombstones`, the range tombstones the table carries, hide
    /// of it. Said before the table's first entry.
    pub(crate) fn hidden_by(&mut self, tombstones: Arc<RangeTombstoneSet>) {
        debug_assert_eq!(
            self.counted,
            Deletions::default(),
            "the table's range tombstones are told before its entries"
        );
        self.covering = (!tombstones.is_empty()).then_some(tombstones);
    }

    /// Count one entry. `first_of_key` says it is the first entry of its
    /// user key in the table.
    pub(crate) fn see(&mut self, user_key: &[u8], seq: u64, value_type: u8, first_of_key: bool) {
        if first_of_key {
            self.under_deletion = false;
        }
        if value_type == VALUE_TYPE_DELETION {
            self.counted.points += 1;
            self.counted.dead += 1;
            self.counted.newest_seq = self.counted.newest_seq.max(seq);
            self.under_deletion = true;
        } else if self.under_deletion
            || self
                .covering
                .as_ref()
                .is_some_and(|set| set.max_covering_seq(user_key, u64::MAX) > seq)
        {
            self.counted.dead += 1;
        }
    }

    /// What was counted so far.
    pub(crate) fn counted(&self) -> Deletions {
        self.counted
    }
}

/// A count of one table's deletions taken a block at a time, for a table
/// read back rather than written: the place it has reached and what it has
/// counted up to there.
pub(crate) struct BlockCount {
    /// The next block to count; `None` once the table's last block is done.
    next: Option<SsTableBlockCursor>,
    counter: DeletionCounter,
    /// The user key of the last entry counted, to tell where a key begins
    /// across a block boundary.
    last_user_key: Vec<u8>,
    /// No entry was counted yet, so the first one begins its key.
    at_start: bool,
    key_buf: Vec<u8>,
    read_buf: Vec<u8>,
}

impl SsTableReader {
    /// Begin counting this table's deletions from its first block.
    pub(crate) fn count_deletions(&self, cache: &BlockCache) -> io::Result<BlockCount> {
        let mut counter = DeletionCounter::default();
        counter.hidden_by(Arc::new(RangeTombstoneSet::from_vec(
            self.range_tombstones().to_vec(),
        )));
        Ok(BlockCount {
            next: self.first_block_cursor(cache)?,
            counter,
            last_user_key: Vec::new(),
            at_start: true,
            key_buf: Vec::new(),
            read_buf: Vec::new(),
        })
    }

    /// Count the next block of `count`, which must be a count of this
    /// table. Returns the bytes the block takes in the file, or `None` when
    /// every block was counted already.
    pub(crate) fn count_next_block(
        &self,
        count: &mut BlockCount,
        cache: &BlockCache,
    ) -> io::Result<Option<u64>> {
        let Some(cursor) = count.next.take() else {
            return Ok(None);
        };
        let handle = self.cursor_handle(&cursor, cache)?;
        let block = self.read_block(handle, cache)?;
        // The place moves only once the block is read, so a failed read
        // leaves the count where it was.
        let after = self.next_block_cursor(&cursor, cache);
        let data = block.entry_data();
        let mut pos = 0;
        count.key_buf.clear();
        while pos < data.len() {
            let (consumed, _, _) = decode_entry_at(data, pos, &mut count.key_buf);
            pos += consumed;
            // As the reader reads it: an ingested table's entries at the
            // sequence it was ingested at.
            let internal = block.read_key(&count.key_buf, &mut count.read_buf);
            let (user_key, seq, value_type) = decode_internal_key(internal);
            let first_of_key = count.at_start || user_key != count.last_user_key.as_slice();
            count.counter.see(user_key, seq, value_type, first_of_key);
            if first_of_key {
                count.last_user_key.clear();
                count.last_user_key.extend_from_slice(user_key);
                count.at_start = false;
            }
        }
        count.next = after?;
        Ok(Some(handle.size))
    }
}

impl BlockCount {
    /// The table's counts, once every block was counted; `None` before.
    pub(crate) fn finished(&self) -> Option<Deletions> {
        self.next.is_none().then(|| self.counter.counted())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::internal_key::{VALUE_TYPE_MERGE, VALUE_TYPE_VALUE, encode_internal_key};
    use crate::engine::range_tombstone::RangeTombstone;
    use crate::engine::sstable::SsTableWriter;
    use crate::options::CompressionType;
    use tempfile::TempDir;

    const PUT: u8 = VALUE_TYPE_VALUE;
    const DEL: u8 = VALUE_TYPE_DELETION;
    const MERGE: u8 = VALUE_TYPE_MERGE;

    /// The counts of `entries` (key, sequence, type), given in table order,
    /// under the range tombstones `tombstones` (start, end, sequence).
    fn count(entries: &[(&[u8], u64, u8)], tombstones: &[(&[u8], &[u8], u64)]) -> Deletions {
        let mut counter = DeletionCounter::default();
        counter.hidden_by(Arc::new(set(tombstones)));
        let mut last: Option<&[u8]> = None;
        for &(key, seq, value_type) in entries {
            counter.see(key, seq, value_type, last != Some(key));
            last = Some(key);
        }
        counter.counted()
    }

    fn set(tombstones: &[(&[u8], &[u8], u64)]) -> RangeTombstoneSet {
        RangeTombstoneSet::from_vec(
            tombstones
                .iter()
                .map(|(start, end, seq)| RangeTombstone::new(start.to_vec(), end.to_vec(), *seq))
                .collect(),
        )
    }

    fn counts(points: u64, dead: u64, newest_seq: u64) -> Deletions {
        Deletions {
            points,
            dead,
            newest_seq,
        }
    }

    #[test]
    fn a_deletion_counts_itself_and_every_older_entry_of_its_key() {
        let got = count(
            &[
                (b"a", 9, PUT),
                (b"a", 7, DEL),
                (b"a", 5, PUT),
                (b"a", 3, MERGE),
                (b"b", 4, PUT),
                (b"c", 8, DEL),
            ],
            &[],
        );
        // `a`: the put above the deletion is live; the deletion and the two
        // entries under it are dead. `b` is live. `c` is a deletion alone.
        assert_eq!(got, counts(2, 4, 8));
    }

    #[test]
    fn an_operand_over_a_deletion_is_not_hidden_by_it() {
        let got = count(&[(b"a", 9, MERGE), (b"a", 7, DEL)], &[]);
        assert_eq!(got, counts(1, 1, 7));
    }

    #[test]
    fn a_range_tombstone_counts_only_the_older_entries_it_covers() {
        let got = count(
            &[
                (b"a", 5, PUT),
                (b"k", 5, PUT),
                (b"k", 4, PUT),
                (b"m", 20, PUT),
                (b"m", 6, PUT),
                (b"z", 5, PUT),
            ],
            &[(b"k", b"n", 10)],
        );
        // `k` at 5 and 4 and `m` at 6 are under the tombstone; `m` at 20 is
        // newer than it, and `a` and `z` are outside it.
        assert_eq!(got, counts(0, 3, 0));
    }

    #[test]
    fn an_entry_under_two_deletions_counts_once() {
        let got = count(
            &[(b"k", 9, DEL), (b"k", 7, DEL), (b"k", 5, PUT)],
            &[(b"a", b"z", 8)],
        );
        assert_eq!(got, counts(2, 3, 9));
        assert!(got.fits(3));
        assert!(!got.fits(2));
    }

    /// A table of `keys` puts with every third key deleted above its put,
    /// in blocks small enough that a key's entries cross block boundaries.
    fn table(dir: &TempDir, partitioned: bool, keys: u32) -> (SsTableReader, Deletions) {
        let path = dir.path().join(format!("t{partitioned}.sst"));
        let mut writer = SsTableWriter::new(
            &path,
            128,
            10,
            CompressionType::None,
            None,
            partitioned,
            128,
        )
        .unwrap();
        writer.hidden_by(Arc::new(set(&[(b"k0010", b"k0020", 2)])));
        for i in 0..keys {
            let key = format!("k{i:04}");
            if i % 3 == 0 {
                writer
                    .add(&encode_internal_key(key.as_bytes(), 3, DEL), b"")
                    .unwrap();
            }
            writer
                .add(&encode_internal_key(key.as_bytes(), 1, PUT), b"value")
                .unwrap();
        }
        writer.add_range_tombstone(b"k0010", b"k0020", 2);
        let summary = writer.finish().unwrap().unwrap();
        (SsTableReader::open(&path, 1).unwrap(), summary.deletions)
    }

    #[test]
    fn a_table_read_back_block_by_block_counts_what_its_writer_counted() {
        for partitioned in [false, true] {
            let dir = TempDir::new().unwrap();
            let (reader, written) = table(&dir, partitioned, 200);
            // 67 deletions, each over one put, and the tombstone over the
            // puts of k0010..k0019 not already under a deletion (7 of 10).
            assert_eq!(written, counts(67, 67 + 67 + 7, 3));

            let cache = BlockCache::new(1 << 20);
            let mut count = reader.count_deletions(&cache).unwrap();
            let mut blocks = 0;
            while reader
                .count_next_block(&mut count, &cache)
                .unwrap()
                .is_some()
            {
                assert_eq!(count.finished().is_some(), count.next.is_none());
                blocks += 1;
            }
            assert!(blocks > 8, "the table has many blocks: {blocks}");
            assert_eq!(count.finished(), Some(written), "partitioned {partitioned}");
        }
    }
}
