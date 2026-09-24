use defra_node::EmbeddedNode;
use gents::defra_node::{ExecuteRetryPolicy, QueryRequest};
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
pub(super) mod streaming_backend;

pub(super) async fn configure_runtime_behavior(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
    backend_id: &str,
    endpoint: &str,
    model: &str,
    subagents_enabled: bool,
) {
    use gents::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    use gents::document_config::{AgentContext, BashTools, HostTools, SubagentTools, Tools};
    use gents::Collection;

    super::seed_test_behavior_configuration(node, agent_did, behavior_id, behavior_id, model, true)
        .await;
    gents::ConfigAccess::transact_local(node, None, "grok.runtime_fixture", |txn| {
        Box::pin(async move {
            use gents::config_client::read_desired_state_record_in_txn as read;
            let (_, mut behavior) = read(txn, Collection::AgentBehavior, agent_did, behavior_id)
                .await?
                .expect("fixture behavior");
            let profile_id = behavior["inference_profile_id"]
                .as_str()
                .expect("fixture profile")
                .to_owned();
            let (_, mut profile) = read(txn, Collection::InferenceProfile, agent_did, &profile_id)
                .await?
                .expect("fixture profile");
            profile["backend_id"] = backend_id.into();
            profile["model_name"] = model.into();
            let tools_id = format!("{behavior_id}:tools");
            let context_id = format!("{behavior_id}:context");
            let mut tools = Tools {
                tools_id: tools_id.clone(),
                agent_did: agent_did.into(),
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
            let target_id = format!("{behavior_id}:self-target");
            if subagents_enabled {
                tools.subagents = Some(SubagentTools {
                    target_ids: vec![target_id.clone()],
                    spawn_enabled: Some(true),
                    steering_enabled: Some(true),
                    background_enabled: Some(true),
                    ..Default::default()
                });
            }
            let context = AgentContext {
                context_id: context_id.clone(),
                agent_did: agent_did.into(),
                display_name: None,
                description: None,
                system_prompt: None,
                tools_id: Some(tools_id),
                compaction_id: None,
                skill_ids: Vec::new(),
                tags: Vec::new(),
            };
            behavior["context_id"] = context_id.into();
            let backend = serde_json::json!({
                "agent_did": agent_did,
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
                (Collection::AgentBehavior, behavior),
            ];
            if subagents_enabled {
                docs.push((
                    Collection::SubagentTarget,
                    serde_json::to_value(gents::SubagentTargetDocument {
                        target_id,
                        agent_did: agent_did.into(),
                        target_agent_did: agent_did.into(),
                        behavior_id: behavior_id.into(),
                        name: behavior_id.into(),
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

/// Publish one typed canonical assistant fixture. Each block owns a distinct
/// authored source coordinate and closed payload stream; no retired inline
/// message content or response row participates in the projection.
pub(super) async fn seed_canonical_assistant_message(
    node: &EmbeddedNode,
    request: &gents_protocol::row::AgentRequestRow,
    message_key: &str,
    sequence: i64,
    reasoning: &str,
    text: &str,
) -> String {
    let agent_did = request.agent_did.as_deref().expect("fixture owner");
    let session_id = request.session_id.as_deref().expect("fixture session");
    let request_doc_id = request.doc_id.as_deref().expect("fixture physical request");
    let execution_generation = format!("fixture:{request_doc_id}");
    let created_at = chrono::Utc::now().to_rfc3339();
    let writer = OutputWriter::RequestExecution {
        execution_generation: execution_generation.clone(),
    };
    let segment = |block_index: u32, payload: &str, stream_payload: StreamPayload| OutputSegment {
        agent_did: agent_did.into(),
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
            .execute_request_with_retry(
                QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                    .with_variables(output_segment_create_variables(value).unwrap()),
                ExecuteRetryPolicy::default(),
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
        agent_did: agent_did.into(),
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
        .execute_request_with_retry(
            QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&message).unwrap()),
            ExecuteRetryPolicy::default(),
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

async fn insert_output_segment(node: &EmbeddedNode, segment: &OutputSegment) -> String {
    let response = node
        .execute_request_with_retry(
            QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(segment).unwrap()),
            ExecuteRetryPolicy::default(),
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

async fn insert_transcript_header(node: &EmbeddedNode, header: &TranscriptMessage) {
    let response = node
        .execute_request_with_retry(
            QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(header).unwrap()),
            ExecuteRetryPolicy::default(),
        )
        .await;
    assert!(
        !response.has_errors(),
        "message seed: {:?}",
        response.errors
    );
}

pub(super) async fn next_request_sequence(node: &EmbeddedNode, request_doc_id: &str) -> u32 {
    let request_doc_id = gents::graphql::escape_graphql_string(request_doc_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{_docID: {{_eq: "{request_doc_id}"}}}}, limit: 2) {{session_id}} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "request scope lookup: {:?}",
        response.errors
    );
    let requests = response.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap();
    assert_eq!(requests.len(), 1, "fixture request scope must be exact");
    let session_id = gents::graphql::escape_graphql_string(
        requests[0]["session_id"].as_str().expect("fixture session"),
    );
    let response = node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{session_id: {{_eq: "{session_id}"}}}}, order: [{{sequence: DESC}}], limit: 1) {{sequence}} }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "sequence lookup: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("sequence"))
        .and_then(serde_json::Value::as_u64)
        .map_or(0, |value| u32::try_from(value).unwrap())
        .checked_add(1)
        .expect("fixture transcript sequence overflow")
}

/// Seed one physical tool row through a matching canonical ToolCall admission
/// and, for terminal lifecycle states, one exact ToolDelivery observation.
/// This is the sole Grok-shim test encoding owner for tool arguments/results.
#[allow(clippy::too_many_arguments)]
pub(super) async fn seed_canonical_tool_call(
    node: &EmbeddedNode,
    request: &gents_protocol::row::AgentRequestRow,
    tool_call_id: &str,
    tool_name: &str,
    lifecycle_state: &str,
    arguments: &str,
    result: Option<&str>,
    child_request_id: Option<&str>,
    spawned_by_tool_call_doc_id: Option<&str>,
    message_sequence: Option<u32>,
    created_at: Option<&str>,
) -> String {
    use gents_protocol::output::ToolResultPart;

    let agent_did = request.agent_did.as_deref().expect("fixture owner");
    let requester_did = request.requester_did.clone();
    let session_id = request.session_id.as_deref().expect("fixture session");
    let request_doc_id = request.doc_id.as_deref().expect("fixture request doc");
    // Keep the physical provider source unique even when a test deliberately
    // gives two calls the same visible message sequence to exercise the
    // projection's stable-identity tie break.
    let source_sequence = next_request_sequence(node, request_doc_id).await;
    let sequence = if let Some(sequence) = message_sequence {
        sequence
    } else if let Some(parent_doc_id) = spawned_by_tool_call_doc_id {
        // A spawned process has no independent provider admission: its
        // visible sequence is the exact spawn_process parent's coordinate.
        let parent = node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{_docID: {{_eq: "{}"}}}}, limit: 2) {{
                    request_doc_id agent_did requester_did session_id tool_name message_sequence
                }} }}"#,
                gents::graphql::escape_graphql_string(parent_doc_id)
            ))
            .await;
        assert!(!parent.has_errors(), "spawn parent: {:?}", parent.errors);
        let rows = parent.data.as_ref().unwrap()["AgentToolCall"]
            .as_array()
            .unwrap();
        assert_eq!(rows.len(), 1, "spawn fixture requires one physical parent");
        let parent = &rows[0];
        assert_eq!(parent["request_doc_id"].as_str(), Some(request_doc_id));
        assert_eq!(parent["agent_did"].as_str(), Some(agent_did));
        assert_eq!(parent["requester_did"].as_str(), requester_did.as_deref());
        assert_eq!(parent["session_id"].as_str(), Some(session_id));
        assert_eq!(parent["tool_name"].as_str(), Some("spawn_process"));
        u32::try_from(parent["message_sequence"].as_u64().unwrap()).unwrap()
    } else {
        source_sequence
    };
    let created_at = created_at
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let requester = requester_did
        .as_deref()
        .map(|did| format!("\"{}\"", gents::graphql::escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    let child = child_request_id
        .map(|id| format!("\"{}\"", gents::graphql::escape_graphql_string(id)))
        .unwrap_or_else(|| "null".into());
    let spawned_by = spawned_by_tool_call_doc_id
        .map(|id| format!("\"{}\"", gents::graphql::escape_graphql_string(id)))
        .unwrap_or_else(|| "null".into());
    let response = node.execute(&format!(r#"mutation {{ create_AgentToolCall(input: {{
        tool_call_key: "{}:{}", request_id: "{}", request_doc_id: "{}",
        agent_did: "{}", requester_did: {requester}, session_id: "{}",
        tool_call_id: "{}", tool_name: "{}", message_sequence: {sequence},
        lifecycle_state: "{}", child_request_id: {child}, spawned_by_tool_call_doc_id: {spawned_by}, started_at: "{}"
    }}) {{_docID}} }}"#,
        gents::graphql::escape_graphql_string(request_doc_id),
        gents::graphql::escape_graphql_string(tool_call_id),
        gents::graphql::escape_graphql_string(&request.request_id),
        gents::graphql::escape_graphql_string(request_doc_id),
        gents::graphql::escape_graphql_string(agent_did),
        gents::graphql::escape_graphql_string(session_id),
        gents::graphql::escape_graphql_string(tool_call_id),
        gents::graphql::escape_graphql_string(tool_name),
        gents::graphql::escape_graphql_string(lifecycle_state),
        gents::graphql::escape_graphql_string(&created_at),
    )).await;
    assert!(
        !response.has_errors(),
        "tool row seed: {:?}",
        response.errors
    );
    let tool_doc_id = single_mutation_document(&response, "create_AgentToolCall")
        .unwrap()
        .unwrap()["_docID"]
        .as_str()
        .unwrap()
        .to_owned();
    if spawned_by_tool_call_doc_id.is_some() {
        assert!(
            result.is_none(),
            "spawned fixture results must be delivered by their lifecycle owner"
        );
        return tool_doc_id;
    }
    let generation = format!("fixture:{request_doc_id}");
    let admitted = OutputSegment {
        agent_did: agent_did.into(),
        requester_did: requester_did.clone(),
        session_id: session_id.into(),
        request_doc_id: request_doc_id.into(),
        source: OutputSource::ProviderTurn {
            scope: format!("inference.{}", source_sequence.saturating_add(1))
                .parse()
                .unwrap(),
            turn_index: source_sequence,
            attempt: 0,
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: generation.clone(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: arguments.len() as u32,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolArguments {
                    id: tool_call_id.into(),
                    call_id: None,
                    name: tool_name.into(),
                },
            }),
        }],
        payload: arguments.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![arguments.len() as u64],
        }),
        created_at: created_at.clone(),
    };
    let admitted_doc = insert_output_segment(node, &admitted).await;
    insert_transcript_header(
        node,
        &TranscriptMessage {
            message_key: format!("accepted:{request_doc_id}:{sequence}"),
            session_id: session_id.into(),
            agent_did: agent_did.into(),
            requester_did: requester_did.clone(),
            request_doc_id: Some(request_doc_id.into()),
            publication: MessagePublication::RequestExecution {
                execution_generation: generation,
            },
            outcome: OutputOutcome::Complete,
            sequence,
            role: MessageRole::Assistant,
            native_id: None,
            blocks: vec![MessageBlock::ToolCall {
                tool_call_doc_id: tool_doc_id.clone().into(),
                id: tool_call_id.into(),
                call_id: None,
                name: tool_name.into(),
                arguments: PayloadRef {
                    close_doc_id: admitted_doc,
                    stream: 0,
                },
                signature: None,
                additional_params: None,
            }],
            created_at: created_at.clone(),
        },
    )
    .await;
    if let Some(result) = result {
        // Tool-row chronology and transcript publication chronology are
        // separate coordinates. In particular, another accepted tool may
        // deliberately use the adjacent (or same) visible tool sequence.
        // Allocate the reply in the session's canonical sequence space
        // instead of fabricating `admission + 1`, which would create an
        // immutable header twin and make both publications conflicting.
        let delivery_sequence = next_request_sequence(node, request_doc_id).await;
        let delivered = OutputSegment {
            agent_did: agent_did.into(),
            requester_did: requester_did.clone(),
            session_id: session_id.into(),
            request_doc_id: request_doc_id.into(),
            source: OutputSource::ToolCall {
                tool_call_doc_id: tool_doc_id.clone().into(),
            },
            writer: OutputWriter::ToolExecution {
                tool_call_doc_id: tool_doc_id.clone().into(),
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
        let delivered_doc = insert_output_segment(node, &delivered).await;
        insert_transcript_header(
            node,
            &TranscriptMessage {
                message_key: format!("delivery:{request_doc_id}:{tool_call_id}"),
                session_id: session_id.into(),
                agent_did: agent_did.into(),
                requester_did,
                request_doc_id: Some(request_doc_id.into()),
                publication: MessagePublication::ToolDelivery {
                    tool_call_doc_id: tool_doc_id.clone().into(),
                },
                outcome: OutputOutcome::Complete,
                sequence: delivery_sequence,
                role: MessageRole::User,
                native_id: None,
                blocks: vec![MessageBlock::ToolResult {
                    tool_call_doc_id: tool_doc_id.clone().into(),
                    id: tool_call_id.into(),
                    call_id: None,
                    parts: vec![ToolResultPart::Text {
                        text: PresentedPayload {
                            output: PayloadRef {
                                close_doc_id: delivered_doc,
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
    }
    tool_doc_id
}

/// Complete an already-admitted ordinary tool through its canonical output
/// and ToolDelivery owners, then publish the terminal lifecycle marker.
pub(super) async fn complete_canonical_tool_call(
    node: &EmbeddedNode,
    request: &gents_protocol::row::AgentRequestRow,
    tool_doc_id: &str,
    tool_call_id: &str,
    result: &str,
) {
    use gents_protocol::output::ToolResultPart;

    let agent_did = request.agent_did.as_deref().expect("fixture owner");
    let requester_did = request.requester_did.clone();
    let session_id = request.session_id.as_deref().expect("fixture session");
    let request_doc_id = request.doc_id.as_deref().expect("fixture request doc");
    let created_at = chrono::Utc::now().to_rfc3339();
    let segment = OutputSegment {
        agent_did: agent_did.into(),
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
            agent_did: agent_did.into(),
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
        agent_did: request.agent_did.as_deref().expect("fixture owner").into(),
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
/// the spawn_process invocation already owns its separate admission reply.
pub(super) async fn complete_canonical_spawned_process_output(
    node: &EmbeddedNode,
    request: &gents_protocol::row::AgentRequestRow,
    tool_doc_id: &str,
    output: &str,
) {
    let agent_did = request.agent_did.as_deref().expect("fixture owner");
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
            agent_did: agent_did.into(),
            requester_did: requester_did.clone(),
            session_id: session_id.into(),
            request_doc_id: request_doc_id.into(),
            source: OutputSource::ToolCall {
                tool_call_doc_id: tool_doc_id.into(),
            },
            writer: OutputWriter::ToolExecution {
                tool_call_doc_id: tool_doc_id.into(),
            },
            // The open output already supplied the only data flush. Closing
            // it adds a closure fact, not a synthetic zero-byte flush.
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
