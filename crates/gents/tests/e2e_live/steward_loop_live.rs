//! Plan 2 Phase 2a — d4f-backed LIVE test harness for the steward emit->triage
//! loop.
//!
//! Plan 1 proved the loop deterministically against a MOCK backend (the
//! `event_trigger_e2e` / `write_tool_trigger_e2e` tests only assert that an
//! `AgentRequest` *materializes*). Phase 2a qualifies the model-dependent
//! decisions against a REAL model: `d4f` (DeepSeek-V4-Flash on workstation-1,
//! OpenAI-compatible). Task 2a-1 (this file) builds the foundation:
//!
//!   * `bind_d4f_backend` — writes an `InferenceBackend` doc pointing at the live
//!     d4f endpoint with `models: ["d4f"]` and points the agent's default behavior
//!     at it (`backend_id` + `model_name = "d4f"`). Reusable by 2a-2 / 2a-3.
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
//!   --features defra-node/http,defra-node/p2p,rocksdb \
//!   d4f_backend_probes_healthy_and_completes \
//!   -- --ignored --test-threads=1 --nocapture
//! ```

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{
    default_behavior_id_for_agent, default_inference_profile_id_for_behavior,
    ensure_agent_principal, load_agent_behavior, upsert_agent_behavior, AgentIdentity,
    DocumentRuntimeOptions, Gents, ToolCeiling,
};
use serde::Deserialize;

use crate::support::fixtures::test_identity;
use crate::support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use crate::support::{first_optional_row, test_db, TestDb};

fn d4f_enabled() -> bool {
    std::env::var("GENTS_D4F_LIVE").as_deref() == Ok("1")
}

const D4F_ENDPOINT: &str = "http://100.73.235.38:8000/v1";
const D4F_MODEL: &str = "d4f";
const D4F_BACKEND_ID: &str = "backend-d4f-live";

pub async fn bind_d4f_backend(
    node: &EmbeddedNode,
    identity: &dyn AgentIdentity,
) -> (String, String) {
    let agent_did = identity.did().to_string();
    let bootstrap = ensure_agent_principal(node, &agent_did)
        .await
        .expect("ensure principal");
    let behavior_id = bootstrap.default_behavior.behavior_id.clone();

    upsert_d4f_backend(node).await;

    let mut behavior = load_agent_behavior(node, &behavior_id)
        .await
        .expect("load default behavior")
        .expect("default behavior document exists after bootstrap");
    behavior.backend_id = Some(D4F_BACKEND_ID.to_string());
    behavior.model_name = Some(D4F_MODEL.to_string());
    behavior.inference_profile_id = Some(default_inference_profile_id_for_behavior(&behavior_id));
    behavior.enabled = true;
    upsert_agent_behavior(node, &behavior)
        .await
        .expect("point default behavior at d4f");

    debug_assert_eq!(behavior_id, default_behavior_id_for_agent(&agent_did));
    (agent_did, behavior_id)
}

async fn upsert_d4f_backend(node: &EmbeddedNode) {
    let escaped_backend_id = escape_graphql_string(D4F_BACKEND_ID);
    let escaped_endpoint = escape_graphql_string(D4F_ENDPOINT);
    let escaped_model = escape_graphql_string(D4F_MODEL);
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
        "upsert d4f backend failed: {:?}",
        response.errors
    );
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
    let url = format!("{}/models", D4F_ENDPOINT.trim_end_matches('/'));
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
    matches!(
        state,
        "completed" | "failed" | "dead" | "interrupted" | "superseded"
    )
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
