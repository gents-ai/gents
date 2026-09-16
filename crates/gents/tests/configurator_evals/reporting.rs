use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{ensure, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::stages::{validate_case_catalog, CaseId, CaseResult, EvaluationFailure};

#[derive(Clone, Copy)]
pub struct EvidenceSource {
    name: &'static str,
    bytes: &'static [u8],
}

impl EvidenceSource {
    pub const fn new(name: &'static str, bytes: &'static [u8]) -> Self {
        Self { name, bytes }
    }
}

/// One isolated suite attempt. Suite modules retain their detailed raw receipts
/// under `artifacts`; this is the common input to aggregate reporting.
#[derive(Debug, Serialize)]
pub struct TrialResult {
    pub case_id: &'static str,
    pub provider: &'static str,
    pub model: String,
    pub trial: usize,
    pub passed: bool,
    pub trial_failure_kind: Option<String>,
    pub terminal_state: Option<String>,
    pub error: Option<String>,
    pub assistant_answer_excerpt: Option<String>,
    pub artifacts: Option<String>,
    pub cases: Vec<CaseResult>,
}

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

fn source_digest(sources: &[EvidenceSource]) -> Result<String> {
    ensure!(!sources.is_empty(), "grader source set must not be empty");
    let mut names = std::collections::BTreeSet::new();
    let mut digest = Sha256::new();
    for source in sources {
        ensure!(
            !source.name.is_empty(),
            "evidence source name must not be empty"
        );
        ensure!(
            names.insert(source.name),
            "duplicate evidence source name: {}",
            source.name
        );
        digest.update(source.name.len().to_le_bytes());
        digest.update(source.name.as_bytes());
        digest.update(source.bytes.len().to_le_bytes());
        digest.update(source.bytes);
    }
    Ok(format!("{:x}", digest.finalize()))
}

impl RunProvenance {
    pub fn current(
        cohort: &'static str,
        grader: &'static str,
        endpoint: String,
        sampling_id: &'static str,
        temperature: f64,
        top_p: f64,
        grader_sources: &[EvidenceSource],
        fixtures: &[EvidenceSource],
    ) -> Result<Self> {
        let mut fixture_names = std::collections::BTreeSet::new();
        let fixture_sha256 = fixtures
            .iter()
            .map(|source| {
                ensure!(
                    fixture_names.insert(source.name),
                    "duplicate fixture name: {}",
                    source.name
                );
                Ok((source.name, sha256(&[source.bytes])))
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            cohort,
            source: SourceRevision {
                commit: std::env::var("GENTS_EVAL_SOURCE_REVISION")
                    .unwrap_or_else(|_| "unknown".to_owned()),
                dirty: std::env::var("GENTS_EVAL_SOURCE_DIRTY")
                    .map_or(true, |value| value != "false"),
            },
            grader: GraderRevision {
                id: grader,
                sha256: source_digest(grader_sources)?,
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
            fixture_sha256,
        })
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
    suite: &'static str,
    case_catalog: &'static [CaseId],
    models: Vec<String>,
    runs: usize,
    provider: &'static str,
    concurrency: usize,
    stage_timeout_secs: u64,
    started_at: String,
    started: Instant,
    provenance: RunProvenance,
    results: Vec<TrialResult>,
}

impl RunReport {
    pub fn new(
        directory: PathBuf,
        suite: &'static str,
        case_catalog: &'static [CaseId],
        models: Vec<String>,
        runs: usize,
        provider: &'static str,
        concurrency: usize,
        stage_timeout_secs: u64,
        provenance: RunProvenance,
    ) -> Result<Self> {
        ensure!(!suite.is_empty(), "eval suite ID must not be empty");
        validate_case_catalog(case_catalog)?;
        Ok(Self {
            directory,
            suite,
            case_catalog,
            models,
            runs,
            provider,
            concurrency,
            stage_timeout_secs,
            started_at: chrono::Utc::now().to_rfc3339(),
            started: Instant::now(),
            provenance,
            results: Vec::new(),
        })
    }

    pub fn failed(&self) -> usize {
        self.results.iter().filter(|result| !result.passed).count()
    }

    pub fn completed(&self) -> usize {
        self.results.len()
    }

    pub fn record(&mut self, result: TrialResult) -> Result<()> {
        ensure!(
            result.case_id == self.suite,
            "trial suite mismatch: expected {}, got {}",
            self.suite,
            result.case_id
        );
        ensure!(
            result.provider == self.provider,
            "trial provider mismatch: expected {}, got {}",
            self.provider,
            result.provider
        );
        ensure!(
            self.models.contains(&result.model),
            "unplanned trial model: {}",
            result.model
        );
        ensure!(
            (1..=self.runs).contains(&result.trial),
            "unplanned trial number: {}",
            result.trial
        );
        ensure!(
            !self
                .results
                .iter()
                .any(|saved| saved.model == result.model && saved.trial == result.trial),
            "duplicate trial result for model {} trial {}",
            result.model,
            result.trial
        );
        if let Some(kind) = result.trial_failure_kind.as_deref() {
            ensure!(
                EvaluationFailure::KINDS.contains(&kind),
                "unknown trial failure kind: {kind}"
            );
        }
        let mut reported_cases = std::collections::BTreeSet::new();
        for case in &result.cases {
            ensure!(
                self.case_catalog
                    .iter()
                    .any(|registered| registered.as_str() == case.case_id),
                "trial contains unregistered case: {}",
                case.case_id
            );
            ensure!(
                reported_cases.insert(case.case_id.as_str()),
                "trial reports case more than once: {}",
                case.case_id
            );
            match case.status.as_str() {
                "passed" => ensure!(
                    case.failure_kind.is_none(),
                    "passed case has a failure kind: {}",
                    case.case_id
                ),
                "failed" => ensure!(
                    case.failure_kind
                        .as_deref()
                        .is_some_and(|kind| EvaluationFailure::KINDS.contains(&kind)),
                    "failed case has an unknown or missing failure kind: {}",
                    case.case_id
                ),
                "skipped" => ensure!(
                    case.failure_kind.as_deref() == Some("prerequisite"),
                    "skipped case must retain prerequisite classification: {}",
                    case.case_id
                ),
                status => anyhow::bail!("unknown case status {status:?}: {}", case.case_id),
            }
        }
        let all_cases_passed = reported_cases.len() == self.case_catalog.len()
            && result.cases.iter().all(|case| case.status == "passed");
        ensure!(
            result.passed == all_cases_passed,
            "trial pass flag disagrees with registered case outcomes"
        );
        self.results.push(result);
        Ok(())
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
            for case_id in self.case_catalog {
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
            "schema_version": 1, "suite": self.suite, "provider": self.provider,
            "outcome_taxonomy": {
                "case_statuses": ["passed", "failed", "skipped"],
                "failure_kinds": EvaluationFailure::KINDS,
                "skip_kinds": ["prerequisite"],
            },
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
    const CASES: &[CaseId] = &[CaseId::new("onboarding"), CaseId::new("builder-readiness")];
    const GRADER: &[EvidenceSource] = &[EvidenceSource::new("grader.rs", b"grader")];
    const FIXTURES: &[EvidenceSource] = &[
        EvidenceSource::new("first.md", b"first"),
        EvidenceSource::new("second.md", b"second"),
    ];
    let mut report = RunReport::new(
        directory.path().into(),
        "test-suite",
        CASES,
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
            GRADER,
            FIXTURES,
        )
        .unwrap(),
    )
    .unwrap();
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
        2
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
        CASES.len()
    );
    assert_eq!(initial["suite"], "test-suite");
    assert!(initial["outcome_taxonomy"]["failure_kinds"]
        .as_array()
        .unwrap()
        .iter()
        .any(|kind| kind == "grader"));
    assert!(initial["outcome_taxonomy"]["failure_kinds"]
        .as_array()
        .unwrap()
        .iter()
        .any(|kind| kind == "model_acceptance"));
    report
        .record(TrialResult {
            case_id: "test-suite",
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
        })
        .unwrap();
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
    report
        .record(TrialResult {
            case_id: "test-suite",
            provider: "d4f",
            model: "model".into(),
            trial: 2,
            passed: false,
            trial_failure_kind: None,
            terminal_state: Some("completed".into()),
            error: None,
            assistant_answer_excerpt: None,
            artifacts: None,
            cases: vec![super::stages::CaseResult {
                case_id: "builder-readiness".into(),
                status: "skipped".into(),
                elapsed_ms: 0,
                error: Some("prerequisite did not pass".into()),
                failure_kind: Some("prerequisite".into()),
            }],
        })
        .unwrap();
    let completed = report.snapshot();
    assert_eq!(completed["status"], "completed");
    assert_eq!(completed["summaries"][0]["counts"]["pass_rate"], 0.0);
    let readiness = &completed["summaries"][0]["cases"][1]["counts"];
    assert_eq!(readiness["skipped"], 1);
    assert_eq!(readiness["unreported"], 1);
    assert!(readiness["pass_rate"].is_null());
}

#[test]
fn report_keeps_model_grader_and_infrastructure_failures_distinct() {
    const CASES: &[CaseId] = &[CaseId::new("acceptance")];
    const SOURCES: &[EvidenceSource] = &[EvidenceSource::new("source", b"source")];
    let provenance = RunProvenance::current(
        "test-cohort",
        "test-grader",
        "http://inference.test/v1".into(),
        "test-sampling",
        1.0,
        0.95,
        SOURCES,
        SOURCES,
    )
    .unwrap();
    let mut report = RunReport::new(
        tempfile::tempdir().unwrap().keep(),
        "test-suite",
        CASES,
        vec!["model".into()],
        3,
        "d4f",
        1,
        1800,
        provenance,
    )
    .unwrap();
    for (trial, kind) in [
        (1, "model_acceptance"),
        (2, "grader"),
        (3, "infrastructure"),
    ] {
        report
            .record(TrialResult {
                case_id: "test-suite",
                provider: "d4f",
                model: "model".into(),
                trial,
                passed: false,
                trial_failure_kind: Some(kind.into()),
                terminal_state: (kind != "infrastructure").then(|| "completed".into()),
                error: Some(format!("{kind} failed")),
                assistant_answer_excerpt: None,
                artifacts: None,
                cases: vec![CaseResult {
                    case_id: "acceptance".into(),
                    status: "failed".into(),
                    elapsed_ms: 1,
                    error: Some(format!("{kind} failed")),
                    failure_kind: Some(kind.into()),
                }],
            })
            .unwrap();
    }
    let summary = &report.snapshot()["summaries"][0];
    for kind in ["model_acceptance", "grader", "infrastructure"] {
        assert_eq!(summary["trial_failure_kinds"][kind], 1);
        assert_eq!(summary["cases"][0]["failure_kinds"][kind], 1);
    }
}
