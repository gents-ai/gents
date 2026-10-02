//! What a watcher reads of a run without opening the home's node.
//!
//! The process hosting a run holds the home's embedded node, whose store
//! opens exclusively, so `gents eval watch` cannot build a report while the
//! run is live. The loop therefore leaves two files a watcher can read:
//!
//! - `<run dir>/trials/<trial_id>/evidence.json`, the [`EvidenceRecord`]
//!   written beside each finished trial's home;
//! - `<run dir>/report.json`, the [`RunView`]: the run's report as
//!   [`crate::eval::report::load_report`] derives it, joined with each
//!   slot's evidence record, rewritten whenever a trial finishes.
//!
//! Both are advisory copies. Nothing the loop, a report or a resume decides
//! reads them back: the documents stay the only record, and a watcher with
//! the node open derives the same view from them through [`run_view`].

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::eval::report::EvalReport;
use crate::eval::runner::goal::GoalEntry;
use crate::eval::runner::progress::LiveSnapshot;
use crate::eval::Anchor;

use super::files::write_json_atomically;

pub const EVIDENCE_FILE: &str = "evidence.json";
pub const REPORT_FILE: &str = "report.json";

/// `<run dir>/trials/<trial_id>/evidence.json`.
///
/// `TrialCompletion` has no field for the digest, so the runner keeps it
/// beside the retained home, with the trial's last [`LiveSnapshot`] and the
/// goal it was measured against. Fields after the anchor are absent from a
/// record an older runner wrote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    pub evidence_digest: String,
    pub anchor: Anchor,
    /// RFC 3339: when the trial's documents were completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<LiveSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub goal: Vec<GoalEntry>,
}

/// The evidence record beside a trial's home, or `None` when there is none
/// or it does not parse.
pub fn read_evidence_record(trial_dir: &Path) -> Option<EvidenceRecord> {
    let bytes = std::fs::read(trial_dir.join(EVIDENCE_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn write_evidence_record(trial_dir: &Path, record: &EvidenceRecord) -> Result<()> {
    std::fs::create_dir_all(trial_dir)
        .with_context(|| format!("creating {}", trial_dir.display()))?;
    write_json_atomically(&trial_dir.join(EVIDENCE_FILE), record)
}

/// `<run dir>/report.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunView {
    /// RFC 3339 with milliseconds.
    pub written_at: String,
    /// The process that wrote it.
    pub pid: u32,
    pub report: EvalReport,
    /// The evidence record of each slot's latest attempt, by trial id.
    pub trials: BTreeMap<String, EvidenceRecord>,
}

/// `report` joined with the evidence records its slots' latest attempts left
/// under `run_dir`.
pub fn run_view(report: EvalReport, run_dir: &Path) -> RunView {
    let trials = report
        .cells
        .iter()
        .flat_map(|cell| &cell.slots)
        .filter_map(|slot| slot.latest.as_ref())
        .filter_map(|latest| {
            read_evidence_record(&run_dir.join("trials").join(&latest.trial_id))
                .map(|record| (latest.trial_id.clone(), record))
        })
        .collect();
    RunView {
        written_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        pid: std::process::id(),
        report,
        trials,
    }
}

/// The view the loop last left under `run_dir`, or `None` when there is none
/// or it does not parse (a file from a runner whose report shape differs).
pub fn read_run_view(run_dir: &Path) -> Option<RunView> {
    let bytes = std::fs::read(run_dir.join(REPORT_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Never creates `run_dir`: a run directory removed under a live loop stays
/// removed, and the write fails.
pub(crate) fn write_run_view(run_dir: &Path, view: &RunView) -> Result<()> {
    write_json_atomically(&run_dir.join(REPORT_FILE), view)
}
