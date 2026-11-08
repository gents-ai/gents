use std::time::Duration;

use gents::defra_node::{Node as EmbeddedNode, QueryResponse};
use gents::session::canonical_rows::{
    decode_output_segment_row, OutputSegmentRow, AGENT_OUTPUT_SEGMENT_FIELDS,
};
use gents::store::graphql::escape_graphql_string;
use gents_protocol::output::live::reconstruct_dense_prefix;
use gents_protocol::output::reconstruction::ObservedSegment;
use gents_protocol::output::OutputSource;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::Deserialize;
use serde_json::Value;

use super::snapshots::{fetch_request_snapshot, SnapshotReader};

const TEST_MUTATION_MAX_RETRIES: u32 = 3;
const TEST_MUTATION_INITIAL_BACKOFF_MS: u64 = 100;
pub const TEST_RUNTIME_READY_TIMEOUT: Duration = Duration::from_secs(120);
const TEST_RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
pub const REQUEST_TERMINAL_PUBLICATION_MARGIN: Duration = Duration::from_secs(60);

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

    pub async fn shutdown(mut self) {
        let _ = self.shutdown_tx.send(true);
        let Some(handle) = self.handle.take() else {
            return;
        };
        tokio::time::timeout(TEST_RUNTIME_SHUTDOWN_TIMEOUT, handle)
            .await
            .unwrap_or_else(|error| {
                panic!("agent did not shut down within {TEST_RUNTIME_SHUTDOWN_TIMEOUT:?}: {error}")
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

/// Waits for readiness while the runtime task runs; a runtime that exits first
/// fails the wait with its own error instead of the readiness timeout.
pub async fn wait_for_runtime_ready_or_exit(
    node: &EmbeddedNode,
    node_did: &str,
    handle: &mut tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    tokio::select! {
        ready = gents::eval::runner::embedded::wait_for_runtime_ready(node, node_did) => {
            ready.expect("runtime ready");
        }
        exited = &mut *handle => match exited {
            Ok(Ok(())) => panic!("runtime exited cleanly before readiness"),
            Ok(Err(error)) => panic!("runtime exited before readiness: {error:#}"),
            Err(error) => panic!("runtime task failed before readiness: {error}"),
        },
    }
}

/// The production runtime on the test Node's own identity, admitted through
/// the home's backend protection, before its installed Agents are added with
/// their identity bindings.
pub fn managed_runtime_builder(db: &super::TestDb, default_agent_did: &str) -> gents::GentsBuilder {
    gents::Gents::builder()
        .node(db.node.clone())
        .identity(db.node_identity.clone())
        .backend_admission(db.home.backend_admission())
        .default_agent_did(default_agent_did)
        .tool_ceiling(gents::ToolCeiling::meta_only())
}

/// Runs a built runtime until it reports ready.
pub async fn spawn_runtime(db: &super::TestDb, runtime: gents::Gents) -> BootedAgent {
    let node_did = runtime.node_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let mut handle = tokio::spawn(runtime.run(shutdown_rx));
    wait_for_runtime_ready_or_exit(db.node.as_ref(), &node_did, &mut handle).await;
    BootedAgent::new(shutdown_tx, handle, node_did)
}

pub async fn wait_for_runtime_ready(node: &EmbeddedNode, node_did: &str) {
    gents::eval::runner::embedded::wait_for_runtime_ready(node, node_did)
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
    create_runtime_request_inner(
        node,
        node_did,
        RuntimeRequestSpec {
            agent_did,
            request_id,
            session_id,
            valid_until: None,
            execution_origin: "interactive",
            caused_by_source_doc_id: None,
            content,
            input: Default::default(),
        },
    )
    .await
}

pub async fn create_runtime_request_with_input(
    node: &EmbeddedNode,
    node_did: &str,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    content: &str,
    input: gents_protocol::request_input::RequestInput,
) -> String {
    create_runtime_request_inner(
        node,
        node_did,
        RuntimeRequestSpec {
            agent_did,
            request_id,
            session_id,
            content,
            input,
            valid_until: None,
            execution_origin: "interactive",
            caused_by_source_doc_id: None,
        },
    )
    .await
}

pub async fn create_runtime_request_with_valid_until(
    node: &EmbeddedNode,
    node_did: &str,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    valid_until: &str,
    content: &str,
) -> String {
    create_runtime_request_inner(
        node,
        node_did,
        RuntimeRequestSpec {
            agent_did,
            request_id,
            session_id,
            valid_until: Some(valid_until),
            execution_origin: "interactive",
            caused_by_source_doc_id: None,
            content,
            input: Default::default(),
        },
    )
    .await
}

pub async fn create_runtime_request_with_execution_origin(
    node: &EmbeddedNode,
    node_did: &str,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    execution_origin: &str,
    content: &str,
) -> String {
    create_runtime_request_inner(
        node,
        node_did,
        RuntimeRequestSpec {
            agent_did,
            request_id,
            session_id,
            valid_until: None,
            execution_origin,
            caused_by_source_doc_id: None,
            content,
            input: Default::default(),
        },
    )
    .await
}

pub async fn create_runtime_request_caused_by_source(
    node: &EmbeddedNode,
    node_did: &str,
    agent_did: &str,
    request_id: &str,
    session_id: &str,
    caused_by_source_doc_id: &str,
    content: &str,
) -> String {
    create_runtime_request_inner(
        node,
        node_did,
        RuntimeRequestSpec {
            agent_did,
            request_id,
            session_id,
            valid_until: None,
            execution_origin: "interactive",
            caused_by_source_doc_id: Some(caused_by_source_doc_id),
            content,
            input: Default::default(),
        },
    )
    .await
}

struct RuntimeRequestSpec<'a> {
    agent_did: &'a str,
    request_id: &'a str,
    session_id: &'a str,
    valid_until: Option<&'a str>,
    execution_origin: &'a str,
    caused_by_source_doc_id: Option<&'a str>,
    content: &'a str,
    input: gents_protocol::request_input::RequestInput,
}

async fn create_runtime_request_inner(
    node: &EmbeddedNode,
    node_did: &str,
    spec: RuntimeRequestSpec<'_>,
) -> String {
    let RuntimeRequestSpec {
        agent_did,
        request_id,
        session_id,
        valid_until,
        execution_origin,
        caused_by_source_doc_id,
        content,
        input,
    } = spec;
    ensure_generated_session(node, node_did, agent_did, session_id).await;

    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        request_id,
        node_did,
        node_did,
        agent_did,
        session_id,
        content,
        execution_origin,
        created_at,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(node_did),
    );
    create.input = input;
    create.max_retries = i64::from(gents::request::lifecycle::DEFAULT_REQUEST_MAX_RETRIES);
    create.valid_until = valid_until.map(str::to_string);
    create.caused_by_source_doc_id = caused_by_source_doc_id.map(str::to_string);
    gents::sign_agent_request_create_as_registered_target(&mut create)
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

pub async fn fetch_output_segments_for_request<R>(
    reader: &R,
    request_doc_id: &str,
) -> Vec<OutputSegmentRow>
where
    R: SnapshotReader + ?Sized,
{
    let request_doc_id = escape_graphql_string(request_doc_id);
    let data = reader
        .read_data(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request_doc_id}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#
        ))
        .await;
    data.get("AgentOutputSegment")
        .and_then(Value::as_array)
        .expect("canonical output rows")
        .iter()
        .map(|row| decode_output_segment_row(row).expect("decode canonical output segment"))
        .collect()
}

pub async fn wait_for_provider_output_contains<R>(node: &R, request_doc_id: &str, expected: &str)
where
    R: SnapshotReader + ?Sized,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let rows = fetch_output_segments_for_request(node, request_doc_id).await;
        let records = rows
            .iter()
            .map(|row| ObservedSegment {
                doc_id: &row.doc_id,
                segment: &row.segment,
            })
            .collect::<Vec<_>>();
        let visible = rows.iter().find_map(|row| {
            matches!(&row.segment.source, OutputSource::ProviderTurn { .. })
                .then(|| {
                    reconstruct_dense_prefix(
                        &records,
                        request_doc_id,
                        &row.segment.source,
                        &row.segment.writer,
                        None,
                    )
                    .ok()
                })
                .flatten()
                .map(|prefix| {
                    prefix
                        .streams
                        .into_iter()
                        .map(|stream| stream.text)
                        .collect::<String>()
                })
        });
        if visible
            .as_deref()
            .is_some_and(|text| text.contains(expected))
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for canonical provider output to contain {expected:?}; last={visible:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub async fn wait_for_request_lifecycle_state<R>(node: &R, request_doc_id: &str, expected: &str)
where
    R: SnapshotReader + ?Sized,
{
    let expected = RequestLifecycleState::parse(expected)
        .expect("expected request lifecycle state must be canonical");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let snapshot = fetch_request_snapshot(node, request_doc_id).await;
        if snapshot.lifecycle_state == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for AgentRequest {request_doc_id} lifecycle_state={expected}; last={}; failure_reason={:?}",
            snapshot.lifecycle_state,
            snapshot.failure_reason
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// `budget` must cover the request's own timing envelope plus
/// [`REQUEST_TERMINAL_PUBLICATION_MARGIN`]; terminal publication is
/// scheduler-bound on a loaded host, so a short fixed bound flakes under the
/// full parallel suite.
pub async fn wait_for_request_terminal_state<R>(
    node: &R,
    request_doc_id: &str,
    budget: Duration,
) -> RequestLifecycleState
where
    R: SnapshotReader + ?Sized,
{
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let snapshot = fetch_request_snapshot(node, request_doc_id).await;
        if snapshot.lifecycle_state.is_terminal() {
            return snapshot.lifecycle_state;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for AgentRequest {request_doc_id} terminal state; last={}; failure_reason={:?}",
            snapshot.lifecycle_state,
            snapshot.failure_reason
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
            let backoff =
                Duration::from_millis(TEST_MUTATION_INITIAL_BACKOFF_MS * (1u64 << (attempt - 1)));
            tracing::warn!(
                operation = %operation,
                attempt = attempt,
                backoff_ms = backoff.as_millis() as u64,
                "retrying test setup mutation after transaction conflict"
            );
            tokio::time::sleep(backoff).await;
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
