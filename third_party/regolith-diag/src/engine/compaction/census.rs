//! The census: deletion counts for the tables recorded without them.
//!
//! A table this build writes is recorded with its deletion counts
//! (`sstable::deletions`). A table recorded by an earlier build, or restored
//! from a backup, has none, and the rule that compacts a range for its
//! deletions (`deletion.rs`) cannot weigh it. Left at that, the rule would
//! never apply to the oldest data of an upgraded database, which is where
//! deletions pile up. The census reads such tables in the background and
//! records what it counts.
//!
//! # A step
//!
//! One step runs at the end of a compaction pass that found nothing to
//! compact, with no write stall near: on a worker when it wakes with
//! nothing else to do, and, with no worker, in the step a write owes after a
//! flush. A step:
//!
//! - reads at most one [`CompactionOptions::target_file_size`] of one table,
//!   block by block, through the block cache like any read, and charges
//!   what it read to the rate limiter as a compaction's output is;
//! - stops at any block when the worker is told of other work or to shut
//!   down, and never waits for another thread: when another thread is in
//!   the census it does nothing;
//! - keeps its place in memory, so a table larger than one step takes
//!   several. A restart, or a table compacted away meanwhile, loses the
//!   place and never a count.
//!
//! It reports nothing to its pass, which stays idle: a worker then waits
//! for its next wake-up before the next step, and a write owes no further
//! step for it. That pacing is the census's whole priority: one step per
//! idle wake-up.
//!
//! # Recording a count
//!
//! When a table's last block is counted, the step appends the counts to the
//! manifest as an edit of that table ([`VersionEdit::CountFile`]), holding:
//!
//! - the pass's share of the compaction gate, so nothing that takes the
//!   gate alone (a `compact_range`, an ingest, a checkpoint, a backup,
//!   `drop_all`) runs;
//! - the table's claim, or the pool's under universal compaction, so no
//!   other pass removes the table;
//! - the version store's step, in which the edit runs one at a time with
//!   every other edit.
//!
//! With the claim held it reads the current version again and records only
//! if the table is still there. The edit changes no read and is not synced:
//! a crash that loses it leaves the table without counts, and it is counted
//! again (`manifest.rs`, tag 8).
//!
//! # Tables it cannot count
//!
//! A table whose blocks do not read back is set aside for the life of the
//! process and named once in the log. It stays in the count of tables not
//! yet counted, which is true. The read error itself surfaces where it
//! matters, on the read or the compaction that meets that block.

use std::sync::{Arc, Weak};

use super::super::block_cache::BlockCache;
use super::super::manifest::{MAX_LEVELS, Version, VersionEdit};
use super::super::sstable::{BlockCount, Deletions, LiveSst};
use super::super::version_store::VersionStore;
use super::CompactionOptions;
use super::claims::{self, Claims};
use crate::sync::Mutex;

/// The census of one engine: what it is counting and where it has been.
#[derive(Default)]
pub(crate) struct Census {
    /// Taken with `try_lock` for a whole step, so one thread counts at a
    /// time and none waits for it.
    state: Mutex<State>,
}

struct State {
    /// The count under way, kept from one step to the next.
    counting: Option<Counting>,
    /// Where the last table to count was found: its level and its index
    /// there. The next search begins here, so a sweep over a database of
    /// tables without counts does not walk the counted ones again each
    /// step.
    found_at: (usize, usize),
    /// Tables that could not be counted, by file id.
    set_aside: Vec<u64>,
    /// The version a whole search last found nothing to count in. Until
    /// another version is published there is nothing new to find.
    nothing_in: Weak<Version>,
    /// The manifest refused an edit. Its failure is reported by whatever
    /// writes it next; the census does not keep trying.
    stopped: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            counting: None,
            // The deepest level first: what compaction is slowest to
            // rewrite, and so what stays without counts longest.
            found_at: (MAX_LEVELS - 1, 0),
            set_aside: Vec::new(),
            nothing_in: Weak::new(),
            stopped: false,
        }
    }
}

/// One table being counted. Held weakly between steps: a table compacted
/// away meanwhile is not kept open for a count nobody will finish.
struct Counting {
    table: Weak<LiveSst>,
    level: usize,
    count: BlockCount,
}

/// What became of a finished count.
enum Recorded {
    /// It is in the manifest and the version.
    Done,
    /// Another pass holds the table: try again at the next step.
    Later,
    /// The table is gone, compacted away while it was counted.
    Gone,
    Failed(std::io::Error),
}

/// Run one step of the census. `pool_held` says the pass holds the pool's
/// claim (universal compaction), which stands in for the table's.
/// `interrupted` says the step should stop where it is.
pub(super) fn step(
    versions: &Arc<VersionStore>,
    cache: &BlockCache,
    opts: &CompactionOptions,
    claims: &Claims,
    pool_held: bool,
    interrupted: &dyn Fn() -> bool,
) {
    let version = versions.current();
    if interrupted() {
        return;
    }
    let Some(mut state) = claims.census.state.try_lock() else {
        return;
    };
    if version.uncounted == 0 {
        // Whatever was being counted has its counts, or is gone.
        state.counting = None;
        return;
    }
    if state.stopped || Weak::ptr_eq(&state.nothing_in, &Arc::downgrade(&version)) {
        return;
    }

    // Go on with the table under way if the version still lists it.
    let under_way = state.counting.take().and_then(|counting| {
        let table = counting.table.upgrade()?;
        claims::all_live(
            &version.levels[counting.level],
            std::slice::from_ref(&table),
        )
        .then_some((table, counting))
    });
    let (table, mut counting) = match under_way {
        Some(under_way) => under_way,
        None => {
            let Some((level, index)) = next_uncounted(&version, &state) else {
                state.nothing_in = Arc::downgrade(&version);
                return;
            };
            state.found_at = (level, index);
            let table = Arc::clone(&version.levels[level][index]);
            match table.reader.count_deletions(cache) {
                Ok(count) => {
                    let counting = Counting {
                        table: Arc::downgrade(&table),
                        level,
                        count,
                    };
                    (table, counting)
                }
                Err(error) => {
                    set_aside(&mut state, &table, &error.to_string());
                    return;
                }
            }
        }
    };

    let budget = opts.target_file_size;
    let mut read = 0u64;
    let counted = loop {
        if let Some(deletions) = counting.count.finished() {
            break Ok(Some(deletions));
        }
        if read >= budget || interrupted() {
            break Ok(None);
        }
        match table.reader.count_next_block(&mut counting.count, cache) {
            Ok(bytes) => read += bytes.unwrap_or(0),
            Err(error) => break Err(error),
        }
    };
    // On a worker only, as for every background write. A limiter that
    // fails has said so itself; the bytes are read either way.
    let _ = crate::rate_limiter::throttle_background(opts.rate_limiter.as_ref(), read);

    match counted {
        Ok(None) => state.counting = Some(counting),
        Ok(Some(deletions)) if !deletions.fits(table.meta.num_entries) => {
            set_aside(
                &mut state,
                &table,
                "its entries do not match the entry count it records",
            );
        }
        Ok(Some(deletions)) => {
            match record(
                versions,
                claims,
                pool_held,
                &table,
                counting.level,
                deletions,
            ) {
                Recorded::Done | Recorded::Gone => {}
                Recorded::Later => state.counting = Some(counting),
                Recorded::Failed(error) => {
                    tracing::warn!(
                        file_id = table.meta.file_id,
                        error = %error,
                        "a table's deletion counts could not be recorded; no more tables are counted until the database is reopened"
                    );
                    state.stopped = true;
                }
            }
        }
        Err(error) => set_aside(&mut state, &table, &error.to_string()),
    }
}

/// Append `deletions` as the counts of `table` of `level`, if it is still
/// in the version. The module docs say what is held meanwhile.
fn record(
    versions: &Arc<VersionStore>,
    claims: &Claims,
    pool_held: bool,
    table: &Arc<LiveSst>,
    level: usize,
    deletions: Deletions,
) -> Recorded {
    let claim = if pool_held {
        None
    } else {
        match claims.tables(vec![Arc::clone(table)]) {
            Some(claim) => Some(claim),
            None => return Recorded::Later,
        }
    };
    // Read again with the claim held: a table still listed stays listed
    // until the claim is given back, which is after the edit.
    let current = versions.current();
    if !claims::all_live(&current.levels[level], std::slice::from_ref(table)) {
        return Recorded::Gone;
    }
    let edit = VersionEdit::CountFile {
        level,
        file_id: table.meta.file_id,
        deletions,
    };
    let applied = versions.run(|versions| versions.apply(&[edit]));
    drop(claim);
    match applied {
        Ok(()) => Recorded::Done,
        Err(error) => Recorded::Failed(error),
    }
}

/// Leave `table` out of the census for the life of this process, and say
/// so once.
fn set_aside(state: &mut State, table: &LiveSst, why: &str) {
    tracing::warn!(
        file_id = table.meta.file_id,
        why,
        "a table's deletions could not be counted; it is left uncounted"
    );
    state.set_aside.push(table.meta.file_id);
}

/// The next table to count: the first one without counts from where the
/// last one was found, through each level from there to L0, then round from
/// the deepest level to where the search began.
fn next_uncounted(version: &Version, state: &State) -> Option<(usize, usize)> {
    let tables = |level: usize| version.levels[level].len();
    let (from_level, from_index) = state.found_at;
    let from_index = from_index.min(tables(from_level));
    let onward = (0..=from_level).rev().flat_map(|level| {
        let start = if level == from_level { from_index } else { 0 };
        (start..tables(level)).map(move |index| (level, index))
    });
    let round = (from_level..MAX_LEVELS).rev().flat_map(|level| {
        let end = if level == from_level {
            from_index
        } else {
            tables(level)
        };
        (0..end).map(move |index| (level, index))
    });
    onward.chain(round).find(|&(level, index)| {
        let table = &version.levels[level][index];
        table.meta.deletions.is_none() && !state.set_aside.contains(&table.meta.file_id)
    })
}

#[cfg(all(test, not(loom)))]
#[path = "census_tests.rs"]
mod tests;
