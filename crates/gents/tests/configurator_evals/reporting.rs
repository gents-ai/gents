use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use serde::Serialize;

use super::{stages::CaseId, ConfiguratorEvalResult, EVAL_CASE_ID};

#[derive(Default)]
struct Counts {
    passed: usize,
    failed: usize,
    skipped: usize,
}

impl Counts {
    fn summary(&self, planned: usize) -> serde_json::Value {
        let attempted = self.passed + self.failed;
        serde_json::json!({
            "passed": self.passed, "failed": self.failed, "skipped": self.skipped,
            "attempted": attempted, "unreported": planned - attempted - self.skipped,
            "pass_rate": (attempted > 0).then(|| self.passed as f64 / attempted as f64),
        })
    }
}

pub struct RunReport {
    directory: PathBuf,
    models: Vec<String>,
    runs: usize,
    provider: &'static str,
    concurrency: usize,
    stage_timeout_secs: u64,
    started_at: String,
    started: Instant,
    pub results: Vec<ConfiguratorEvalResult>,
}

impl RunReport {
    pub fn new(
        directory: PathBuf,
        models: Vec<String>,
        runs: usize,
        provider: &'static str,
        concurrency: usize,
        stage_timeout_secs: u64,
    ) -> Self {
        Self {
            directory,
            models,
            runs,
            provider,
            concurrency,
            stage_timeout_secs,
            started_at: chrono::Utc::now().to_rfc3339(),
            started: Instant::now(),
            results: Vec::new(),
        }
    }

    pub fn failed(&self) -> usize {
        self.results.iter().filter(|result| !result.passed).count()
    }

    fn snapshot(&self) -> serde_json::Value {
        let mut summaries = Vec::new();
        for model in &self.models {
            let results = self
                .results
                .iter()
                .filter(|result| &result.model == model)
                .collect::<Vec<_>>();
            let counts = Counts {
                passed: results.iter().filter(|result| result.passed).count(),
                failed: results.iter().filter(|result| !result.passed).count(),
                skipped: 0,
            };
            let mut cases = Vec::new();
            for case_id in CaseId::ALL {
                let mut counts = Counts::default();
                let mut failure_kinds = BTreeMap::<&str, usize>::new();
                let mut elapsed_ms = 0;
                for case in results
                    .iter()
                    .flat_map(|result| &result.cases)
                    .filter(|case| case.case_id == case_id.as_str())
                {
                    elapsed_ms += case.elapsed_ms;
                    match case.status.as_str() {
                        "passed" => counts.passed += 1,
                        "failed" => {
                            counts.failed += 1;
                            *failure_kinds
                                .entry(case.failure_kind.as_deref().unwrap_or("acceptance"))
                                .or_default() += 1;
                        }
                        "skipped" => counts.skipped += 1,
                        other => panic!("unknown case status {other}"),
                    }
                }
                cases.push(serde_json::json!({
                    "case_id": case_id.as_str(), "counts": counts.summary(self.runs),
                    "failure_kinds": failure_kinds, "elapsed_ms": elapsed_ms,
                }));
            }
            summaries.push(serde_json::json!({
                "model": model, "counts": counts.summary(self.runs), "cases": cases,
                "fixture_or_harness_failures": results.iter().filter(|result| result.terminal_state.is_none()).count(),
            }));
        }
        let planned = self.models.len() * self.runs;
        serde_json::json!({
            "schema_version": 1, "suite": EVAL_CASE_ID, "provider": self.provider,
            "models": self.models, "runs_per_model": self.runs, "concurrency": self.concurrency,
            "stage_timeout_secs": self.stage_timeout_secs,
            "started_at": self.started_at, "updated_at": chrono::Utc::now().to_rfc3339(),
            "elapsed_ms": self.started.elapsed().as_millis(),
            "status": if self.results.len() == planned { "completed" } else { "running" },
            "planned": planned, "completed": self.results.len(), "failed": self.failed(),
            "passed": self.results.len() - self.failed(), "unfinished": planned - self.results.len(),
            "summaries": summaries, "trials": self.results,
        })
    }

    pub fn save(&self) -> Result<()> {
        write_json(&self.directory.join("report.json"), &self.snapshot())
    }
}

pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().expect("report parent"))?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.flush()?;
    file.persist(path)?;
    Ok(())
}

#[test]
fn report_preserves_partial_results_and_failure_categories() {
    let directory = tempfile::tempdir().unwrap();
    let mut report = RunReport::new(
        directory.path().into(),
        vec!["model".into()],
        2,
        "d4f",
        1,
        1800,
    );
    report.save().unwrap();
    let initial = report.snapshot();
    assert_eq!(initial["stage_timeout_secs"], 1800);
    assert_eq!(initial["unfinished"], 2);
    assert!(initial["summaries"][0]["counts"]["pass_rate"].is_null());
    assert_eq!(
        initial["summaries"][0]["cases"].as_array().unwrap().len(),
        CaseId::ALL.len()
    );
    report.results.push(ConfiguratorEvalResult {
        case_id: EVAL_CASE_ID,
        provider: "d4f",
        model: "model".into(),
        trial: 1,
        passed: false,
        terminal_state: None,
        error: Some("fixture failure".into()),
        assistant_answer_excerpt: None,
        artifacts: None,
        cases: vec![super::stages::CaseResult {
            case_id: "onboarding".into(),
            status: "failed".into(),
            elapsed_ms: 12,
            error: Some("cannot establish outcome".into()),
            failure_kind: Some("inconclusive".into()),
        }],
    });
    report.save().unwrap();
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.path().join("report.json")).unwrap())
            .unwrap();
    assert_eq!(saved["status"], "running");
    assert_eq!(saved["completed"], 1);
    assert_eq!(saved["unfinished"], 1);
    assert_eq!(saved["summaries"][0]["fixture_or_harness_failures"], 1);
    assert_eq!(
        saved["summaries"][0]["cases"][0]["failure_kinds"]["inconclusive"],
        1
    );
    assert_eq!(saved["summaries"][0]["cases"][1]["counts"]["skipped"], 0);
    assert_eq!(saved["summaries"][0]["cases"][1]["counts"]["unreported"], 2);
    report.results.push(ConfiguratorEvalResult {
        case_id: EVAL_CASE_ID,
        provider: "d4f",
        model: "model".into(),
        trial: 2,
        passed: true,
        terminal_state: Some("completed".into()),
        error: None,
        assistant_answer_excerpt: None,
        artifacts: None,
        cases: vec![super::stages::CaseResult {
            case_id: "builder-readiness".into(),
            status: "skipped".into(),
            elapsed_ms: 0,
            error: None,
            failure_kind: None,
        }],
    });
    let completed = report.snapshot();
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["summaries"][0]["counts"]["pass_rate"], 0.5);
    let readiness = &completed["summaries"][0]["cases"][1]["counts"];
    assert_eq!(readiness["skipped"], 1);
    assert_eq!(readiness["unreported"], 1);
    assert!(readiness["pass_rate"].is_null());
}
