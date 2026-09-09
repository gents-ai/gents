//! Provider-view compaction: the pure reduction engine and history-shaping
//! logic moved to `gents-loop` (G-1), since the loop's own dispatch needs it
//! (`sanitize_history_for_provider` runs inside `run_loop_stream`) with no
//! DefraDB dependency. Re-exported here so `crate::compaction` keeps every
//! symbol this crate's callers already use.
#[cfg(test)]
#[path = "compaction/tests.rs"]
mod tests;

pub use gents_loop::compaction::*;
