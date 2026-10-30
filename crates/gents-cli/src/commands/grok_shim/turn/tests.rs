mod cancel;
mod child;
mod disconnect;
mod latch;
mod observation;
mod observer;
mod prompt;
mod replacement;
mod session;
mod wire;

use super::super::projection::{ProjectionEngine, ProjectionSequencer, SessionUpdateChannel};
use super::super::server::AcpOutbound;
use super::sender::PromptSender;
use super::wire::{
    parse_cancel_notification, parse_prompt_request, prompt_result, prompt_text,
    stop_reason_from_rows, stop_reason_from_wire, PromptBlock, PromptRequest, StopReason,
    TerminalOutcome,
};
use super::*;
use gents::graphql::{ensure_no_errors, escape_graphql_string};
use serde_json::json;

fn text_block(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

use axum::{extract::State, routing::post, Json, Router};

#[derive(Clone)]
struct MockGraphqlState {
    node: Arc<EmbeddedNode>,
    gate_armed: Option<Arc<std::sync::atomic::AtomicBool>>,
    submission_arrived: Option<Arc<tokio::sync::Notify>>,
    submission_release: Option<Arc<tokio::sync::Notify>>,
    submission_fail: Option<Arc<std::sync::atomic::AtomicBool>>,
}

async fn mock_graphql(
    State(state): State<MockGraphqlState>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let query = body
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let is_create_request = query.contains("create_AgentRequest");
    tracing::debug!(
        target: "grok_shim::mock",
        query_len = query.len(),
        is_create_request,
        "mock handler forwarding query"
    );
    if is_create_request {
        let first_gated_submission = state
            .gate_armed
            .as_ref()
            .is_some_and(|armed| armed.swap(false, std::sync::atomic::Ordering::SeqCst));
        if first_gated_submission {
            if let Some(arrived) = state.submission_arrived.as_ref() {
                arrived.notify_one();
            }
            if let Some(release) = state.submission_release.as_ref() {
                release.notified().await;
            }
            if let Some(fail) = state.submission_fail.as_ref() {
                if fail.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    return Json(json!({
                        "errors": [{"message": "injected submission failure"}],
                    }));
                }
            }
        }
    }
    let response = state.node.execute(&query).await;
    Json(serde_json::to_value(&response).unwrap_or_default())
}

async fn spawn_mock_graphql(node: Arc<EmbeddedNode>) -> String {
    spawn_gated_mock_graphql(node, None).await
}

async fn spawn_gated_mock_graphql(
    node: Arc<EmbeddedNode>,
    submission_gate: Option<(
        Arc<tokio::sync::Notify>,
        Arc<tokio::sync::Notify>,
        Arc<std::sync::atomic::AtomicBool>,
    )>,
) -> String {
    spawn_gated_mock_graphql_with_failure(node, submission_gate, None).await
}

async fn spawn_gated_mock_graphql_with_failure(
    node: Arc<EmbeddedNode>,
    submission_gate: Option<(
        Arc<tokio::sync::Notify>,
        Arc<tokio::sync::Notify>,
        Arc<std::sync::atomic::AtomicBool>,
    )>,
    submission_fail: Option<Arc<std::sync::atomic::AtomicBool>>,
) -> String {
    let (submission_arrived, submission_release, gate_armed) = match submission_gate {
        Some((arrived, release, armed)) => (Some(arrived), Some(release), Some(armed)),
        None => (None, None, None),
    };
    let state = MockGraphqlState {
        node,
        gate_armed,
        submission_arrived,
        submission_release,
        submission_fail,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock graphql listener");
    let addr = listener
        .local_addr()
        .expect("mock graphql listener address");
    let router = Router::new()
        .route("/", post(mock_graphql))
        .with_state(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}/")
}

async fn test_node() -> (tempfile::TempDir, Arc<EmbeddedNode>, String) {
    let tempdir = tempfile::tempdir().expect("tempdir");
    // Started sessions in these fixtures carry receipts the fixture key signs.
    let node_did = crate::caused_sessions::fixtures::FIXTURE_DID.to_owned();
    let agent_id = gents::default_agent_id_for_node(&node_did);
    let node = Arc::new(
        crate::shared::test_support::node_builder()
            .data_path(tempdir.path().join("node"))
            .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
            .build()
            .await
            .expect("embedded node"),
    );
    gents::schema::ensure_runtime_schemas(&node)
        .await
        .expect("runtime schemas");
    super::super::seed_test_agent_configuration(
        &node,
        &node_did,
        &agent_id,
        &agent_id,
        "GLM-5.3-NVFP4",
        true,
    )
    .await;
    (tempdir, node, node_did)
}

fn test_config(graphql: String, node_did: &str) -> TurnManagerConfig {
    TurnManagerConfig {
        actor: ::identity::Did::new(node_did.to_string()).expect("fixture creator DID"),
        node_did: node_did.to_string(),
        agent_id: gents::default_agent_id_for_node(node_did),
        graphql,
    }
}

fn test_engine(node: Arc<EmbeddedNode>) -> Arc<ProjectionEngine> {
    Arc::new(ProjectionEngine::new(
        node,
        super::super::projection::BoundModelContext::new(
            "GLM-5.3-NVFP4".to_string(),
            "GLM-5.3-NVFP4".to_string(),
            262_144,
        ),
    ))
}

async fn seed_runtime_wake(
    node: &EmbeddedNode,
    node_did: &str,
    content: &str,
) -> gents_protocol::row::AgentRequestRow {
    let agent_id = gents::default_agent_id_for_node(node_did);
    let request = uuid::Uuid::new_v4().to_string();
    let result = node
        .execute(&format!(
            r#"mutation {{create_AgentRequest(input:{{
        request_id:"{request}",purpose:"normal",node_did:"{node_did}",requester_did:"{node_did}",
        agent_id:"{agent_id}",session_id:"session-1",runtime_source_kind:"local-control",lifecycle_state:"pending",
        content:"{}",created_at:"{}"}}) {{_docID}}}}"#,
            escape_graphql_string(content),
            chrono::Utc::now().to_rfc3339(),
            node_did = escape_graphql_string(node_did)
        ))
        .await;
    ensure_no_errors(&result, "seed runtime wake fixture").unwrap();
    let doc = gents_protocol::graphql::extract_mutation_doc_id(
        &json!({"data":result.data}),
        "AgentRequest",
    )
    .unwrap();
    serde_json::from_value(json!({"_docID":doc,"request_id":request,"node_did":node_did,"requester_did":node_did,"agent_id":agent_id,"session_id":"session-1","runtime_source_kind":"local-control"})).unwrap()
}

#[test]
fn job_tick_runs_before_observe_session_tick() {
    let source = include_str!("observe.rs");
    let observer = &source[source.find("async fn observe_session_since").unwrap()..];
    let before_cards = &observer[..observer.find(".observe_session_tick(").unwrap()];
    assert!(
        before_cards.matches("job_cursor").count() >= 2,
        "the job cursor is declared and refreshed ahead of the card tick"
    );
}

fn buffer_sender() -> (Arc<Mutex<Vec<String>>>, PromptSender) {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    (buffer.clone(), PromptSender::Buffer { buffer })
}

fn fixture_request_filter(request: &gents_protocol::row::AgentRequestRow) -> String {
    format!(
        "{}, _docID: {{_eq: \"{}\"}}, request_id: {{_eq: \"{}\"}}",
        gents::session::session_scope_filter(
            request.node_did.as_deref().expect("fixture owner"),
            request.session_id.as_deref().expect("fixture session"),
            request.requester_did.as_deref()
        ),
        escape_graphql_string(request.doc_id.as_deref().expect("fixture physical request")),
        escape_graphql_string(&request.request_id)
    )
}

async fn terminalize_request(
    node: &Arc<EmbeddedNode>,
    request: &gents_protocol::row::AgentRequestRow,
    lifecycle_state: &str,
) {
    let filter = fixture_request_filter(request);
    let state = escape_graphql_string(lifecycle_state);
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let response = node.execute(&format!(r#"mutation {{
        update_AgentRequest(filter: {{{filter}}}, input: {{lifecycle_state: "{state}", terminalized_at: "{now}"}}) {{_docID}}
    }}"#)).await;
    ensure_no_errors(&response, "test terminalize request").expect("terminalize");
}

async fn seed_assistant_message(
    node: &Arc<EmbeddedNode>,
    request: &gents_protocol::row::AgentRequestRow,
    _sequence: i64,
    text: &str,
) {
    let sequence = i64::from(
        crate::commands::grok_shim::test_fixtures::next_request_sequence(
            node,
            request.doc_id.as_deref().expect("fixture request doc"),
        )
        .await,
    );
    seed_canonical_text_message(
        node,
        request,
        &gents::session::sequence_message_key(
            request.node_did.as_deref().expect("fixture owner"),
            request.session_id.as_deref().expect("fixture session"),
            request.requester_did.as_deref(),
            sequence.try_into().expect("fixture sequence fits u32"),
        ),
        &format!("turn:{sequence}"),
        sequence,
        gents_protocol::output::MessageRole::Assistant,
        text,
    )
    .await;
}

async fn seed_completion_notice(
    node: &Arc<EmbeddedNode>,
    request: &gents_protocol::row::AgentRequestRow,
    _sequence: i64,
    text: &str,
) {
    let sequence = i64::from(
        crate::commands::grok_shim::test_fixtures::next_request_sequence(
            node,
            request.doc_id.as_deref().expect("fixture request doc"),
        )
        .await,
    );
    seed_canonical_text_message(
        node,
        request,
        &format!("background-completion-notification:{sequence}:tool"),
        &format!("background-completion-notification:{sequence}:tool"),
        sequence,
        gents_protocol::output::MessageRole::User,
        text,
    )
    .await;
}

async fn seed_canonical_text_message(
    node: &Arc<EmbeddedNode>,
    request: &gents_protocol::row::AgentRequestRow,
    message_key: &str,
    source_key: &str,
    sequence: i64,
    role: gents_protocol::output::MessageRole,
    text: &str,
) {
    use gents::session::canonical_rows::{
        output_segment_create_variables, transcript_message_create_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{
        MessageBlock, MessagePublication, OutputOutcome, OutputSegment, OutputSource, OutputWriter,
        PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload, TranscriptMessage,
    };

    let node_did = request.node_did.as_deref().expect("fixture owner");
    let session_id = request.session_id.as_deref().expect("fixture session");
    let request_doc_id = request.doc_id.as_deref().expect("fixture physical request");
    let execution_generation = format!("fixture:{request_doc_id}");
    let created_at = chrono::Utc::now().to_rfc3339();
    let segment = OutputSegment {
        node_did: node_did.into(),
        requester_did: request.requester_did.clone(),
        session_id: session_id.into(),
        request_doc_id: request_doc_id.into(),
        source: OutputSource::Authored {
            key: source_key.into(),
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: execution_generation.clone(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: text.len() as u32,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::Text,
            }),
        }],
        payload: text.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![text.len() as u64],
        }),
        created_at: created_at.clone(),
    };
    let response = node
        .execute_request(
            gents::defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(&segment).unwrap()),
        )
        .await;
    ensure_no_errors(&response, "seed canonical output segment").expect("seed segment");
    let close_doc_id =
        gents::graphql::single_mutation_document(&response, "create_AgentOutputSegment")
            .expect("segment mutation envelope")
            .expect("created segment row")["_docID"]
            .as_str()
            .expect("physical segment id")
            .to_string();
    let message = TranscriptMessage {
        message_key: message_key.into(),
        session_id: session_id.into(),
        node_did: node_did.into(),
        requester_did: request.requester_did.clone(),
        request_doc_id: Some(request_doc_id.into()),
        publication: MessagePublication::RequestExecution {
            execution_generation,
        },
        outcome: OutputOutcome::Complete,
        sequence: sequence.try_into().expect("fixture sequence fits u32"),
        role,
        native_id: None,
        blocks: vec![MessageBlock::Text {
            text: PresentedPayload {
                output: PayloadRef {
                    close_doc_id,
                    stream: 0,
                },
                presentation: PayloadPresentation::Full,
            },
        }],
        created_at,
    };
    let response = node
        .execute_request(
            gents::defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&message).unwrap()),
        )
        .await;
    ensure_no_errors(&response, "seed canonical transcript header")
        .expect("seed transcript header");
}

async fn seed_tool_call(
    node: &Arc<EmbeddedNode>,
    request: &gents_protocol::row::AgentRequestRow,
    tool_call_id: &str,
    tool_name: &str,
    lifecycle_state: &str,
    result: &str,
) -> String {
    crate::commands::grok_shim::test_fixtures::seed_canonical_tool_call(
        node,
        request,
        tool_call_id,
        tool_name,
        lifecycle_state,
        "{}",
        matches!(
            lifecycle_state,
            "completed" | "failed" | "cancelled" | "timedOut"
        )
        .then_some(result),
        None,
        None,
        None,
    )
    .await
}

async fn seed_child_request(
    node: &Arc<EmbeddedNode>,
    parent: &gents_protocol::row::AgentRequestRow,
    tool_doc_id: &str,
    child_request_id: &str,
    lifecycle_state: &str,
) -> gents_protocol::row::AgentRequestRow {
    let owner = escape_graphql_string(parent.node_did.as_deref().unwrap());
    let requester = format!("\"{owner}\"");
    let agent = escape_graphql_string(parent.agent_id.as_deref().expect("fixture agent"));
    let logical_parent = escape_graphql_string(&parent.request_id);
    let physical_parent = escape_graphql_string(parent.doc_id.as_deref().unwrap());
    let tool = escape_graphql_string(tool_doc_id);
    let child = escape_graphql_string(child_request_id);
    let state = escape_graphql_string(lifecycle_state);
    let now = chrono::Utc::now().to_rfc3339();
    let response = node.execute(&format!(r#"mutation {{create_AgentRequest(input: {{
        request_id: "{child}", purpose: "normal", node_did: "{owner}", requester_did: {requester}, agent_id: "{agent}", session_id: "session-1-child",
        caused_by_parent_request_id: "{logical_parent}", caused_by_parent_request_doc_id: "{physical_parent}", caused_by_parent_tool_call_id: "call-1", caused_by_parent_tool_call_doc_id: "{tool}", content: "child work", lifecycle_state: "{state}", created_at: "{now}"
    }}) {{_docID}} }}"#)).await;
    ensure_no_errors(&response, "test seed child request").unwrap();
    let doc = gents_protocol::graphql::extract_mutation_doc_id(
        &json!({"data":response.data}),
        "AgentRequest",
    )
    .unwrap();
    crate::caused_sessions::fixtures::create_started_session(
        node,
        gents_protocol::session::AgentSession {
            agent_id: parent.agent_id.clone().expect("fixture agent"),
            ..crate::caused_sessions::fixtures::fixture_session(
                parent.node_did.as_deref().unwrap(),
                "session-1-child",
            )
        },
        gents_protocol::session::SessionReference {
            node_did: parent.node_did.clone().unwrap(),
            session_id: parent.session_id.clone().unwrap(),
            requester_did: parent.requester_did.clone(),
        },
        parent.doc_id.as_deref().unwrap(),
        &doc,
    )
    .await;
    serde_json::from_value(json!({"_docID":doc,"request_id":child_request_id,"node_did":parent.node_did,"requester_did":parent.node_did,"agent_id":parent.agent_id,"session_id":"session-1-child", "caused_by_parent_request_id":parent.request_id,"caused_by_parent_request_doc_id":parent.doc_id,"caused_by_parent_tool_call_id":"call-1","caused_by_parent_tool_call_doc_id":tool_doc_id})).unwrap()
}

async fn complete_tool_call(node: &Arc<EmbeddedNode>, tool_doc_id: &str, result: &str) {
    let escaped_id = escape_graphql_string(tool_doc_id);
    let response = node.execute(&format!(r#"{{ AgentToolCall(filter: {{_docID: {{_eq: "{escaped_id}"}}}}, limit: 2) {{tool_call_id request_doc_id}} }}"#)).await;
    ensure_no_errors(&response, "load tool completion owner").unwrap();
    let rows = response.data.as_ref().unwrap()["AgentToolCall"]
        .as_array()
        .unwrap();
    assert_eq!(rows.len(), 1, "tool completion owner must be exact");
    let call_id = rows[0]["tool_call_id"].as_str().unwrap().to_owned();
    let request_doc_id = escape_graphql_string(rows[0]["request_doc_id"].as_str().unwrap());
    let response = node.execute(&format!(r#"{{ AgentRequest(filter: {{_docID: {{_eq: "{request_doc_id}"}}}}, limit: 2) {{_docID request_id node_did requester_did session_id}} }}"#)).await;
    ensure_no_errors(&response, "load tool request owner").unwrap();
    let rows = response.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap();
    assert_eq!(rows.len(), 1, "tool request owner must be exact");
    let request = serde_json::from_value(rows[0].clone()).unwrap();
    crate::commands::grok_shim::test_fixtures::complete_canonical_tool_call(
        node,
        &request,
        tool_doc_id,
        &call_id,
        result,
    )
    .await;
}

async fn complete_child_request(
    node: &Arc<EmbeddedNode>,
    request: &gents_protocol::row::AgentRequestRow,
) {
    let filter = fixture_request_filter(request);
    let escaped_state = escape_graphql_string("completed");
    let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{{filter}}},
                input: {{
                    lifecycle_state: "{escaped_state}"
                    terminalized_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    ensure_no_errors(&response, "test complete child request").expect("complete child request");
}

#[tokio::test]
async fn fixture_receipts_keep_colliding_requests_and_child_owners_separate() {
    let (_dir, node, node_did) = test_node().await;
    let agent_id = gents::default_agent_id_for_node(&node_did);
    let mut receipts = Vec::new();
    for content in ["selected", "same-label other document"] {
        let result = node.execute(&format!(r#"mutation {{create_AgentRequest(input: {{
            request_id:"collision", purpose:"normal", node_did:"{node_did}", requester_did:"{node_did}",
            session_id:"session-1", agent_id:"{agent_id}", content:"{content}", lifecycle_state:"pending"
        }}) {{_docID}} }}"#)).await;
        ensure_no_errors(&result, "seed colliding requests").unwrap();
        let doc = gents_protocol::graphql::extract_mutation_doc_id(
            &json!({"data":result.data}),
            "AgentRequest",
        )
        .unwrap();
        receipts.push(
            serde_json::from_value::<gents_protocol::row::AgentRequestRow>(json!({
                "_docID":doc,"request_id":"collision","node_did":node_did,"requester_did":node_did,
                "session_id":"session-1","agent_id":agent_id
            }))
            .unwrap(),
        );
    }
    assert_ne!(receipts[0].doc_id, receipts[1].doc_id);
    let selected = &receipts[0];
    seed_assistant_message(&node, selected, 1, "selected output").await;
    let tool = seed_tool_call(&node, selected, "call-1", "agent_new", "running", "").await;
    let child = seed_child_request(&node, selected, &tool, "child", "processing").await;
    assert_eq!(child.node_did.as_deref(), Some(node_did.as_str()));
    assert_eq!(child.requester_did.as_deref(), Some(node_did.as_str()));
    seed_assistant_message(&node, &child, 1, "child output").await;
    complete_child_request(&node, &child).await;
    complete_tool_call(&node, &tool, "done").await;
    terminalize_request(&node, selected, "completed").await;
    for (index, receipt) in receipts.iter().enumerate() {
        let query = format!(
            "{{AgentRequest(filter: {{{}}}) {{lifecycle_state}} AgentMessage(filter: {{request_doc_id: {{_eq: \"{}\"}}}}) {{message_key node_did requester_did request_doc_id}}}}",
            fixture_request_filter(receipt),
            escape_graphql_string(receipt.doc_id.as_deref().unwrap())
        );
        let result = node.execute(&query).await;
        ensure_no_errors(&result, "read fixture effects").unwrap();
        let data = result.data.unwrap();
        assert_eq!(
            data["AgentRequest"][0]["lifecycle_state"],
            if index == 0 { "completed" } else { "pending" }
        );
        for collection in ["AgentMessage"] {
            let rows = data[collection].as_array().unwrap();
            assert_eq!(
                rows.len(),
                if index == 0 { 3 } else { 0 },
                "the selected request owns its assistant output, canonical tool admission, and exact delivery; the colliding request owns none"
            );
            if index == 0 {
                assert_eq!(
                    rows.iter()
                        .filter(|row| row["message_key"]
                            .as_str()
                            .is_some_and(|key| key.starts_with("accepted:")))
                        .count(),
                    1,
                    "exactly one coordinator admission header is owned by the selected request"
                );
                assert_eq!(
                    rows.iter()
                        .filter(|row| row["message_key"]
                            .as_str()
                            .is_some_and(|key| key.starts_with("delivery:")))
                        .count(),
                    1,
                    "exactly one physical ToolDelivery header is owned by the selected request"
                );
            }
            for row in rows {
                assert_eq!(row["node_did"], node_did);
                assert_eq!(row["requester_did"], node_did);
                assert_eq!(row["request_doc_id"], receipt.doc_id.as_deref().unwrap());
            }
        }
    }
}

async fn parse_buffered_updates(buffer: &Arc<Mutex<Vec<String>>>) -> Vec<Value> {
    let lines = buffer.lock().await.clone();
    lines
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

async fn buffered_update_kinds(buffer: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    parse_buffered_updates(buffer)
        .await
        .iter()
        .map(|value| {
            value["params"]["update"]["sessionUpdate"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

async fn wait_for_pending_request(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
) -> gents_protocol::row::AgentRequestRow {
    let scope = gents::session::session_scope_filter(node_did, "session-1", Some(node_did));
    loop {
        let response = node.execute(&format!("{{AgentRequest(filter: {{{scope}, lifecycle_state: {{_eq: \"pending\"}}}}) {{{}}}}}", gents::SIGNED_REQUEST_FIELDS)).await;
        ensure_no_errors(&response, "wait for fixture request").unwrap();
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data["AgentRequest"].as_array())
            .expect("request rows");
        assert!(rows.len() <= 1, "ambiguous pending fixture request");
        if let Some(row) = rows.first() {
            let row: gents_protocol::row::AgentRequestRow =
                serde_json::from_value(row.clone()).unwrap();
            assert!(row.doc_id.is_some());
            return row;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
