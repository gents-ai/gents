//! Repair the overlapping runs a legacy ingest could record.
//!
//! Old manifests list surviving tables in installation order. Keep that
//! order within each demoted level, and keep shallower levels newer than
//! deeper ones. Key-sorting first would lose the age of an ingested table
//! whose range tombstone widened its recorded range.
//!
//! Moving the tables is half of it. The ingest that left the overlap gave
//! its table one fresh sequence and put it in the bottom level, so a range
//! tombstone of that table can be newer than a key it covers in a table
//! above it, in L0, or in a log not flushed yet. Every other step of the
//! engine relies on the opposite: a read stops at the first source holding
//! the key, and a compaction retires a tombstone once nothing below it
//! holds what it covers (`compaction::retire`). So the open also hands the
//! range tombstones of every table it moved to the engine
//! ([`Demotion::range_deletes`]), which puts them in the memtable it
//! recovers, the newest source of all. From there they are in front of
//! every key they cover, as a `delete_range` written today is.
//!
//! The move reaches the manifest with the open's first edit
//! ([`VersionSet::apply`]), not before: the engine writes that edit once the
//! log holding those tombstones is durable. A power cut in between leaves
//! the old layout, which the next open moves again.

use std::collections::BTreeSet;

use super::super::range_tombstone::{RangeTombstone, coalesced};
use super::*;

/// What an open moved to L0, for the engine to finish and report.
pub(crate) struct Demotion {
    /// Tables moved out of the levels below L0.
    pub(crate) tables: u64,
    /// The deepest level that held overlapping tables.
    pub(crate) deepest_level: usize,
    /// Every range tombstone of a moved table, each range once per
    /// sequence, oldest sequence first.
    pub(crate) range_deletes: Vec<RangeTombstone>,
}

#[derive(Default)]
pub(super) struct Repair {
    pub(super) records: Vec<ManifestRecord>,
    pub(super) tables: u64,
    deepest_level: usize,
}

impl Repair {
    /// Move the whole prefix through the deepest overlapping level into
    /// L0. Its oldest-to-newest order is deep levels, shallow levels, then
    /// the existing L0. Demoting only the offending level would put its
    /// older values ahead of newer shallow-level values.
    pub(super) fn prepare(levels: &mut [Vec<SsTableMeta>]) -> io::Result<Self> {
        let deepest = levels
            .iter()
            .enumerate()
            .skip(1)
            .rev()
            .find(|(_, files)| overlaps(files))
            .map(|(level, _)| level);
        let Some(deepest_level) = deepest else {
            return Ok(Self::default());
        };

        // Moving a duplicate reference would replay a merge twice. It
        // is not the range-widening defect this repair accounts for.
        let mut seen = BTreeSet::new();
        let mut records = Vec::new();
        let mut tables = 0;
        for (level, files) in levels.iter().enumerate().take(deepest_level + 1) {
            for meta in files {
                if !seen.insert(meta.file_id) && level > 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "manifest lists table {} more than once below level 0, which no \
                             release writes: the file is damaged",
                            meta.file_id
                        ),
                    ));
                }
                records.push(ManifestRecord::RemoveFile {
                    level,
                    file_id: meta.file_id,
                });
                tables += u64::from(level > 0);
            }
        }

        let old_l0 = std::mem::take(&mut levels[0]);
        for level in (1..=deepest_level).rev() {
            let files = std::mem::take(&mut levels[level]);
            levels[0].extend(files);
        }
        levels[0].extend(old_l0);
        records.extend(levels[0].iter().map(|meta| ManifestRecord::AddFile {
            level: 0,
            meta: meta.clone(),
        }));
        Ok(Self {
            records,
            tables,
            deepest_level,
        })
    }

    /// What this repair moved, given the L0 it left behind: the moved
    /// tables are the oldest of `l0`, in front of the ones L0 already held.
    /// `None` when nothing overlapped.
    pub(super) fn demotion(&self, l0: &[Arc<LiveSst>]) -> Option<Demotion> {
        if self.records.is_empty() {
            return None;
        }
        let moved = l0.iter().take(self.tables as usize);
        let range_deletes = coalesced(
            moved
                .flat_map(|file| file.reader.range_tombstones())
                .cloned()
                .collect(),
        );
        Some(Demotion {
            tables: self.tables,
            deepest_level: self.deepest_level,
            range_deletes,
        })
    }
}

/// Check key order without changing the installation order needed by a
/// repair. Already key-ordered runs need no temporary allocation.
fn overlaps(files: &[SsTableMeta]) -> bool {
    if files.is_sorted_by(|a, b| a.smallest_key <= b.smallest_key) {
        return files
            .windows(2)
            .any(|p| p[0].largest_key > p[1].smallest_key);
    }
    let mut sorted: Vec<_> = files.iter().collect();
    sorted.sort_by(|a, b| a.smallest_key.cmp(&b.smallest_key));
    sorted
        .windows(2)
        .any(|p| p[0].largest_key > p[1].smallest_key)
}
