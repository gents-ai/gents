//! Shared native execution and evidence collection for progressive consumer evals.
//! Assertions remain outside the evaluated behaviors and their writable roots.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
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

// One declaration drives both call-site identifiers and report enumeration.
macro_rules! case_catalog {
    ($($variant:ident => $id:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug)]
        pub enum CaseId { $($variant),+ }
        impl CaseId {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $id),+ }
            }
        }
    };
}

case_catalog! {
    Onboarding => "onboarding",
    BuilderReadiness => "builder-readiness",
    SkillWorkflow => "skill-workflow",
    Pagoda => "pagoda",
    Review => "review",
    Improve => "improve",
    DocumentAutomation => "document-automation",
}

#[derive(Debug, thiserror::Error)]
pub enum EvaluationFailure {
    #[error("evaluation deadline exceeded for {0}")]
    Deadline(String),
    #[error("model request failed: {0}")]
    ModelRequest(String),
    #[error("evaluation inconclusive: {0}")]
    Inconclusive(String),
    #[error("evaluation infrastructure failed: {0}")]
    Infrastructure(String),
}

impl EvaluationFailure {
    fn kind(&self) -> &'static str {
        match self {
            Self::Deadline(_) => "deadline",
            Self::ModelRequest(_) => "model_request",
            Self::Inconclusive(_) => "inconclusive",
            Self::Infrastructure(_) => "infrastructure",
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
}

impl StageResult {
    pub fn ensure_completed(&self) -> Result<()> {
        if self.observation_timed_out {
            return Err(EvaluationFailure::Deadline(self.stage.clone()).into());
        }
        if self.terminal_state != "completed" {
            return Err(EvaluationFailure::ModelRequest(format!(
                "{}: {}",
                self.stage, self.terminal_state
            ))
            .into());
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
    for &case in CaseId::ALL {
        assert!(ids.insert(case.as_str()));
        checked(case, evidence.path(), async { Ok(()) })
            .await
            .unwrap();
    }
    let results = case_results(evidence.path()).unwrap();
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
    assert!(case_results(evidence.path()).is_err());
}

#[tokio::test]
async fn case_reporting_preserves_failure_classification_and_skipped_prerequisites() {
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
        checked(CaseId::Pagoda, evidence.path(), async { inconclusive })
            .await
            .is_err()
    );
    let infrastructure: Result<()> =
        Err(EvaluationFailure::Infrastructure("Chrome unavailable".into()).into());
    assert!(
        checked(CaseId::Improve, evidence.path(), async { infrastructure })
            .await
            .is_err()
    );
    let results = case_results(evidence.path()).unwrap();
    assert_eq!(results.len(), 7);
    assert_eq!(results[0].failure_kind.as_deref(), Some("deadline"));
    assert_eq!(results[1].status, "passed");
    assert_eq!(results[2].status, "skipped");
    assert_eq!(results[3].failure_kind.as_deref(), Some("inconclusive"));
    assert_eq!(results[5].failure_kind.as_deref(), Some("infrastructure"));
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
                .map_or("acceptance", EvaluationFailure::kind)
                .to_owned()
        }),
    };
    tracing::info!(target: "gents::configurator_eval", result = %serde_json::to_string(&receipt)?, "eval case acceptance");
    let retention = (|| {
        std::fs::create_dir_all(evidence)?;
        std::fs::write(
            evidence.join(format!("{case_id}-acceptance.json")),
            serde_json::to_vec_pretty(&receipt)?,
        )?;
        Ok(())
    })();
    retain_outcome(result, retention)
}

pub fn case_results(evidence: &Path) -> Result<Vec<CaseResult>> {
    CaseId::ALL
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
    activation.wait().await?;
    let prompt = escape_graphql_string(prompt);
    let submitted = node.execute(&format!(r#"mutation {{ create_GentsEvalStageInput(input: {{stage: "{stage_escaped}", prompt: "{prompt}"}}) {{_docID}} }}"#)).await;
    ensure!(
        !submitted.has_errors(),
        "stage input write failed: {:?}",
        submitted.errors
    );
    let trigger = escape_graphql_string(&trigger);
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
            "native task did not materialize for stage {stage}"
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
    let (agent, runtime) = crate::support::live_inference::boot_d4f_agent_with_options(
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
    node: &EmbeddedNode,
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

async fn execute_inner(
    activation: &ActivationFence,
    node: &EmbeddedNode,
    owner: &str,
    behavior: &str,
    stage: &str,
    prompt: &str,
    evidence: &Path,
) -> Result<StageResult> {
    let timeout = stage_timeout()?;
    let started = Instant::now();
    std::fs::create_dir_all(evidence)?;
    std::fs::write(
        evidence.join(format!("{stage}-input.json")),
        serde_json::to_vec_pretty(&serde_json::json!({
            "stage":stage,"owner":owner,"behavior_id":behavior,"prompt":prompt,
            "transport":"GentsEvalStageInput -> EventSource -> Trigger -> Task -> AgentRequest",
        }))?,
    )?;
    let request_id = submit_stage(activation, node, owner, behavior, stage, prompt).await?;
    let escaped = escape_graphql_string(&request_id);
    let query = format!(
        r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{escaped}"}}}}) {{lifecycle_state session_id}} }}"#
    );
    let mut observation_timed_out = false;
    let mut next_usage_snapshot = Instant::now();
    let (terminal_state, session_id) = loop {
        let response = node.execute(&query).await;
        ensure!(
            !response.has_errors(),
            "stage observation failed: {:?}",
            response.errors
        );
        let state = response
            .data
            .as_ref()
            .and_then(|data| data["AgentRequest"][0]["lifecycle_state"].as_str())
            .unwrap_or("missing");
        let session_id = response
            .data
            .as_ref()
            .and_then(|data| data["AgentRequest"][0]["session_id"].as_str())
            .map(str::to_owned);
        if gents_protocol::request_lifecycle::RequestLifecycleState::is_terminal_str(Some(state)) {
            break (state.to_owned(), session_id);
        }
        if Instant::now() >= next_usage_snapshot {
            // Display sampling must not fail the evaluated request or stall its deadline.
            let _ = tokio::time::timeout(
                Duration::from_millis(500),
                retain_inference_evidence(node, &request_id, stage, evidence),
            )
            .await;
            next_usage_snapshot = Instant::now() + Duration::from_secs(2);
        }
        if started.elapsed() > timeout {
            if !observation_timed_out {
                observation_timed_out = true;
                gents::interrupt_request(node, &request_id)
                    .await
                    .context("interrupt stage after evaluation deadline")?;
            }
            if started.elapsed() > timeout + Duration::from_secs(30) {
                break (format!("nonterminal_after_interrupt:{state}"), session_id);
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let answer = crate::support::live_inference::wait_for_assistant_answer(
        node,
        &request_id,
        Duration::from_secs(2),
    )
    .await;
    let result = StageResult {
        stage: stage.to_owned(),
        request_id,
        session_id,
        terminal_state,
        elapsed_ms: started.elapsed().as_millis(),
        answer,
        observation_timed_out,
    };
    let retention = async {
        std::fs::create_dir_all(evidence).context("create evaluator evidence directory")?;
        std::fs::write(
            evidence.join(format!("{stage}.json")),
            serde_json::to_vec_pretty(&result)?,
        )?;
        retain_request_evidence(node, &result.request_id, stage, evidence).await
    }
    .await;
    if let Err(error) = retention {
        retain_outcome(result.ensure_completed(), Err(error))?;
    }
    Ok(result)
}

/// Also used for requests created by model-authored automation, which the
/// runner observes but does not submit or repair.
pub async fn retain_request_evidence(
    node: &EmbeddedNode,
    request_id: &str,
    stage: &str,
    evidence: &Path,
) -> Result<()> {
    let escaped = escape_graphql_string(request_id);
    // Tool evidence records concrete execution, including failures and recovery.
    let calls = node.execute(&format!(
        r#"{{ AgentToolCall(filter: {{request_id: {{_eq: "{escaped}"}}}}) {{tool_name status lifecycle_state started_at completed_at args result}} }}"#
    )).await;
    ensure!(
        !calls.has_errors(),
        "tool evidence query failed: {:?}",
        calls.errors
    );
    std::fs::write(
        evidence.join(format!("{stage}-tools.json")),
        serde_json::to_vec_pretty(&calls.data.unwrap_or(Value::Null))?,
    )?;
    retain_inference_evidence(node, request_id, stage, evidence).await
}

async fn retain_inference_evidence(
    node: &EmbeddedNode,
    request_id: &str,
    stage: &str,
    evidence: &Path,
) -> Result<()> {
    let escaped = escape_graphql_string(request_id);
    let diagnostics = node.execute(&format!(
        r#"{{
            AgentResponse(filter: {{request_id: {{_eq: "{escaped}"}}}}) {{status error_message}}
            InferenceCall(filter: {{request_id: {{_eq: "{escaped}"}}}}) {{call_seq call_state failure_reason prompt_tokens completion_tokens queued_at started_at ended_at}}
        }}"#
    )).await;
    ensure!(
        !diagnostics.has_errors(),
        "stage diagnostics failed: {:?}",
        diagnostics.errors
    );
    super::reporting::write_json(
        &evidence.join(format!("{stage}-inference.json")),
        &diagnostics.data.unwrap_or(Value::Null),
    )
}
