//! Pure projections from eval documents to comparable numbers; reports are
//! derived on demand and never stored. `compare` reuses the optimizer's pure
//! statistics so an operator's p-value is the optimizer's.

pub mod breakdown;
pub mod build;
pub mod compare;
pub mod evidence;
#[cfg(test)]
pub(crate) mod fixtures;
pub mod store;

pub use breakdown::{by_check, by_stage, case_view, CaseView, CheckDiff, StageDiff};
pub use build::{
    build, AttemptSummary, CaseReport, CellReport, EvalReport, RunSummary, SlotClass, SlotCounts,
    SlotReport, SlotScore, SlotVerdict, REPORT_VERSION,
};
pub use compare::{
    compare, CaseComparison, CompareOptions, Comparison, GateView, PairedTrial, PolicyOutcome,
    SideTrial,
};
pub use evidence::{
    cell_trial_scores, cell_usage, concat_paired, counted_verdicts, latest_attempts, load_run_rows,
    paired_evidence, CellUsage, RunRows,
};
pub use store::{load_report, load_report_among, load_runs, run_header};

use crate::eval::documents::EvalError;

/// Documents a report cannot be built or compared from, and why.
#[derive(Debug)]
pub struct ReportRefused(pub String);

impl std::fmt::Display for ReportRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReportRefused {}

pub fn report_refused<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a ReportRefused> {
    std::iter::successors(Some(error), |error| error.source()).find_map(|error| {
        match error.downcast_ref::<EvalError>() {
            Some(EvalError::ReportRefused(found)) => Some(found),
            _ => error.downcast_ref::<ReportRefused>(),
        }
    })
}

pub(crate) fn refused(reason: impl Into<String>) -> EvalError {
    EvalError::ReportRefused(ReportRefused(reason.into()))
}
