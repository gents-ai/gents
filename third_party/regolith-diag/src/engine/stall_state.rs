//! The write-stall thresholds.
//!
//! One classification for every thread that changes what it reads: a writer
//! after a rotation, a compaction pass, and a flush on a background worker,
//! which adds an L0 file the stop trigger counts (E9). Writers read the
//! level `commit::stall::StallSignal` caches from it on every write and
//! classify again only when it is not zero.

use super::read_view::ReadView;
use super::{EngineOptions, STOP_TOO_MANY_MEMTABLES};

/// Snapshot the current write-stall inputs: L0 file count,
/// in-memory memtable count (active + frozen), and total bytes
/// across all L0 files (regolith's approximation of pending
/// compaction bytes).
fn snapshot(view: &ReadView) -> (usize, usize, u64) {
    let l0 = view.version.levels[0].len();
    let pending_bytes: u64 = view.version.levels[0]
        .iter()
        .map(|f| f.meta.file_size)
        .sum();
    // The active memtable always counts as 1; frozen memtables
    // are whatever is still waiting for the flush path.
    let memtable_count = 1 + view.frozen.len();
    (l0, memtable_count, pending_bytes)
}

/// Classify the current state against the configured stall
/// thresholds. Returns:
///
/// * `None` - writes may proceed freely.
/// * `Some(("...", true))` - hard stop: a write applies nothing and returns
///   a wait for the stall to clear.
/// * `Some(("...", false))` - slowdown: writes proceed; with no worker and
///   inline compaction on, each owes a step of background work.
pub(super) fn classify(view: &ReadView, opts: &EngineOptions) -> Option<(&'static str, bool)> {
    let (l0, memtables, pending_bytes) = snapshot(view);
    // Stop conditions dominate over slowdown. An unconfigured
    // threshold (`0`) disables that particular trigger.
    if opts.level0_stop_writes_trigger > 0 && l0 >= opts.level0_stop_writes_trigger {
        // The L0 *count* triggers are level-style back-pressure:
        // only level compaction reduces the L0 file count in
        // response to them. Under the other two styles the count
        // can sit above the trigger with the picker correctly
        // declining to merge, so name the real cause and the knob
        // rather than pointing the caller at a compaction that
        // provably cannot help.
        return Some((
            match opts.compaction_style {
                crate::options::CompactionStyle::Level => "stop: too many L0 files",
                crate::options::CompactionStyle::Fifo => {
                    "stop: too many L0 files, and FIFO compaction never merges them - \
                     set level0_stop_writes_trigger to 0 to disable this level-style \
                     trigger, or lower fifo_compaction_options.max_table_files_size"
                }
                crate::options::CompactionStyle::Universal => {
                    "stop: too many L0 files, and the universal picker's size-ratio and \
                     size-amplification rules decline to merge them - set \
                     level0_stop_writes_trigger to 0 to disable this level-style \
                     trigger, or lower \
                     universal_compaction_options.max_size_amplification_percent"
                }
            },
            true,
        ));
    }
    if opts.max_write_buffer_number > 0
        && memtables >= opts.max_write_buffer_number.saturating_mul(2)
    {
        return Some((STOP_TOO_MANY_MEMTABLES, true));
    }
    if opts.hard_pending_compaction_bytes_limit > 0
        && pending_bytes >= opts.hard_pending_compaction_bytes_limit
    {
        return Some(("stop: pending compaction bytes over hard limit", true));
    }
    if opts.level0_slowdown_writes_trigger > 0 && l0 >= opts.level0_slowdown_writes_trigger {
        return Some(("slowdown: L0 files over trigger", false));
    }
    if opts.max_write_buffer_number > 0 && memtables > opts.max_write_buffer_number {
        return Some(("slowdown: memtables over trigger", false));
    }
    if opts.soft_pending_compaction_bytes_limit > 0
        && pending_bytes >= opts.soft_pending_compaction_bytes_limit
    {
        return Some(("slowdown: pending compaction bytes over soft limit", false));
    }
    None
}
