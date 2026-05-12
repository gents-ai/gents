use std::time::Duration;

use defra_agent::defra_node::{EmbeddedNode, QueryResponse};
use defra_agent::graphql::escape_graphql_string;
use serde::Deserialize;
use serde_json::Value;

use super::snapshots::{fetch_request_snapshot, fetch_response_content, fetch_runtime_snapshot};

pub struct BootedAgent {
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    handle: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    pub agent_did: String,
}

impl BootedAgent {
    pub fn new(
        shutdown_tx: tokio::sync::watch::Sender<bool>,
        handle: tokio::task::JoinHandle<anyhow::Result<()>>,
        agent_did: String,
    ) -> Self {
        Self {
            shutdown_tx,
            handle: Some(handle),
            agent_did,
        }
    }

    pub async fn shutdown(mut self) {
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

pub async fn wait_for_runtime_ready(node: &EmbeddedNode, agent_did: &str) {
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

pub async fn create_runtime_request(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    request_id: &str,
    session_id: &str,
    content: &str,
) -> String {
    upsert_generated_conversation(node, agent_did, behavior_id, session_id).await;

    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_behavior_id = escape_graphql_string(behavior_id);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_content = escape_graphql_string(content);
    let created_at = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                agent_did: "{escaped_agent_did}",
                behavior_id: "{escaped_behavior_id}",
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
        "create runtime AgentRequest failed: {:?}",
        response.errors
    );
    lookup_request_doc_id(node, request_id).await
}

async fn upsert_generated_conversation(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    session_id: &str,
) {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_behavior_id = escape_graphql_string(behavior_id);
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            upsert_AgentConversation(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                add: {{
                    session_id: "{escaped_session_id}",
                    agent_name: "{escaped_behavior_id}",
                    agent_did: "{escaped_agent_did}",
                    behavior_id: "{escaped_behavior_id}",
                    title: "generated-title",
                    title_source: "generated",
                    preview_text: "",
                    status: "active",
                    created_at: "{now}",
                    updated_at: "{now}",
                    latest_request_id: ""
                }},
                update: {{
                    agent_name: "{escaped_behavior_id}",
                    agent_did: "{escaped_agent_did}",
                    behavior_id: "{escaped_behavior_id}",
                    title: "generated-title",
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
        "upsert generated conversation failed: {:?}",
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

pub async fn wait_for_response_doc_id(node: &EmbeddedNode, request_id: &str) -> String {
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

pub async fn wait_for_response_content_contains(
    node: &EmbeddedNode,
    response_doc_id: &str,
    expected: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let content = fetch_response_content(node, response_doc_id).await;
        if content.contains(expected) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for response content to contain {expected:?}; last={content:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub async fn wait_for_response_content_min_len(
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

pub async fn wait_for_request_lifecycle_state(
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
pub struct InferenceCallSnapshot {
    pub call_seq: i64,
    pub call_state: String,
    pub failure_reason: Option<String>,
}

/// Wait for the latest inference attempt for `request_id` to reach `expected`.
///
/// The daemon retries transient provider failures, so a historical failed
/// attempt must not hide the current attempt's running or terminal state.
pub async fn wait_for_inference_call_state(
    node: &EmbeddedNode,
    request_id: &str,
    expected: &str,
) -> InferenceCallSnapshot {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let row = fetch_latest_inference_call(node, request_id).await;
        if row
            .as_ref()
            .is_some_and(|row| row.call_state.as_str() == expected)
        {
            return row.expect("checked Some");
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for latest inference call request_id={request_id} call_state={expected}; last={row:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn fetch_latest_inference_call(
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
                order: {{ call_seq: DESC }},
                limit: 1
            ) {{
                call_seq
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
