use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{stages::CaseId, ConfiguratorEvalResult, EVAL_CASE_ID};

#[derive(Clone, Debug, Serialize)]
pub struct RunProvenance {
    cohort: &'static str,
    source: SourceRevision,
    grader: GraderRevision,
    inference: InferenceRevision,
    fixture_sha256: BTreeMap<&'static str, String>,
}

#[derive(Clone, Debug, Serialize)]
struct SourceRevision {
    commit: String,
    dirty: bool,
}

#[derive(Clone, Debug, Serialize)]
struct GraderRevision {
    id: &'static str,
    sha256: String,
}

#[derive(Clone, Debug, Serialize)]
struct InferenceRevision {
    endpoint: String,
    sampling_id: &'static str,
    effective_sampling: serde_json::Value,
}

fn sha256(parts: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part);
    }
    format!("{:x}", digest.finalize())
}

impl RunProvenance {
    pub fn current(
        cohort: &'static str,
        grader: &'static str,
        endpoint: String,
        sampling_id: &'static str,
        temperature: f64,
        top_p: f64,
    ) -> Self {
        let fixtures = [
            (
                "builder_readiness.md",
                include_bytes!("../fixtures/configurator_evals/builder_readiness.md").as_slice(),
            ),
            (
                "document_automation.md",
                include_bytes!("../fixtures/configurator_evals/document_automation.md").as_slice(),
            ),
            (
                "improve_pagoda.md",
                include_bytes!("../fixtures/configurator_evals/improve_pagoda.md").as_slice(),
            ),
            (
                "review_pagoda.md",
                include_bytes!("../fixtures/configurator_evals/review_pagoda.md").as_slice(),
            ),
            (
                "skill_setup.md",
                include_bytes!("../fixtures/configurator_evals/skill_setup.md").as_slice(),
            ),
            (
                "skill_use.md",
                include_bytes!("../fixtures/configurator_evals/skill_use.md").as_slice(),
            ),
            (
                "software_team_and_code_review.md",
                include_bytes!("../fixtures/configurator_evals/software_team_and_code_review.md")
                    .as_slice(),
            ),
            (
                "tool_surface_audit.md",
                include_bytes!("../fixtures/configurator_evals/tool_surface_audit.md").as_slice(),
            ),
            (
                "voxel_pagoda.md",
                include_bytes!("../fixtures/configurator_evals/voxel_pagoda.md").as_slice(),
            ),
        ];
        Self {
            cohort,
            source: SourceRevision {
                commit: std::env::var("GENTS_EVAL_SOURCE_REVISION")
                    .unwrap_or_else(|_| "unknown".to_owned()),
                dirty: std::env::var("GENTS_EVAL_SOURCE_DIRTY")
                    .map_or(true, |value| value != "false"),
            },
            grader: GraderRevision {
                id: grader,
                sha256: sha256(&[
                    include_bytes!("cases.rs"),
                    include_bytes!("readiness.rs"),
                    include_bytes!("stages.rs"),
                    include_bytes!("../../../../scripts/evals/check-pagoda.mjs"),
                ]),
            },
            inference: InferenceRevision {
                endpoint,
                sampling_id,
                effective_sampling: serde_json::json!({
                    "temperature": temperature,
                    "top_p": top_p,
                    "seed": null,
                }),
            },
            fixture_sha256: fixtures
                .into_iter()
                .map(|(name, bytes)| (name, sha256(&[bytes])))
                .collect(),
        }
    }
}

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
    provenance: RunProvenance,
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
        provenance: RunProvenance,
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
            provenance,
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
            let mut trial_failure_kinds = BTreeMap::<&str, usize>::new();
            for result in &results {
                if let Some(kind) = result.trial_failure_kind.as_deref() {
                    *trial_failure_kinds.entry(kind).or_default() += 1;
                }
            }
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
                "trial_failure_kinds": trial_failure_kinds,
            }));
        }
        let planned = self.models.len() * self.runs;
        serde_json::json!({
            "schema_version": 1, "suite": EVAL_CASE_ID, "provider": self.provider,
            "models": self.models, "runs_per_model": self.runs, "concurrency": self.concurrency,
            "stage_timeout_secs": self.stage_timeout_secs,
            "provenance": self.provenance,
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

pub fn write_json_new(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().expect("report parent"))?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.flush()?;
    file.persist_noclobber(path)?;
    Ok(())
}

#[test]
fn immutable_evidence_writer_never_replaces_an_existing_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reassessment.json");
    write_json_new(&path, &serde_json::json!({"verdict":"original"})).unwrap();
    assert!(write_json_new(&path, &serde_json::json!({"verdict":"replacement"})).is_err());
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(saved["verdict"], "original");
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
        RunProvenance::current(
            "test-cohort",
            "test-grader",
            "http://inference.test/v1".to_owned(),
            "test-sampling",
            1.0,
            0.95,
        ),
    );
    report.save().unwrap();
    let initial = report.snapshot();
    assert_eq!(initial["stage_timeout_secs"], 1800);
    assert_eq!(initial["provenance"]["cohort"], "test-cohort");
    assert_eq!(
        initial["provenance"]["inference"]["effective_sampling"]["temperature"],
        1.0
    );
    assert_eq!(
        initial["provenance"]["fixture_sha256"]
            .as_object()
            .unwrap()
            .len(),
        9
    );
    assert_eq!(
        initial["provenance"]["grader"]["sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
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
        trial_failure_kind: Some("infrastructure".into()),
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
        trial_failure_kind: None,
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
