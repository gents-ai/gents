//! Shared native execution and evidence collection for progressive consumer evals.
//! Assertions remain outside the evaluated behaviors and their writable roots.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::access::RuntimeAccess;
use anyhow::{ensure, Context, Result};
use gents::defra_node::EmbeddedNode;
use gents::eval::runner::embedded::{
    await_terminal_with, classify_request_outcome, collect_request_evidence, evidence_query,
    inference_sample_query, request_evidence_from_query_data, ObservationHook, RequestEvidence,
};
use gents::graphql::escape_graphql_string;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::Serialize;
use serde_json::Value;

pub const INPUT_SCHEMA: &str = "type GentsEvalStageInput { stage: String prompt: String }";

pub fn stage_timeout() -> Result<Duration> {
    parse_stage_timeout(
        std::env::var("GENTS_LIVE_CONFIG_STAGE_TIMEOUT_SECS")
            .ok()
            .as_deref(),
    )
}

fn parse_stage_timeout(value: Option<&str>) -> Result<Duration> {
    let seconds = value
        .unwrap_or("1800")
        .parse::<u64>()
        .context("GENTS_LIVE_CONFIG_STAGE_TIMEOUT_SECS must be an integer")?;
    ensure!(
        (1..=14400).contains(&seconds),
        "GENTS_LIVE_CONFIG_STAGE_TIMEOUT_SECS must be between 1 and 14400"
    );
    Ok(Duration::from_secs(seconds))
}

#[test]
fn stage_budget_defaults_to_thirty_minutes_and_validates_overrides() {
    assert_eq!(
        parse_stage_timeout(None).unwrap(),
        Duration::from_secs(1800)
    );
    assert_eq!(
        parse_stage_timeout(Some("3600")).unwrap(),
        Duration::from_secs(3600)
    );
    for invalid in ["0", "-1", "14401", "forever"] {
        assert!(parse_stage_timeout(Some(invalid)).is_err());
    }
}

/// Stable receipt identity supplied by the suite that owns the scenario.
///
/// Keeping this as a value rather than a closed enum lets adjacent suites use
/// the shared receipt and report machinery without adding their case names to
/// this module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaseId(&'static str);

#[allow(non_upper_case_globals)]
impl CaseId {
    pub const Onboarding: Self = Self::new("onboarding");
    pub const BuilderReadiness: Self = Self::new("builder-readiness");
    pub const SkillWorkflow: Self = Self::new("skill-workflow");
    pub const DocumentAutomation: Self = Self::new("document-automation");

    pub const fn new(id: &'static str) -> Self {
        Self(id)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

pub const PROGRESSIVE_CASES: &[CaseId] = &[
    CaseId::Onboarding,
    CaseId::BuilderReadiness,
    CaseId::SkillWorkflow,
    CaseId::DocumentAutomation,
];

pub fn validate_case_catalog(cases: &[CaseId]) -> Result<()> {
    ensure!(!cases.is_empty(), "eval case catalog must not be empty");
    let mut unique = std::collections::BTreeSet::new();
    for case in cases {
        let id = case.as_str();
        ensure!(
            !id.is_empty()
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
            "eval case ID must contain only lowercase ASCII letters, digits, and hyphens: {id:?}"
        );
        ensure!(unique.insert(id), "duplicate eval case ID: {id}");
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum EvaluationFailure {
    #[error("evaluation deadline exceeded for {0}")]
    Deadline(String),
    #[error("provider failed: {0}")]
    Provider(String),
    #[error("tool execution failed: {0}")]
    Tool(String),
    #[error("runtime failed: {0}")]
    Runtime(String),
    #[error("evaluation inconclusive: {0}")]
    Inconclusive(String),
    #[error("evaluation infrastructure failed: {0}")]
    Infrastructure(String),
    #[error("grader failed: {0}")]
    Grader(String),
    #[error("model output failed acceptance: {0}")]
    ModelAcceptance(String),
    #[error("evaluation failure has unknown cause: {0}")]
    Unknown(String),
}

impl EvaluationFailure {
    pub const KINDS: &[&str] = &[
        "deadline",
        "provider",
        "tool",
        "runtime",
        "inconclusive",
        "infrastructure",
        "grader",
        "model_acceptance",
        "unknown",
    ];

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Deadline(_) => "deadline",
            Self::Provider(_) => "provider",
            Self::Tool(_) => "tool",
            Self::Runtime(_) => "runtime",
            Self::Inconclusive(_) => "inconclusive",
            Self::Infrastructure(_) => "infrastructure",
            Self::Grader(_) => "grader",
            Self::ModelAcceptance(_) => "model_acceptance",
            Self::Unknown(_) => "unknown",
        }
    }
}

pub fn infrastructure(error: anyhow::Error) -> anyhow::Error {
    if error.downcast_ref::<EvaluationFailure>().is_some() {
        error
    } else {
        EvaluationFailure::Infrastructure(format!("{error:#}")).into()
    }
}

pub fn model_acceptance(error: anyhow::Error) -> anyhow::Error {
    if error.downcast_ref::<EvaluationFailure>().is_some() {
        error
    } else {
        EvaluationFailure::ModelAcceptance(format!("{error:#}")).into()
    }
}

pub fn grader(error: anyhow::Error) -> anyhow::Error {
    if error.downcast_ref::<EvaluationFailure>().is_some() {
        error
    } else {
        EvaluationFailure::Grader(format!("{error:#}")).into()
    }
}

pub async fn acceptance<T>(check: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    check.await.map_err(model_acceptance)
}

/// Evidence failures invalidate a pass, but never replace an existing verdict.
pub fn retain_outcome<T>(result: Result<T>, retention: Result<()>) -> Result<T> {
    match (result, retention) {
        (result, Ok(())) => result,
        (Ok(_), Err(error)) => Err(infrastructure(error)),
        (Err(error), Err(retention)) => {
            Err(error.context(format!("evidence retention also failed: {retention:#}")))
        }
    }
}

#[test]
fn evidence_failure_preserves_primary_verdict_and_invalidates_pass() {
    let error = retain_outcome::<()>(
        Err(EvaluationFailure::Deadline("stage".into()).into()),
        Err(anyhow::anyhow!("disk full")),
    )
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<EvaluationFailure>().unwrap().kind(),
        "deadline"
    );
    assert!(format!("{error:#}").contains("disk full"));
    let error = retain_outcome(Ok(()), Err(anyhow::anyhow!("disk full"))).unwrap_err();
    assert_eq!(
        error.downcast_ref::<EvaluationFailure>().unwrap().kind(),
        "infrastructure"
    );
}

#[derive(Debug, Serialize)]
pub struct StageResult {
    pub stage: String,
    pub request_id: String,
    pub session_id: Option<String>,
    pub terminal_state: String,
    pub elapsed_ms: u128,
    pub answer: String,
    pub observation_timed_out: bool,
    pub failure_kind: Option<String>,
    pub observed_outcome: Value,
}

impl StageResult {
    pub fn ensure_completed(&self) -> Result<()> {
        if self.observation_timed_out {
            return Err(EvaluationFailure::Deadline(self.stage.clone()).into());
        }
        if self.terminal_state != "completed" {
            let detail = format!(
                "{}: {}; observed={}",
                self.stage, self.terminal_state, self.observed_outcome
            );
            let failure = match self.failure_kind.as_deref() {
                Some("provider") => EvaluationFailure::Provider(detail),
                Some("tool") => EvaluationFailure::Tool(detail),
                Some("runtime") => EvaluationFailure::Runtime(detail),
                _ => EvaluationFailure::Unknown(detail),
            };
            return Err(failure.into());
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, serde::Deserialize)]
pub struct CaseResult {
    pub case_id: String,
    pub status: String,
    pub elapsed_ms: u128,
    pub error: Option<String>,
    pub failure_kind: Option<String>,
}

#[tokio::test]
async fn every_catalog_case_is_reported_and_receipt_identity_is_checked() {
    let evidence = tempfile::tempdir().unwrap();
    let mut ids = std::collections::BTreeSet::new();
    for &case in PROGRESSIVE_CASES {
        assert!(ids.insert(case.as_str()));
        checked(case, evidence.path(), async { Ok(()) })
            .await
            .unwrap();
    }
    let results = case_results(PROGRESSIVE_CASES, evidence.path()).unwrap();
    assert_eq!(results.len(), ids.len());
    assert!(results
        .iter()
        .all(|result| result.status == "passed" && ids.contains(result.case_id.as_str())));
    let mut incorrect = results.into_iter().next().unwrap();
    incorrect.case_id = "not-the-file-identity".into();
    std::fs::write(
        evidence.path().join("onboarding-acceptance.json"),
        serde_json::to_vec(&incorrect).unwrap(),
    )
    .unwrap();
    assert!(case_results(PROGRESSIVE_CASES, evidence.path()).is_err());
}

#[test]
fn suite_owned_case_catalogs_are_validated_without_reporting_missing_cases_as_passed() {
    const CASES: &[CaseId] = &[
        CaseId::new("onboarding-fresh-setup"),
        CaseId::new("onboarding-after-restart"),
    ];
    validate_case_catalog(CASES).unwrap();
    assert!(validate_case_catalog(&[CaseId::new("duplicate"), CaseId::new("duplicate")]).is_err());
    assert!(validate_case_catalog(&[CaseId::new("../receipt")]).is_err());

    let evidence = tempfile::tempdir().unwrap();
    let results = case_results(CASES, evidence.path()).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|result| {
        result.status == "skipped" && result.failure_kind.as_deref() == Some("prerequisite")
    }));
}

#[tokio::test]
async fn case_receipts_are_immutable_after_first_publication() {
    let evidence = tempfile::tempdir().unwrap();
    let case = CaseId::new("suite-owned-case");
    checked(case, evidence.path(), async { Ok(()) })
        .await
        .unwrap();
    let replacement = checked::<()>(case, evidence.path(), async {
        Err(model_acceptance(anyhow::anyhow!("replacement verdict")))
    })
    .await
    .unwrap_err();
    assert!(format!("{replacement:#}").contains("replacement verdict"));
    assert!(format!("{replacement:#}").contains("evidence retention also failed"));
    let retained = case_results(&[case], evidence.path()).unwrap();
    assert_eq!(retained[0].status, "passed");
    assert!(retained[0].failure_kind.is_none());
}

#[tokio::test]
async fn case_reporting_preserves_failure_classification_and_skipped_prerequisites() {
    let inconclusive_case = CaseId::new("inconclusive-fixture");
    let infrastructure_case = CaseId::new("infrastructure-fixture");
    let evidence = tempfile::tempdir().unwrap();
    let deadline: Result<()> = Err(EvaluationFailure::Deadline("onboarding".into()).into());
    assert!(
        checked(CaseId::Onboarding, evidence.path(), async { deadline })
            .await
            .is_err()
    );
    checked(CaseId::BuilderReadiness, evidence.path(), async { Ok(()) })
        .await
        .unwrap();
    let inconclusive: Result<()> =
        Err(EvaluationFailure::Inconclusive("animated scene".into()).into());
    assert!(
        checked(inconclusive_case, evidence.path(), async { inconclusive })
            .await
            .is_err()
    );
    let infrastructure: Result<()> =
        Err(EvaluationFailure::Infrastructure("disk unavailable".into()).into());
    assert!(checked(infrastructure_case, evidence.path(), async {
        infrastructure
    })
    .await
    .is_err());
    let cases = [
        CaseId::Onboarding,
        CaseId::BuilderReadiness,
        CaseId::SkillWorkflow,
        inconclusive_case,
        infrastructure_case,
    ];
    let results = case_results(&cases, evidence.path()).unwrap();
    assert_eq!(results.len(), 5);
    assert_eq!(results[0].failure_kind.as_deref(), Some("deadline"));
    assert_eq!(results[1].status, "passed");
    assert_eq!(results[2].status, "skipped");
    assert_eq!(results[3].failure_kind.as_deref(), Some("inconclusive"));
    assert_eq!(results[4].failure_kind.as_deref(), Some("infrastructure"));
}

#[test]
fn untyped_failures_remain_unknown_until_the_check_owner_classifies_them() {
    let raw = anyhow::anyhow!("ambiguous failure");
    let classified = raw
        .downcast_ref::<EvaluationFailure>()
        .map_or("unknown", EvaluationFailure::kind);
    assert_eq!(classified, "unknown");
    let accepted = model_acceptance(anyhow::anyhow!("missing requested file"));
    assert_eq!(
        accepted.downcast_ref::<EvaluationFailure>().unwrap().kind(),
        "model_acceptance"
    );
    let grader = grader(anyhow::anyhow!("invalid checker receipt"));
    assert_eq!(
        grader.downcast_ref::<EvaluationFailure>().unwrap().kind(),
        "grader"
    );
}

#[tokio::test]
async fn acceptance_preserves_host_observation_failures_as_infrastructure() {
    let error = acceptance(async {
        let observation: Result<()> = Err(anyhow::anyhow!("host controller unavailable"));
        observation.map_err(infrastructure)?;
        Ok(())
    })
    .await
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<EvaluationFailure>().unwrap().kind(),
        "infrastructure"
    );

    let error = acceptance(async {
        anyhow::ensure!(false, "host observed but repair did not recover service");
        Ok(())
    })
    .await
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<EvaluationFailure>().unwrap().kind(),
        "model_acceptance"
    );
}

/// Independent acceptance is distinct from a model request terminalizing.
/// Preserve the check result even when it fails and prevents dependent stages.
pub async fn checked<T>(
    case: CaseId,
    evidence: &Path,
    check: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    let case_id = case.as_str();
    let started = Instant::now();
    let result = check.await;
    let receipt = CaseResult {
        case_id: case_id.to_owned(),
        status: if result.is_ok() { "passed" } else { "failed" }.to_owned(),
        elapsed_ms: started.elapsed().as_millis(),
        error: result.as_ref().err().map(|error| format!("{error:#}")),
        failure_kind: result.as_ref().err().map(|error| {
            error
                .downcast_ref::<EvaluationFailure>()
                .map_or("unknown", EvaluationFailure::kind)
                .to_owned()
        }),
    };
    tracing::info!(target: "gents::configurator_eval", result = %serde_json::to_string(&receipt)?, "eval case acceptance");
    let retention = (|| {
        std::fs::create_dir_all(evidence)?;
        super::reporting::write_json_new(
            &evidence.join(format!("{case_id}-acceptance.json")),
            &receipt,
        )?;
        Ok(())
    })();
    retain_outcome(result, retention)
}

pub fn case_results(cases: &[CaseId], evidence: &Path) -> Result<Vec<CaseResult>> {
    validate_case_catalog(cases)?;
    cases
        .iter()
        .map(|case| case.as_str())
        .map(|case_id| {
            let path = evidence.join(format!("{case_id}-acceptance.json"));
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let result: CaseResult = serde_json::from_slice(&bytes)?;
                    ensure!(
                        result.case_id == case_id,
                        "case receipt identity mismatch: {path:?}"
                    );
                    Ok(result)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(CaseResult {
                    case_id: case_id.to_owned(),
                    status: "skipped".into(),
                    elapsed_ms: 0,
                    error: Some("prerequisite did not pass".into()),
                    failure_kind: Some("prerequisite".into()),
                }),
                Err(error) => Err(error.into()),
            }
        })
        .collect()
}

/// Observational fence only: configuration resolution and subscription setup
/// remain owned by the runtime. Call after writes, with no concurrent authoring.
type SubscriptionObservation = (u64, String, Result<(), String>);

pub struct ActivationObserver {
    ready: tokio::sync::watch::Sender<Option<SubscriptionObservation>>,
}

impl Default for ActivationObserver {
    fn default() -> Self {
        Self {
            ready: tokio::sync::watch::channel(None).0,
        }
    }
}

impl gents::RuntimeSnapshotObserver for ActivationObserver {
    fn on_generation_published(&self, _generation: u64, _fingerprint: &str, _behaviors: &[String]) {
    }

    fn on_event_sources_reconciled(
        &self,
        generation: u64,
        fingerprint: &str,
        result: Result<(), &str>,
    ) {
        self.ready.send_replace(Some((
            generation,
            fingerprint.to_owned(),
            result.map_err(str::to_owned),
        )));
    }
}

pub struct ActivationFence {
    runtime: gents::Gents,
    observer: std::sync::Arc<ActivationObserver>,
    node: std::sync::Arc<EmbeddedNode>,
}

impl ActivationFence {
    pub fn new(
        runtime: gents::Gents,
        observer: std::sync::Arc<ActivationObserver>,
        node: std::sync::Arc<EmbeddedNode>,
    ) -> Self {
        Self {
            runtime,
            observer,
            node,
        }
    }

    pub async fn wait(&self) -> Result<()> {
        let wait = wait_for_exact_configuration(self.observer.ready.subscribe(), || {
            self.runtime.document_runtime_configuration_fingerprint()
        });
        match tokio::time::timeout(Duration::from_secs(120), wait).await {
            Ok(result) => result.map_err(infrastructure),
            Err(error) => {
                // Runtime status is diagnostic evidence, never the activation predicate.
                let owner = escape_graphql_string(self.runtime.agent_did());
                let query = format!(
                    r#"{{ AgentRuntime(filter: {{agent_did: {{_eq: "{owner}"}}}}) {{reconcile_phase last_reconcile_result last_reconcile_error}} }}"#
                );
                let diagnostics =
                    tokio::time::timeout(Duration::from_secs(5), self.node.execute(&query)).await;
                Err(infrastructure(anyhow::anyhow!("exact configuration did not reach event-source subscription readiness: {error}; runtime diagnostics: {diagnostics:?}; last subscription observation: {:?}", self.observer.ready.borrow().clone())))
            }
        }
    }
}

async fn wait_for_exact_configuration<F, Fut>(
    mut ready: tokio::sync::watch::Receiver<Option<SubscriptionObservation>>,
    mut resolve: F,
) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    loop {
        // Mark before the asynchronous resolve so an acknowledgement arriving
        // during it remains available to changed().
        drop(ready.borrow_and_update());
        let expected = resolve().await?;
        if let Some((_, actual, result)) = ready.borrow().as_ref() {
            if actual == &expected {
                return result
                    .clone()
                    .map_err(|error| infrastructure(anyhow::anyhow!(error)));
            }
        }
        ready
            .changed()
            .await
            .context("event-source readiness observer closed")?;
    }
}

#[tokio::test]
async fn configuration_fence_reports_matching_seed_failure_without_waiting() {
    let (_tx, rx) = tokio::sync::watch::channel(Some((
        2,
        "current".to_owned(),
        Err("seed query failed".to_owned()),
    )));
    let result = wait_for_exact_configuration(rx, || async { Ok("current".to_owned()) }).await;
    let error = result.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<EvaluationFailure>(),
        Some(EvaluationFailure::Infrastructure(_))
    ));
    assert!(error.to_string().contains("seed query failed"));
}

#[tokio::test]
async fn configuration_fence_does_not_lose_acknowledgement_during_resolution() {
    let (tx, rx) = tokio::sync::watch::channel(Some((1, "old".to_owned(), Ok(()))));
    let mut resolves = 0;
    let wait = wait_for_exact_configuration(rx, || {
        resolves += 1;
        let first = resolves == 1;
        let tx = tx.clone();
        async move {
            if first {
                tx.send_replace(Some((2, "new".to_owned(), Ok(()))));
                tokio::task::yield_now().await;
                Ok("old".to_owned())
            } else {
                Ok("new".to_owned())
            }
        }
    });
    tokio::time::timeout(Duration::from_secs(1), wait)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolves, 2);
}

/// The runner installs reusable task/trigger definitions, then invokes only by
/// writing an input document. Native materialization owns request admission,
/// session creation, templating, deduplication, and request identity.
async fn submit_stage(
    activation: &ActivationFence,
    node: &EmbeddedNode,
    owner: &str,
    behavior: &str,
    stage: &str,
    prompt: &str,
) -> Result<String> {
    prepare_stage(activation, node, owner, behavior, stage).await?;
    let stage_escaped = escape_graphql_string(stage);
    let trigger = format!("eval-trigger-{stage}");
    let prompt = escape_graphql_string(prompt);
    let submitted = node.execute(&format!(r#"mutation {{ create_GentsEvalStageInput(input: {{stage: "{stage_escaped}", prompt: "{prompt}"}}) {{_docID}} }}"#)).await;
    ensure!(
        !submitted.has_errors(),
        "stage input write failed: {:?}",
        submitted.errors
    );
    observe_stage_request(node, &trigger).await
}

pub(super) async fn prepare_stage(
    activation: &ActivationFence,
    node: &EmbeddedNode,
    owner: &str,
    behavior: &str,
    stage: &str,
) -> Result<()> {
    use gents::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    use gents::Collection;
    let task = format!("eval-task-{stage}");
    let source = format!("eval-source-{stage}");
    let trigger = format!("eval-trigger-{stage}");
    let stage_escaped = escape_graphql_string(stage);
    let documents = [
        (
            Collection::Task,
            serde_json::json!({"agent_did":owner,"task_id":task,"behavior_id":behavior,"prompt_template":"{{doc.prompt}}","enabled":true}),
        ),
        (
            Collection::EventSource,
            serde_json::json!({"agent_did":owner,"event_source_id":source,"source_collection":"GentsEvalStageInput","event_kind":"created","filter":format!("{{stage: {{_eq: \"{stage_escaped}\"}}}}"),"correlation_field":"stage"}),
        ),
        (
            Collection::Trigger,
            serde_json::json!({"agent_did":owner,"trigger_id":trigger,"task_id":task,"source":{"kind":"event","event_source_id":source},"enabled":true}),
        ),
    ];
    let plan = DesiredStateApplyPlan::new(
        documents
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )?;
    gents::ConfigAccess::transact_local(node, None, "eval.stage_definitions", |txn| {
        let plan = &plan;
        Box::pin(async move {
            gents::config_client::apply_desired_state_plan(txn, plan)
                .await
                .map(|_| ())
        })
    })
    .await?;
    activation.wait().await
}

async fn observe_stage_request(node: &EmbeddedNode, trigger: &str) -> Result<String> {
    let trigger = escape_graphql_string(trigger);
    let started = Instant::now();
    loop {
        let response = node.execute(&format!(r#"{{ AgentRequest(filter: {{caused_by_trigger_id: {{_eq: "{trigger}"}}}}) {{request_id}} }}"#)).await;
        ensure!(
            !response.has_errors(),
            "stage request observation failed: {:?}",
            response.errors
        );
        let rows = response
            .data
            .as_ref()
            .and_then(|v| v["AgentRequest"].as_array())
            .context("stage request rows missing")?;
        ensure!(
            rows.len() <= 1,
            "stage input materialized duplicate requests"
        );
        if let Some(id) = rows.first().and_then(|v| v["request_id"].as_str()) {
            return Ok(id.to_owned());
        }
        ensure!(
            started.elapsed() < Duration::from_secs(120),
            "native task did not materialize for trigger {trigger}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[test]
fn native_stage_waits_for_exact_subscription_configuration_without_live_inference() {
    let test_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(8 * 1024 * 1024)
        .enable_all()
        .build()
        .expect("configurator activation test runtime");
    test_runtime.block_on(native_stage_waits_for_exact_subscription_configuration());
}

async fn native_stage_waits_for_exact_subscription_configuration() {
    use gents::AgentIdentity;
    let db = crate::support::test_db("eval-activation-fence").await;
    let identity = std::sync::Arc::new(crate::support::fixtures::test_identity(
        "eval-activation-fence",
    ));
    crate::support::fixtures::bind_behavior_backend(
        db.node.as_ref(),
        identity.did(),
        "eval-observer",
        "unused-local-backend",
        "http://127.0.0.1:9/v1",
        "unused",
    )
    .await;
    let access = gents::ConfigAccess::Local(db.node.clone());
    let schema = gents::config_client::preview_schema_install(&access, INPUT_SCHEMA)
        .await
        .unwrap();
    gents::config_client::apply_schema_install(&access, INPUT_SCHEMA, &schema.artifact_digest)
        .await
        .unwrap();
    let observer = std::sync::Arc::new(ActivationObserver::default());
    let (agent, runtime) = crate::support::live_inference::boot_live_agent_with_options(
        &db,
        identity,
        gents::DocumentRuntimeOptions {
            runtime_snapshot_observer: Some(observer.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let owner = runtime.agent_did().to_owned();
    let fence = ActivationFence::new(runtime, observer, db.node.clone());
    let result = async {
        fence.wait().await?;
        let before = fence.observer.ready.borrow().clone();
        let id = submit_stage(
            &fence,
            db.node.as_ref(),
            &owner,
            "eval-observer",
            "activation-regression",
            "Materialization only; no provider success required.",
        )
        .await?;
        ensure!(!id.is_empty(), "native request did not materialize");
        let after = fence.observer.ready.borrow().clone();
        ensure!(
            before != after,
            "old subscription acknowledgement satisfied a changed configuration"
        );
        let expected = fence
            .runtime
            .document_runtime_configuration_fingerprint()
            .await?;
        ensure!(
            after.is_some_and(|(_, fingerprint, result)| fingerprint == expected && result.is_ok()),
            "wrong activated configuration"
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;
    agent.shutdown().await;
    result.unwrap();
}

/// Each stage uses a fresh session; configuration stages must await activation.
pub async fn execute(
    activation: &ActivationFence,
    node: &std::sync::Arc<EmbeddedNode>,
    owner: &str,
    behavior: &str,
    stage: &str,
    prompt: &str,
    evidence: &Path,
) -> Result<StageResult> {
    execute_inner(activation, node, owner, behavior, stage, prompt, evidence)
        .await
        .map_err(infrastructure)
}

pub(super) fn write_stage_progress(
    evidence: &Path,
    stage: &str,
    phase: &str,
    request_id: Option<&str>,
    session_id: Option<&str>,
    lifecycle_state: Option<&str>,
    started_at: &str,
) -> Result<()> {
    super::reporting::write_json(
        &evidence.join(format!("{stage}-progress.json")),
        &serde_json::json!({
            "stage": stage,
            "phase": phase,
            "request_id": request_id,
            "session_id": session_id,
            "lifecycle_state": lifecycle_state,
            "started_at": started_at,
            "updated_at": chrono::Utc::now().to_rfc3339(),
        }),
    )
}

pub(super) async fn observed_request_outcome<'a>(
    access: impl Into<RuntimeAccess<'a>>,
    request_id: &str,
) -> Result<Value> {
    let escaped = escape_graphql_string(request_id);
    let access = access.into();
    let mut outcome = access.query(&format!(
        r#"{{
            AgentRequest(filter: {{request_id: {{_eq: "{escaped}"}}}}) {{request_id lifecycle_state failure_reason session_id}}
            InferenceCall(filter: {{request_id: {{_eq: "{escaped}"}}}}) {{call_seq call_state failure_reason queued_at started_at ended_at}}
        }}"#
    )).await?;
    let calls = access
        .timeline(request_id)
        .await?
        .tool_calls
        .into_iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()?;
    outcome
        .as_object_mut()
        .context("request outcome response was not an object")?
        .insert("AgentToolCall".to_owned(), Value::Array(calls));
    Ok(outcome)
}

async fn execute_inner(
    activation: &ActivationFence,
    node: &std::sync::Arc<EmbeddedNode>,
    owner: &str,
    behavior: &str,
    stage: &str,
    prompt: &str,
    evidence: &Path,
) -> Result<StageResult> {
    let started = Instant::now();
    let started_at = chrono::Utc::now().to_rfc3339();
    std::fs::create_dir_all(evidence)?;
    std::fs::write(
        evidence.join(format!("{stage}-input.json")),
        serde_json::to_vec_pretty(&serde_json::json!({
            "stage":stage,"owner":owner,"behavior_id":behavior,"prompt":prompt,
            "transport":"GentsEvalStageInput -> EventSource -> Trigger -> Task -> AgentRequest",
        }))?,
    )?;
    write_stage_progress(evidence, stage, "submitting", None, None, None, &started_at)?;
    let request_id = submit_stage(activation, node, owner, behavior, stage, prompt).await?;
    observe_request(
        node.into(),
        request_id,
        stage,
        evidence,
        started,
        started_at,
    )
    .await
}

pub(super) async fn observe_request(
    access: RuntimeAccess<'_>,
    request_id: String,
    stage: &str,
    evidence: &Path,
    started: Instant,
    started_at: String,
) -> Result<StageResult> {
    match access {
        RuntimeAccess::Embedded(node) => {
            observe_embedded(node, request_id, stage, evidence, started, started_at).await
        }
        access @ RuntimeAccess::ControlPlane(_) => {
            observe_control_plane(access, request_id, stage, evidence, started, started_at).await
        }
    }
}

struct EmbeddedObservation<'a> {
    node: &'a std::sync::Arc<EmbeddedNode>,
    evidence: PathBuf,
    stage: String,
    request_id: String,
    started_at: String,
    next_sample: Mutex<Instant>,
    last_state: Mutex<String>,
    last_session: Mutex<Option<String>>,
    error: Mutex<Option<anyhow::Error>>,
}

#[async_trait::async_trait]
impl ObservationHook for EmbeddedObservation<'_> {
    async fn on_state(&self, state: &str, session_id: Option<&str>) {
        {
            let mut last_state = self
                .last_state
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            *last_state = state.to_owned();
            let mut last_session = self
                .last_session
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            *last_session = session_id.map(str::to_owned);
        }
        self.note(write_stage_progress(
            &self.evidence,
            &self.stage,
            "observing",
            Some(&self.request_id),
            session_id,
            Some(state),
            &self.started_at,
        ));
    }

    async fn on_interrupt_requested(&self) {
        let state = self
            .last_state
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone();
        let session = self
            .last_session
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone();
        self.note(write_stage_progress(
            &self.evidence,
            &self.stage,
            "interrupt_requested",
            Some(&self.request_id),
            session.as_deref(),
            Some(&state),
            &self.started_at,
        ));
    }

    async fn on_tick(&self, _elapsed: Duration) {
        let due = {
            let next = self
                .next_sample
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            Instant::now() >= *next
        };
        if !due {
            return;
        }
        // Display sampling must not fail the evaluated request or stall its deadline.
        let _ = tokio::time::timeout(
            Duration::from_millis(500),
            retain_inference_evidence(self.node, &self.request_id, &self.stage, &self.evidence),
        )
        .await;
        let mut next = self
            .next_sample
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        *next = Instant::now() + Duration::from_secs(2);
    }
}

impl EmbeddedObservation<'_> {
    fn note(&self, result: Result<()>) {
        if let Err(error) = result {
            let mut slot = self.error.lock().unwrap_or_else(|err| err.into_inner());
            if slot.is_none() {
                *slot = Some(error);
            }
        }
    }
}

async fn observe_embedded(
    node: &std::sync::Arc<EmbeddedNode>,
    request_id: String,
    stage: &str,
    evidence: &Path,
    started: Instant,
    started_at: String,
) -> Result<StageResult> {
    let timeout = stage_timeout()?.saturating_sub(started.elapsed());
    write_stage_progress(
        evidence,
        stage,
        "observing",
        Some(&request_id),
        None,
        Some("materialized"),
        &started_at,
    )?;
    let hook = EmbeddedObservation {
        node,
        evidence: evidence.to_path_buf(),
        stage: stage.to_owned(),
        request_id: request_id.clone(),
        started_at: started_at.clone(),
        next_sample: Mutex::new(Instant::now()),
        last_state: Mutex::new(String::new()),
        last_session: Mutex::new(None),
        error: Mutex::new(None),
    };
    let observed = await_terminal_with(
        node,
        &request_id,
        timeout,
        Duration::from_secs(30),
        Duration::from_millis(250),
        &hook,
    )
    .await?;
    if let Some(error) = hook
        .error
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .take()
    {
        return Err(error);
    }
    let collected = collect_request_evidence(node, &request_id).await?;
    let access = RuntimeAccess::Embedded(node);
    let observed_outcome = observed_request_outcome(access, &request_id)
        .await
        .map_err(infrastructure)?;
    let failure_kind = classify_request_outcome(
        observed.terminal_state,
        observed.interrupted_on_deadline,
        &collected,
    )
    .map(str::to_owned);
    let answer = access.terminal_answer(&request_id).await?;
    finish_observation(
        access,
        request_id,
        stage,
        evidence,
        started,
        &started_at,
        observed.terminal_state.as_str().to_owned(),
        observed.session_id,
        observed.interrupted_on_deadline,
        failure_kind,
        observed_outcome,
        answer,
    )
    .await
}

async fn observe_control_plane(
    access: RuntimeAccess<'_>,
    request_id: String,
    stage: &str,
    evidence: &Path,
    started: Instant,
    started_at: String,
) -> Result<StageResult> {
    let timeout = stage_timeout()?;
    write_stage_progress(
        evidence,
        stage,
        "observing",
        Some(&request_id),
        None,
        Some("materialized"),
        &started_at,
    )?;
    let escaped = escape_graphql_string(&request_id);
    let query = format!(
        r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{escaped}"}}}}) {{lifecycle_state session_id}} }}"#
    );
    let mut observation_timed_out = false;
    let mut next_usage_snapshot = Instant::now();
    let mut last_progress = None;
    let (terminal_state, session_id) = loop {
        let response = access.query(&query).await?;
        let state = response["AgentRequest"][0]["lifecycle_state"]
            .as_str()
            .unwrap_or("missing");
        let session_id = response["AgentRequest"][0]["session_id"]
            .as_str()
            .map(str::to_owned);
        let progress = (state.to_owned(), session_id.clone());
        if last_progress.as_ref() != Some(&progress) {
            write_stage_progress(
                evidence,
                stage,
                "observing",
                Some(&request_id),
                session_id.as_deref(),
                Some(state),
                &started_at,
            )?;
            last_progress = Some(progress);
        }
        if gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal_str(Some(state)) {
            break (state.to_owned(), session_id);
        }
        if Instant::now() >= next_usage_snapshot {
            // Display sampling must not fail the evaluated request or stall its deadline.
            let _ = tokio::time::timeout(
                Duration::from_millis(500),
                retain_inference_evidence(access, &request_id, stage, evidence),
            )
            .await;
            next_usage_snapshot = Instant::now() + Duration::from_secs(2);
        }
        if started.elapsed() > timeout {
            if !observation_timed_out {
                observation_timed_out = true;
                write_stage_progress(
                    evidence,
                    stage,
                    "interrupt_requested",
                    Some(&request_id),
                    session_id.as_deref(),
                    Some(state),
                    &started_at,
                )?;
                access
                    .interrupt(&request_id)
                    .await
                    .context("interrupt stage after evaluation deadline")?;
            }
            if started.elapsed() > timeout + Duration::from_secs(30) {
                break (format!("nonterminal_after_interrupt:{state}"), session_id);
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let observed_outcome = observed_request_outcome(access, &request_id)
        .await
        .map_err(infrastructure)?;
    let parsed_state =
        RequestLifecycleState::parse(&terminal_state).unwrap_or(RequestLifecycleState::Interrupted);
    let failure_kind = classify_request_outcome(
        parsed_state,
        observation_timed_out,
        &request_evidence_from_query_data(&observed_outcome),
    )
    .map(str::to_owned);
    let answer = access.terminal_answer(&request_id).await?;
    finish_observation(
        access,
        request_id,
        stage,
        evidence,
        started,
        &started_at,
        terminal_state,
        session_id,
        observation_timed_out,
        failure_kind,
        observed_outcome,
        answer,
    )
    .await
}

async fn finish_observation(
    access: RuntimeAccess<'_>,
    request_id: String,
    stage: &str,
    evidence: &Path,
    started: Instant,
    started_at: &str,
    terminal_state: String,
    session_id: Option<String>,
    observation_timed_out: bool,
    failure_kind: Option<String>,
    observed_outcome: Value,
    answer: String,
) -> Result<StageResult> {
    super::reporting::write_json(
        &evidence.join(format!("{stage}-outcome.json")),
        &observed_outcome,
    )?;
    write_stage_progress(
        evidence,
        stage,
        "terminal",
        Some(&request_id),
        session_id.as_deref(),
        Some(&terminal_state),
        started_at,
    )?;
    let result = StageResult {
        stage: stage.to_owned(),
        request_id,
        session_id,
        terminal_state,
        elapsed_ms: started.elapsed().as_millis(),
        answer,
        observation_timed_out,
        failure_kind,
        observed_outcome,
    };
    let retention = async {
        std::fs::create_dir_all(evidence).context("create evaluator evidence directory")?;
        std::fs::write(
            evidence.join(format!("{stage}.json")),
            serde_json::to_vec_pretty(&result)?,
        )?;
        retain_request_evidence(access, &result.request_id, stage, evidence).await
    }
    .await;
    if let Err(error) = retention {
        retain_outcome(result.ensure_completed(), Err(error))?;
    }
    Ok(result)
}

/// Also used for requests created by model-authored automation, which the
/// runner observes but does not submit or repair.
pub async fn retain_request_evidence<'a>(
    access: impl Into<RuntimeAccess<'a>>,
    request_id: &str,
    stage: &str,
    evidence: &Path,
) -> Result<()> {
    let collected = load_request_evidence(access.into(), request_id).await?;
    let tools = serde_json::json!({ "AgentToolCall": collected.tool_calls });
    std::fs::write(
        evidence.join(format!("{stage}-tools.json")),
        serde_json::to_vec_pretty(&tools)?,
    )?;
    write_inference_evidence(evidence, stage, &collected)
}

async fn retain_inference_evidence<'a>(
    access: impl Into<RuntimeAccess<'a>>,
    request_id: &str,
    stage: &str,
    evidence: &Path,
) -> Result<()> {
    let data = match access.into() {
        RuntimeAccess::Embedded(node) => {
            let response = gents::graphql::graphql_with_transaction_retry(
                node,
                &inference_sample_query(request_id),
                "inference sample",
            )
            .await?;
            response.data.context("inference sample returned no data")?
        }
        control @ RuntimeAccess::ControlPlane(_) => {
            control.query(&inference_sample_query(request_id)).await?
        }
    };
    write_inference_evidence(evidence, stage, &request_evidence_from_query_data(&data))
}

async fn load_request_evidence(
    access: RuntimeAccess<'_>,
    request_id: &str,
) -> Result<RequestEvidence> {
    match access {
        RuntimeAccess::Embedded(node) => collect_request_evidence(node, request_id).await,
        control @ RuntimeAccess::ControlPlane(_) => {
            let data = control.query(&evidence_query(request_id)).await?;
            Ok(request_evidence_from_query_data(&data))
        }
    }
}

fn write_inference_evidence(
    evidence: &Path,
    stage: &str,
    collected: &RequestEvidence,
) -> Result<()> {
    super::reporting::write_json(
        &evidence.join(format!("{stage}-inference.json")),
        &serde_json::json!({
            "AgentResponse": collected.responses,
            "InferenceCall": collected.inference_calls,
        }),
    )
}
