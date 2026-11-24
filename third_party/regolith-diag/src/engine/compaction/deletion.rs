//! Compaction triggered by deletions (D14): which pass is worth running
//! because most of what it would read is dead.
//!
//! A deletion costs every read that walks it until a pass drops it with
//! what it deletes. The size triggers do not look at deletions, so a key
//! range that was written and then deleted, and that nothing writes to
//! again, would keep its deletions for good and every scan over it would
//! walk them. This module picks the pass that removes them.
//!
//! # The rule
//!
//! A pass is picked when **more than half of the entries it would read are
//! dead**: it then discards more than it copies. Dead is counted from what
//! each table records about itself ([`Deletions`](crate::engine::sstable::Deletions)),
//! with nothing read:
//!
//! - exactly: each table's own dead entries (its point deletions, the older
//!   entries under them, the entries its own range tombstones cover), each
//!   range tombstone, and every entry of a table that lies wholly inside a
//!   range tombstone of a newer table of the pass;
//! - by estimate, where one table's deletions reach into an older table of
//!   the pass, whose entries they hide cannot be known without reading it:
//!   one entry of the older tables for each point deletion of the newer
//!   ones, since a deletion is written to delete something; and, for an
//!   older table a newer range tombstone covers in part, the share of its
//!   bytes inside the tombstone, from the table's block index. A pass over
//!   one table has no older table, so it is judged on exact counts alone.
//!
//! There is no percentage to tune. The half is the point where the pass
//! pays for itself, and the rest comes from the tables.
//!
//! # The passes
//!
//! Each is an ordinary pass through `perform_compaction_to`, with the same
//! gate, claims, rate limiter, statistics and tombstone retirement
//! (`retire.rs`) as any other.
//!
//! - **Level style.** A table with its overlap one level down. At L0 the
//!   table brings the L0 tables its range drags in (`l0_inputs`), and
//!   nothing outside that range is rewritten. A table of the bottom level
//!   is rewritten in place. The shallowest level with a candidate goes
//!   first, and within it the densest pass.
//! - **Universal style.** The newest runs merged into one, for the shortest
//!   such group that qualifies; or the only run, rewritten, when nothing
//!   deeper could hold what it deletes.
//! - **FIFO style** never merges tables, so nothing is picked there.
//!
//! # Snapshots
//!
//! A pass drops a deletion only when no live snapshot is below it. So a
//! candidate is left alone while a live snapshot is below the newest
//! deletion any of its tables holds: it could remove little, and at the
//! bottom nothing. It becomes a candidate again once that snapshot is
//! released. This is what keeps the rule from running the same pass over
//! and over while a snapshot pins a range.
//!
//! # Why it ends
//!
//! The estimate can be wrong, so nothing below relies on it.
//!
//! - A level pass moves its table one level down, which happens at most
//!   once per level.
//! - A table rewritten in place, with no snapshot below its deletions and
//!   nothing deeper, comes out without its range tombstones and with only
//!   the point deletions that sit under a merge operand, each of which has
//!   that operand beside it: at most half of the output, so the exact count
//!   does not pick it again.
//! - A universal merge of two or more runs leaves fewer runs.
//!
//! # Turns
//!
//! Size-triggered work keeps L0 under the thresholds that slow and stop
//! writes, so it goes first. A database under steady writes always has
//! some, though, and deletions would then wait for ever. [`Turn`] bounds
//! that: a deletion pass is passed over by size-triggered passes at most
//! [`crate::sync::MAX_BYPASS`] times, then it goes first, unless a stall
//! threshold is near ([`stall_near`]).

use std::io;
use std::sync::Arc;

use super::super::block_cache::BlockCache;
use super::super::manifest::{MAX_LEVELS, Version, overlapping};
use super::super::range_tombstone::exclusive_successor;
use super::super::sstable::LiveSst;
use super::{CompactionOptions, CompactionOutcome, key_range, l0_inputs, merge_key_ranges};
use crate::portability::{AtomicUsize, Ordering};

/// A pass picked for its deletions.
pub(super) struct Job {
    /// The level the inputs are at.
    pub(super) level: usize,
    /// The level the pass writes: the next one, or `level` itself for a
    /// table rewritten in place.
    pub(super) target: usize,
    pub(super) inputs: Vec<Arc<LiveSst>>,
    /// The tables of `target` the inputs overlap.
    pub(super) below: Vec<Arc<LiveSst>>,
}

/// How much of what a pass would read is dead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Share {
    dead: u64,
    items: u64,
}

impl Share {
    /// More than half: the pass discards more than it copies.
    fn mostly_dead(self) -> bool {
        u128::from(self.dead) * 2 > u128::from(self.items)
    }

    fn denser_than(self, other: Self) -> bool {
        u128::from(self.dead) * u128::from(other.items)
            > u128::from(other.dead) * u128::from(self.items)
    }
}

/// What a read walks in `table`: its entries and its range tombstones.
fn items(table: &LiveSst) -> u64 {
    table.meta.num_entries + table.reader.range_tombstones().len() as u64
}

/// The entries of `table` its own deletions hide. Nothing for a table not
/// counted yet: its entries then all count as live.
fn hidden(table: &LiveSst) -> u64 {
    table.meta.deletions.map_or(0, |deletions| deletions.dead)
}

fn points(table: &LiveSst) -> u64 {
    table.meta.deletions.map_or(0, |deletions| deletions.points)
}

/// Whether `table` holds anything this rule looks at.
fn has_deletions(table: &LiveSst) -> bool {
    hidden(table) > 0 || !table.reader.range_tombstones().is_empty()
}

/// The newest sequence a deletion of `table` can have. A table not counted
/// yet was recorded without counts, so it was written before this process
/// opened the database and before any snapshot of it; an ingested one reads
/// at its ingest sequence.
fn newest_deletion(table: &LiveSst) -> u64 {
    let point = match table.meta.deletions {
        Some(deletions) => deletions.newest_seq,
        None => table.meta.global_seq.unwrap_or(0),
    };
    table
        .reader
        .range_tombstones()
        .iter()
        .map(|rt| rt.seq)
        .fold(point, u64::max)
}

/// Whether no live snapshot is below a deletion any of `tables` holds.
/// `oldest_live` is `u64::MAX` with no snapshot live.
fn clear_of_snapshots<'t>(
    mut tables: impl Iterator<Item = &'t Arc<LiveSst>>,
    oldest_live: u64,
) -> bool {
    tables.all(|table| !has_deletions(table) || newest_deletion(table) <= oldest_live)
}

/// The dead share of a pass over `layers`: its tables in read order, the
/// newest layer first. A deletion of one layer can hide entries of the
/// layers after it. See the module docs for what is exact and what is not.
fn share(layers: &[&[Arc<LiveSst>]], cache: &BlockCache) -> Share {
    let mut all = 0u64;
    let mut dead = 0u64;
    // Point deletions of the layers before the one being added, and the
    // live entries after the first layer that one of them could hide.
    let mut deletions_above = 0u64;
    let mut hideable = 0u64;
    let mut by_points = 0u64;
    let mut by_ranges = 0u64;
    for (depth, layer) in layers.iter().enumerate() {
        for table in layer.iter() {
            let live = table.meta.num_entries.saturating_sub(hidden(table));
            all += items(table);
            dead += items(table) - live;
            if depth == 0 {
                continue;
            }
            match covered_from_above(table, live, &layers[..depth], cache) {
                Covered::Whole => dead += live,
                Covered::Part(covered) => {
                    by_ranges += covered;
                    hideable += live - covered;
                }
            }
        }
        if depth > 0 {
            by_points = deletions_above.min(hideable);
        }
        deletions_above += layer.iter().map(|table| points(table)).sum::<u64>();
    }
    Share {
        dead: dead + by_points + by_ranges,
        items: all,
    }
}

/// How much of a table the range tombstones of newer tables cover.
enum Covered {
    /// One tombstone covers its whole key range: every entry, exactly.
    Whole,
    /// This many of its live entries, by estimate; 0 when none reaches it.
    Part(u64),
}

/// What the range tombstones of `above`, the layers read before `table`,
/// cover of its `live` entries. Everything in a table is older than a
/// tombstone read before it, so what a tombstone covers there is dead.
fn covered_from_above(
    table: &LiveSst,
    live: u64,
    above: &[&[Arc<LiveSst>]],
    cache: &BlockCache,
) -> Covered {
    let (first, last) = (&table.meta.smallest_key, &table.meta.largest_key);
    let end = exclusive_successor(last);
    let mut spans: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for newer in above.iter().flat_map(|layer| layer.iter()) {
        if newer.reader.range_tombstone_over(first, last) {
            return Covered::Whole;
        }
        spans.extend(
            newer
                .reader
                .range_tombstones_within(first, &end)
                .into_iter()
                .map(|rt| (rt.start, rt.end)),
        );
    }
    if spans.is_empty() || table.meta.file_size == 0 {
        return Covered::Part(0);
    }
    // Overlapping tombstones are merged first, so no byte counts twice.
    let bytes: u64 = merge_key_ranges(&spans)
        .iter()
        .map(|(start, end)| table.reader.approximate_size_in_range(start, end, cache))
        .sum();
    let size = u128::from(table.meta.file_size);
    let covered = u128::from(live) * u128::from(bytes).min(size) / size;
    Covered::Part(covered as u64)
}

/// The level-style pass to run for its deletions, if any: the densest
/// qualifying pass of the shallowest level that has one. `oldest_live` is
/// the oldest live snapshot, `u64::MAX` with none.
pub(super) fn pick_level(version: &Version, oldest_live: u64, cache: &BlockCache) -> Option<Job> {
    let held = |table: &Arc<LiveSst>| table.claimed.load(Ordering::Acquire);
    (0..MAX_LEVELS).find_map(|level| {
        let mut best: Option<(Share, Job)> = None;
        for table in &version.levels[level] {
            if !has_deletions(table) || held(table) {
                continue;
            }
            let job = level_job(version, level, table);
            if job.inputs.iter().chain(&job.below).any(held)
                || !clear_of_snapshots(job.inputs.iter().chain(&job.below), oldest_live)
            {
                continue;
            }
            // L0 tables are read newest first, each before the older ones:
            // a layer each. A deeper level's one table is the only input.
            let mut layers: Vec<&[Arc<LiveSst>]> =
                job.inputs.iter().rev().map(std::slice::from_ref).collect();
            layers.push(&job.below);
            let share = share(&layers, cache);
            if share.mostly_dead() && best.as_ref().is_none_or(|(top, _)| share.denser_than(*top)) {
                best = Some((share, job));
            }
        }
        best.map(|(_, job)| job)
    })
}

/// The pass that takes `table` of `level` down one level, or rewrites it in
/// place at the bottom.
fn level_job(version: &Version, level: usize, table: &Arc<LiveSst>) -> Job {
    if level == MAX_LEVELS - 1 {
        return Job {
            level,
            target: level,
            inputs: vec![Arc::clone(table)],
            below: Vec::new(),
        };
    }
    let inputs = if level == 0 {
        l0_inputs(
            &version.levels[0],
            Some(&table.meta.smallest_key),
            Some(&exclusive_successor(&table.meta.largest_key)),
        )
    } else {
        vec![Arc::clone(table)]
    };
    let (first, last) = key_range(&inputs);
    Job {
        level,
        target: level + 1,
        below: overlapping(&version.levels[level + 1], &first, &last).to_vec(),
        inputs,
    }
}

/// The universal-style merge to run for its deletions, if any: the newest
/// runs of the pool, oldest first as L0 lists them. The shortest group of
/// two or more that qualifies; or the pool's only run, rewritten, when no
/// deeper level holds a key of its range, so that the rewrite retires what
/// it counts.
pub(super) fn pick_universal(
    version: &Version,
    oldest_live: u64,
    cache: &BlockCache,
) -> Option<Vec<Arc<LiveSst>>> {
    let runs = &version.levels[0];
    if !runs.iter().any(|run| has_deletions(run)) {
        return None;
    }
    if let [only] = runs.as_slice() {
        let alone = version.levels[1..].iter().all(|files| {
            overlapping(files, &only.meta.smallest_key, &only.meta.largest_key).is_empty()
        });
        return (alone
            && share(&[runs.as_slice()], cache).mostly_dead()
            && clear_of_snapshots(runs.iter(), oldest_live))
        .then(|| runs.clone());
    }
    (2..=runs.len()).find_map(|width| {
        let group = &runs[runs.len() - width..];
        // Each run is a layer of its own, newest first.
        let layers: Vec<&[Arc<LiveSst>]> = group.iter().rev().map(std::slice::from_ref).collect();
        (share(&layers, cache).mostly_dead() && clear_of_snapshots(group.iter(), oldest_live))
            .then(|| group.to_vec())
    })
}

/// Whether L0 is at, or one table short of, a threshold that slows or stops
/// writes (`stall_state::classify`): one more table like its largest would
/// reach the count or the byte limit. Size-triggered work is what brings L0
/// back down, so it goes first then. A threshold set to 0 is off.
pub(super) fn stall_near(version: &Version, opts: &CompactionOptions) -> bool {
    let l0 = &version.levels[0];
    let (bytes, largest) = l0.iter().fold((0u64, 0u64), |(sum, max), file| {
        (sum + file.meta.file_size, max.max(file.meta.file_size))
    });
    let by_count = [
        opts.level0_slowdown_writes_trigger,
        opts.level0_stop_writes_trigger,
    ]
    .into_iter()
    .any(|trigger| trigger > 0 && l0.len() + 1 >= trigger);
    let by_bytes = [
        opts.soft_pending_compaction_bytes_limit,
        opts.hard_pending_compaction_bytes_limit,
    ]
    .into_iter()
    .any(|limit| limit > 0 && bytes.saturating_add(largest) >= limit);
    by_count || by_bytes
}

/// Whose turn a pass is: size-triggered work, or a deletion pass that was
/// passed over often enough. One per engine, shared by every pass.
///
/// The count is of size-triggered passes since deletions last had a look,
/// whether or not a deletion pass was due all that time. So a pass that is
/// due is passed over at most [`Turn::MAX`] times, and with none due the
/// table list is walked for one once in `MAX` passes, not in each.
#[derive(Default)]
pub(crate) struct Turn {
    passed_over: AtomicUsize,
}

impl Turn {
    /// The bound the locks of [`crate::sync`] use for the same purpose.
    const MAX: usize = crate::sync::MAX_BYPASS as usize;

    /// Run one pass: `by_deletions` first when it is owed the turn and no
    /// stall is near (`pressed`), else `by_size`, and `by_deletions` when
    /// that had nothing to run.
    pub(super) fn pass(
        &self,
        pressed: bool,
        by_size: impl FnOnce() -> io::Result<CompactionOutcome>,
        mut by_deletions: impl FnMut() -> io::Result<CompactionOutcome>,
    ) -> io::Result<CompactionOutcome> {
        let owed = !pressed && self.passed_over.load(Ordering::Acquire) >= Self::MAX;
        let mut outcome = CompactionOutcome::Idle;
        if owed {
            match by_deletions()? {
                CompactionOutcome::DidWork => {
                    self.passed_over.store(0, Ordering::Release);
                    return Ok(CompactionOutcome::DidWork);
                }
                // None due: the count starts again.
                CompactionOutcome::Idle => self.passed_over.store(0, Ordering::Release),
                // Another pass holds its tables: still owed.
                CompactionOutcome::Contended => outcome = CompactionOutcome::Contended,
            }
        }
        match by_size()? {
            CompactionOutcome::DidWork => {
                let _ =
                    self.passed_over
                        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |passed| {
                            (passed < Self::MAX).then_some(passed + 1)
                        });
                return Ok(CompactionOutcome::DidWork);
            }
            CompactionOutcome::Contended => outcome = CompactionOutcome::Contended,
            CompactionOutcome::Idle => {}
        }
        if !owed {
            match by_deletions()? {
                CompactionOutcome::DidWork => {
                    self.passed_over.store(0, Ordering::Release);
                    return Ok(CompactionOutcome::DidWork);
                }
                CompactionOutcome::Contended => outcome = CompactionOutcome::Contended,
                CompactionOutcome::Idle => {}
            }
        }
        Ok(outcome)
    }

    /// Test-only: how many size-triggered passes have gone ahead.
    #[cfg(test)]
    pub(crate) fn passed_over(&self) -> usize {
        self.passed_over.load(Ordering::Acquire)
    }
}

#[cfg(all(test, not(loom)))]
#[path = "deletion_tests.rs"]
pub(super) mod tests;
