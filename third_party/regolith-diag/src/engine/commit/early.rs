//! Validation ahead of the pipeline (plan 4.4, `early_check_split` in
//! `PipelineCheck.lean`).
//!
//! Before a transaction takes a slot of the commit ring it samples the read
//! horizon `h` and checks every item of its commit (each read, each
//! validated scan, each written key) against the versions committed up to
//! `h`, on its own thread. Whichever thread then decides its slot looks only
//! above `h`: an item nothing landed on above `h` keeps the verdict it had
//! at `h`, and an item something did land on is checked in full, exactly as
//! a commit checked alone would check it. Splitting the check at `h` decides as one
//! check of every version does, per item and so for the whole commit.
//!
//! Why the split is exact, item by item:
//! - The horizon is sampled before the view is loaded, so the view holds
//!   every version at or below `h`; a version compaction dropped was shadowed
//!   by a newer one, which the view holds or which lies above `h`.
//! - An item marked clean had no version newer than its read in that view.
//!   If nothing lands on it above `h` either, a check of every version finds
//!   nothing newer: clean. A blind merge marked as commuting saw only
//!   operands; with no replacement above `h` it still commutes.
//! - An item with a newer version is decided in full when the slot is, with
//!   one exception, a conflict that no later version can undo: a read
//!   validated by sequence, and a validated scan, conflict with any newer
//!   version, and a blind merge with any newer replacement, so the
//!   transaction aborts at once, without taking a slot (the model's early
//!   abort).
//!   It aborts only when every item before it is settled for good: clean,
//!   and not a put to a content-addressed key. A clean item something lands
//!   on above `h` can only become a conflict itself, which changes the key
//!   the abort names and nothing else. A put to a content-addressed key is
//!   different: a write above `h` that leaves the key other bytes makes the
//!   full check refuse the commit there, a different outcome, and it meets
//!   that item first. So a conflict behind such a put is left to the full
//!   check, and the reason a commit is given is the one a check of every
//!   version gives, whether or not the check at the horizon settled it.
//!   Every version at or below `h` is of a published commit: the abort
//!   sorts after a commit the transaction did not see.
//! - Identical-write elision, value reads and content-addressed puts can be
//!   undone by a later write of the same bytes, so for them a newer version
//!   only marks the item for the full check.
//!
//! What lands above `h` is found in the memtables alone whenever the
//! version's `last_seq` is at or below `h`: a flushed or ingested table holds
//! no sequence above the `last_seq` it raised, so every version above `h` is
//! still in a memtable the view holds. That is the work left for deciding
//! the slot: one memtable probe per item.

use std::io;

use super::super::lookup_key::LookupKey;
use super::super::{ReadView, RegolithEngine, ValidationSet};
use super::replaced::Replaced;
use super::terminator::Landed;
use crate::statistics::Ticker;
use crate::{Access, Conflict, WriteBatchOp};

/// What the check at the horizon found for one item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    /// Nothing newer than the item's read up to the horizon.
    Clean,
    /// A blind merge that only operands landed on up to the horizon.
    Commuted,
    /// A newer version the full check, made when the slot is decided, has
    /// to judge.
    Dirty,
}

/// The check at the horizon, carried into the commit's slot.
#[derive(Debug)]
pub(crate) struct Early {
    /// The read horizon the check ran at.
    pub(crate) horizon: u64,
    /// One mark per item: the reads, then the validated scans, then every
    /// operation of the batch, in that order. Empty when every item is
    /// clean, which is what a commit nothing landed under looks like, so the
    /// common case allocates nothing.
    marks: Vec<Mark>,
}

impl Early {
    /// The mark of item `at`, counted as `marks` is.
    pub(crate) fn mark(&self, at: usize) -> Mark {
        self.marks.get(at).copied().unwrap_or(Mark::Clean)
    }

    /// Whether every item is [`Mark::Clean`].
    pub(crate) fn is_clean(&self) -> bool {
        self.marks.is_empty()
    }
}

/// The early check's answer: a conflict found at the horizon, or the marks
/// the slot's decision finishes from.
pub(crate) enum EarlyVerdict {
    Conflict(Conflict),
    Marks(Early),
}

/// Records marks, allocating only once a mark is not clean.
struct Marker {
    marks: Vec<Mark>,
    total: usize,
    /// An item before the current one is not settled for good: it is dirty,
    /// or a put to a content-addressed key, which a write above the horizon
    /// can still turn into a refusal. A conflict found now may then not be
    /// what a check of every version reports, and is left to the full check.
    dirty_before: bool,
}

impl Marker {
    fn set(&mut self, at: usize, mark: Mark) {
        if mark == Mark::Clean {
            return;
        }
        if self.marks.is_empty() {
            self.marks = vec![Mark::Clean; self.total];
        }
        self.marks[at] = mark;
        self.dirty_before |= mark == Mark::Dirty;
    }
}

/// A cold check retains its completed prefix and original horizon. Raising
/// that horizon after a miss would hide changes to already checked items
/// from the slot's remaining validation (`ValidationProgress.tla`).
pub(super) struct EarlyProgress {
    horizon: u64,
    at: usize,
    marker: Marker,
}

impl RegolithEngine {
    #[cfg(test)]
    pub(super) fn check_early(
        &self,
        checks: &ValidationSet,
        ops: &[WriteBatchOp],
    ) -> io::Result<EarlyVerdict> {
        self.check_early_with_progress(checks, ops, &mut None)
    }

    /// Check `checks` and `ops` against the versions committed up to the
    /// horizon captured on the first attempt, before the commit takes a
    /// slot. A cold miss retains the marker and current item; the slot
    /// checks every change above that original horizon.
    pub(super) fn check_early_with_progress(
        &self,
        checks: &ValidationSet,
        ops: &[WriteBatchOp],
        progress: &mut Option<EarlyProgress>,
    ) -> io::Result<EarlyVerdict> {
        let reads = checks.reads.len();
        let ranges = checks.ranges.len();
        let horizon = progress
            .as_ref()
            .map_or_else(|| self.visible_seq.visible(), |progress| progress.horizon);
        if progress.is_none() {
            let newest_read = checks.reads.iter().map(|r| r.observed_seq);
            let newest_range = checks.ranges.iter().map(|r| r.observed_seq);
            if newest_read
                .chain(newest_range)
                .chain(checks.writes_at.filter(|_| !ops.is_empty()))
                .all(|observed| horizon <= observed)
            {
                return Ok(EarlyVerdict::Marks(Early {
                    horizon,
                    marks: Vec::new(),
                }));
            }
        }
        let progress = progress.get_or_insert_with(|| EarlyProgress {
            horizon,
            at: 0,
            marker: Marker {
                marks: Vec::new(),
                total: reads + ranges + ops.len(),
                dirty_before: false,
            },
        });
        let horizon = progress.horizon;
        let marker = &mut progress.marker;
        let view = self.view.load();
        for (at, check) in checks.reads.iter().enumerate().skip(progress.at) {
            progress.at = at;
            if horizon <= check.observed_seq {
                continue;
            }
            let Some((seq, theirs)) =
                self.latest_version_after(&check.key, check.observed_seq, &view)?
            else {
                continue;
            };
            if seq <= check.observed_seq {
                continue;
            }
            let final_conflict =
                matches!(check.rule, super::super::ReadRule::Seq) && !check.presence_only();
            if final_conflict && !marker.dirty_before {
                if let Some(s) = self.statistics() {
                    s.add(Ticker::CommitConflictsOnRead, 1);
                }
                return Ok(EarlyVerdict::Conflict(Conflict::new(
                    check.key.clone(),
                    check.access,
                    theirs,
                    check.observed_seq,
                    seq,
                )));
            }
            marker.set(at, Mark::Dirty);
        }
        for (at, range) in checks
            .ranges
            .iter()
            .enumerate()
            .skip(progress.at.saturating_sub(reads))
        {
            progress.at = reads + at;
            if horizon <= range.observed_seq {
                continue;
            }
            if let Some((key, seq, theirs)) =
                self.written_in_range(&range.lo, &range.hi, range.observed_seq, &view)?
            {
                if !marker.dirty_before {
                    if let Some(s) = self.statistics() {
                        s.add(Ticker::CommitConflictsOnRead, 1);
                    }
                    return Ok(EarlyVerdict::Conflict(Conflict::new(
                        key,
                        Access::ScannedRange,
                        theirs,
                        range.observed_seq,
                        seq,
                    )));
                }
                marker.set(reads + at, Mark::Dirty);
            }
        }
        if let Some(observed_seq) = checks.writes_at
            && horizon > observed_seq
        {
            let replaced = checks.blind_merges_commute.then(|| Replaced::of(ops));
            let mut last_merged: Option<&[u8]> = None;
            // The same items, skipped the same way, as the full check.
            for (at, op) in ops
                .iter()
                .enumerate()
                .skip(progress.at.saturating_sub(reads + ranges))
            {
                let at = reads + ranges + at;
                progress.at = at;
                let (key, mine) = match op {
                    WriteBatchOp::Put { key, .. } => (key, Access::Put),
                    WriteBatchOp::Delete { key } => (key, Access::Delete),
                    WriteBatchOp::Merge { key, .. } => {
                        if last_merged == Some(key.as_slice()) {
                            continue;
                        }
                        last_merged = Some(key.as_slice());
                        (key, Access::Merge)
                    }
                    WriteBatchOp::DeleteRange { .. } => continue,
                };
                if checks
                    .exempt
                    .binary_search_by(|exempt| exempt.as_slice().cmp(key))
                    .is_ok()
                {
                    // Only an exempt put is ever looked up, for its contract.
                    if matches!(op, WriteBatchOp::Put { .. }) {
                        if self.landed_above_horizon(key, observed_seq, &view, false)? {
                            marker.set(at, Mark::Dirty);
                        }
                        // Clean now, and still not settled: the contract
                        // is checked against what lands above the horizon.
                        marker.dirty_before = true;
                    }
                    continue;
                }
                if Self::read_stands_for(checks, key) {
                    continue;
                }
                let blind =
                    mine == Access::Merge && replaced.as_ref().is_some_and(|r| !r.contains(key));
                if blind {
                    match self.landed_above(key, observed_seq, &view)? {
                        Landed::Nothing => {}
                        Landed::Operands => marker.set(at, Mark::Commuted),
                        Landed::Replaced { seq, kind } if !marker.dirty_before => {
                            if let Some(s) = self.statistics() {
                                s.add(Ticker::CommitConflictsOnWrite, 1);
                            }
                            return Ok(EarlyVerdict::Conflict(Conflict::new(
                                key.clone(),
                                mine,
                                kind,
                                observed_seq,
                                seq,
                            )));
                        }
                        Landed::Replaced { .. } => marker.set(at, Mark::Dirty),
                    }
                } else if self.landed_above_horizon(key, observed_seq, &view, false)? {
                    marker.set(at, Mark::Dirty);
                }
            }
        }
        Ok(EarlyVerdict::Marks(Early {
            horizon,
            marks: std::mem::take(&mut marker.marks),
        }))
    }

    /// Whether a read of `key` in `checks` is validated in place of the write:
    /// every read but a presence-only one.
    pub(super) fn read_stands_for(checks: &ValidationSet, key: &[u8]) -> bool {
        checks
            .reads
            .binary_search_by(|read| read.key.as_slice().cmp(key))
            .is_ok_and(|at| !checks.reads[at].presence_only())
    }

    /// Whether anything landed on `key` above `horizon` in `view`: a point
    /// version or a covering range delete. With `memtables_only` the tables
    /// are not consulted, which is exact when the version's `last_seq` is at
    /// or below `horizon` (see the module docs).
    pub(super) fn landed_above_horizon(
        &self,
        key: &[u8],
        horizon: u64,
        view: &ReadView,
        memtables_only: bool,
    ) -> io::Result<bool> {
        if !memtables_only {
            return Ok(self
                .latest_version_after(key, horizon, view)?
                .is_some_and(|(seq, _)| seq > horizon));
        }
        let lk = LookupKey::from_prefixed(key, u64::MAX);
        Ok(std::iter::once(&view.active)
            .chain(view.frozen.iter().rev())
            .any(|mt| {
                mt.latest_version(&lk).is_some_and(|(seq, _)| seq > horizon)
                    || mt.covering_range_tombstone_seq(key, u64::MAX) > horizon
            }))
    }
}

#[cfg(all(test, not(loom)))]
#[path = "early_progress_tests.rs"]
mod progress_tests;
