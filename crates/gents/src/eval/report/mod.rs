//! Pure projections from eval documents to comparable numbers.
//!
//! Unfrozen by design (ruling R3): spec 4a's versioned report grows here.
//! `gents::optimization` consumes it today and M4's CLI will consume it too,
//! so neither re-derives how a run's rows become paired evidence. Nothing in
//! this module imports from `optimization`.

pub mod evidence;

pub use evidence::{
    cell_trial_scores, cell_usage, concat_paired, counted_verdicts, latest_attempts, load_run_rows,
    paired_evidence, CellUsage, RunRows,
};
