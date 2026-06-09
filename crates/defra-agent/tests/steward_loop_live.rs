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
//!   * `boot_d4f_agent` — boots a full `DefraAgent` from those behavior documents
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
//! boot a full agent (`DefraAgent::from_default_behavior_documents` + `.run()`),
//! submit work by creating a `pending` `AgentRequest` via
//! `support::interrupt::create_runtime_request`, then WAIT for the request's
//! `lifecycle_state` to terminalize and read the assistant answer back from
//! `AgentResponse` / the latest assistant `AgentMessage`. That is the daemon
//! actually claiming the request, calling d4f, and persisting the completion —
//! not a mock "row appeared" assertion.
//!
//! ## Running
//!
//! Gated on `DEFRA_AGENT_D4F_LIVE=1`. Without it every test early-returns and
//! passes as a no-op, so offline/CI runs skip cleanly.
//!
//! ```bash
//! DEFRA_AGENT_D4F_LIVE=1 cargo test --test steward_loop_live \
//!   --features defra-node/http,defra-node/p2p,rocksdb \
//!   -- --test-threads=1 d4f_backend_probes_healthy_and_completes --nocapture
//! ```

mod support;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{
    default_behavior_id_for_agent, default_inference_profile_id_for_behavior,
    ensure_agent_principal, load_agent_behavior, upsert_agent_behavior, AgentIdentity, DefraAgent,
    DocumentRuntimeOptions, ToolCeiling,
};
use serde::Deserialize;

use support::fixtures::test_identity;
use support::interrupt::{create_runtime_request, wait_for_runtime_ready, BootedAgent};
use support::{first_optional_row, test_db, TestDb};

// ---------------------------------------------------------------------------
// Gate + d4f connection constants
// ---------------------------------------------------------------------------

/// Every test in this file early-returns unless this is set, so offline/CI runs
/// skip cleanly.
fn d4f_enabled() -> bool {
    std::env::var("DEFRA_AGENT_D4F_LIVE").as_deref() == Ok("1")
}

const D4F_ENDPOINT: &str = "http://100.73.235.38:8000/v1";
const D4F_MODEL: &str = "d4f";
const D4F_BACKEND_ID: &str = "backend-d4f-live";

// ---------------------------------------------------------------------------
// Reusable harness (2a-2 / 2a-3 call these)
// ---------------------------------------------------------------------------

/// Bind the agent's default behavior to the live d4f backend.
///
/// Modeled on `support::fixtures::bind_default_behavior_backend`, but pointed at
/// the real d4f endpoint: the `InferenceBackend` doc advertises `models: ["d4f"]`
/// and `probe_status: "healthy"`, and the default behavior is updated to use
/// `backend_id = D4F_BACKEND_ID` AND `model_name = "d4f"` (the mock helper leaves
/// `model_name` untouched; a real backend rejects an unknown model, so we MUST
/// set it).
///
/// Returns `(agent_did, default_behavior_id)` for the caller to drive work
/// against. Reusable by the emit (2a-2) and triage (2a-3) qualifications.
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

    // Point the default behavior at d4f. Load the bootstrapped document and
    // overwrite backend_id + model_name (a live backend rejects an unknown
    // model id, so model_name MUST be "d4f", not the bootstrap default).
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

/// Upsert the live d4f `InferenceBackend` document (OpenAI-compatible vLLM).
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

/// Boot a full `DefraAgent` from the behavior documents owned by `identity`'s
/// DID and wait for it to reach `ready`. Returns a `BootedAgent` whose
/// `.shutdown()` cleanly stops the daemon. Reusable by 2a-2 / 2a-3.
pub async fn boot_d4f_agent(db: &TestDb, identity: Arc<dyn AgentIdentity>) -> Result<BootedAgent> {
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

/// Assert the endpoint answers `GET /models` before we spend a full agent run on
/// an unreachable backend. Fails fast with a clear message.
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

/// Wait for `request_id`'s `lifecycle_state` to reach a terminal value (the
/// daemon ran it against d4f and finished). Returns the terminal state.
/// Reusable by 2a-2 / 2a-3 to await a full real-backend run.
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

/// Read the assistant answer for `request_id`: prefer the `AgentResponse`
/// content, fall back to the latest assistant `AgentMessage` on the request's
/// session. Mirrors the helper in `subagent_delegation_live.rs`. Reusable.
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

/// Poll until a non-empty assistant answer is available for `request_id`, or the
/// deadline passes (returns whatever was last seen). Reusable by 2a-2 / 2a-3.
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

// ---------------------------------------------------------------------------
// Smoke test (Step 2)
// ---------------------------------------------------------------------------

/// Boot an agent bound to d4f, submit one trivial unit of work, and prove the
/// REAL model actually completes it: the request terminalizes `completed` and a
/// non-empty assistant answer lands. This is the foundation 2a-2 / 2a-3 build on
/// (it exercises bind + boot + full-run-and-wait against d4f end to end).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d4f_backend_probes_healthy_and_completes() {
    if !d4f_enabled() {
        eprintln!("DEFRA_AGENT_D4F_LIVE is not 1; skipping d4f live smoke test");
        return;
    }

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

    // Wait for the daemon to actually claim the request, call d4f, and finish.
    // Generous deadline: d4f is a real model with real latency.
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
    // Tolerant content check: the model was asked to reply "ok". Don't assert
    // exact text (real models add punctuation/casing), just that it answered and
    // ideally acknowledged.
    if answer.to_lowercase().contains("ok") {
        eprintln!("[d4f-smoke] OK: answer contains 'ok'");
    } else {
        eprintln!("[d4f-smoke] SOFT-WARN: answer did not contain 'ok': {answer:?}");
    }

    agent.shutdown().await;
}
