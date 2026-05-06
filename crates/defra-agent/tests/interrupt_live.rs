//! Live end-to-end interrupt test against an OpenAI-compatible streaming backend.
//!
//! Normal test runs skip this file because it depends on a reachable live
//! inference service. To run it locally:
//!
//! ```bash
//! DEFRA_AGENT_LIVE_OPENAI=1 \
//! DEFRA_AGENT_LIVE_OPENAI_ENDPOINT=http://100.74.68.88:8000/v1 \
//! DEFRA_AGENT_LIVE_OPENAI_MODEL=Qwen3.5-122B-A10B-NVFP4 \
//! cargo test -p defra-agent --test interrupt_live -- --ignored --nocapture
//! ```

mod support;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use defra_agent::defra_node::{EmbeddedNode, QueryResponse};
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{interrupt_request, AgentIdentity, DefraAgent, ToolCeiling};
use serde::Deserialize;
use serde_json::Value;

use support::fixtures::test_identity;
use support::snapshots::{
    fetch_request_snapshot, fetch_response_content, fetch_response_interrupted_at,
    fetch_runtime_snapshot,
};

const DEFAULT_LIVE_ENDPOINT: &str = "http://100.74.68.88:8000/v1";
const DEFAULT_LIVE_MODEL: &str = "Qwen3.5-122B-A10B-NVFP4";
const LIVE_BACKEND_ID: &str = "backend-live-openai-interrupt";
const LIVE_BEHAVIOR_ID: &str = "live-interrupt";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: set DEFRA_AGENT_LIVE_OPENAI=1 and pass --ignored"]
async fn live_interrupt_mid_stream_on_openai_compatible() -> Result<()> {
    if std::env::var("DEFRA_AGENT_LIVE_OPENAI").as_deref() != Ok("1") {
        eprintln!("DEFRA_AGENT_LIVE_OPENAI is not 1; skipping live interrupt smoke");
        return Ok(());
    }

    let endpoint = std::env::var("DEFRA_AGENT_LIVE_OPENAI_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_LIVE_ENDPOINT.to_string());
    let model = std::env::var("DEFRA_AGENT_LIVE_OPENAI_MODEL")
        .unwrap_or_else(|_| DEFAULT_LIVE_MODEL.to_string());
    let api_key = std::env::var("DEFRA_AGENT_LIVE_OPENAI_API_KEY").unwrap_or_default();

    let db = support::test_db("live-openai-interrupt").await;
    let agent = boot_live_agent(&db, &endpoint, &model, &api_key).await?;

    let request_id = "req-live-openai-interrupt";
    let session_id = "session-live-openai-interrupt";
    let request_doc_id = create_runtime_request(
        db.node.as_ref(),
        agent.agent_did.as_str(),
        request_id,
        session_id,
        "Write a long numbered list from 1 to 200. Use one short sentence per item. Start immediately and keep streaming.",
    )
    .await;

    let response_doc_id = wait_for_response_doc_id(db.node.as_ref(), request_id).await;
    let before_interrupt =
        wait_for_response_content_min_len(db.node.as_ref(), &response_doc_id, 40).await;

    interrupt_request(db.node.as_ref(), request_id)
        .await
        .expect("interrupt_request should latch interrupt_requested_at");

    wait_for_request_lifecycle_state(db.node.as_ref(), &request_doc_id, "interrupted").await;
    let call = wait_for_inference_call_state(db.node.as_ref(), request_id, "cancelled").await;
    assert_eq!(call.failure_reason.as_deref(), Some("Cancelled"));

    let final_content = fetch_response_content(&db.node, &response_doc_id).await;
    assert!(
        final_content.starts_with(&before_interrupt),
        "live interrupt must preserve content streamed before the interrupt; before={before_interrupt:?} final={final_content:?}"
    );
    assert!(
        fetch_response_interrupted_at(&db.node, &response_doc_id)
            .await
            .is_some(),
        "live interrupt must stamp AgentResponse.interrupted_at"
    );

    tokio::time::sleep(Duration::from_millis(750)).await;
    let settled_content = fetch_response_content(&db.node, &response_doc_id).await;
    assert_eq!(
        settled_content, final_content,
        "response content should stop changing after interrupted lifecycle is persisted"
    );

    agent.shutdown().await;
    Ok(())
}

struct BootedAgent {
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    handle: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    agent_did: String,
}

impl BootedAgent {
    async fn shutdown(mut self) {
        let _ = self.shutdown_tx.send(true);
        let Some(handle) = self.handle.take() else {
            return;
        };
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("agent did not shut down within 5s")
            .expect("agent task should join")
            .expect("agent run should return ok");
    }
}

impl Drop for BootedAgent {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

async fn boot_live_agent(
    db: &support::TestDb,
    endpoint: &str,
    model: &str,
    api_key: &str,
) -> Result<BootedAgent> {
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("live-openai-interrupt"));
    upsert_live_backend(db.node.as_ref(), endpoint, model, api_key).await;

    let agent = DefraAgent::builder()
        .node(db.node.clone())
        .identity(identity)
        .default_behavior_id(LIVE_BEHAVIOR_ID)
        .tool_ceiling(ToolCeiling::meta_only())
        .behavior(LIVE_BEHAVIOR_ID)
        .backend_id(LIVE_BACKEND_ID)
        .model_name(model)
        .max_output_tokens(512)
        .stream_batch_ms(0)
        .done()
        .build()
        .await?;

    let agent_did = agent.agent_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;

    Ok(BootedAgent {
        shutdown_tx,
        handle: Some(handle),
        agent_did,
    })
}

async fn upsert_live_backend(node: &EmbeddedNode, endpoint: &str, model: &str, api_key: &str) {
    let escaped_backend_id = escape_graphql_string(LIVE_BACKEND_ID);
    let escaped_endpoint = escape_graphql_string(endpoint);
    let escaped_model = escape_graphql_string(model);
    let escaped_api_key = escape_graphql_string(api_key);
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                add: {{
                    backend_id: "{escaped_backend_id}",
                    name: "{escaped_backend_id}",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "{escaped_endpoint}",
                    api_key: "{escaped_api_key}",
                    api_key_env_var: "",
                    max_concurrent: 2,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{escaped_model}"],
                    probe_status: "healthy"
                }},
                update: {{
                    name: "{escaped_backend_id}",
                    provider_kind: "OpenAiCompatible",
                    endpoint: "{escaped_endpoint}",
                    api_key: "{escaped_api_key}",
                    api_key_env_var: "",
                    max_concurrent: 2,
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

async fn wait_for_runtime_ready(node: &EmbeddedNode, agent_did: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(snapshot) = fetch_runtime_snapshot(node, agent_did).await {
            if snapshot.process_state == "ready"
                && snapshot.reconcile_phase == "idle"
                && snapshot.runnable_behavior_count >= 1
            {
                return;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "agent did not reach ready state"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn create_runtime_request(
    node: &EmbeddedNode,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    content: &str,
) -> String {
    upsert_generated_conversation(node, agent_did, session_id).await;

    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_content = escape_graphql_string(content);
    let created_at = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                agent_did: "{escaped_agent_did}",
                behavior_id: "{LIVE_BEHAVIOR_ID}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "",
                retry_root_request: "{escaped_request_id}",
                superseded_by_request: "",
                content: "{escaped_content}",
                status: "pending",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "interactive",
                created_at: "{created_at}",
                retry_count: 0,
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#,
        max_retries = defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create live AgentRequest failed: {:?}",
        response.errors
    );
    lookup_request_doc_id(node, request_id).await
}

async fn upsert_generated_conversation(node: &EmbeddedNode, agent_did: &str, session_id: &str) {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            upsert_AgentConversation(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                add: {{
                    session_id: "{escaped_session_id}",
                    agent_name: "{LIVE_BEHAVIOR_ID}",
                    agent_did: "{escaped_agent_did}",
                    behavior_id: "{LIVE_BEHAVIOR_ID}",
                    title: "live interrupt",
                    title_source: "generated",
                    preview_text: "",
                    status: "active",
                    created_at: "{now}",
                    updated_at: "{now}",
                    latest_request_id: ""
                }},
                update: {{
                    agent_name: "{LIVE_BEHAVIOR_ID}",
                    agent_did: "{escaped_agent_did}",
                    behavior_id: "{LIVE_BEHAVIOR_ID}",
                    title: "live interrupt",
                    title_source: "generated",
                    preview_text: "",
                    status: "active",
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "upsert live conversation failed: {:?}",
        response.errors
    );
}

async fn lookup_request_doc_id(node: &EmbeddedNode, request_id: &str) -> String {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 1) {{
                _docID
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    first_doc_id(&response, "AgentRequest")
}

async fn wait_for_response_doc_id(node: &EmbeddedNode, request_id: &str) -> String {
    let escaped_request_id = escape_graphql_string(request_id);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let query = format!(
            r#"{{
                AgentResponse(
                    filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                    limit: 1
                ) {{
                    _docID
                }}
            }}"#
        );
        let response = node.execute(&query).await;
        assert!(
            !response.has_errors(),
            "AgentResponse lookup failed: {:?}",
            response.errors
        );
        if let Some(doc_id) = optional_doc_id(&response, "AgentResponse") {
            return doc_id;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for AgentResponse for request_id={request_id}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_response_content_min_len(
    node: &EmbeddedNode,
    response_doc_id: &str,
    min_len: usize,
) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let content = fetch_response_content(node, response_doc_id).await;
        if content.len() >= min_len {
            return content;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for live response content length >= {min_len}; last={content:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_request_lifecycle_state(
    node: &EmbeddedNode,
    request_doc_id: &str,
    expected: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let snapshot = fetch_request_snapshot(node, request_doc_id).await;
        if snapshot.lifecycle_state == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for AgentRequest {request_doc_id} lifecycle_state={expected}; last={}",
            snapshot.lifecycle_state
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[derive(Debug, Clone, Deserialize)]
struct InferenceCallSnapshot {
    call_state: String,
    failure_reason: Option<String>,
}

async fn wait_for_inference_call_state(
    node: &EmbeddedNode,
    request_id: &str,
    expected: &str,
) -> InferenceCallSnapshot {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let row = fetch_inference_call(node, request_id).await;
        if row
            .as_ref()
            .is_some_and(|row| row.call_state.as_str() == expected)
        {
            return row.expect("checked Some");
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for inference call request_id={request_id} call_state={expected}; last={row:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn fetch_inference_call(
    node: &EmbeddedNode,
    request_id: &str,
) -> Option<InferenceCallSnapshot> {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{
                    request_id: {{ _eq: "{escaped_request_id}" }},
                    call_kind: {{ _eq: "inference" }}
                }},
                order: {{ call_seq: ASC }},
                limit: 1
            ) {{
                call_state
                failure_reason
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "InferenceCall query failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceCall"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .map(|value| serde_json::from_value(value).expect("decode InferenceCallSnapshot"))
}

fn first_doc_id(response: &QueryResponse, key: &str) -> String {
    optional_doc_id(response, key).unwrap_or_else(|| panic!("missing {key} _docID"))
}

fn optional_doc_id(response: &QueryResponse, key: &str) -> Option<String> {
    assert!(
        !response.has_errors(),
        "{key} doc id query failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get(key))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}
