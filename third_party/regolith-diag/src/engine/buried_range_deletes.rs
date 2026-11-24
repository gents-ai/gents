//! Range deletes an earlier release may have left behind older keys they
//! delete (E37), found at open so the open can copy them in front of every
//! key.
//!
//! Release 0.1.x placed an ingested table by the range of its point keys:
//! in level 0, or in the bottom level when that range met no table. The
//! table's range deletes, all at the table's one sequence, could cover older
//! keys in a source read before the bottom level. A point read stops at the
//! first source holding the key, so it returned a key the range delete had
//! deleted, while a scan, which gathers every range delete first, did not.
//! On this release it is worse: a compaction writing the bottom level
//! retires a range delete once nothing below it holds what it covers
//! (`compaction::retire`), and the key it deleted, still above it, comes back
//! for good. Releases that rotated the memtable into an ingest without
//! flushing it first could leave the same order inside level 0 as well.
//!
//! The order the rest of the engine relies on (a range delete is never read
//! after an older key it covers) is put back the way the open of an
//! overlapping level puts it back: the open copies the range deletes into
//! the memtable it recovers, the first source of every read, before the
//! engine is handed out, so before any compaction can retire one.
//!
//! Which ones: every range delete of a table whose range meets a source read
//! before that table, which is a key of the recovered memtable, a level 0
//! table newer than it, or a table of a level above it. That is a superset
//! of the ones behind an older key they delete. A copy of one that was
//! behind nothing changes no read (`RangeDelete.lean`,
//! `hoisted_delete_reads_by_sequence`), so the superset is sound, and it is
//! found from what the open holds in memory: each table's reader decoded
//! its range deletes when it opened, a table's range is its manifest
//! record, and the recovered memtable is in memory. No table data is read.
//! Telling the real ones apart would take reading every key of the sources
//! above each range delete.
//!
//! Cost: for `t` range deletes over `n` tables, one memtable seek and a
//! binary search over level 0 and over each deeper level per range delete,
//! `O(t log n)`. The check runs once per database: the manifest records it
//! (`ManifestRecord::RangeDeletesChecked`), and every manifest this build
//! writes carries the record from the start.

use std::sync::Arc;

use super::manifest::{Version, overlapping};
use super::memtable::MemTable;
use super::range_tombstone::{RangeTombstone, coalesced};
use super::sstable::LiveSst;

/// The range deletes of `version`'s tables that may sit behind an older key
/// they delete, as above, pieces of one delete joined; empty for a version
/// that holds none. The first `moved` level 0 tables are left out: the open
/// moved them out of overlapping levels and copies all of their range
/// deletes already. `memtable` is the one the open recovered, before
/// anything is copied into it.
pub(super) fn buried_range_deletes(
    version: &Version,
    memtable: &MemTable,
    moved: usize,
) -> Vec<RangeTombstone> {
    let l0 = &version.levels[0];
    let mut newer = LevelZero::new(
        l0.iter()
            .map(|file| {
                (
                    file.meta.smallest_key.as_slice(),
                    file.meta.largest_key.as_slice(),
                )
            })
            .collect(),
    );
    let mut found = Vec::new();
    let met_by_memtable = |rt: &RangeTombstone| memtable.holds_key_in(&rt.start, &rt.end);

    // Level 0 from its newest table to its oldest: by the time a table is
    // looked at, `newer` holds exactly the level 0 tables read before it.
    for position in (0..l0.len()).rev() {
        if position >= moved {
            found.extend(
                l0[position]
                    .reader
                    .range_tombstones()
                    .iter()
                    .filter(|rt| met_by_memtable(rt) || newer.meets(&rt.start, &rt.end))
                    .cloned(),
            );
        }
        newer.add(position);
    }

    // `newer` now holds every level 0 table, each read before any deeper
    // level, and the levels above a level are read before it.
    for (level, files) in version.levels.iter().enumerate().skip(1) {
        let above = &version.levels[1..level];
        for file in files {
            found.extend(
                file.reader
                    .range_tombstones()
                    .iter()
                    .filter(|rt| {
                        met_by_memtable(rt)
                            || newer.meets(&rt.start, &rt.end)
                            || above.iter().any(|files| level_meets(files, rt))
                    })
                    .cloned(),
            );
        }
    }
    coalesced(found)
}

/// Whether a table of `files`, one level below level 0 in key order, has a
/// range that meets `[rt.start, rt.end)`.
fn level_meets(files: &[Arc<LiveSst>], rt: &RangeTombstone) -> bool {
    // `overlapping` takes a closed range; its run is in key order, so its
    // first table starts lowest, and a table starting at the exclusive end
    // holds none of the range.
    overlapping(files, &rt.start, &rt.end)
        .first()
        .is_some_and(|file| file.meta.smallest_key < rt.end)
}

/// The level 0 tables added so far, asked whether one of them has a range
/// meeting a given one: a prefix maximum of their largest keys over all
/// level 0 tables in order of their smallest key, so a question costs a
/// binary search and a walk of `O(log n)` slots, and an addition the same.
struct LevelZero<'v> {
    /// Each table's largest key, by its position in level 0.
    largest: Vec<&'v [u8]>,
    /// Every table's smallest key, ascending.
    starts: Vec<&'v [u8]>,
    /// Where each table, by its position in level 0, sits in `starts`.
    rank: Vec<usize>,
    /// A Fenwick tree over `starts` of the greatest largest key added.
    most: Vec<Option<&'v [u8]>>,
}

impl<'v> LevelZero<'v> {
    /// Over the tables whose `(smallest, largest)` keys `ranges` lists, by
    /// position, none of them added yet.
    fn new(ranges: Vec<(&'v [u8], &'v [u8])>) -> Self {
        let mut order: Vec<usize> = (0..ranges.len()).collect();
        order.sort_by(|&a, &b| ranges[a].0.cmp(ranges[b].0));
        let mut rank = vec![0; ranges.len()];
        for (sorted, &position) in order.iter().enumerate() {
            rank[position] = sorted;
        }
        Self {
            starts: order.iter().map(|&position| ranges[position].0).collect(),
            largest: ranges.iter().map(|range| range.1).collect(),
            rank,
            most: vec![None; ranges.len()],
        }
    }

    /// Add the table at `position`.
    fn add(&mut self, position: usize) {
        let largest = self.largest[position];
        let mut slot = self.rank[position];
        while slot < self.most.len() {
            if self.most[slot].is_none_or(|most| most < largest) {
                self.most[slot] = Some(largest);
            }
            slot |= slot + 1;
        }
    }

    /// Whether a table added so far has a range meeting `[start, end)`:
    /// among those starting below the end, the greatest largest key reaches
    /// the start.
    fn meets(&self, start: &[u8], end: &[u8]) -> bool {
        let below_end = self.starts.partition_point(|first| *first < end);
        let mut most: Option<&[u8]> = None;
        let mut prefix = below_end;
        while prefix > 0 {
            let slot = prefix - 1;
            most = most.max(self.most[slot]);
            prefix = slot & (slot + 1);
        }
        most.is_some_and(|largest| largest >= start)
    }
}

#[cfg(test)]
#[path = "buried_range_deletes_tests.rs"]
mod tests;
