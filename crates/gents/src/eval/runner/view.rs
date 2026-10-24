use std::collections::BTreeMap;
use std::path::Path;

use crate::eval::documents::EvalError;
use serde::{Deserialize, Serialize};

use crate::eval::report::EvalReport;
use crate::eval::runner::goal::GoalEntry;
use crate::eval::runner::progress::LiveSnapshot;
use crate::eval::Anchor;

use super::files::write_json_atomically;

pub const EVIDENCE_FILE: &str = "evidence.json";
pub const REPORT_FILE: &str = "report.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRecord {
    pub evidence_digest: String,
    pub anchor: Anchor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<LiveSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub goal: Vec<GoalEntry>,
}

pub fn read_evidence_record(trial_dir: &Path) -> Option<EvidenceRecord> {
    let bytes = std::fs::read(trial_dir.join(EVIDENCE_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn write_evidence_record(
    trial_dir: &Path,
    record: &EvidenceRecord,
) -> Result<(), EvalError> {
    std::fs::create_dir_all(trial_dir).map_err(|source| EvalError::CreateDir {
        path: trial_dir.to_path_buf(),
        source,
    })?;
    write_json_atomically(&trial_dir.join(EVIDENCE_FILE), record)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunView {
    pub written_at: String,
    pub pid: u32,
    pub report: EvalReport,
    pub trials: BTreeMap<String, EvidenceRecord>,
}

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

pub fn read_run_view(run_dir: &Path) -> Option<RunView> {
    let bytes = std::fs::read(run_dir.join(REPORT_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn write_run_view(run_dir: &Path, view: &RunView) -> Result<(), EvalError> {
    write_json_atomically(&run_dir.join(REPORT_FILE), view)
}
