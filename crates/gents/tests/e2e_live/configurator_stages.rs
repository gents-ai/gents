//! Shared execution and evidence collection for progressive consumer evals.
//! Assertions remain outside the evaluated behaviors and their writable roots.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use serde::Serialize;
use serde_json::Value;

pub const INPUT_SCHEMA: &str = "type GentsEvalStageInput { stage: String prompt: String }";

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
async fn case_reporting_preserves_failure_classification_and_skipped_prerequisites() {
    let evidence = tempfile::tempdir().unwrap();
    let deadline: Result<()> = Err(EvaluationFailure::Deadline("onboarding".into()).into());
    assert!(checked("onboarding", evidence.path(), async { deadline })
        .await
        .is_err());
    checked("builder-readiness", evidence.path(), async { Ok(()) })
        .await
        .unwrap();
    let inconclusive: Result<()> =
        Err(EvaluationFailure::Inconclusive("animated scene".into()).into());
    assert!(checked("pagoda", evidence.path(), async { inconclusive })
        .await
        .is_err());
    let infrastructure: Result<()> =
        Err(EvaluationFailure::Infrastructure("Chrome unavailable".into()).into());
    assert!(
        checked("improve", evidence.path(), async { infrastructure })
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
    case_id: &str,
    evidence: &Path,
    check: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
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
    [
        "onboarding",
        "builder-readiness",
        "skill-workflow",
        "pagoda",
        "review",
        "improve",
        "document-automation",
    ]
    .into_iter()
    .map(|case_id| {
        let path = evidence.join(format!("{case_id}-acceptance.json"));
        match std::fs::read(&path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
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

/// Wait for runtime activation after this stage's last committed config patch.
/// A fresh session alone does not bypass the reconciliation debounce.
pub async fn wait_for_config_activation(
    node: &EmbeddedNode,
    owner: &str,
    request_id: &str,
) -> Result<()> {
    config_activation(node, owner, request_id)
        .await
        .map_err(infrastructure)
}

async fn config_activation(node: &EmbeddedNode, owner: &str, request_id: &str) -> Result<()> {
    let request = escape_graphql_string(request_id);
    let calls = node.execute(&format!(r#"{{ AgentToolCall(filter: {{request_id: {{_eq: "{request}"}}, tool_name: {{_eq: "config"}}, lifecycle_state: {{_eq: "completed"}}}}) {{args result completed_at}} }}"#)).await;
    ensure!(
        !calls.has_errors(),
        "config activation evidence: {:?}",
        calls.errors
    );
    let mut latest = None;
    for row in calls
        .data
        .as_ref()
        .and_then(|v| v["AgentToolCall"].as_array())
        .context("config call rows missing")?
    {
        // Schema registration is outside configuration-document reconciliation.
        // It is already verified by its publication owner and cannot provide a
        // later AgentRuntime completion timestamp by itself.
        let args = row["args"]
            .as_str()
            .and_then(|v| serde_json::from_str::<Value>(v).ok());
        if args
            .as_ref()
            .is_some_and(|args| args["argv"][0] == "schema")
        {
            continue;
        }
        let result = row["result"]
            .as_str()
            .and_then(|v| serde_json::from_str::<Value>(v).ok());
        if result.as_ref().is_some_and(|v| v["committed"] == true) {
            let at = chrono::DateTime::parse_from_rfc3339(
                row["completed_at"]
                    .as_str()
                    .context("config completion timestamp missing")?,
            )?;
            latest =
                Some(latest.map_or(at, |old: chrono::DateTime<chrono::FixedOffset>| old.max(at)));
        }
    }
    let Some(latest) = latest else {
        return Ok(());
    };
    wait_for_reconcile_after(node, owner, latest).await
}

async fn wait_for_reconcile_after(
    node: &EmbeddedNode,
    owner: &str,
    latest: chrono::DateTime<chrono::FixedOffset>,
) -> Result<()> {
    let owner = escape_graphql_string(owner);
    let started = Instant::now();
    loop {
        let response = node.execute(&format!(r#"{{ AgentRuntime(filter: {{agent_did: {{_eq: "{owner}"}}}}) {{reconcile_phase last_reconcile_completed_at last_reconcile_result last_reconcile_error}} }}"#)).await;
        ensure!(
            !response.has_errors(),
            "runtime activation observation: {:?}",
            response.errors
        );
        let row = response
            .data
            .as_ref()
            .and_then(|v| v["AgentRuntime"].get(0));
        if let Some(row) = row {
            let completed = row["last_reconcile_completed_at"]
                .as_str()
                .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok());
            if row["reconcile_phase"] == "idle" && completed.is_some_and(|at| at >= latest) {
                ensure!(
                    row["last_reconcile_result"] != "error",
                    "runtime rejected configuration: {}",
                    row["last_reconcile_error"]
                );
                return Ok(());
            }
        }
        ensure!(
            started.elapsed() < Duration::from_secs(120),
            "configuration did not activate after {latest}; last runtime: {row:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The runner installs reusable task/trigger definitions, then invokes only by
/// writing an input document. Native materialization owns request admission,
/// session creation, templating, deduplication, and request identity.
async fn submit_stage(
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
    let written_after = chrono::Utc::now().fixed_offset();
    gents::ConfigAccess::transact_local(node, None, "eval.stage_definitions", |txn| {
        let plan = &plan;
        Box::pin(async move {
            gents::config_client::apply_desired_state_plan(txn, plan)
                .await
                .map(|_| ())
        })
    })
    .await?;
    wait_for_reconcile_after(node, owner, written_after).await?;
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

/// Each stage uses a fresh session; configuration stages must await activation.
pub async fn execute(
    node: &EmbeddedNode,
    owner: &str,
    behavior: &str,
    stage: &str,
    prompt: &str,
    evidence: &Path,
) -> Result<StageResult> {
    execute_inner(node, owner, behavior, stage, prompt, evidence)
        .await
        .map_err(infrastructure)
}

async fn execute_inner(
    node: &EmbeddedNode,
    owner: &str,
    behavior: &str,
    stage: &str,
    prompt: &str,
    evidence: &Path,
) -> Result<StageResult> {
    let started = Instant::now();
    std::fs::create_dir_all(evidence)?;
    std::fs::write(
        evidence.join(format!("{stage}-input.json")),
        serde_json::to_vec_pretty(&serde_json::json!({
            "stage":stage,"owner":owner,"behavior_id":behavior,"prompt":prompt,
            "transport":"GentsEvalStageInput -> EventSource -> Trigger -> Task -> AgentRequest",
        }))?,
    )?;
    let request_id = submit_stage(node, owner, behavior, stage, prompt).await?;
    let escaped = escape_graphql_string(&request_id);
    let query = format!(
        r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{escaped}"}}}}) {{lifecycle_state session_id}} }}"#
    );
    let mut observation_timed_out = false;
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
        if started.elapsed() > Duration::from_secs(600) {
            if !observation_timed_out {
                observation_timed_out = true;
                gents::interrupt_request(node, &request_id)
                    .await
                    .context("interrupt stage after evaluation deadline")?;
            }
            if started.elapsed() > Duration::from_secs(630) {
                break (format!("nonterminal_after_interrupt:{state}"), session_id);
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let answer = crate::steward_loop_live::wait_for_assistant_answer(
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
    std::fs::write(
        evidence.join(format!("{stage}-inference.json")),
        serde_json::to_vec_pretty(&diagnostics.data.unwrap_or(Value::Null))?,
    )?;
    Ok(())
}
