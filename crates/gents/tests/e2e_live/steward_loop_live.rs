//! Plan 2 Phase 2a — d4f-backed LIVE test harness for the steward emit->triage
//! loop.
//!
//! Plan 1 proved the loop deterministically against a MOCK backend (the
//! `event_trigger_e2e` / `write_tool_trigger_e2e` tests only assert that an
//! `AgentRequest` *materializes*). Phase 2a qualifies the model-dependent
//! decisions against a real OpenAI-compatible model on workstation-1. Task
//! 2a-1 (this file) builds the foundation:
//!
//!   * `bind_d4f_backend` — writes an `InferenceBackend` doc pointing at the live
//!     endpoint and points the agent's default inference profile at it. Reusable
//!     by 2a-2 / 2a-3.
//!   * `boot_d4f_agent` — boots a full `Gents` from those behavior documents
//!     and waits for `process_state == "ready"`. Reusable.
//!   * `wait_for_request_terminal` / `wait_for_assistant_answer` — drive + AWAIT a
//!     full real-backend agent run (not just a materialized row). Reusable.
//!   * `d4f_backend_probes_healthy_and_completes` — the smoke test: submit a
//!     trivial unit of work, let the live model actually answer, and assert a
//!     non-empty assistant response landed.
//!
//! ## Live-run mechanism (modeled on `tests/subagent_delegation_live.rs`)
//!
//! The full-run pattern is lifted directly from `live_local_subagent_delegation`:
//! boot a full agent (`Gents::from_default_behavior_documents` + `.run()`),
//! submit work by creating a `pending` `AgentRequest` via
//! `crate::support::interrupt::create_runtime_request`, then WAIT for the request's
//! `lifecycle_state` to terminalize and read the assistant answer back from
//! `AgentResponse` / the latest assistant `AgentMessage`. That is the daemon
//! actually claiming the request, calling d4f, and persisting the completion —
//! not a mock "row appeared" assertion.
//!
//! ## Running
//!
//! Ignored by default and gated on `GENTS_D4F_LIVE=1`, so offline/CI runs
//! skip cleanly. Explicit runs fail if the live gate is missing.
//!
//! ```bash
//! GENTS_D4F_LIVE=1 cargo test --test e2e_live \
//!   --features defra-node/http,defra-node/p2p \
//!   d4f_backend_probes_healthy_and_completes \
//!   -- --ignored --test-threads=1 --nocapture
//! ```

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gents::defra_node::EmbeddedNode;
use gents::document_config::{
    AgentBehavior, AgentPrincipal, BackendAuth, InferenceBackend, InferenceProfile,
};
use gents::graphql::escape_graphql_string;
use gents::{
    default_behavior_id_for_agent, default_inference_profile_id_for_behavior,
    ensure_agent_principal, AgentIdentity, BackendProviderKind, Collection, DocumentRuntimeOptions,
    Gents, OpenAiWireApi, ToolCeiling,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::Deserialize;

use crate::support::fixtures::test_identity;
use crate::support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use crate::support::{first_optional_row, test_db, TestDb};

fn d4f_enabled() -> bool {
    std::env::var("GENTS_D4F_LIVE").as_deref() == Ok("1")
}

const D4F_BACKEND_ID: &str = "backend-d4f-live";

/// Live backend endpoint/model, overridable for workstation deployments.
fn d4f_endpoint() -> String {
    std::env::var("GENTS_D4F_ENDPOINT")
        .unwrap_or_else(|_| "http://workstation-1:8000/v1".to_string())
}

fn d4f_model() -> String {
    std::env::var("GENTS_D4F_MODEL").unwrap_or_else(|_| "GLM-5.3-Flash-NVFP4".to_string())
}

pub async fn bind_d4f_backend(
    node: &EmbeddedNode,
    identity: &dyn AgentIdentity,
) -> (String, String) {
    let agent_did = identity.did().to_string();
    let mut principal = ensure_agent_principal(node, &agent_did)
        .await
        .expect("ensure principal");
    let behavior_id = default_behavior_id_for_agent(&agent_did);
    let profile_id = default_inference_profile_id_for_behavior(&behavior_id);
    principal.default_behavior_id = Some(behavior_id.clone());
    let backend = d4f_backend(&agent_did);
    let profile = InferenceProfile {
        agent_did: agent_did.clone(),
        profile_id: profile_id.clone(),
        backend_id: D4F_BACKEND_ID.to_string(),
        model_name: d4f_model(),
        ..Default::default()
    };
    let behavior = AgentBehavior {
        behavior_id: behavior_id.clone(),
        agent_did: agent_did.clone(),
        display_name: Some("Live default behavior".to_string()),
        description: None,
        context_id: None,
        inference_profile_id: profile_id,
        enabled: true,
        tags: Vec::new(),
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    };

    apply_d4f_documents(node, principal, backend, profile, behavior).await;

    debug_assert_eq!(behavior_id, default_behavior_id_for_agent(&agent_did));
    (agent_did, behavior_id)
}

fn d4f_backend(agent_did: &str) -> InferenceBackend {
    InferenceBackend {
        agent_did: agent_did.to_string(),
        backend_id: D4F_BACKEND_ID.to_string(),
        name: D4F_BACKEND_ID.to_string(),
        provider_kind: BackendProviderKind::OpenAiCompatible,
        openai_wire_api: Some(OpenAiWireApi::ChatCompletions),
        endpoint: d4f_endpoint(),
        auth: BackendAuth::Unauthenticated,
        connect_timeout_secs: None,
        discovery_timeout_secs: None,
        max_concurrent: Some(4),
        max_queue_depth: Some(100),
        enabled: true,
        tags: Vec::new(),
    }
}

async fn apply_d4f_documents(
    node: &EmbeddedNode,
    principal: AgentPrincipal,
    backend: InferenceBackend,
    profile: InferenceProfile,
    behavior: AgentBehavior,
) {
    use gents::config_client::{
        apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    let plan = DesiredStateApplyPlan::new(
        [
            (Collection::AgentPrincipal, serde_json::to_value(principal)),
            (Collection::InferenceBackend, serde_json::to_value(backend)),
            (Collection::InferenceProfile, serde_json::to_value(profile)),
            (Collection::AgentBehavior, serde_json::to_value(behavior)),
        ]
        .into_iter()
        .map(|(collection, value)| {
            let value = value.expect("serialize d4f configuration document");
            DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            }
        })
        .collect(),
    )
    .expect("build d4f backend plan");
    gents::ConfigAccess::transact_local(node, None, "test.bind_d4f_backend", |txn| {
        let plan = &plan;
        Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
    })
    .await
    .expect("upsert d4f backend");
}

pub async fn boot_d4f_agent(db: &TestDb, identity: Arc<dyn AgentIdentity>) -> Result<BootedAgent> {
    let agent = Gents::from_default_behavior_documents(
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

async fn assert_d4f_reachable() {
    let url = format!("{}/models", d4f_endpoint().trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client");
    let resp = tokio::time::timeout(Duration::from_secs(20), client.get(&url).send()).await;
    match resp {
        Ok(Ok(r)) if r.status().is_success() => {}
        Ok(Ok(r)) => panic!("d4f endpoint {url} returned status {}", r.status()),
        Ok(Err(e)) => panic!("d4f endpoint {url} unreachable: {e}"),
        Err(_) => panic!("d4f endpoint {url} timed out (not reachable)"),
    }
}

fn is_terminal(state: &str) -> bool {
    RequestLifecycleState::is_terminal_str(Some(state))
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
    first_optional_row::<Row>(&resp, "AgentRequest").and_then(|r| r.lifecycle_state)
}

pub async fn wait_for_request_terminal(
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

async fn fetch_assistant_answer(node: &EmbeddedNode, request_id: &str) -> String {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentResponse(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                content
                session_id
            }}
        }}"#
    );
    #[derive(Deserialize)]
    struct RespRow {
        content: Option<String>,
        session_id: Option<String>,
    }
    let resp = node.execute(&query).await;
    let row = first_optional_row::<RespRow>(&resp, "AgentResponse");
    if let Some(row) = &row {
        if let Some(content) = row.content.as_deref() {
            if !content.trim().is_empty() {
                return content.to_string();
            }
        }
    }
    let session_id = match row.and_then(|r| r.session_id) {
        Some(s) if !s.is_empty() => s,
        _ => return String::new(),
    };
    let escaped_session = escape_graphql_string(&session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{escaped_session}" }}, role: {{ _eq: "assistant" }} }},
                order: {{ sequence: DESC }},
                limit: 1
            ) {{ content }}
        }}"#
    );
    #[derive(Deserialize)]
    struct MsgRow {
        content: String,
    }
    let resp = node.execute(&query).await;
    first_optional_row::<MsgRow>(&resp, "AgentMessage")
        .map(|m| m.content)
        .unwrap_or_default()
}

pub async fn wait_for_assistant_answer(
    node: &EmbeddedNode,
    request_id: &str,
    timeout: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let answer = fetch_assistant_answer(node, request_id).await;
        if !answer.trim().is_empty() {
            return answer;
        }
        if tokio::time::Instant::now() >= deadline {
            return answer;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: set GENTS_D4F_LIVE=1 and pass --ignored"]
async fn d4f_backend_probes_healthy_and_completes() {
    assert!(
        d4f_enabled(),
        "set GENTS_D4F_LIVE=1 and pass --ignored to run the d4f live smoke test"
    );

    assert_d4f_reachable().await;

    let db = test_db("steward-loop-d4f-smoke").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("steward-loop-d4f-smoke"));

    let (agent_did, behavior_id) = bind_d4f_backend(db.node.as_ref(), identity.as_ref()).await;

    let agent = boot_d4f_agent(&db, identity).await.expect("boot d4f agent");

    let request_id = "req-d4f-smoke";
    let session_id = "session-d4f-smoke";
    let started = std::time::Instant::now();
    create_runtime_request(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        request_id,
        session_id,
        "Reply with the single word: ok",
    )
    .await;

    let terminal =
        wait_for_request_terminal(db.node.as_ref(), request_id, Duration::from_secs(120)).await;
    let elapsed = started.elapsed();
    eprintln!("[d4f-smoke] request terminal state = {terminal} (latency {elapsed:?})");
    assert_eq!(
        terminal, "completed",
        "d4f-backed request must complete; got {terminal}"
    );

    let answer =
        wait_for_assistant_answer(db.node.as_ref(), request_id, Duration::from_secs(30)).await;
    eprintln!("[d4f-smoke] assistant answer = {answer:?}");
    assert!(
        !answer.trim().is_empty(),
        "d4f must produce a non-empty assistant response; got empty"
    );
    if answer.to_lowercase().contains("ok") {
        eprintln!("[d4f-smoke] OK: answer contains 'ok'");
    } else {
        eprintln!("[d4f-smoke] SOFT-WARN: answer did not contain 'ok': {answer:?}");
    }

    agent.shutdown().await;
}
