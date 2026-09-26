use gents::defra_node::{EmbeddedNode, ExecuteRetryPolicy, QueryRequest};
use gents::graphql::escape_graphql_string;
use gents::session::canonical_rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents::session::sequence_message_key;
use gents::{DocumentRuntimeOptions, ToolCeiling};
use gents_protocol::message::{AssistantContent, Message, Text, ToolResultContent, UserContent};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
    StreamDeclaration, StreamPayload, TranscriptMessage,
};
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};
use serde_json::{json, Value};

use crate::support::accepted_turn::{
    boot_accepted_turn_with_backend_capacity_and_dynamic_followups, AcceptedTurnRuntime,
    AcceptedTurnSpec,
};
use crate::support::fixtures::{
    configure_subagent_behavior, spawn_subagent_source, subagent_target,
};
use crate::support::streaming_backend::{StreamChunk, StreamPlan, StreamResponse, StreamScript};
use crate::support::test_db;

const PARENT_BEHAVIOR_ID: &str = "r4c-parent";
const CHILD_BEHAVIOR_ID: &str = "r4c-child";

async fn setup_db(
    name: &str,
) -> (
    crate::support::TestDb,
    crate::support::fixtures::SubagentSourceGuard,
) {
    let db = test_db(name).await;
    let agent_did = db.node_identity.did().to_string();
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        CHILD_BEHAVIOR_ID,
        "r4c-child-tools",
        Vec::new(),
        false,
        false,
        None,
    )
    .await;
    configure_subagent_behavior(
        db.node.as_ref(),
        &agent_did,
        PARENT_BEHAVIOR_ID,
        "r4c-parent-tools",
        vec![subagent_target(
            &agent_did,
            CHILD_BEHAVIOR_ID,
            &agent_did,
            CHILD_BEHAVIOR_ID,
        )],
        true,
        true,
        None,
    )
    .await;
    let source = spawn_subagent_source(
        db.node.clone(),
        &agent_did,
        PARENT_BEHAVIOR_ID,
        CHILD_BEHAVIOR_ID,
    );
    (db, source)
}

struct ReaderParent {
    runtime: AcceptedTurnRuntime,
    request_id: String,
    session_id: String,
    prompt: String,
}

async fn create_parent_hook(
    db: &crate::support::TestDb,
    request_id: &str,
    session_id: &str,
    spawn_call_id: &str,
    child_prompt: &str,
) -> ReaderParent {
    let prompt = format!("{request_id}-prompt");
    let runtime = boot_accepted_turn_with_backend_capacity_and_dynamic_followups(
        db,
        AcceptedTurnSpec {
            backend_id: "r4c-reader-backend",
            model: "r4c-reader-model",
            parent_behavior_id: PARENT_BEHAVIOR_ID,
            configured_behavior_ids: &[PARENT_BEHAVIOR_ID, CHILD_BEHAVIOR_ID],
            request_id,
            session_id,
            prompt: &prompt,
            accepted_chunks: vec![StreamChunk::tool_call(
                spawn_call_id,
                "spawn_subagent",
                json!({"name": CHILD_BEHAVIOR_ID, "prompt": child_prompt, "await_mode": "background"}).to_string(),
            )],
            child_plans: vec![StreamPlan::new(
                child_prompt,
                vec![StreamResponse::Stream(StreamScript::paused(
                    child_prompt,
                    std::iter::empty::<&'static str>(),
                ))],
            )],
            valid_until: None,
            subagent_depth: None,
            request_setup: None,
        },
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
        2,
        &prompt,
    )
    .await;
    ReaderParent {
        runtime,
        request_id: request_id.to_string(),
        session_id: session_id.to_string(),
        prompt,
    }
}

async fn spawn_background_child(
    node: &EmbeddedNode,
    parent: &ReaderParent,
    provider_call_id: &str,
) -> Value {
    let escaped = escape_graphql_string(&parent.request_id);
    let query = format!(
        r#"{{ AgentRequest(filter: {{ caused_by_parent_request_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{ _docID request_id session_id }} }}"#
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let (child_request_id, child_request_doc_id, child_session_id) = loop {
        let response = node.execute(&query).await;
        if let Some(row) = response
            .data
            .as_ref()
            .and_then(|data| data["AgentRequest"].as_array())
            .and_then(|rows| rows.first())
        {
            if let (Some(request), Some(doc), Some(session)) = (
                row["request_id"].as_str(),
                row["_docID"].as_str(),
                row["session_id"].as_str(),
            ) {
                break (request.to_string(), doc.to_string(), session.to_string());
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for accepted child {provider_call_id}; parent facts: {}",
            node.execute(&format!(r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ lifecycle_state failure_reason }} AgentToolCall(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ tool_call_id tool_name lifecycle_state tool_failure_class denial_reason child_request_id }} }}"#, escape_graphql_string(&parent.request_id), escape_graphql_string(&parent.session_id))).await.data.unwrap_or(Value::Null)
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    json!({"ok": true, "child_request_id": child_request_id, "child_request_doc_id": child_request_doc_id, "child_session_id": child_session_id})
}

async fn read_transcript(
    db: &crate::support::TestDb,
    parent: &ReaderParent,
    provider_call_id: &str,
    args: Value,
) -> Value {
    parent.runtime.backend.enqueue_response(
        &parent.prompt,
        StreamResponse::streams(
            &parent.prompt,
            vec![StreamChunk::tool_call(
                provider_call_id,
                "read_subagent",
                args.to_string(),
            )],
        ),
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let history = gents::load_history(
            db.node.as_ref(),
            &parent.session_id,
            db.node_identity.did(),
            Some(db.node_identity.did()),
        )
        .await
        .expect("load accepted reader history");
        let mut accepted_ids = std::collections::BTreeSet::from([provider_call_id.to_string()]);
        for message in &history {
            if let Message::Assistant { content, .. } = message {
                for item in content {
                    if let AssistantContent::ToolCall(call) = item {
                        if call.id == provider_call_id
                            || call.call_id.as_deref() == Some(provider_call_id)
                        {
                            accepted_ids.insert(call.id.clone());
                            if let Some(call_id) = &call.call_id {
                                accepted_ids.insert(call_id.clone());
                            }
                        }
                    }
                }
            }
        }
        for message in history.iter().rev() {
            if let Message::User { content } = message {
                for item in content {
                    if let UserContent::ToolResult(result) = item {
                        if !accepted_ids.contains(&result.id)
                            && result
                                .call_id
                                .as_ref()
                                .is_none_or(|id| !accepted_ids.contains(id))
                        {
                            continue;
                        }
                        for part in &result.content {
                            if let ToolResultContent::Text(Text { text }) = part {
                                if let Ok(value) = serde_json::from_str::<Value>(text) {
                                    if value.get("transcript").is_some()
                                        || value.get("failure_class").is_some()
                                    {
                                        return value;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            let parent_id = escape_graphql_string(&parent.request_id);
            let parent_state = db.node.execute(&format!(r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{parent_id}" }} }}, limit: 2) {{ _docID lifecycle_state failure_reason deadline valid_until execution_generation execution_lease_expires_at terminal_output }} }}"#)).await;
            panic!(
                "timed out waiting for read_subagent result {provider_call_id}; matching provider requests={} parent_state={} history={}",
                parent.runtime.backend.observed_requests(&parent.prompt),
                parent_state.data.unwrap_or(Value::Null),
                serde_json::to_string(&history).unwrap()
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

async fn append_message(
    node: &EmbeddedNode,
    session_id: &str,
    fixture_index: u32,
    role: &str,
    content: &str,
) {
    let scope = child_source_scope(node, session_id).await;
    // The child session already contains its forked input. Append after the
    // canonical sequence owner instead of colliding with that inherited row.
    // The child provider is paused throughout these fixture writes.
    let sequence = next_child_sequence(node, session_id, &scope).await;
    let role = match role {
        "assistant" => MessageRole::Assistant,
        "user" => MessageRole::User,
        other => panic!("unsupported reader fixture role {other}"),
    };
    let source = if role == MessageRole::User {
        OutputSource::Authored {
            key: format!("reader-fixture-user-{fixture_index}"),
        }
    } else {
        OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: u64::from(sequence),
            },
            turn_index: 0,
            attempt: 0,
        }
    };
    let created_at = format!("2026-05-14T00:00:{sequence:02}Z");
    let segment = OutputSegment {
        agent_did: scope.agent_did.clone(),
        requester_did: scope.requester_did.clone(),
        session_id: session_id.into(),
        request_doc_id: scope.request_doc_id.clone(),
        source,
        writer: OutputWriter::RequestExecution {
            execution_generation: scope.generation.clone(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: content.len().try_into().unwrap(),
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::Text,
            }),
        }],
        payload: content.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![content.len() as u64],
        }),
        created_at: created_at.clone(),
    };
    let segment_response = node
        .execute_request_with_retry(
            QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(&segment).unwrap()),
            ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(
        !segment_response.has_errors(),
        "create reader text segment failed: {:?}",
        segment_response.errors
    );
    let close_doc_id =
        gents::graphql::single_mutation_document(&segment_response, "create_AgentOutputSegment")
            .unwrap()
            .unwrap()["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
    let native_id = (role == MessageRole::Assistant).then(|| format!("reader-native-{sequence}"));
    let message = TranscriptMessage {
        message_key: sequence_message_key(
            &scope.agent_did,
            session_id,
            scope.requester_did.as_deref(),
            sequence,
        ),
        session_id: session_id.into(),
        agent_did: scope.agent_did,
        requester_did: scope.requester_did,
        request_doc_id: Some(scope.request_doc_id),
        publication: MessagePublication::RequestExecution {
            execution_generation: scope.generation,
        },
        outcome: OutputOutcome::Complete,
        sequence,
        role,
        native_id,
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
        .execute_request_with_retry(
            QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&message).unwrap()),
            ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(
        !response.has_errors(),
        "create reader text header failed: {:?}",
        response.errors
    );
}

struct ChildSourceScope {
    request_doc_id: String,
    agent_did: String,
    requester_did: Option<String>,
    generation: String,
}

async fn next_child_sequence(
    node: &EmbeddedNode,
    session_id: &str,
    scope: &ChildSourceScope,
) -> u32 {
    gents::session::max_sequence(
        node,
        session_id,
        &scope.agent_did,
        scope.requester_did.as_deref(),
    )
    .await
    .expect("read child canonical sequence")
    .checked_add(1)
    .expect("child fixture sequence overflow")
}

async fn child_source_scope(node: &EmbeddedNode, session_id: &str) -> ChildSourceScope {
    let session_id = escape_graphql_string(session_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }}, purpose: {{ _eq: "normal" }} }}, limit: 2) {{ _docID agent_did requester_did execution_generation }} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "child scope read failed: {:?}",
        response.errors
    );
    let rows = response.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .expect("child scope rows");
    assert_eq!(rows.len(), 1, "reader fixture expects one child request");
    let row = &rows[0];
    ChildSourceScope {
        request_doc_id: row["_docID"].as_str().unwrap().into(),
        agent_did: row["agent_did"].as_str().unwrap().into(),
        requester_did: row["requester_did"].as_str().map(str::to_owned),
        generation: row["execution_generation"]
            .as_str()
            .unwrap_or("fixture-generation")
            .into(),
    }
}

/// Seed an assistant tool-call turn as typed canonical observations. This is
/// imported history for the reader family: it uses the child's exact request
/// membership and a fixture-only generation writer and never claims live
/// publication authority.
async fn append_assistant_tool_call_message(
    node: &EmbeddedNode,
    session_id: &str,
    request_doc_id: &str,
    sequence: u32,
    body: &str,
    tool_call_id: &str,
    tool_call_doc_id: &str,
    tool_name: &str,
) {
    let scope = child_source_scope(node, session_id).await;
    assert_eq!(scope.request_doc_id, request_doc_id);
    let agent_did = scope.agent_did.clone();
    let request_doc_id = request_doc_id.to_string();
    let arguments = json!({"name": CHILD_BEHAVIOR_ID}).to_string();

    let segment = OutputSegment {
        agent_did: agent_did.clone(),
        requester_did: scope.requester_did.clone(),
        session_id: session_id.into(),
        request_doc_id: request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: u64::from(sequence),
            },
            turn_index: 0,
            attempt: 0,
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: scope.generation.clone(),
        },
        ordinal: Some(0),
        runs: vec![
            SegmentRun {
                stream: 0,
                bytes: body.len().try_into().unwrap(),
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::Text,
                }),
            },
            SegmentRun {
                stream: 1,
                bytes: arguments.len().try_into().unwrap(),
                declaration: Some(StreamDeclaration {
                    block_index: 1,
                    part_index: 0,
                    payload: StreamPayload::ToolArguments {
                        id: tool_call_id.to_string(),
                        call_id: Some(tool_call_id.to_string()),
                        name: tool_name.to_string(),
                    },
                }),
            },
        ],
        payload: format!("{body}{arguments}"),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![body.len() as u64, arguments.len() as u64],
        }),
        created_at: format!("2026-05-14T00:00:{sequence:02}Z"),
    };
    let segment_response = node
        .execute_request_with_retry(
            QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(&segment).unwrap()),
            ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(
        !segment_response.has_errors(),
        "create_AgentOutputSegment failed: {:?}",
        segment_response.errors
    );
    let close_doc_id =
        gents::graphql::single_mutation_document(&segment_response, "create_AgentOutputSegment")
            .unwrap()
            .unwrap()["_docID"]
            .as_str()
            .unwrap()
            .to_owned();

    let message = TranscriptMessage {
        message_key: sequence_message_key(
            &agent_did,
            session_id,
            scope.requester_did.as_deref(),
            sequence,
        ),
        session_id: session_id.into(),
        agent_did: agent_did,
        requester_did: scope.requester_did,
        request_doc_id: Some(request_doc_id),
        publication: MessagePublication::RequestExecution {
            execution_generation: scope.generation,
        },
        outcome: OutputOutcome::Complete,
        sequence,
        role: MessageRole::Assistant,
        native_id: Some(format!("native-{sequence}")),
        blocks: vec![
            MessageBlock::Text {
                text: PresentedPayload {
                    output: PayloadRef {
                        close_doc_id: close_doc_id.clone(),
                        stream: 0,
                    },
                    presentation: PayloadPresentation::Full,
                },
            },
            MessageBlock::ToolCall {
                tool_call_doc_id: tool_call_doc_id.to_string(),
                id: tool_call_id.to_string(),
                call_id: Some(tool_call_id.to_string()),
                name: tool_name.to_string(),
                arguments: PayloadRef {
                    close_doc_id,
                    stream: 1,
                },
                signature: None,
                additional_params: None,
            },
        ],
        created_at: format!("2026-05-14T00:00:{sequence:02}Z"),
    };
    let response = node
        .execute_request_with_retry(
            QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&message).unwrap()),
            ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(
        !response.has_errors(),
        "create_AgentMessage failed: {:?}",
        response.errors
    );
}

fn node_identity_did(node: &EmbeddedNode) -> String {
    node.node_identity_did()
        .expect("reader fixture requires the configured node principal")
        .to_owned()
}

async fn create_child_bridge_tool_call(
    node: &EmbeddedNode,
    child_request_id: &str,
    child_request_doc_id: &str,
    child_session_id: &str,
    message_sequence: u32,
    tool_call_id: &str,
) -> String {
    let agent_did = escape_graphql_string(&node_identity_did(node));
    let request_doc_id = escape_graphql_string(child_request_doc_id);
    let child_request_id = escape_graphql_string(child_request_id);
    let child_session_id = escape_graphql_string(child_session_id);
    let tool_call_id = escape_graphql_string(tool_call_id);
    let tool_call_key = format!("{child_session_id}:{tool_call_id}");
    let mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "{tool_call_key}",
                request_id: "{child_request_id}",
                request_doc_id: "{request_doc_id}",
                session_id: "{child_session_id}",
                agent_did: "{agent_did}",
                requester_did: "{agent_did}",
                message_sequence: {message_sequence},
                tool_name: "spawn_subagent",
                tool_call_id: "{tool_call_id}",
                lifecycle_state: "running",
                started_at: "2026-05-14T00:01:00Z",
                deadline_at: "2026-05-14T00:06:00Z",
                await_mode: "background",
                cancel_policy: "propagate",
                child_request_id: "grandchild-request"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create child bridge AgentToolCall failed: {:?}",
        response.errors
    );
    gents::graphql::single_mutation_document(&response, "create_AgentToolCall")
        .unwrap()
        .unwrap()["_docID"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn create_background_tool_call(
    node: &EmbeddedNode,
    child_request_id: &str,
    child_request_doc_id: &str,
    child_session_id: &str,
    message_sequence: u32,
    tool_call_id: &str,
) -> String {
    let agent_did = escape_graphql_string(&node_identity_did(node));
    let request_doc_id = escape_graphql_string(child_request_doc_id);
    let child_request_id = escape_graphql_string(child_request_id);
    let child_session_id = escape_graphql_string(child_session_id);
    let tool_call_id = escape_graphql_string(tool_call_id);
    let tool_call_key = format!("{child_session_id}:{tool_call_id}");
    let mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "{tool_call_key}",
                request_id: "{child_request_id}",
                request_doc_id: "{request_doc_id}",
                session_id: "{child_session_id}",
                agent_did: "{agent_did}",
                requester_did: "{agent_did}",
                message_sequence: {message_sequence},
                tool_name: "bash",
                tool_call_id: "{tool_call_id}",
                lifecycle_state: "running",
                started_at: "2026-05-14T00:01:00Z",
                deadline_at: "2026-05-14T00:06:00Z",
                await_mode: "background",
                cancel_policy: "propagate"
            }}) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "create background AgentToolCall failed: {:?}",
        response.errors
    );
    gents::graphql::single_mutation_document(&response, "create_AgentToolCall")
        .unwrap()
        .unwrap()["_docID"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn mark_child_completed(node: &EmbeddedNode, child_request_id: &str) {
    let request_id = escape_graphql_string(child_request_id);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{ lifecycle_state: "completed" }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "mark child completed failed: {:?}",
        response.errors
    );
}

async fn count_tool_calls_by_name(node: &EmbeddedNode, session_id: &str, tool_name: &str) -> usize {
    let session_id = escape_graphql_string(session_id);
    let tool_name = escape_graphql_string(tool_name);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    tool_name: {{ _eq: "{tool_name}" }}
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(
        !response.has_errors(),
        "count AgentToolCall by name failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|rows| rows.as_array())
        .map_or(0, Vec::len)
}

#[tokio::test]
async fn read_transcript_assistant_only_default() {
    let (db, _source) = setup_db("r4c-read-default").await;
    let hook = create_parent_hook(
        &db,
        "parent-default",
        "session-default",
        "spawn-default",
        "do work",
    )
    .await;
    let child = spawn_background_child(db.node.as_ref(), &hook, "spawn-default").await;
    let child_request_id = child["child_request_id"].as_str().unwrap();
    let child_session_id = child["child_session_id"].as_str().unwrap();
    append_message(
        db.node.as_ref(),
        child_session_id,
        1,
        "assistant",
        "first thought",
    )
    .await;
    append_message(db.node.as_ref(), child_session_id, 2, "user", "feedback").await;
    append_message(
        db.node.as_ref(),
        child_session_id,
        3,
        "assistant",
        "second thought",
    )
    .await;

    let result = read_transcript(
        &db,
        &hook,
        "read-default",
        json!({ "child_request_id": child_request_id }),
    )
    .await;
    let transcript = result["transcript"].as_str().unwrap();
    assert!(transcript.contains("first thought"));
    assert!(transcript.contains("second thought"));
    assert!(!transcript.contains("feedback"));
}

#[tokio::test]
async fn read_transcript_includes_user_when_opted_in() {
    let (db, _source) = setup_db("r4c-read-user").await;
    let hook =
        create_parent_hook(&db, "parent-user", "session-user", "spawn-user", "do work").await;
    let child = spawn_background_child(db.node.as_ref(), &hook, "spawn-user").await;
    let child_request_id = child["child_request_id"].as_str().unwrap();
    let child_session_id = child["child_session_id"].as_str().unwrap();
    append_message(db.node.as_ref(), child_session_id, 1, "assistant", "a1").await;
    append_message(db.node.as_ref(), child_session_id, 2, "user", "u1").await;

    let result = read_transcript(
        &db,
        &hook,
        "read-user",
        json!({
            "child_request_id": child_request_id,
            "include_user_messages": true
        }),
    )
    .await;
    let transcript = result["transcript"].as_str().unwrap();
    assert!(transcript.contains("a1"));
    assert!(transcript.contains("u1"));
}

#[tokio::test]
async fn read_transcript_hides_bridge_rows() {
    let (db, _source) = setup_db("r4c-read-bridge").await;
    let hook = create_parent_hook(
        &db,
        "parent-bridge",
        "session-bridge",
        "spawn-bridge",
        "do work",
    )
    .await;
    let child = spawn_background_child(db.node.as_ref(), &hook, "spawn-bridge").await;
    let child_request_id = child["child_request_id"].as_str().unwrap();
    let child_request_doc_id = child["child_request_doc_id"].as_str().unwrap();
    let child_session_id = child["child_session_id"].as_str().unwrap();
    let sequence = next_child_sequence(
        db.node.as_ref(),
        child_session_id,
        &child_source_scope(db.node.as_ref(), child_session_id).await,
    )
    .await;
    let tool_call_doc_id = create_child_bridge_tool_call(
        db.node.as_ref(),
        child_request_id,
        child_request_doc_id,
        child_session_id,
        sequence,
        "bridge-tc-1",
    )
    .await;
    append_assistant_tool_call_message(
        db.node.as_ref(),
        child_session_id,
        child_request_doc_id,
        sequence,
        "plain assistant message",
        "bridge-tc-1",
        &tool_call_doc_id,
        "spawn_subagent",
    )
    .await;

    let result = read_transcript(
        &db,
        &hook,
        "read-bridge",
        json!({ "child_request_id": child_request_id }),
    )
    .await;
    let transcript = result["transcript"].as_str().unwrap();
    assert!(transcript.contains("plain assistant message"));
    assert!(!transcript.contains("bridge-tc-1"));
    assert!(!transcript.contains("tool_calls="));
}

#[tokio::test]
async fn read_transcript_hides_tool_kind_background_bridge_rows() {
    let (db, _source) = setup_db("r4c-read-tool-bridge").await;
    let hook = create_parent_hook(
        &db,
        "parent-tool-bridge",
        "session-tool-bridge",
        "spawn-tool-bridge",
        "do work",
    )
    .await;
    let child = spawn_background_child(db.node.as_ref(), &hook, "spawn-tool-bridge").await;
    let child_request_id = child["child_request_id"].as_str().unwrap();
    let child_request_doc_id = child["child_request_doc_id"].as_str().unwrap();
    let child_session_id = child["child_session_id"].as_str().unwrap();
    let sequence = next_child_sequence(
        db.node.as_ref(),
        child_session_id,
        &child_source_scope(db.node.as_ref(), child_session_id).await,
    )
    .await;
    let tool_call_doc_id = create_background_tool_call(
        db.node.as_ref(),
        child_request_id,
        child_request_doc_id,
        child_session_id,
        sequence,
        "background-tc-1",
    )
    .await;
    append_assistant_tool_call_message(
        db.node.as_ref(),
        child_session_id,
        child_request_doc_id,
        sequence,
        "checking files",
        "background-tc-1",
        &tool_call_doc_id,
        "bash",
    )
    .await;

    let result = read_transcript(
        &db,
        &hook,
        "read-tool-bridge",
        json!({ "child_request_id": child_request_id }),
    )
    .await;
    let transcript = result["transcript"].as_str().unwrap();
    assert!(transcript.contains("checking files"));
    assert!(!transcript.contains("background-tc-1"));
    assert!(!transcript.contains("tool_calls="));
}

#[tokio::test]
async fn read_transcript_cursor_advances_cleanly() {
    let (db, _source) = setup_db("r4c-read-cursor").await;
    let hook = create_parent_hook(
        &db,
        "parent-cursor",
        "session-cursor",
        "spawn-cursor",
        "do work",
    )
    .await;
    let child = spawn_background_child(db.node.as_ref(), &hook, "spawn-cursor").await;
    let child_request_id = child["child_request_id"].as_str().unwrap();
    let child_session_id = child["child_session_id"].as_str().unwrap();
    let pad = "x".repeat(120);
    for sequence in 1..=10 {
        append_message(
            db.node.as_ref(),
            child_session_id,
            sequence,
            "assistant",
            &format!("turn {sequence} {pad}"),
        )
        .await;
    }

    let mut cursor = 0u64;
    let mut pages = 0;
    let mut seen = Vec::new();
    loop {
        let page = read_transcript(
            &db,
            &hook,
            &format!("read-cursor-{pages}"),
            json!({
                "child_request_id": child_request_id,
                "since_sequence": cursor,
                "max_tokens": 40
            }),
        )
        .await;
        let transcript = page["transcript"].as_str().unwrap();
        for sequence in 1..=10 {
            if transcript.contains(&format!("turn {sequence} ")) {
                seen.push(sequence);
            }
        }
        let next = page["next_sequence"].as_u64().unwrap();
        let has_more = page["has_more"].as_bool().unwrap();
        if !has_more {
            break;
        }
        assert!(
            next > cursor,
            "cursor must advance: next={next} cursor={cursor}"
        );
        cursor = next;
        pages += 1;
        assert!(pages < 50, "paging did not terminate");
    }
    assert!(pages >= 1, "small budget should force more than one page");
    seen.sort_unstable();
    assert_eq!(seen, (1..=10).collect::<Vec<u64>>(), "gap-free coverage");
}

#[tokio::test]
async fn read_transcript_terminal_flag_tracks_child_lifecycle() {
    let (db, _source) = setup_db("r4c-read-terminal").await;
    let hook = create_parent_hook(
        &db,
        "parent-terminal",
        "session-terminal",
        "spawn-terminal",
        "do work",
    )
    .await;
    let child = spawn_background_child(db.node.as_ref(), &hook, "spawn-terminal").await;
    let child_request_id = child["child_request_id"].as_str().unwrap();
    let child_session_id = child["child_session_id"].as_str().unwrap();
    // The child materializes `pending` and a separate worker claims it; read
    // only once that claim is durable.
    crate::support::interrupt::wait_for_request_lifecycle_state(
        db.node.as_ref(),
        child["child_request_doc_id"].as_str().unwrap(),
        "processing",
    )
    .await;
    append_message(
        db.node.as_ref(),
        child_session_id,
        1,
        "assistant",
        "working",
    )
    .await;

    let running = read_transcript(
        &db,
        &hook,
        "read-running",
        json!({ "child_request_id": child_request_id }),
    )
    .await;
    assert_eq!(running["terminal"].as_bool(), Some(false));
    assert_eq!(running["lifecycle_state"].as_str(), Some("processing"));

    mark_child_completed(db.node.as_ref(), child_request_id).await;
    let done = read_transcript(
        &db,
        &hook,
        "read-done",
        json!({ "child_request_id": child_request_id }),
    )
    .await;
    assert_eq!(done["terminal"].as_bool(), Some(true));
    assert_eq!(done["lifecycle_state"].as_str(), Some("completed"));
    assert_eq!(done["has_more"].as_bool(), Some(false));
}

#[tokio::test]
async fn read_transcript_rejects_unauthorized_child() {
    let (db, _source) = setup_db("r4c-read-unauthorized").await;
    let hook_2 =
        create_parent_hook(&db, "parent-two", "session-two", "spawn-sibling", "do work").await;
    let child = spawn_background_child(db.node.as_ref(), &hook_2, "spawn-sibling").await;
    let child_request_id = child["child_request_id"].as_str().unwrap();
    hook_2.runtime.shutdown().await;
    let hook_1 =
        create_parent_hook(&db, "parent-one", "session-one", "spawn-one", "do work one").await;

    let result = read_transcript(
        &db,
        &hook_1,
        "read-unauthorized",
        json!({ "child_request_id": child_request_id }),
    )
    .await;
    assert_eq!(result["ok"].as_bool(), Some(false));
    assert_eq!(result["failure_class"].as_str(), Some("tool_not_allowed"));
}

#[tokio::test]
async fn read_transcript_keeps_one_accepted_parent_control_row() {
    let (db, _source) = setup_db("r4c-read-no-row").await;
    let parent_session_id = "session-no-row";
    let hook = create_parent_hook(
        &db,
        "parent-no-row",
        parent_session_id,
        "spawn-no-row",
        "do work",
    )
    .await;
    let child = spawn_background_child(db.node.as_ref(), &hook, "spawn-no-row").await;
    let child_request_id = child["child_request_id"].as_str().unwrap();

    let _ = read_transcript(
        &db,
        &hook,
        "read-no-row",
        json!({ "child_request_id": child_request_id }),
    )
    .await;
    assert_eq!(
        count_tool_calls_by_name(db.node.as_ref(), parent_session_id, "read_subagent").await,
        1
    );
    let session = escape_graphql_string(parent_session_id);
    let response = db.node.execute(&format!(r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }}, tool_name: {{ _eq: "read_subagent" }} }}, limit: 2) {{ _docID request_doc_id message_sequence lifecycle_state }} }}"#)).await;
    assert!(
        !response.has_errors(),
        "accepted control query: {:?}",
        response.errors
    );
    let row = &response.data.as_ref().unwrap()["AgentToolCall"][0];
    assert_eq!(row["lifecycle_state"], "completed");
    let tool_doc_id = row["_docID"].as_str().unwrap();
    let request_doc_id = row["request_doc_id"].as_str().unwrap();
    let sequence = row["message_sequence"].as_u64().unwrap();
    let headers = db.node.execute(&format!(r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session}" }}, request_doc_id: {{ _eq: "{}" }}, sequence: {{ _eq: {sequence} }} }}) {{ _docID }} }}"#, escape_graphql_string(request_doc_id))).await;
    assert!(
        !headers.has_errors(),
        "accepted header query: {:?}",
        headers.errors
    );
    let rows = headers.data.as_ref().unwrap()["AgentMessage"]
        .as_array()
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "accepted control has one exact assistant header"
    );
    let header_doc_id = rows[0]["_docID"].as_str().unwrap();
    let (header, _) = gents::session::load_canonical_message_from_node(
        db.node.as_ref(),
        header_doc_id,
        db.node_identity.did(),
        Some(db.node_identity.did()),
    )
    .await
    .unwrap();
    assert!(
        header.blocks.iter().any(|block| matches!(block,
            MessageBlock::ToolCall { tool_call_doc_id, name, .. }
                if tool_call_doc_id == tool_doc_id && name == "read_subagent"
        )),
        "accepted header must bind the exact physical read control"
    );
}
