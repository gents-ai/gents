//! Which tombstones a compaction pass drops from its outputs (E27).
//!
//! A tombstone deletes the versions older than it of the keys it covers: a
//! range tombstone every key of its range, a point deletion its own key.
//! Once a pass has nothing left for one to delete, carrying it on costs
//! every later pass over its keys and deletes nothing, and what a dropped
//! column family leaves (a range tombstone over the family's keys, a point
//! deletion of its name in the metadata family) would otherwise pile up for
//! the life of the database. A pass drops a tombstone, retires it, when both
//! hold:
//!
//! - the pass writes the deepest level holding a key it covers, so every
//!   version it covers is in the pass's inputs: an older version of a key is
//!   never above the tombstone that covers it in read order (LsmOrder.tla),
//!   and nothing below the output level holds those keys;
//! - no live snapshot is below its sequence, so each covered version shares
//!   its snapshot stripe and the pass drops it (`Stripes::shadowed` for a
//!   range tombstone, `Stripes::reduce_group` beneath a point deletion). A
//!   snapshot registered after the live list was read is at or above every
//!   sequence in the inputs (E5), the tombstone's included. A transaction's
//!   commit check reads from its own snapshot, which is registered, so it
//!   never misses a write it has to see.
//!
//! The pass still shadows with every range tombstone of its inputs; only the
//! outputs leave the retired ones out. A point deletion retires only as the
//! oldest entry of its key, with no merge operand on it.
//!
//! A pass writing L0 (a universal merge, or one table rewritten in place)
//! follows the same rule. L0 is in age order and its tables overlap, so what
//! reads below such a pass's output is every level below L0 and every L0
//! table older than the pass's newest input that is not one of its inputs:
//! the runs a model of the tree lists after the merged ones. A merge of
//! every L0 table over empty deeper levels has nothing below it, and
//! retires every tombstone no snapshot needs.
//!
//! TombstoneRetirement.tla checks the rule on runs in age order, which is
//! what both a level and an L0 table are (a point deletion is a range
//! tombstone over one key); its RED cases retire a tombstone a snapshot
//! still needs, or one above data it covers.

use std::sync::Arc;

use super::super::internal_key::{VALUE_TYPE_DELETION, VALUE_TYPE_MERGE, decode_internal_key};
use super::super::manifest::{Version, covering, overlapping};
use super::super::range_tombstone::RangeTombstoneSet;
use super::super::sstable::LiveSst;

/// What one pass may retire, fixed once its inputs are: what reads below
/// its output and the oldest live snapshot.
pub(super) struct Retirement<'v> {
    /// Every level below the output, deepest last. Each is one sorted run,
    /// searched by key.
    deeper: &'v [Vec<Arc<LiveSst>>],
    /// For a pass writing L0: the L0 tables that read below its output.
    /// They overlap one another, so each is tested on its own.
    older: Vec<&'v Arc<LiveSst>>,
    oldest_live: Option<u64>,
}

impl<'v> Retirement<'v> {
    /// A pass over `inputs` writing `target_level` of `version`, with
    /// `oldest_live` the oldest live snapshot, if any.
    pub(super) fn new(
        target_level: usize,
        version: &'v Version,
        inputs: &[Arc<LiveSst>],
        oldest_live: Option<u64>,
    ) -> Self {
        let older = if target_level == 0 {
            let l0 = &version.levels[0];
            let is_input = |file: &Arc<LiveSst>| {
                inputs
                    .iter()
                    .any(|input| input.meta.file_id == file.meta.file_id)
            };
            // Up to the newest input: a table before it that the pass does
            // not read is older than something the pass reads. With no
            // input found in L0 every table of it counts.
            let newest = l0.iter().rposition(is_input).unwrap_or(l0.len());
            l0[..newest].iter().filter(|file| !is_input(file)).collect()
        } else {
            Vec::new()
        };
        Self {
            deeper: version.levels.get(target_level + 1..).unwrap_or(&[]),
            older,
            oldest_live,
        }
    }

    /// No live snapshot is below `seq`.
    fn clear_of_snapshots(&self, seq: u64) -> bool {
        self.oldest_live.is_none_or(|oldest| seq <= oldest)
    }

    /// The range tombstones of `merged` the pass writes out.
    pub(super) fn carried_tombstones(&self, merged: &RangeTombstoneSet) -> RangeTombstoneSet {
        if merged.is_empty() {
            return merged.clone();
        }
        let carried = merged
            .iter()
            .filter(|rt| {
                !self.clear_of_snapshots(rt.seq)
                    || self.deeper.iter().any(|files| {
                        // `overlapping` takes a closed range; a table starting
                        // at the tombstone's exclusive end holds none of it.
                        overlapping(files, &rt.start, &rt.end)
                            .iter()
                            .any(|file| file.meta.smallest_key < rt.end)
                    })
                    || self.older.iter().any(|file| {
                        file.meta.smallest_key < rt.end && rt.start <= file.meta.largest_key
                    })
            })
            .cloned()
            .collect();
        RangeTombstoneSet::from_vec(carried)
    }

    /// Drop the point deletion that ends `reduced`, one key's versions
    /// newest first as the pass writes them, when it retires.
    pub(super) fn retire_deletion(&self, reduced: &mut Vec<(Vec<u8>, Vec<u8>)>) {
        let Some((last, _)) = reduced.last() else {
            return;
        };
        let (user_key, seq, value_type) = decode_internal_key(last);
        let on_an_operand = reduced
            .len()
            .checked_sub(2)
            .is_some_and(|i| decode_internal_key(&reduced[i].0).2 == VALUE_TYPE_MERGE);
        if value_type == VALUE_TYPE_DELETION
            && !on_an_operand
            && self.clear_of_snapshots(seq)
            && self
                .deeper
                .iter()
                .all(|files| covering(files, user_key).is_empty())
            && self.older.iter().all(|file| {
                user_key < file.meta.smallest_key.as_slice()
                    || file.meta.largest_key.as_slice() < user_key
            })
        {
            reduced.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::internal_key::{VALUE_TYPE_VALUE, encode_internal_key};
    use crate::engine::manifest::MAX_LEVELS;
    use crate::engine::range_tombstone::RangeTombstone;
    use crate::engine::sstable::{LiveSst, SsTableMeta, SsTableReader, SsTableWriter};
    use crate::options::CompressionType;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// A table holding `smallest` and `largest`.
    fn table(dir: &TempDir, smallest: &[u8], largest: &[u8]) -> Arc<LiveSst> {
        table_with_id(dir, 1, smallest, largest)
    }

    fn table_with_id(dir: &TempDir, file_id: u64, smallest: &[u8], largest: &[u8]) -> Arc<LiveSst> {
        let path = dir.path().join(format!("table{file_id}.sst"));
        let mut writer =
            SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096).unwrap();
        for key in [smallest, largest] {
            writer
                .add(&encode_internal_key(key, 1, VALUE_TYPE_VALUE), b"v")
                .unwrap();
        }
        writer.finish().unwrap().unwrap();
        LiveSst::new(
            SsTableMeta {
                file_id,
                smallest_key: smallest.to_vec(),
                largest_key: largest.to_vec(),
                file_size: 1,
                num_entries: 2,
                global_seq: None,
                deletions: None,
            },
            Arc::new(SsTableReader::open(&path, file_id).unwrap()),
            1,
        )
    }

    fn set(tombstones: &[(&[u8], &[u8], u64)]) -> RangeTombstoneSet {
        RangeTombstoneSet::from_vec(
            tombstones
                .iter()
                .map(|(start, end, seq)| RangeTombstone::new(start.to_vec(), end.to_vec(), *seq))
                .collect(),
        )
    }

    fn kept(carried: &RangeTombstoneSet) -> Vec<(Vec<u8>, u64)> {
        carried
            .iter()
            .map(|rt| (rt.start.clone(), rt.seq))
            .collect()
    }

    #[test]
    fn a_tombstone_retires_only_below_its_range_and_every_snapshot_under_it() {
        let dir = TempDir::new().unwrap();
        let mut version = Version::new();
        // L6 holds [m, p]; the pass writes L5.
        version.levels[MAX_LEVELS - 1].push(table(&dir, b"m", b"p"));
        let merged = set(&[(b"a", b"c", 10), (b"n", b"o", 10), (b"q", b"r", 20)]);

        let none = Retirement::new(MAX_LEVELS - 2, &version, &[], None).carried_tombstones(&merged);
        assert_eq!(
            kept(&none),
            vec![(b"n".to_vec(), 10)],
            "only the tombstone above data it covers stays"
        );

        let snapshot =
            Retirement::new(MAX_LEVELS - 2, &version, &[], Some(15)).carried_tombstones(&merged);
        assert_eq!(
            kept(&snapshot),
            vec![(b"n".to_vec(), 10), (b"q".to_vec(), 20)],
            "a snapshot below a tombstone holds it"
        );

        // A table starting at a tombstone's exclusive end holds none of it.
        let touching = set(&[(b"k", b"m", 10)]);
        assert!(
            Retirement::new(MAX_LEVELS - 2, &version, &[], None)
                .carried_tombstones(&touching)
                .is_empty()
        );

        // The bottom level has nothing below it.
        assert!(
            Retirement::new(MAX_LEVELS - 1, &version, &[], None)
                .carried_tombstones(&merged)
                .is_empty()
        );
    }

    /// L0 in age order: `old`, then `mid`, then `new`. What reads below a
    /// pass writing L0 is every L0 table before its newest input that it
    /// does not read, and every deeper level.
    #[test]
    fn a_pass_writing_l0_retires_by_the_older_tables_it_leaves_out() {
        let dir = TempDir::new().unwrap();
        let mut version = Version::new();
        let old = table_with_id(&dir, 1, b"a", b"c");
        let mid = table_with_id(&dir, 2, b"k", b"m");
        let new = table_with_id(&dir, 3, b"x", b"z");
        version.levels[0] = vec![Arc::clone(&old), Arc::clone(&mid), Arc::clone(&new)];
        version.levels[MAX_LEVELS - 1].push(table_with_id(&dir, 4, b"q", b"r"));
        let merged = set(&[
            (b"a", b"b", 10),
            (b"k", b"l", 10),
            (b"q", b"s", 10),
            (b"t", b"u", 10),
        ]);
        let starts = |inputs: &[Arc<LiveSst>]| {
            kept(&Retirement::new(0, &version, inputs, None).carried_tombstones(&merged))
                .into_iter()
                .map(|(start, _)| start)
                .collect::<Vec<_>>()
        };

        // The two newest merged: `old` is left out and holds [a, c]; the
        // bottom level holds [q, r].
        assert_eq!(
            starts(&[Arc::clone(&mid), Arc::clone(&new)]),
            vec![b"a".to_vec(), b"q".to_vec()]
        );
        // Every L0 table merged: only the bottom level is below.
        assert_eq!(
            starts(&[Arc::clone(&old), Arc::clone(&mid), Arc::clone(&new)]),
            vec![b"q".to_vec()]
        );
        // The newest alone, rewritten in place: both older tables are below.
        assert_eq!(
            starts(&[Arc::clone(&new)]),
            vec![b"a".to_vec(), b"k".to_vec(), b"q".to_vec()]
        );
        // A table newer than every input is above the output, not below it.
        assert_eq!(starts(&[Arc::clone(&old)]), vec![b"q".to_vec()]);

        // A point deletion follows the same tables.
        let deletion = |key: &[u8], inputs: &[Arc<LiveSst>]| {
            let mut reduced = vec![(
                encode_internal_key(key, 10, crate::engine::internal_key::VALUE_TYPE_DELETION),
                Vec::new(),
            )];
            Retirement::new(0, &version, inputs, None).retire_deletion(&mut reduced);
            reduced.len()
        };
        assert_eq!(deletion(b"b", &[Arc::clone(&mid), Arc::clone(&new)]), 1);
        assert_eq!(deletion(b"l", &[Arc::clone(&mid), Arc::clone(&new)]), 0);
        assert_eq!(deletion(b"b", &[old, mid, new]), 0);
    }

    #[test]
    fn a_point_deletion_retires_only_as_its_keys_oldest_entry_below_it_and_every_snapshot() {
        use crate::engine::internal_key::{VALUE_TYPE_DELETION, VALUE_TYPE_MERGE};

        let dir = TempDir::new().unwrap();
        let mut version = Version::new();
        version.levels[MAX_LEVELS - 1].push(table(&dir, b"m", b"p"));
        let entry =
            |key: &[u8], seq, value_type| (encode_internal_key(key, seq, value_type), Vec::new());
        let after = |level, oldest, mut reduced: Vec<(Vec<u8>, Vec<u8>)>| {
            Retirement::new(level, &version, &[], oldest).retire_deletion(&mut reduced);
            reduced.len()
        };
        let pass = MAX_LEVELS - 2;

        assert_eq!(
            after(pass, None, vec![entry(b"a", 10, VALUE_TYPE_DELETION)]),
            0
        );
        assert_eq!(
            after(pass, None, vec![entry(b"n", 10, VALUE_TYPE_DELETION)]),
            1,
            "a deeper table holds the key"
        );
        assert_eq!(
            after(pass, Some(5), vec![entry(b"a", 10, VALUE_TYPE_DELETION)]),
            1,
            "a snapshot is below the deletion"
        );
        assert_eq!(
            after(
                pass,
                Some(10),
                vec![
                    entry(b"a", 11, VALUE_TYPE_VALUE),
                    entry(b"a", 10, VALUE_TYPE_DELETION)
                ]
            ),
            1,
            "a snapshot at the deletion reads the key as absent either way"
        );
        assert_eq!(
            after(
                pass,
                None,
                vec![
                    entry(b"a", 11, VALUE_TYPE_MERGE),
                    entry(b"a", 10, VALUE_TYPE_DELETION)
                ]
            ),
            2,
            "a merge operand rests on it"
        );
        assert_eq!(
            after(pass, None, vec![entry(b"a", 10, VALUE_TYPE_VALUE)]),
            1,
            "only a deletion retires"
        );
    }
}
