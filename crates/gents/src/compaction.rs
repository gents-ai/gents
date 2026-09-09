//! Provider-view compaction: the pure reduction engine and history-shaping
//! logic moved to `gents-loop` (G-1), since the loop's own dispatch needs it
//! (`sanitize_history_for_provider` runs inside `run_loop_stream`) with no
//! DefraDB dependency. Re-exported here so `crate::compaction` keeps every
//! symbol this crate's callers already use.
#[cfg(test)]
#[path = "compaction/tests.rs"]
mod tests;

pub use gents_loop::compaction::*;

// Glue for the test suite above, which reaches these bare through
// `use super::*` the way it did before the move (gents-loop's own
// `compaction` module imports them privately for its own use, so they do not
// ride the glob re-export above).
#[cfg(test)]
use crate::provider_input::budget::{
    rolling_summary_input_budget, summary_output_ceiling, threshold_decision, ThresholdDecision,
};
