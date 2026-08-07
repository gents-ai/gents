//! End-to-end fence for durable rendered-request capture (#840).
//!
//! `Proofs/RenderedCapture.lean` proves the order — a provider send is legal
//! only after the matching `(capture key, canonical request)` is durable — and
//! `crates/gents/src/agent/loop_stream/tests.rs` fences that order against the
//! owned loop with an in-process sink. What neither can reach is the claim this
//! slice actually makes: that the bytes in `RenderedRequest.request_json` are
//! the bytes the provider received, and that a sink failure stops the HTTP call
//! rather than merely being logged next to it.
//!
//! Both need a real HTTP round trip, so these tests run a full daemon against
//! the deterministic mock backend and compare the persisted payload with the
//! body that backend was posted.

use std::sync::Arc;
use std::time::Duration;

use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::rendered_request::{
    RenderedCompletionRequest, RenderedRequestCaptureFactory, RenderedRequestCaptureSink,
};
use gents::{AgentIdentity, Gents, ToolCeiling};
use serde_json::Value;

use crate::support::fixtures::test_identity;
use crate::support::interrupt::{
    create_runtime_request, wait_for_request_lifecycle_state, wait_for_runtime_ready, BootedAgent,
};
use crate::support::snapshots::fetch_request_snapshot;
use crate::support::streaming_backend::{MockStreamingBackend, StreamScript};
use crate::support::test_db;

const CAPTURE_MODEL: &str = "capture-model";
const CAPTURE_BACKEND_ID: &str = "capture-backend";
const CAPTURE_BEHAVIOR_ID: &str = "capture-behavior";

/// The exact bytes the provider received must be the exact bytes on the row.
///
/// This is the claim the transport seam exists to make true. Capturing the
/// rig-assembled request would pass a weaker test — "the row looks like the
/// request we meant to send" — while the ChatGPT-Codex and Grok transports
/// rewrite the body underneath it. Comparing against the backend's own
/// observation is the only version of this assertion that cannot be satisfied
/// by a second serializer agreeing with itself.
#[tokio::test]
async fn the_persisted_request_json_is_the_body_the_provider_received() {
    let backend = MockStreamingBackend::start(
        CAPTURE_MODEL,
        vec![StreamScript::completes("capture-me", ["ok"])],
    )
    .expect("mock backend");
    let db = test_db("rendered-request-capture").await;
    let agent = boot_capture_agent(&db, "rendered-request-capture", backend.endpoint(), None).await;

    let doc_id = create_runtime_request(
        db.node.as_ref(),
        &agent.agent_did,
        CAPTURE_BEHAVIOR_ID,
        "req-capture-1",
        "session-capture-1",
        "please capture-me",
    )
    .await;
    wait_for_request_lifecycle_state(db.node.as_ref(), &doc_id, "completed").await;

    let observed = backend.observed_completion_bodies();
    assert_eq!(
        observed.len(),
        1,
        "the mock backend should have served exactly one completion"
    );

    let rows = wait_for_rendered_requests(db.node.as_ref(), "req-capture-1", observed.len()).await;
    let row = &rows[0];

    assert_eq!(
        parse_json(&row["request_json"]),
        canonical(&observed[0]),
        "the persisted payload must be the body the provider was posted"
    );
    assert_eq!(row["capture_scope"], "inference.1");
    assert_eq!(row["turn_index"], 0);
    assert_eq!(row["attempt"], 0);
    assert_eq!(row["source"], "openai_chat_completions");
    assert_eq!(row["session_id"], "session-capture-1");
    assert_eq!(row["agent_did"].as_str(), Some(agent.agent_did.as_str()));
    assert_eq!(
        row["model_name"], CAPTURE_MODEL,
        "the model column must name the model the provider was asked for"
    );
    assert!(
        row["capture_key"]
            .as_str()
            .is_some_and(|key| key.starts_with("rendered:v1:")),
        "unexpected capture key {:?}",
        row["capture_key"]
    );

    // Provenance says, positively, where the bytes were read.
    let provenance = parse_json(&row["provenance_json"]);
    assert_eq!(provenance["capture_seam"], "transport_body");
    assert_eq!(provenance["status"], "captured_only");
    assert_eq!(provenance["capture_scope"], "inference.1");
    assert!(
        provenance["assembly_trace"]["effective_messages"].is_array(),
        "the manifest must carry the leak set: {provenance}"
    );

    agent.shutdown().await;
}

/// The fail-closed property, measured where it matters: at the provider.
///
/// `capture_failure_blocks_send` says a rejected capture leaves `sent`
/// unreachable. A sink that logged its failure and let the request through
/// would still pass every in-process test that only inspects the loop's error;
/// only the backend's own request count can distinguish "refused" from
/// "reported".
#[tokio::test]
async fn a_failing_capture_sink_issues_no_provider_request() {
    let backend = MockStreamingBackend::start(
        CAPTURE_MODEL,
        vec![StreamScript::completes("must-not-send", ["ok"])],
    )
    .expect("mock backend");
    let db = test_db("rendered-request-capture-faults").await;
    let agent = boot_capture_agent(
        &db,
        "rendered-request-capture-faults",
        backend.endpoint(),
        Some(failing_capture_factory()),
    )
    .await;

    let doc_id = create_runtime_request(
        db.node.as_ref(),
        &agent.agent_did,
        CAPTURE_BEHAVIOR_ID,
        "req-capture-fault-1",
        "session-capture-fault-1",
        "please must-not-send",
    )
    .await;
    // Wait for *any* terminal state, then assert both halves separately. If the
    // zero-request assertion only ran after `lifecycle_state == "failed"`, a
    // fail-open sink would fail this test on a timeout instead of on the claim
    // under test, and the diagnostic would point at the wrong thing.
    let terminal = wait_for_request_terminal_state(db.node.as_ref(), &doc_id).await;

    assert_eq!(
        backend.observed_completion_requests(),
        0,
        "a failed capture must not issue the provider call; terminal state {terminal}, \
         bodies observed: {:?}",
        backend.observed_completion_bodies()
    );
    assert_eq!(
        terminal, "failed",
        "a request whose capture never succeeded must terminate as failed"
    );
    assert!(
        rendered_requests(db.node.as_ref(), "req-capture-fault-1")
            .await
            .is_empty(),
        "a failed capture must not leave a partial fact record"
    );

    agent.shutdown().await;
}

/// Redelivering the identical canonical request is a success without a write;
/// reusing the key for a different one is an integrity error, never an update.
/// This drives the sink directly because the loop can never produce the second
/// case — that is the point of proving it here rather than assuming it.
#[tokio::test]
async fn capture_is_idempotent_and_never_rebinds_a_key() {
    let db = test_db("rendered-request-capture-idempotency").await;
    let sink = gents::rendered_request::DefraRenderedRequestSink::new(
        db.node.clone(),
        "did:key:z6MkCaptureIdempotency",
    );

    let first = rendered_fixture(serde_json::json!({"model": "m", "messages": [{"role": "user"}]}));
    sink.capture(first.clone()).await.expect("first capture");
    assert_eq!(
        rendered_requests(db.node.as_ref(), &first.request_id)
            .await
            .len(),
        1
    );

    // Same key, same canonical value, keys reordered: still one fact.
    let mut redelivered = first.clone();
    redelivered.request_json = serde_json::json!({"messages": [{"role": "user"}], "model": "m"});
    sink.capture(redelivered)
        .await
        .expect("identical redelivery is idempotent");
    assert_eq!(
        rendered_requests(db.node.as_ref(), &first.request_id)
            .await
            .len(),
        1,
        "an idempotent redelivery must not write a second row"
    );

    // Same key, different canonical value: integrity error, no write.
    let mut conflicting = first.clone();
    conflicting.request_json = serde_json::json!({"model": "m", "messages": []});
    let error = sink
        .capture(conflicting)
        .await
        .expect_err("a rebound key must be an integrity error");
    assert!(
        error.to_string().contains("integrity violation"),
        "unexpected error: {error:#}"
    );

    let rows = rendered_requests(db.node.as_ref(), &first.request_id).await;
    assert_eq!(rows.len(), 1, "the store must be left exactly as it was");
    assert_eq!(
        parse_json(&rows[0]["request_json"]),
        canonical(&first.request_json),
        "the original fact must survive the rejected rebinding"
    );
}

// ===== helpers =====

fn canonical(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
        Value::Object(map) => {
            let sorted = map
                .iter()
                .map(|(key, value)| (key.clone(), canonical(value)))
                .collect::<std::collections::BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect())
        }
        other => other.clone(),
    }
}

fn parse_json(value: &Value) -> Value {
    let text = value
        .as_str()
        .unwrap_or_else(|| panic!("expected a JSON string column, got {value}"));
    canonical(&serde_json::from_str::<Value>(text).expect("stored column must be valid JSON"))
}

fn rendered_fixture(request_json: Value) -> RenderedCompletionRequest {
    let agent_did = "did:key:z6MkCaptureIdempotency".to_string();
    let session_id = "session-idem".to_string();
    let request_id = "req-idem".to_string();
    let capture_scope = "inference.1".to_string();
    let assembly_trace = gents::rendered_request::AssemblyTrace::from_effective_messages(
        gents::rendered_request::AssemblyBuildPath::Budgeted,
        Vec::new(),
    );
    RenderedCompletionRequest {
        capture_key: gents::rendered_request::capture_key(
            &agent_did,
            &session_id,
            &request_id,
            &capture_scope,
            0,
            0,
        )
        .expect("capture key"),
        capture_version: gents::rendered_request::CAPTURE_VERSION,
        request_id,
        capture_scope: capture_scope.clone(),
        turn_index: 0,
        attempt: 0,
        agent_did,
        requester_did: String::new(),
        behavior_id: "behavior".to_string(),
        session_id,
        model_name: "m".to_string(),
        source: gents::rendered_request::RenderedRequestSource::OpenAiChatCompletions,
        request_json,
        messages_json: serde_json::json!([]),
        tools_json: serde_json::json!([]),
        tool_choice_json: Value::Null,
        sampling_json: Value::Null,
        prompt_hash: "0".repeat(64),
        tools_hash: "0".repeat(64),
        provenance_json: serde_json::to_value(
            gents::rendered_request::ProvenanceManifest::captured_only(
                capture_scope,
                assembly_trace.clone(),
            ),
        )
        .expect("provenance"),
        assembly_trace,
    }
}

/// A sink that always fails, standing in for a DefraDB outage or a rejected
/// integrity check.
fn failing_capture_factory() -> RenderedRequestCaptureFactory {
    Arc::new(|_context| {
        let sink: RenderedRequestCaptureSink = Arc::new(|_rendered| {
            Box::pin(async { anyhow::bail!("injected rendered-request capture failure") })
        });
        sink
    })
}

async fn rendered_requests(node: &EmbeddedNode, request_id: &str) -> Vec<Value> {
    let query = format!(
        r#"query {{
            RenderedRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{
                capture_key
                request_id
                session_id
                agent_did
                requester_did
                behavior_id
                capture_scope
                turn_index
                attempt
                capture_version
                model_name
                source
                request_json
                prompt_hash
                tools_hash
                provenance_json
            }}
        }}"#,
        request_id = escape_graphql_string(request_id),
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "RenderedRequest query failed: {:?}",
        response.errors
    );
    let mut rows = response
        .data
        .and_then(|data| data.get("RenderedRequest").cloned())
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    rows.sort_by_key(|row| {
        (
            row["capture_scope"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            row["turn_index"].as_i64().unwrap_or_default(),
            row["attempt"].as_i64().unwrap_or_default(),
        )
    });
    rows
}

/// Wait for any terminal lifecycle state and report which one it reached.
async fn wait_for_request_terminal_state(node: &EmbeddedNode, request_doc_id: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let snapshot = fetch_request_snapshot(node, request_doc_id).await;
        if matches!(
            snapshot.lifecycle_state.as_str(),
            "completed" | "failed" | "cancelled" | "expired"
        ) {
            return snapshot.lifecycle_state;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "request {request_doc_id} never reached a terminal state; last={}",
            snapshot.lifecycle_state
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_rendered_requests(
    node: &EmbeddedNode,
    request_id: &str,
    expected: usize,
) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let rows = rendered_requests(node, request_id).await;
        if rows.len() >= expected {
            return rows;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected {expected} RenderedRequest rows for {request_id}, saw {}",
            rows.len()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn boot_capture_agent(
    db: &crate::support::TestDb,
    test_name: &str,
    endpoint: &str,
    capture_factory: Option<RenderedRequestCaptureFactory>,
) -> BootedAgent {
    upsert_capture_backend(db.node.as_ref(), endpoint).await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity(test_name));
    let mut builder = Gents::builder()
        .node(db.node.clone())
        .identity(identity)
        .default_behavior_id(CAPTURE_BEHAVIOR_ID)
        .tool_ceiling(ToolCeiling::meta_only());
    if let Some(factory) = capture_factory {
        builder = builder.rendered_request_capture_factory(factory);
    }
    let agent = builder
        .behavior(CAPTURE_BEHAVIOR_ID)
        .backend_id(CAPTURE_BACKEND_ID)
        .model_name(CAPTURE_MODEL)
        .stream_batch_ms(0)
        .deadline_duration_secs(30)
        .done()
        .build()
        .await
        .expect("build rendered-request capture agent");
    let agent_did = agent.agent_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;
    BootedAgent::new(shutdown_tx, handle, agent_did)
}

async fn upsert_capture_backend(node: &EmbeddedNode, endpoint: &str) {
    let escaped_backend_id = escape_graphql_string(CAPTURE_BACKEND_ID);
    let escaped_endpoint = escape_graphql_string(endpoint);
    let escaped_model = escape_graphql_string(CAPTURE_MODEL);
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
                    max_concurrent: 4,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{escaped_model}"],
                    probe_status: "healthy"
                }}
            ) {{ _docID }}
        }}"#,
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "upserting the capture backend failed: {:?}",
        response.errors
    );
}
