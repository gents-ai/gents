use std::sync::Arc;

pub(crate) use crate::commands::test_fixtures::{
    insert_output_segment, insert_transcript_header, next_request_sequence,
    seed_canonical_tool_call, seed_job_row,
};

use defra_node::EmbeddedNode;
use gents::defra_node::QueryRequest;
use gents::graphql::single_mutation_document;
use gents::session::canonical_rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, ReasoningPart, SegmentRun,
    SourceClose, StreamDeclaration, StreamPayload, TranscriptMessage,
};

#[path = "../../../../gents/tests/support/streaming_backend.rs"]
#[expect(
    dead_code,
    reason = "The shared backend also supports stream gates and counters used by other test binaries."
)]
pub(super) mod streaming_backend;

pub(super) async fn configure_runtime_agent(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
    backend_id: &str,
    endpoint: &str,
    model: &str,
    agents_enabled: bool,
) {
    use gents::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    use gents::document_config::{AgentContext, AgentTools, BashTools, HostTools, Tools};
    use gents::Collection;

    super::seed_test_agent_configuration(node, node_did, agent_id, agent_id, model, true).await;
    gents::ConfigAccess::transact_local_write(node, None, "grok.runtime_fixture", |txn| {
        Box::pin(async move {
            use gents::config_client::read_desired_state_record_in_txn as read;
            let (_, mut agent) = read(txn, Collection::Agent, node_did, agent_id)
                .await?
                .expect("fixture agent");
            let profile_id = agent["inference_profile_id"]
                .as_str()
                .expect("fixture profile")
                .to_owned();
            let (_, mut profile) = read(txn, Collection::InferenceProfile, node_did, &profile_id)
                .await?
                .expect("fixture profile");
            profile["backend_id"] = backend_id.into();
            profile["model_name"] = model.into();
            let tools_id = format!("{agent_id}:tools");
            let context_id = format!("{agent_id}:context");
            let mut tools = Tools {
                tools_id: tools_id.clone(),
                node_did: node_did.into(),
                host: Some(HostTools {
                    bash: Some(BashTools {
                        mode: gents::BashMode::ReadOnly,
                        read_only_commands: Some(vec!["sleep".into(), "printf".into()]),
                        background_enabled: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let target_id = format!("{agent_id}:self-target");
            if agents_enabled {
                tools.agents = Some(AgentTools {
                    target_ids: vec![target_id.clone()],
                    enabled: Some(true),
                });
            }
            let context = AgentContext {
                context_id: context_id.clone(),
                node_did: node_did.into(),
                display_name: None,
                description: None,
                system_prompt: None,
                tools_id: Some(tools_id),
                compaction_id: None,
                skill_ids: Vec::new(),
                tags: Vec::new(),
            };
            agent["context_id"] = context_id.into();
            let backend = serde_json::json!({
                "node_did": node_did,
                "backend_id": backend_id,
                "name": "grok runtime fixture",
                "provider_kind": "OpenAiCompatible",
                "openai_wire_api": "chat_completions",
                "endpoint": endpoint,
                "auth": {"kind":"unauthenticated"},
                "enabled": true
            });
            let mut docs = vec![
                (Collection::InferenceBackend, backend),
                (Collection::InferenceProfile, profile),
                (Collection::Tools, serde_json::to_value(tools)?),
                (Collection::AgentContext, serde_json::to_value(context)?),
                (Collection::Agent, agent),
            ];
            if agents_enabled {
                docs.push((
                    Collection::AgentTarget,
                    serde_json::to_value(gents::AgentTargetDocument {
                        target_id,
                        node_did: node_did.into(),
                        target_node_did: node_did.into(),
                        agent_id: agent_id.into(),
                        name: agent_id.into(),
                        description: Some("self target for Grok control fixture".into()),
                        tags: Vec::new(),
                    })?,
                ));
            }
            let plan = DesiredStateApplyPlan::new(
                docs.into_iter()
                    .map(|(collection, value)| DesiredStateApplyDocument {
                        collection,
                        add: value.clone(),
                        update: value,
                    })
                    .collect(),
            )?;
            gents::config_client::apply_desired_state_plan(txn, &plan)
                .await
                .map(|_| ())
        })
    })
    .await
    .unwrap();
}

pub(super) async fn seed_canonical_assistant_message(
    node: &EmbeddedNode,
    request: &gents_protocol::row::AgentRequestRow,
    message_key: &str,
    sequence: i64,
    reasoning: &str,
    text: &str,
) -> String {
    let node_did = request.node_did.as_deref().expect("fixture owner");
    let session_id = request.session_id.as_deref().expect("fixture session");
    let request_doc_id = request.doc_id.as_deref().expect("fixture physical request");
    let execution_generation = format!("fixture:{request_doc_id}");
    let created_at = chrono::Utc::now().to_rfc3339();
    let writer = OutputWriter::RequestExecution {
        execution_generation: execution_generation.clone(),
    };
    let segment = |block_index: u32, payload: &str, stream_payload: StreamPayload| OutputSegment {
        node_did: node_did.into(),
        requester_did: request.requester_did.clone(),
        session_id: session_id.into(),
        request_doc_id: request_doc_id.into(),
        source: OutputSource::Authored {
            key: format!("{message_key}:block:{block_index}"),
        },
        writer: writer.clone(),
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: payload.len() as u32,
            declaration: Some(StreamDeclaration {
                block_index,
                part_index: 0,
                payload: stream_payload,
            }),
        }],
        payload: payload.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![payload.len() as u64],
        }),
        created_at: created_at.clone(),
    };
    async fn insert_segment(node: &EmbeddedNode, value: &OutputSegment) -> String {
        let response = node
            .execute_request(
                QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                    .with_variables(output_segment_create_variables(value).unwrap()),
            )
            .await;
        assert!(
            !response.has_errors(),
            "segment seed: {:?}",
            response.errors
        );
        single_mutation_document(&response, "create_AgentOutputSegment")
            .unwrap()
            .unwrap()["_docID"]
            .as_str()
            .unwrap()
            .to_string()
    }
    let reasoning_close_doc_id =
        insert_segment(node, &segment(0, reasoning, StreamPayload::Reasoning)).await;
    let text_close_doc_id = insert_segment(node, &segment(1, text, StreamPayload::Text)).await;
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
        role: MessageRole::Assistant,
        native_id: None,
        blocks: vec![
            MessageBlock::Reasoning {
                id: None,
                parts: vec![ReasoningPart::Text {
                    text: PayloadRef {
                        close_doc_id: reasoning_close_doc_id,
                        stream: 0,
                    },
                    signature: None,
                }],
            },
            MessageBlock::Text {
                text: PresentedPayload {
                    output: PayloadRef {
                        close_doc_id: text_close_doc_id,
                        stream: 0,
                    },
                    presentation: PayloadPresentation::Full,
                },
            },
        ],
        created_at,
    };
    let response = node
        .execute_request(
            QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&message).unwrap()),
        )
        .await;
    assert!(
        !response.has_errors(),
        "message seed: {:?}",
        response.errors
    );
    single_mutation_document(&response, "create_AgentMessage")
        .unwrap()
        .unwrap()["_docID"]
        .as_str()
        .unwrap()
        .to_string()
}

pub(super) async fn complete_canonical_tool_call(
    node: &EmbeddedNode,
    request: &gents_protocol::row::AgentRequestRow,
    tool_doc_id: &str,
    tool_call_id: &str,
    result: &str,
) {
    use gents_protocol::output::ToolResultPart;

    let node_did = request.node_did.as_deref().expect("fixture owner");
    let requester_did = request.requester_did.clone();
    let session_id = request.session_id.as_deref().expect("fixture session");
    let request_doc_id = request.doc_id.as_deref().expect("fixture request doc");
    let created_at = chrono::Utc::now().to_rfc3339();
    let segment = OutputSegment {
        node_did: node_did.into(),
        requester_did: requester_did.clone(),
        session_id: session_id.into(),
        request_doc_id: request_doc_id.into(),
        source: OutputSource::ToolCall {
            tool_call_doc_id: tool_doc_id.into(),
        },
        writer: OutputWriter::ToolExecution {
            tool_call_doc_id: tool_doc_id.into(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: result.len() as u32,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolOutput,
            }),
        }],
        payload: result.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![result.len() as u64],
        }),
        created_at: created_at.clone(),
    };
    let close_doc_id = insert_output_segment(node, &segment).await;
    let sequence = next_request_sequence(node, request_doc_id).await;
    insert_transcript_header(
        node,
        &TranscriptMessage {
            message_key: format!("delivery:{request_doc_id}:{tool_call_id}"),
            session_id: session_id.into(),
            node_did: node_did.into(),
            requester_did,
            request_doc_id: Some(request_doc_id.into()),
            publication: MessagePublication::ToolDelivery {
                tool_call_doc_id: tool_doc_id.into(),
            },
            outcome: OutputOutcome::Complete,
            sequence,
            role: MessageRole::User,
            native_id: None,
            blocks: vec![MessageBlock::ToolResult {
                tool_call_doc_id: tool_doc_id.into(),
                id: tool_call_id.into(),
                call_id: None,
                parts: vec![ToolResultPart::Text {
                    text: PresentedPayload {
                        output: PayloadRef {
                            close_doc_id,
                            stream: 0,
                        },
                        presentation: PayloadPresentation::Full,
                    },
                }],
            }],
            created_at,
        },
    )
    .await;
    let response = node
        .execute(&format!(
            r#"mutation {{ update_AgentToolCall(filter: {{_docID: {{_eq: "{}"}}}}, input: {{lifecycle_state: "completed"}}) {{_docID}} }}"#,
            gents::graphql::escape_graphql_string(tool_doc_id)
        ))
        .await;
    assert!(
        !response.has_errors(),
        "tool completion: {:?}",
        response.errors
    );
}

/// Append an open canonical ToolOutput window for an admitted running tool.
pub(super) async fn seed_canonical_live_tool_output(
    node: &EmbeddedNode,
    request: &gents_protocol::row::AgentRequestRow,
    tool_doc_id: &str,
    output: &str,
) {
    let segment = OutputSegment {
        node_did: request.node_did.as_deref().expect("fixture owner").into(),
        requester_did: request.requester_did.clone(),
        session_id: request
            .session_id
            .as_deref()
            .expect("fixture session")
            .into(),
        request_doc_id: request
            .doc_id
            .as_deref()
            .expect("fixture request doc")
            .into(),
        source: OutputSource::ToolCall {
            tool_call_doc_id: tool_doc_id.into(),
        },
        writer: OutputWriter::ToolExecution {
            tool_call_doc_id: tool_doc_id.into(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: output.len() as u32,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolOutput,
            }),
        }],
        payload: output.into(),
        close: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    insert_output_segment(node, &segment).await;
}

/// A spawned process closes its raw output without inventing a native reply;
/// the spawn_process invocation owns its separate admission reply.
pub(super) async fn complete_canonical_spawned_process_output(
    node: &EmbeddedNode,
    request: &gents_protocol::row::AgentRequestRow,
    tool_doc_id: &str,
    output: &str,
) {
    let node_did = request.node_did.as_deref().expect("fixture owner");
    let requester_did = request.requester_did.clone();
    let session_id = request.session_id.as_deref().expect("fixture session");
    let request_doc_id = request.doc_id.as_deref().expect("fixture request doc");
    let created_at = chrono::Utc::now().to_rfc3339();
    let owner = node.execute(&format!(
        r#"{{ AgentToolCall(filter: {{_docID: {{_eq: "{}"}}}}, limit: 2) {{request_doc_id spawned_by_tool_call_doc_id}} }}"#,
        gents::graphql::escape_graphql_string(tool_doc_id)
    )).await;
    assert!(!owner.has_errors(), "spawned owner: {:?}", owner.errors);
    let rows = owner.data.as_ref().unwrap()["AgentToolCall"]
        .as_array()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["request_doc_id"].as_str(), Some(request_doc_id));
    assert!(rows[0]["spawned_by_tool_call_doc_id"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
    insert_output_segment(
        node,
        &OutputSegment {
            node_did: node_did.into(),
            requester_did: requester_did.clone(),
            session_id: session_id.into(),
            request_doc_id: request_doc_id.into(),
            source: OutputSource::ToolCall {
                tool_call_doc_id: tool_doc_id.into(),
            },
            writer: OutputWriter::ToolExecution {
                tool_call_doc_id: tool_doc_id.into(),
            },
            ordinal: None,
            runs: Vec::new(),
            payload: String::new(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![output.len() as u64],
            }),
            created_at: created_at.clone(),
        },
    )
    .await;
    let response = node.execute(&format!(r#"mutation {{ update_AgentToolCall(filter: {{_docID: {{_eq: "{}"}}}}, input: {{lifecycle_state: "completed"}}) {{_docID}} }}"#, gents::graphql::escape_graphql_string(tool_doc_id))).await;
    assert!(
        !response.has_errors(),
        "live tool completion: {:?}",
        response.errors
    );
}

/// Record that `session_id` was started by `parent_request_doc_id`, the
/// stored provenance every lineage reader follows. Idempotent per scope.
pub(super) async fn seed_started_session(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    agent_id: &str,
    parent_request_doc_id: &str,
    opening_request_doc_id: Option<&str>,
) {
    use gents::graphql::escape_graphql_string;
    let opening = opening_request_doc_id
        .map(|doc_id| {
            format!(
                r#", opening_request_doc_id: "{}""#,
                escape_graphql_string(doc_id)
            )
        })
        .unwrap_or_default();
    let requester = requester_did
        .map(|did| format!("\"{}\"", escape_graphql_string(did)))
        .unwrap_or_else(|| "null".to_owned());
    let existing = node
        .execute(&format!(
            r#"{{ AgentSession(filter: {{session_id: {{_eq: "{}"}}, node_did: {{_eq: "{}"}}, requester_did: {{_eq: {requester}}}}}) {{ _docID }} }}"#,
            escape_graphql_string(session_id),
            escape_graphql_string(node_did),
        ))
        .await;
    assert!(!existing.has_errors(), "{:?}", existing.errors);
    if existing
        .data
        .as_ref()
        .and_then(|data| data.get("AgentSession"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|rows| !rows.is_empty())
    {
        return;
    }
    let created = node
        .execute(&format!(
            r#"mutation {{ create_AgentSession(input: {{
                session_id: "{}", node_did: "{}", requester_did: {requester}, agent_id: "{}",
                created_at: "2026-09-01T00:00:00Z", provenance: {{parent_request_doc_id: "{}"{opening}}}
            }}) {{ _docID }} }}"#,
            escape_graphql_string(session_id),
            escape_graphql_string(node_did),
            escape_graphql_string(agent_id),
            escape_graphql_string(parent_request_doc_id),
        ))
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);
}
