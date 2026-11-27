use std::time::Duration;

use gents::defra_node::{Node as EmbeddedNode, QueryResponse};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_store::graphql::escape_graphql_string;
use gents_store::test_support::snapshot_reader::SnapshotReader;
use serde::Deserialize;
use serde_json::Value;

use super::snapshots::fetch_request_snapshot;
use gents_store::test_support::{read_when_changed, STALL_CEILING};

const TEST_MUTATION_MAX_RETRIES: u32 = 3;
pub const TEST_RUNTIME_READY_TIMEOUT: Duration = Duration::from_secs(120);

pub struct BootedAgent {
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    handle: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    signal_shutdown_on_drop: bool,
    pub node_did: String,
}

impl BootedAgent {
    pub fn new(
        shutdown_tx: tokio::sync::watch::Sender<bool>,
        handle: tokio::task::JoinHandle<anyhow::Result<()>>,
        node_did: String,
    ) -> Self {
        Self {
            shutdown_tx,
            handle: Some(handle),
            signal_shutdown_on_drop: true,
            node_did,
        }
    }

    pub fn from_running(running: gents::runtime::embedded::RunningRuntime) -> Self {
        Self::new(running.shutdown, running.handle, running.node_did)
    }

    pub async fn shutdown(mut self) {
        let _ = self.shutdown_tx.send(true);
        let Some(handle) = self.handle.take() else {
            return;
        };
        tokio::time::timeout(STALL_CEILING, handle)
            .await
            .unwrap_or_else(|error| {
                panic!("agent did not shut down within {STALL_CEILING:?}: {error}")
            })
            .expect("agent task should join")
            .expect("agent run should return ok");
    }

    /// Establishes a deterministic crash boundary: no live observer may race
    /// the subsequent startup sweep, and graceful shutdown must not author
    /// terminal lifecycle state first.
    pub async fn crash(mut self) {
        self.signal_shutdown_on_drop = false;
        let Some(handle) = self.handle.take() else {
            return;
        };
        handle.abort();
        let error = handle
            .await
            .expect_err("aborted agent task must be cancelled");
        assert!(error.is_cancelled(), "aborted agent task: {error}");
    }
}

impl Drop for BootedAgent {
    fn drop(&mut self) {
        if self.signal_shutdown_on_drop {
            let _ = self.shutdown_tx.send(true);
        }
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

/// The production runtime on the test Node's own identity, admitted through
/// the home's backend protection, before its installed Agents are added with
/// their identity bindings.
pub fn managed_runtime_builder(
    db: &gents_store::test_support::TestStore,
    default_agent_did: &str,
) -> gents::GentsBuilder {
    gents::Gents::builder()
        .node(db.node.clone())
        .identity(db.node_identity.clone())
        .backend_admission(db.backend_admission())
        .default_agent_did(default_agent_did)
        .tool_ceiling(gents_tools::surface::ToolCeiling::meta_only())
}

/// Runs a built runtime until it reports ready.
pub async fn spawn_runtime(
    db: &gents_store::test_support::TestStore,
    runtime: gents::Gents,
) -> BootedAgent {
    let running = gents::runtime::embedded::start_running_within(
        db.node.as_ref(),
        runtime,
        TEST_RUNTIME_READY_TIMEOUT,
    )
    .await
    .expect("runtime becomes ready");
    BootedAgent::from_running(running)
}

pub async fn wait_for_runtime_ready(node: &EmbeddedNode, node_did: &str) {
    gents::runtime::embedded::wait_for_runtime_ready(node, node_did)
        .await
        .expect("runtime ready");
}

pub async fn create_runtime_request(
    node: &EmbeddedNode,
    node_did: &str,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    content: &str,
) -> String {
    ensure_generated_session(node, node_did, agent_did, session_id).await;

    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        request_id,
        gents_protocol::request_admission::RequestScope::new(
            node_did, node_did, agent_did, session_id,
        ),
        content,
        "interactive",
        created_at,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(node_did),
    );
    gents_session::request::admission::sign_agent_request_create_as_registered_target(&mut create)
        .await
        .expect("sign local-self runtime test request");
    let mutation = create
        .graphql_mutation()
        .expect("render canonical runtime test request");
    let response =
        execute_mutation_with_transaction_retry(node, &mutation, "create_runtime_request").await;
    assert!(
        !response.has_errors(),
        "create runtime AgentRequest failed: {:?}",
        response.errors
    );
    lookup_request_doc_id(node, request_id).await
}

async fn ensure_generated_session(
    node: &EmbeddedNode,
    node_did: &str,
    agent_did: &str,
    session_id: &str,
) {
    if super::snapshots::fetch_session_snapshot(node, session_id)
        .await
        .is_some()
    {
        return;
    }
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut session = super::session_document(session_id, agent_did, &now);
    session.node_did = node_did.to_owned();
    session.requester_did = Some(node_did.to_owned());
    session.title = Some(gents_protocol::session::SessionTitle {
        text: "generated-title".into(),
        source: gents_protocol::session::SessionTitleSource::Generated,
    });
    super::create_session_document(node, &session).await;
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

pub async fn wait_for_request_lifecycle_state<R>(node: &R, request_doc_id: &str, expected: &str)
where
    R: SnapshotReader + ?Sized,
{
    let expected = RequestLifecycleState::parse(expected)
        .expect("expected request lifecycle state must be canonical");
    read_when_changed(
        node,
        "AgentRequest",
        &format!("{request_doc_id} reaching lifecycle_state={expected}"),
        async || {
            let snapshot = fetch_request_snapshot(node, request_doc_id).await;
            if snapshot.lifecycle_state == expected {
                return Ok(());
            }
            Err(format!(
                "lifecycle_state={}; failure_reason={:?}",
                snapshot.lifecycle_state, snapshot.failure_reason
            ))
        },
    )
    .await
}

pub async fn wait_for_request_terminal_state<R>(
    node: &R,
    request_doc_id: &str,
) -> RequestLifecycleState
where
    R: SnapshotReader + ?Sized,
{
    read_when_changed(
        node,
        "AgentRequest",
        &format!("{request_doc_id} terminalizing"),
        async || {
            let snapshot = fetch_request_snapshot(node, request_doc_id).await;
            if snapshot.lifecycle_state.is_terminal() {
                return Ok(snapshot.lifecycle_state);
            }
            Err(format!(
                "lifecycle_state={}; failure_reason={:?}",
                snapshot.lifecycle_state, snapshot.failure_reason
            ))
        },
    )
    .await
}

#[derive(Debug, Clone, Deserialize)]
pub struct InferenceCallSnapshot {
    pub call_seq: i64,
    pub call_state: String,
    pub failure_reason: Option<String>,
}

/// The daemon retries transient provider failures, so a historical failed
/// attempt must not hide the current attempt's state.
pub async fn wait_for_inference_call_state<R>(
    node: &R,
    request_id: &str,
    expected: &str,
) -> InferenceCallSnapshot
where
    R: SnapshotReader + ?Sized,
{
    read_when_changed(
        node,
        "InferenceCall",
        &format!("request_id={request_id} reaching call_state={expected}"),
        async || match fetch_latest_inference_call(node, request_id).await {
            Some(row) if row.call_state == expected => Ok(row),
            last => Err(format!("{last:?}")),
        },
    )
    .await
}

async fn fetch_latest_inference_call<R>(node: &R, request_id: &str) -> Option<InferenceCallSnapshot>
where
    R: SnapshotReader + ?Sized,
{
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
    node.read_data(&query)
        .await
        .get("InferenceCall")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .map(|value| serde_json::from_value(value).expect("decode InferenceCallSnapshot"))
}

async fn execute_mutation_with_transaction_retry(
    node: &EmbeddedNode,
    mutation: &str,
    operation: &str,
) -> QueryResponse {
    let mut last_response = None;
    for attempt in 0..=TEST_MUTATION_MAX_RETRIES {
        if attempt > 0 {
            tracing::warn!(
                operation = %operation,
                attempt = attempt,
                "retrying test setup mutation after transaction conflict"
            );
            tokio::task::yield_now().await;
        }

        let response = node.execute(mutation).await;
        if !response.has_errors() || !response_has_transaction_conflict(&response) {
            return response;
        }
        last_response = Some(response);
    }

    last_response.expect("retry loop always stores the failed response")
}

fn response_has_transaction_conflict(response: &QueryResponse) -> bool {
    let error_text = format!("{:?}", response.errors);
    error_text.contains("transaction conflict") || error_text.contains("Please retry")
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
