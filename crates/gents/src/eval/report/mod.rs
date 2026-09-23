//! Pure projections from eval documents to comparable numbers.
//!
//! Unfrozen by design (ruling R3). `evidence` is the verdict-to-evidence
//! projection the optimizer and the operator share; `build` is spec 4a's
//! versioned report over it. `build`, `compare` and the breakdowns are pure;
//! `store` is the module's one I/O boundary, loading rows for an owner,
//! alongside `evidence::load_run_rows` (ruling U9). Reports are derived on
//! demand and never stored, so a regrade changes a report with no migration.
//! `compare` reuses the optimizer's pure statistics so an operator's p-value
//! is the optimizer's; nothing here reads or writes an optimization document.

pub mod build;
pub mod compare;
pub mod evidence;
#[cfg(test)]
pub(crate) mod fixtures;
pub mod store;

pub use build::{
    build, AttemptSummary, CaseReport, CellReport, EvalReport, RunSummary, SlotClass, SlotCounts,
    SlotReport, SlotScore, REPORT_VERSION,
};
pub use compare::{compare, CaseComparison, Comparison, GateView, PolicyOutcome};
pub use evidence::{
    cell_trial_scores, cell_usage, concat_paired, counted_verdicts, latest_attempts, load_run_rows,
    paired_evidence, CellUsage, RunRows,
};
pub use store::{load_report, load_report_among, load_runs, run_header};

/// Documents a report cannot be built or compared from, and why.
#[derive(Debug)]
pub struct ReportRefused(pub String);

impl std::fmt::Display for ReportRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReportRefused {}

pub fn report_refused(error: &anyhow::Error) -> Option<&ReportRefused> {
    error.downcast_ref::<ReportRefused>()
}

pub(crate) fn refused(reason: impl Into<String>) -> anyhow::Error {
    anyhow::Error::from(ReportRefused(reason.into()))
}
