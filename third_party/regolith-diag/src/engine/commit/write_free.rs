//! The commit of a transaction that writes nothing (plan 3.8, E50).
//!
//! A commit with no put, delete, merge or append changes nothing, at any
//! level and in either flavour, so it takes no slot of the commit pipeline
//! and no sequence. It checks what its level asked to have checked on the
//! committing thread and returns.
//!
//! # The two points it is checked at
//!
//! - **The published horizon `h`**, the newest sequence a reader sees. The
//!   commit is placed there: every commit published by then comes before it,
//!   every later one after it, and it writes nothing a later one could miss
//!   (`writeFree_at_horizon` in `Validation.lean`).
//! - **The decided frontier `c`**, the newest sequence a decided commit
//!   took, at or above `h`. A commit decided and not yet published is one a
//!   commit through the pipeline would be decided after, and so checked
//!   against. A write-free commit is checked against it too, so it is
//!   refused whenever a commit through the pipeline, decided after every
//!   commit decided by then, would be. The TLA+ row
//!   `MC_Serializable_Red_ReadOnlyAnomalyPublishedOnly` is the check that
//!   looks at `h` alone, and lets the read-only anomaly through.
//!
//! A commit that took a slot before the check and is not decided yet is not
//! counted: the write-free commit is placed before it, as it would be before
//! a commit that took its slot after the check. Through the pipeline the
//! write-free commit would have taken a slot after it and helped decide it
//! first; that is the one ordering the two routes give differently, and only
//! for commits whose writers have not returned.
//!
//! A read passes when its rule finds it current at both points. A read
//! checked by sequence that is current at `c` is current at `h` too, so it
//! is judged once. A read checked by value, by parts or for presence is
//! judged at each: an identical rewrite decided and not yet published leaves
//! it current at both, and is no conflict (E50 F1).
//!
//! # Why the points are sampled before the view is loaded
//!
//! Every version at or below `h` was published before `h` was read, and every
//! version at or below `c` was applied before `c` was read (the tip moves
//! only after its slot is applied, `settle` in `pipeline.rs`), so a view
//! loaded after both holds each of them, in a memtable or in the table a
//! flush wrote it to. Loaded first, the view could miss a commit applied to a
//! memtable sealed in between (the loom model `horizon_then_view`).
//!
//! # Versions above a point
//!
//! A version above a point is left out of the judgement there when no table
//! holds a sequence above that point: every version above it is then in a
//! memtable, where nothing is folded, so the newest version at or below the
//! point is the one a reader there would see. That leaves out commits decided
//! after `c` was read, and the operations a rollback discarded (P4) and that
//! no reader will ever see, on a latched database too (E50 F3). When a table
//! does hold a sequence above the point, compaction may have folded a version
//! at or below it under a newer one, so a version above the point fails the
//! read: a refusal the exact check might not make, never an acceptance it
//! would refuse.
//!
//! # The slot between deciding and applying
//!
//! A slot's decision is installed before it is applied, and the tip moves
//! only after: at most one slot, the one just past the decided frontier,
//! holds a decision that is not applied yet. The check applies it itself
//! with [`RegolithEngine::settle`], as any helper of the pipeline does: one
//! memtable insert per operation and no I/O. When another thread is applying
//! that slot already, the check first yields to it a bounded number of times
//! (`courtesy` in `pipeline.rs`: 32 yields plus two per operation of the
//! slot) and then applies the slot itself, so it never waits on that thread
//! beyond those yields. A rollback in progress is not helped: the frontiers are then
//! frozen, `c` still counts the operations the rollback is discarding, and a
//! read they changed is refused until it ends.

use std::io;

use super::super::{CommitOutcome, ConflictKey, ReadRule, ReadView, RegolithEngine, ValidationSet};
use super::hook;
use super::pipeline::Pipeline;
use super::ring::Loaded;
use crate::statistics::Ticker;
use crate::{Access, Conflict, WriteKind};

/// The two points a write-free commit is checked at (module docs).
#[derive(Clone, Copy, Debug)]
pub(super) struct Points {
    /// The published horizon `h`, where the commit is placed.
    pub(super) published: u64,
    /// The decided frontier `c`, at or above `published`: every commit that
    /// took a sequence at or below it is decided and applied.
    pub(super) decided: u64,
}

impl RegolithEngine {
    /// Commit a transaction that writes nothing (no put, delete, merge or
    /// append), at any level and in either flavour (plan 3.8): check what
    /// its level asked to have checked, `checks`, at the published horizon and
    /// at the decided frontier (module docs), and take no part in the commit
    /// pipeline.
    ///
    /// An item read at or after the decided frontier has nothing over it and
    /// is not looked up, so a commit nobody committed under loads no view.
    ///
    /// It writes nothing, so a database latched read-only commits it too
    /// (only `ensure_open` is asked), and it takes no sequence: the caller
    /// reports the transaction's snapshot as its receipt. With `read_queue`
    /// set (a `commit_nowait` through a `CacheOnly` handle) the check reads
    /// the cache alone and a miss refuses the commit with the wait for the
    /// block, as the pipeline's validation does (`read_mode.rs`).
    pub(crate) fn commit_write_free(
        &self,
        checks: &ValidationSet,
        read_queue: Option<crate::QueueId>,
    ) -> io::Result<CommitOutcome> {
        self.ensure_open()?;
        // Nothing to check: no point is needed, and no other commit's slot is
        // touched (ReadCommitted, or SnapshotIsolation with plain reads only).
        if checks.reads.is_empty() && checks.ranges.is_empty() {
            return Ok(CommitOutcome::Ok { seq: None });
        }
        let at = self.write_free_points();
        // Only an item read below the decided frontier can have been written
        // over.
        let open = |observed: u64| observed < at.decided;
        if !checks.reads.iter().any(|read| open(read.observed_seq))
            && !checks.ranges.iter().any(|range| open(range.observed_seq))
        {
            return Ok(CommitOutcome::Ok { seq: None });
        }
        let _scope = read_queue.map(super::super::io::scope::cache_only);
        hook::at(hook::Stage::WriteFree);
        let view = self.view.load();
        for check in checks.reads.iter().filter(|read| open(read.observed_seq)) {
            if let Some((seq, theirs)) = self.write_free_read(check, at, &view)? {
                return Ok(self.write_free_conflict(
                    check.key.clone(),
                    check.access,
                    theirs,
                    check.observed_seq,
                    seq,
                ));
            }
        }
        // A validated scan decided on a whole range: any write inside it after
        // the scan's snapshot, up to the decided frontier, fails it. Current
        // there, it is current at every point below too.
        let ceil = Self::exact_ceiling(at.decided, &view);
        for range in checks
            .ranges
            .iter()
            .filter(|range| open(range.observed_seq))
        {
            if let Some((key, seq, theirs)) = self.written_in_range_of(
                &range.lo,
                &range.hi,
                range.observed_seq,
                ceil,
                &view,
                false,
            )? {
                return Ok(self.write_free_conflict(
                    key,
                    Access::ScannedRange,
                    theirs,
                    range.observed_seq,
                    seq,
                ));
            }
        }
        Ok(CommitOutcome::Ok { seq: None })
    }

    /// Where a write-free commit is checked (module docs). With nothing in
    /// flight, every slot reserved is published and both points are the
    /// published horizon: two loads of the ring's frontiers, and no
    /// read-modify-write.
    pub(super) fn write_free_points(&self) -> Points {
        hook::at(hook::Stage::WriteFreePoints);
        if let Some(pipe) = self.pipeline.as_ref() {
            // Read before `published`: when nothing reserved by then is still
            // unpublished, nothing decided by then is either, and the
            // horizon read after covers it (P3: the horizon moves before the
            // published frontier).
            let tail = pipe.ring.tail();
            if tail - 1 > pipe.ring.published() {
                self.apply_decided(pipe);
                let decided = self.last_decided_seq();
                let published = self.visible_seq.visible();
                return Points {
                    published,
                    decided: decided.max(published),
                };
            }
        }
        let published = self.visible_seq.visible();
        Points {
            published,
            decided: published,
        }
    }

    /// Apply the slot just past the decided frontier when it holds a decision
    /// that is not applied yet, so the tip read next counts it (module docs).
    /// A slot with no decision is decided after this check, and a rollback
    /// in progress is left alone.
    fn apply_decided(&self, pipe: &Pipeline) {
        if pipe.ring.is_deciding_frozen() {
            return;
        }
        let next = pipe.ring.decided() + 1;
        if next >= pipe.ring.tail() {
            return;
        }
        if let Loaded::Ready(desc) = pipe.ring.load(next)
            && desc.decision().is_some()
        {
            self.settle(pipe, next, &desc, None, false);
        }
    }

    /// The newer write that makes `check` stale at the write-free commit's
    /// points, as `(sequence, kind)`, or `None` when it is current at both.
    fn write_free_read(
        &self,
        check: &ConflictKey,
        at: Points,
        view: &ReadView,
    ) -> io::Result<Option<(u64, WriteKind)>> {
        let Some(latest) = self.latest_version_in_view(&check.key, view)? else {
            return Ok(None);
        };
        if latest.0 <= check.observed_seq {
            return Ok(None);
        }
        // As strict as a commit through the pipeline: current at `c`.
        if let Some(stale) = self.stale_at(check, latest, at.decided, view)? {
            return Ok(Some(stale));
        }
        // A read checked by sequence that holds at `c` holds below it. So
        // does any read when nothing above `h` was written to its key.
        let by_sequence = matches!(check.rule, ReadRule::Seq) && !check.presence_only();
        if by_sequence || latest.0 <= at.published {
            return Ok(None);
        }
        // Where the commit is placed: current at `h`.
        self.stale_at(check, latest, at.published, view)
    }

    /// What `check`'s rule says of the key's versions at or below `ceil`:
    /// the write that makes it stale there, or `None`. `latest` is the key's
    /// newest version in `view`, already known to be newer than the read.
    fn stale_at(
        &self,
        check: &ConflictKey,
        latest: (u64, WriteKind),
        ceil: u64,
        view: &ReadView,
    ) -> io::Result<Option<(u64, WriteKind)>> {
        let newest = if latest.0 <= ceil {
            latest
        } else if view.version.last_seq <= ceil {
            // Every version above `ceil` is in a memtable, where nothing is
            // folded: the newest at or below it is what a reader there sees.
            match self.latest_version_upto(&check.key, ceil, view)? {
                Some(version) if version.0 > check.observed_seq => version,
                _ => return Ok(None),
            }
        } else {
            // A table holds a sequence above `ceil`: a version at or below it
            // may be folded under `latest` (module docs).
            return Ok(Some(latest));
        };
        self.read_changed(check, newest, ceil, view)
    }

    /// The ceiling a range walk can be bounded at: `ceil` when no table
    /// holds a sequence above it, so every version above it is in a
    /// memtable, and no bound otherwise (module docs).
    fn exact_ceiling(ceil: u64, view: &ReadView) -> u64 {
        if view.version.last_seq <= ceil {
            ceil
        } else {
            u64::MAX
        }
    }

    /// A write-free commit's conflict, counted.
    fn write_free_conflict(
        &self,
        key: Vec<u8>,
        mine: Access,
        theirs: WriteKind,
        observed_seq: u64,
        latest_seq: u64,
    ) -> CommitOutcome {
        if let Some(s) = self.statistics() {
            s.add(Ticker::CommitConflictsOnRead, 1);
        }
        CommitOutcome::Conflict(Conflict::new(key, mine, theirs, observed_seq, latest_seq))
    }
}
