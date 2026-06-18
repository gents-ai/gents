//! Single-node live e2e for `fan_out_and_synthesize` (issue #378, cut 1).
//!
//! Normal test runs skip this (`#[ignore]` + env gate). When explicitly run
//! with `DEFRA_AGENT_LIVE_WORKFLOW=1`, it boots one document-driven agent,
//! configures an orchestrator plus researcher/synthesizer subagent behaviors,
//! drives one workflow tool call, and asserts the durable barrier projection
//! over `AgentToolCall.workflow_group_id` / `workflow_role`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{
    default_behavior_id_for_agent, default_inference_profile_id_for_behavior,
    ensure_agent_principal, load_agent_behavior, upsert_agent_behavior, upsert_tool_selection,
    AgentBehaviorDocument, AgentIdentity, DefraAgent, DocumentRuntimeOptions, SubagentTarget,
    ToolCeiling, ToolSelectionDocument,
};
use serde::Deserialize;

use crate::support::fixtures::test_identity;
use crate::support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use crate::support::{first_optional_row, test_db, TestDb};

const DEFAULT_LIVE_ENDPOINT: &str = "http://100.73.235.38:8000/v1";
const DEFAULT_LIVE_MODEL: &str = "d4f";
const LIVE_BACKEND_ID: &str = "backend-live-workflow";
const ORCH_TOOL: &str = "fan_out_and_synthesize";
const RESEARCHER_BEHAVIOR_ID: &str = "workflow-researcher";
const SYNTHESIZER_BEHAVIOR_ID: &str = "workflow-synthesizer";
const RESEARCHER_TARGET_NAME: &str = "researcher";
const SYNTHESIZER_TARGET_NAME: &str = "synthesizer";

fn live_enabled() -> bool {
    std::env::var("DEFRA_AGENT_LIVE_WORKFLOW").as_deref() == Ok("1")
}

fn live_endpoint() -> String {
    std::env::var("DEFRA_AGENT_LIVE_WORKFLOW_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_LIVE_ENDPOINT.to_string())
}

fn live_model() -> String {
    std::env::var("DEFRA_AGENT_LIVE_WORKFLOW_MODEL")
        .unwrap_or_else(|_| DEFAULT_LIVE_MODEL.to_string())
}

#[derive(Debug, Deserialize)]
struct WorkflowToolCallRow {
    tool_name: String,
    tool_call_id: String,
    lifecycle_state: Option<String>,
    workflow_group_id: Option<String>,
    workflow_role: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    child_request_id: Option<String>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set DEFRA_AGENT_LIVE_WORKFLOW=1 and pass --ignored"]
async fn fan_out_and_synthesize_barrier_live() -> Result<()> {
    if !live_enabled() {
        eprintln!("DEFRA_AGENT_LIVE_WORKFLOW != 1; skipping workflow orchestration e2e");
        return Ok(());
    }

    let endpoint = live_endpoint();
    let model = live_model();
    assert_endpoint_reachable(&endpoint).await;

    let db: TestDb = test_db("workflow-fanout-live").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("workflow-fanout-live"));
    let agent_did = identity.did().to_string();
    let orchestrator_behavior_id = default_behavior_id_for_agent(&agent_did);
    let profile_id = default_inference_profile_id_for_behavior(&orchestrator_behavior_id);

    ensure_agent_principal(db.node.as_ref(), &agent_did)
        .await
        .expect("ensure principal");
    upsert_live_backend(db.node.as_ref(), &endpoint, &model).await;

    configure_behavior(
        db.node.as_ref(),
        &orchestrator_behavior_id,
        &agent_did,
        &model,
        &profile_id,
        ORCHESTRATOR_SYSTEM_PROMPT,
        None,
    )
    .await;
    configure_behavior(
        db.node.as_ref(),
        RESEARCHER_BEHAVIOR_ID,
        &agent_did,
        &model,
        &profile_id,
        "Answer the assigned research question in one short factual sentence.",
        Some("Researches one factual sub-question and returns a concise answer."),
    )
    .await;
    configure_behavior(
        db.node.as_ref(),
        SYNTHESIZER_BEHAVIOR_ID,
        &agent_did,
        &model,
        &profile_id,
        "Synthesize the supplied JSON outcomes into one concise final answer. Mention failed items if any.",
        Some("Synthesizes workflow fan-out outcomes into a final answer."),
    )
    .await;

    authorize_workflow_tools(
        db.node.as_ref(),
        &agent_did,
        &orchestrator_behavior_id,
        vec![
            SubagentTarget {
                name: RESEARCHER_TARGET_NAME.to_string(),
                agent_did: agent_did.clone(),
                behavior_id: RESEARCHER_BEHAVIOR_ID.to_string(),
                description: Some("Researches factual sub-questions.".to_string()),
            },
            SubagentTarget {
                name: SYNTHESIZER_TARGET_NAME.to_string(),
                agent_did: agent_did.clone(),
                behavior_id: SYNTHESIZER_BEHAVIOR_ID.to_string(),
                description: Some("Synthesizes fan-out outcomes.".to_string()),
            },
        ],
    )
    .await;

    let agent = boot_document_agent(&db, identity).await?;
    let request_id = "req-workflow-fanout-live";
    let session_id = "session-workflow-fanout-live";
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &orchestrator_behavior_id,
        request_id,
        session_id,
        "Use workflow orchestration to answer: compare the capitals of France, Germany, and Italy, then synthesize a one-sentence answer.",
    )
    .await;

    let parent_terminal =
        wait_for_request_terminal(db.node.as_ref(), request_id, Duration::from_secs(180)).await;
    eprintln!("[workflow-live] parent terminal state = {parent_terminal}");
    if !is_terminal(&parent_terminal) {
        dump_session_diagnostics(db.node.as_ref(), session_id).await;
        panic!("workflow parent request did not terminalize; got {parent_terminal}");
    }

    let orch_calls = fetch_orchestration_tool_calls(db.node.as_ref(), session_id).await;
    if orch_calls.is_empty() {
        dump_session_diagnostics(db.node.as_ref(), session_id).await;
    }
    assert_eq!(
        orch_calls.len(),
        1,
        "expected exactly one `{ORCH_TOOL}` tool call; got {orch_calls:?}"
    );
    let group_id = orch_calls[0].tool_call_id.clone();
    assert_eq!(orch_calls[0].lifecycle_state.as_deref(), Some("completed"));

    let workflow_rows = fetch_workflow_group_rows(db.node.as_ref(), session_id, &group_id).await;
    assert!(
        workflow_rows
            .iter()
            .all(|row| row.tool_name == "spawn_subagent"),
        "workflow group rows must be subagent bridge rows; got {workflow_rows:?}"
    );
    let fan_out = workflow_rows
        .iter()
        .filter(|row| row.workflow_role.as_deref() == Some("fan_out_child"))
        .collect::<Vec<_>>();
    let synthesis = workflow_rows
        .iter()
        .filter(|row| row.workflow_role.as_deref() == Some("synthesis"))
        .collect::<Vec<_>>();

    // The barrier-completeness property is N-agnostic: assert the D6 width bound
    // (1..=maxBackgroundedPerParent = 8), not the exact count the prompt asks
    // for, so a model that emits 2 or 4 tasks does not masquerade as a barrier
    // violation. The structural barrier checks below hold for whatever N ran.
    assert!(
        (1..=8).contains(&fan_out.len()),
        "expected 1..=8 fan_out_child bridges (D6 width bound) in group {group_id}; got {} ({workflow_rows:?})",
        fan_out.len()
    );
    assert_eq!(
        synthesis.len(),
        1,
        "expected exactly 1 synthesis bridge in group {group_id}; got {workflow_rows:?}"
    );
    assert!(fan_out.iter().all(|row| {
        row.workflow_group_id.as_deref() == Some(group_id.as_str())
            && is_tool_terminal(row.lifecycle_state.as_deref().unwrap_or_default())
            && row
                .child_request_id
                .as_deref()
                .is_some_and(|id| !id.is_empty())
    }));

    let max_fan_out_completed = fan_out
        .iter()
        .map(|row| parse_time(row.completed_at.as_deref(), "fan_out.completed_at"))
        .max()
        .expect("non-empty fan-out");
    let synthesis_started = parse_time(synthesis[0].started_at.as_deref(), "synthesis.started_at");
    assert!(
        synthesis_started >= max_fan_out_completed,
        "synthesis must start after every fan-out bridge completed; synthesis_started={synthesis_started}, max_fan_out_completed={max_fan_out_completed}"
    );
    assert_eq!(synthesis[0].lifecycle_state.as_deref(), Some("completed"));

    for row in fan_out.iter().chain(synthesis.iter()) {
        let child_request_id = row
            .child_request_id
            .as_deref()
            .expect("workflow bridge must carry child_request_id");
        let child = fetch_request_lineage(db.node.as_ref(), child_request_id)
            .await
            .expect("workflow child AgentRequest must exist");
        assert_eq!(
            child.caused_by_parent_request_id.as_deref(),
            Some(request_id)
        );
        assert_eq!(
            child.caused_by_parent_tool_call_id.as_deref(),
            Some(row.tool_call_id.as_str())
        );
        assert_eq!(child.caused_by_trigger_kind.as_deref(), Some("subagent"));
    }

    // ---- The actual DATA the workflow produced (not just the structure) ------
    // Show how the model authored the workflow: the single orchestration tool
    // call carries the fan-out task prompts + synthesis target/prompt as args.
    let authored = fetch_tool_call_args(db.node.as_ref(), &group_id).await;
    eprintln!("[workflow-live] orchestrator authored fan_out_and_synthesize args:\n{authored}");

    // Every fan-out child must have returned non-empty research data.
    for (i, row) in fan_out.iter().enumerate() {
        let crid = row.child_request_id.as_deref().expect("fan-out child id");
        let answer = fetch_answer(db.node.as_ref(), crid).await;
        eprintln!("[workflow-live] fan-out researcher #{i} answer: {answer:?}");
        assert!(
            !answer.trim().is_empty(),
            "fan-out researcher #{i} ({crid}) must return a non-empty answer"
        );
    }

    // The synthesis child must have produced a non-empty synthesized report.
    let synthesis_crid = synthesis[0]
        .child_request_id
        .as_deref()
        .expect("synthesis child id");
    let report = fetch_answer(db.node.as_ref(), synthesis_crid).await;
    eprintln!("[workflow-live] SYNTHESIZED REPORT:\n{report}");
    assert!(
        !report.trim().is_empty(),
        "synthesis child ({synthesis_crid}) must return a non-empty synthesized report"
    );
    // Soft signal that synthesis actually reasoned over the fan-out data — the
    // three capitals should surface. Never hard-fail on model wording.
    let lowered = report.to_lowercase();
    let hits = ["paris", "berlin", "rome"]
        .iter()
        .filter(|city| lowered.contains(*city))
        .count();
    if hits >= 2 {
        eprintln!("[workflow-live] SOFT-OK: synthesized report references {hits}/3 capitals");
    } else {
        eprintln!(
            "[workflow-live] SOFT-WARN: synthesized report referenced only {hits}/3 capitals: {report:?}"
        );
    }

    // The synthesized report must flow back as the orchestrator's final answer
    // (D5: synthesis returns to the orchestrator continuation).
    let final_answer = fetch_answer(db.node.as_ref(), request_id).await;
    eprintln!("[workflow-live] ORCHESTRATOR FINAL ANSWER:\n{final_answer}");
    assert!(
        !final_answer.trim().is_empty(),
        "orchestrator final answer must be non-empty (synthesis must return to the parent)"
    );

    agent.shutdown().await;
    Ok(())
}

const ORCHESTRATOR_SYSTEM_PROMPT: &str = "You are a workflow orchestrator. You have a workflow tool \
named `fan_out_and_synthesize`. For the user's request, you MUST make exactly one call to \
`fan_out_and_synthesize`; do not call `spawn_subagent` directly and do not answer directly. Use \
target exactly \"researcher\", synthesis_target exactly \"synthesizer\", exactly three tasks, and a \
synthesis_prompt asking for one concise comparison sentence. The three task prompts must ask for the \
capital of France, Germany, and Italy respectively.";

async fn fetch_orchestration_tool_calls(
    node: &EmbeddedNode,
    session_id: &str,
) -> Vec<WorkflowToolCallRow> {
    let session = escape_graphql_string(session_id);
    let tool = escape_graphql_string(ORCH_TOOL);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{session}" }}, tool_name: {{ _eq: "{tool}" }} }},
                order: {{ started_at: ASC }}
            ) {{
                tool_name tool_call_id lifecycle_state workflow_group_id workflow_role started_at completed_at child_request_id
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "AgentToolCall orchestration query failed: {:?}",
        resp.errors
    );
    rows(&resp, "AgentToolCall")
}

async fn fetch_workflow_group_rows(
    node: &EmbeddedNode,
    session_id: &str,
    group_id: &str,
) -> Vec<WorkflowToolCallRow> {
    let session = escape_graphql_string(session_id);
    let group = escape_graphql_string(group_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{session}" }}, workflow_group_id: {{ _eq: "{group}" }} }},
                order: {{ started_at: ASC }}
            ) {{
                tool_name tool_call_id lifecycle_state workflow_group_id workflow_role started_at completed_at child_request_id
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "AgentToolCall workflow group query failed: {:?}",
        resp.errors
    );
    rows(&resp, "AgentToolCall")
}

#[derive(Debug, Deserialize)]
struct ChildLineageRow {
    caused_by_parent_request_id: Option<String>,
    caused_by_parent_tool_call_id: Option<String>,
    caused_by_trigger_kind: Option<String>,
}

async fn fetch_request_lineage(node: &EmbeddedNode, request_id: &str) -> Option<ChildLineageRow> {
    let request = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{request}" }} }}, limit: 1) {{
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
                caused_by_trigger_kind
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_optional_row::<ChildLineageRow>(&resp, "AgentRequest")
}

/// Fetch the assistant answer for a request: prefer the `AgentResponse` content,
/// fall back to the latest assistant `AgentMessage` on the request's session.
async fn fetch_answer(node: &EmbeddedNode, request_id: &str) -> String {
    let request = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentResponse(filter: {{ request_id: {{ _eq: "{request}" }} }}, limit: 1) {{
                content
                session_id
            }}
        }}"#
    );
    #[derive(Debug, Deserialize)]
    struct RespRow {
        content: Option<String>,
        session_id: Option<String>,
    }
    let resp = node.execute(&query).await;
    let row = first_optional_row::<RespRow>(&resp, "AgentResponse");
    if let Some(content) = row.as_ref().and_then(|r| r.content.clone()) {
        if !content.trim().is_empty() {
            return content;
        }
    }
    let Some(session_id) = row.and_then(|r| r.session_id).filter(|s| !s.is_empty()) else {
        return String::new();
    };
    let session = escape_graphql_string(&session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{session}" }}, role: {{ _eq: "assistant" }} }},
                order: {{ sequence: DESC }},
                limit: 1
            ) {{ content }}
        }}"#
    );
    #[derive(Debug, Deserialize)]
    struct MsgRow {
        content: String,
    }
    let resp = node.execute(&query).await;
    first_optional_row::<MsgRow>(&resp, "AgentMessage")
        .map(|m| m.content)
        .unwrap_or_default()
}

/// Fetch the raw `args` the model emitted for the orchestration tool call — the
/// runtime-authored "workflow" (fan-out task prompts + synthesis target/prompt).
async fn fetch_tool_call_args(node: &EmbeddedNode, tool_call_id: &str) -> String {
    let tcid = escape_graphql_string(tool_call_id);
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ tool_call_id: {{ _eq: "{tcid}" }} }}, limit: 1) {{ args }} }}"#
    );
    #[derive(Debug, Deserialize)]
    struct ArgsRow {
        args: Option<String>,
    }
    let resp = node.execute(&query).await;
    first_optional_row::<ArgsRow>(&resp, "AgentToolCall")
        .and_then(|r| r.args)
        .unwrap_or_default()
}

async fn assert_endpoint_reachable(endpoint: &str) {
    let url = format!("{}/models", endpoint.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client");
    let resp = tokio::time::timeout(Duration::from_secs(20), client.get(&url).send()).await;
    match resp {
        Ok(Ok(r)) if r.status().is_success() => {}
        Ok(Ok(r)) => panic!("live endpoint {url} returned status {}", r.status()),
        Ok(Err(e)) => panic!("live endpoint {url} unreachable: {e}"),
        Err(_) => panic!("live endpoint {url} timed out"),
    }
}

async fn boot_document_agent(db: &TestDb, identity: Arc<dyn AgentIdentity>) -> Result<BootedAgent> {
    let agent = DefraAgent::from_default_behavior_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await?;
    let agent_did = agent.agent_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;
    Ok(BootedAgent::new(shutdown_tx, handle, agent_did))
}

async fn upsert_live_backend(node: &EmbeddedNode, endpoint: &str, model: &str) {
    let escaped_backend_id = escape_graphql_string(LIVE_BACKEND_ID);
    let escaped_endpoint = escape_graphql_string(endpoint);
    let escaped_model = escape_graphql_string(model);
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                add: {{
                    backend_id: "{escaped_backend_id}",
                    name: "{escaped_backend_id}",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "{escaped_endpoint}",
                    api_key: "",
                    api_key_env_var: "",
                    max_concurrent: 4,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{escaped_model}"],
                    probe_status: "healthy"
                }},
                update: {{
                    name: "{escaped_backend_id}",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "{escaped_endpoint}",
                    api_key: "",
                    api_key_env_var: "",
                    max_concurrent: 4,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{escaped_model}"],
                    probe_status: "healthy"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "upsert live backend failed: {:?}",
        response.errors
    );
}

async fn configure_behavior(
    node: &EmbeddedNode,
    behavior_id: &str,
    agent_did: &str,
    model: &str,
    inference_profile_id: &str,
    system_prompt: &str,
    description: Option<&str>,
) {
    let mut behavior = load_agent_behavior(node, behavior_id)
        .await
        .expect("load behavior")
        .unwrap_or_else(|| AgentBehaviorDocument {
            behavior_id: behavior_id.to_string(),
            agent_did: agent_did.to_string(),
            display_name: Some(behavior_id.to_string()),
            description: None,
            summary: None,
            system_prompt: None,
            request_context_template: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            skill_refs: Vec::new(),
            skill_excludes: Vec::new(),
            enabled: true,
            created_at: Some("2026-06-17T00:00:00Z".to_string()),
        });
    behavior.agent_did = agent_did.to_string();
    behavior.backend_id = Some(LIVE_BACKEND_ID.to_string());
    behavior.model_name = Some(model.to_string());
    behavior.inference_profile_id = Some(inference_profile_id.to_string());
    behavior.system_prompt = Some(system_prompt.to_string());
    behavior.description = description.map(ToOwned::to_owned);
    behavior.enabled = true;
    upsert_agent_behavior(node, &behavior)
        .await
        .expect("upsert behavior");
}

async fn authorize_workflow_tools(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    subagent_targets: Vec<SubagentTarget>,
) {
    let selection_id = format!("{behavior_id}-workflow-tools");
    let target_entries = subagent_targets
        .iter()
        .map(SubagentTarget::to_entry)
        .collect();
    upsert_tool_selection(
        node,
        &ToolSelectionDocument {
            selection_id: selection_id.clone(),
            agent_did: agent_did.to_string(),
            subagent_targets: Some(target_entries),
            subagent_spawn_enabled: Some(true),
            orchestration_enabled: Some(true),
            subagent_background_enabled: Some(true),
            subagent_allow_cross_deployment: Some(false),
            enable_meta_tools: Some(false),
            enable_defra_query: Some(false),
            ..Default::default()
        },
    )
    .await
    .expect("upsert workflow tool selection");

    let mut behavior = load_agent_behavior(node, behavior_id)
        .await
        .expect("load behavior for tool-selection link")
        .expect("behavior must exist before linking tool selection");
    behavior.tool_selection_id = Some(selection_id);
    upsert_agent_behavior(node, &behavior)
        .await
        .expect("link tool selection");
}

fn is_terminal(state: &str) -> bool {
    matches!(
        state,
        "completed" | "failed" | "dead" | "interrupted" | "superseded"
    )
}

fn is_tool_terminal(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "timedOut" | "cancelled")
}

async fn fetch_request_lifecycle(node: &EmbeddedNode, request_id: &str) -> Option<String> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                lifecycle_state
            }}
        }}"#
    );
    #[derive(Deserialize)]
    struct Row {
        lifecycle_state: Option<String>,
    }
    let resp = node.execute(&query).await;
    first_optional_row::<Row>(&resp, "AgentRequest").and_then(|row| row.lifecycle_state)
}

async fn wait_for_request_terminal(
    node: &EmbeddedNode,
    request_id: &str,
    timeout: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last = String::from("<none>");
    loop {
        if let Some(state) = fetch_request_lifecycle(node, request_id).await {
            last = state.clone();
            if is_terminal(&state) {
                return state;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for request {request_id} to terminalize; last={last}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn dump_session_diagnostics(node: &EmbeddedNode, session_id: &str) {
    let escaped = escape_graphql_string(session_id);
    let tc_query = format!(
        r#"{{
            AgentToolCall(filter: {{ session_id: {{ _eq: "{escaped}" }} }}, order: {{ message_sequence: ASC }}) {{
                tool_name tool_call_id lifecycle_state workflow_group_id workflow_role args result child_request_id await_mode tool_failure_class started_at completed_at
            }}
        }}"#
    );
    let resp = node.execute(&tc_query).await;
    eprintln!(
        "[workflow-diag] tool calls for session {session_id}: {}",
        serde_json::to_string_pretty(
            &resp
                .data
                .unwrap_or_default()
                .get("AgentToolCall")
                .cloned()
                .unwrap_or_default()
        )
        .unwrap_or_default()
    );
}

fn rows<T>(resp: &defra_agent::defra_node::QueryResponse, collection: &str) -> Vec<T>
where
    T: for<'de> Deserialize<'de>,
{
    resp.data
        .as_ref()
        .and_then(|data| data.get(collection))
        .and_then(|rows| serde_json::from_value(rows.clone()).ok())
        .unwrap_or_default()
}

fn parse_time(value: Option<&str>, field: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value.unwrap_or_else(|| panic!("{field} missing")))
        .unwrap_or_else(|error| panic!("{field} invalid: {error}"))
        .with_timezone(&Utc)
}
