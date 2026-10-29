use std::sync::Arc;

use defra_node::EmbeddedNode;
use gents::config_client::ConfigAccess;
use gents::defra_node::QueryRequest;
use gents::graphql::{ensure_no_errors, single_mutation_document};
use gents::session::canonical_rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
#[cfg(unix)]
use gents_protocol::output::ReasoningPart;
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
    StreamDeclaration, StreamPayload, TranscriptMessage,
};
use serde_json::json;

#[path = "../../../gents/tests/support/streaming_backend.rs"]
#[expect(
    dead_code,
    reason = "The shared backend also supports stream gates and counters used by other test binaries."
)]
pub(crate) mod streaming_backend;

#[cfg(unix)]
pub(crate) async fn configure_runtime_agent(
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

    seed_test_agent_configuration(node, node_did, agent_id, agent_id, model, true).await;
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

#[cfg(unix)]
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

async fn insert_output_segment(node: &EmbeddedNode, segment: &OutputSegment) -> String {
    let response = node
        .execute_request(
            QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(segment).unwrap()),
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
        .execute_request(
            QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(header).unwrap()),
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
#[allow(clippy::too_many_arguments)]
pub(crate) async fn seed_canonical_tool_call(
    node: &Arc<EmbeddedNode>,
    request: &gents_protocol::row::AgentRequestRow,
    tool_call_id: &str,
    tool_name: &str,
    lifecycle_state: &str,
    arguments: &str,
    result: Option<&str>,
    spawned_by_tool_call_doc_id: Option<&str>,
    message_sequence: Option<u32>,
    created_at: Option<&str>,
) -> String {
    use gents_protocol::output::ToolResultPart;

    let node_did = request.node_did.as_deref().expect("fixture owner");
    let requester_did = request.requester_did.clone();
    let session_id = request.session_id.as_deref().expect("fixture session");
    let request_doc_id = request.doc_id.as_deref().expect("fixture request doc");
    let source_sequence = next_request_sequence(node, request_doc_id).await;
    let sequence = if let Some(sequence) = message_sequence {
        sequence
    } else if let Some(parent_doc_id) = spawned_by_tool_call_doc_id {
        let parent = node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{_docID: {{_eq: "{}"}}}}, limit: 2) {{
                    request_doc_id node_did requester_did session_id tool_name message_sequence
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
        assert_eq!(parent["node_did"].as_str(), Some(node_did));
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
    let spawned_by = spawned_by_tool_call_doc_id
        .map(|id| format!("\"{}\"", gents::graphql::escape_graphql_string(id)))
        .unwrap_or_else(|| "null".into());
    let tool_mutation = format!(
        r#"mutation {{ create_AgentToolCall(input: {{
        tool_call_key: "{}:{}", request_id: "{}", request_doc_id: "{}",
        node_did: "{}", requester_did: {requester}, session_id: "{}",
        tool_call_id: "{}", tool_name: "{}", message_sequence: {sequence},
        lifecycle_state: "{}", spawned_by_tool_call_doc_id: {spawned_by}, started_at: "{}"
    }}) {{_docID}} }}"#,
        gents::graphql::escape_graphql_string(request_doc_id),
        gents::graphql::escape_graphql_string(tool_call_id),
        gents::graphql::escape_graphql_string(&request.request_id),
        gents::graphql::escape_graphql_string(request_doc_id),
        gents::graphql::escape_graphql_string(node_did),
        gents::graphql::escape_graphql_string(session_id),
        gents::graphql::escape_graphql_string(tool_call_id),
        gents::graphql::escape_graphql_string(tool_name),
        gents::graphql::escape_graphql_string(lifecycle_state),
        gents::graphql::escape_graphql_string(&created_at),
    );
    let is_spawned_child = spawned_by_tool_call_doc_id.is_some();
    if is_spawned_child {
        assert!(
            result.is_none(),
            "spawned fixture results must be delivered by their lifecycle owner"
        );
    }
    let generation = format!("fixture:{request_doc_id}");
    let admitted = OutputSegment {
        node_did: node_did.into(),
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
    let admitted_variables = output_segment_create_variables(&admitted).unwrap();
    let admitted_generation = generation.clone();
    let requester_did_for_header = requester_did.clone();
    let node_did_for_header = node_did.to_owned();
    let session_id_for_header = session_id.to_owned();
    let request_doc_id_for_header = request_doc_id.to_owned();
    let tool_call_id_for_header = tool_call_id.to_owned();
    let tool_name_for_header = tool_name.to_owned();
    let created_at_for_header = created_at.clone();
    let message_key = format!("accepted:{request_doc_id}:{sequence}");
    let tool_doc_id = gents::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "grok.seed_canonical_tool_call",
        move |txn| {
            let tool_mutation = tool_mutation.clone();
            let admitted_variables = admitted_variables.clone();
            let admitted_generation = admitted_generation.clone();
            let requester_did_for_header = requester_did_for_header.clone();
            let node_did_for_header = node_did_for_header.clone();
            let session_id_for_header = session_id_for_header.clone();
            let request_doc_id_for_header = request_doc_id_for_header.clone();
            let tool_call_id_for_header = tool_call_id_for_header.clone();
            let tool_name_for_header = tool_name_for_header.clone();
            let created_at_for_header = created_at_for_header.clone();
            let message_key = message_key.clone();
            Box::pin(async move {
                let tool_response = txn.execute(&tool_mutation).await?;
                let tool_doc_id = gents_protocol::graphql::extract_mutation_doc_id(
                    &tool_response,
                    "AgentToolCall",
                )?;
                if is_spawned_child {
                    return Ok::<_, anyhow::Error>(tool_doc_id);
                }
                let admitted_response = txn
                    .execute_with_variables(
                        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                        &admitted_variables,
                    )
                    .await?;
                let admitted_doc = gents_protocol::graphql::extract_mutation_doc_id(
                    &admitted_response,
                    "AgentOutputSegment",
                )?;
                let header = TranscriptMessage {
                    message_key,
                    session_id: session_id_for_header,
                    node_did: node_did_for_header,
                    requester_did: requester_did_for_header,
                    request_doc_id: Some(request_doc_id_for_header),
                    publication: MessagePublication::RequestExecution {
                        execution_generation: admitted_generation,
                    },
                    outcome: OutputOutcome::Complete,
                    sequence,
                    role: MessageRole::Assistant,
                    native_id: None,
                    blocks: vec![MessageBlock::ToolCall {
                        tool_call_doc_id: tool_doc_id.clone(),
                        id: tool_call_id_for_header,
                        call_id: None,
                        name: tool_name_for_header,
                        arguments: PayloadRef {
                            close_doc_id: admitted_doc,
                            stream: 0,
                        },
                        signature: None,
                        additional_params: None,
                    }],
                    created_at: created_at_for_header,
                };
                let header_variables = transcript_message_create_variables(&header)?;
                txn.execute_with_variables(CREATE_AGENT_MESSAGE_MUTATION, &header_variables)
                    .await?;
                Ok(tool_doc_id)
            })
        },
    )
    .await
    .expect("atomically seed canonical tool admission");
    if spawned_by_tool_call_doc_id.is_some() {
        return tool_doc_id;
    }
    if let Some(result) = result {
        let delivery_sequence = next_request_sequence(node, request_doc_id).await;
        let delivered = OutputSegment {
            node_did: node_did.into(),
            requester_did: requester_did.clone(),
            session_id: session_id.into(),
            request_doc_id: request_doc_id.into(),
            source: OutputSource::ToolCall {
                tool_call_doc_id: tool_doc_id.clone(),
            },
            writer: OutputWriter::ToolExecution {
                tool_call_doc_id: tool_doc_id.clone(),
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
                node_did: node_did.into(),
                requester_did,
                request_doc_id: Some(request_doc_id.into()),
                publication: MessagePublication::ToolDelivery {
                    tool_call_doc_id: tool_doc_id.clone(),
                },
                outcome: OutputOutcome::Complete,
                sequence: delivery_sequence,
                role: MessageRole::User,
                native_id: None,
                blocks: vec![MessageBlock::ToolResult {
                    tool_call_doc_id: tool_doc_id.clone(),
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

/// Publish the background receipt an accepted background row carries before
/// its terminal, in the shape `publish_background_receipt_in_txn` writes.
pub(crate) async fn seed_background_receipt(
    node: &EmbeddedNode,
    request: &gents_protocol::row::AgentRequestRow,
    tool_doc_id: &str,
    tool_call_id: &str,
) {
    use gents_protocol::output::ToolResultPart;

    let node_did = request.node_did.as_deref().expect("fixture owner");
    let session_id = request.session_id.as_deref().expect("fixture session");
    let request_doc_id = request.doc_id.as_deref().expect("fixture request doc");
    let key = format!("{session_id}:background-receipt:{tool_doc_id}");
    let created_at = chrono::Utc::now().to_rfc3339();
    let text = "started";
    let segment = OutputSegment {
        node_did: node_did.into(),
        requester_did: request.requester_did.clone(),
        session_id: session_id.into(),
        request_doc_id: request_doc_id.into(),
        source: OutputSource::Authored { key: key.clone() },
        writer: OutputWriter::ToolExecution {
            tool_call_doc_id: tool_doc_id.into(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: text.len() as u32,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolOutput,
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
    let close_doc_id = insert_output_segment(node, &segment).await;
    let sequence = next_request_sequence(node, request_doc_id).await;
    insert_transcript_header(
        node,
        &TranscriptMessage {
            message_key: key,
            session_id: session_id.into(),
            node_did: node_did.into(),
            requester_did: request.requester_did.clone(),
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
}

/// Complete an already-admitted ordinary tool through its canonical output
/// and ToolDelivery owners, then publish the terminal lifecycle marker.
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
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

pub(crate) async fn seed_job_row(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    session: &str,
    call: &str,
    handle: &str,
    lifecycle: &str,
    state: gents_protocol::job::JobState,
) -> gents_protocol::job::JobView {
    use gents_protocol::job::{JobBudget, JobCeiling, JobOrigin, JobPin, JobProgress};
    let mut pin = JobPin {
        script_id: "fixture-script".into(),
        source_digest: "sha256:source".into(),
        origin: JobOrigin::Installed,
        engine_digest: "sha256:engine".into(),
        plugin_ref: "fixture/workflow".into(),
        agent_budget: 4,
        handle: handle.into(),
        cwd: "/fixture/cwd".into(),
        adopted_at: 1_780_000_000_000,
        args_digest: "sha256:args".into(),
        chain_root_doc_id: String::new(),
        resume_of: None,
        replayed_from: None,
        ceiling_at_pin: JobCeiling {
            targets: vec!["general".into()],
            max_live: 2,
            max_spawns: 8,
            max_messages_per_slot: 4,
            result_bytes: 65_536,
            max_wall_secs: 600,
        },
    };
    let progress = JobProgress {
        title: format!("Job {call}"),
        state,
        pause_reason: None,
        phases: Vec::new(),
        active: Vec::new(),
        spawned: 2,
        budget: JobBudget {
            total: 4,
            spent: 2,
            remaining: 2,
        },
        last_event: "seeded".into(),
        last_event_at: None,
        question: None,
        observed_terminal_children: 0,
        revision: 1,
    };
    let ended = if lifecycle == "running" {
        ""
    } else {
        r#"completed_at: "2026-06-01T10:05:00Z","#
    };
    let request = seed_signed_request(
        node,
        node_did,
        session,
        &format!("request-{call}"),
        "2026-06-01T10:00:00Z",
    )
    .await;
    let doc_id = seed_canonical_tool_call(
        node, &request, call, "workflow", lifecycle, "{}", None, None, None, None,
    )
    .await;
    pin.chain_root_doc_id = doc_id.clone();
    let escaped_doc_id = gents::graphql::escape_graphql_string(&doc_id);
    let mutation = format!(
        r#"mutation($job_pin: JSON, $job_progress: JSON) {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, input: {{
            await_mode: "background", status: "{lifecycle}", deadline_at: "2126-06-01T10:00:00Z", {ended}
            job_pin: $job_pin, job_progress: $job_progress
        }}) {{ _docID }} }}"#
    );
    let variables = json!({ "job_pin": pin, "job_progress": progress });
    ConfigAccess::transact_local_write(node, None, "test.grok.workflow.job", |txn| {
        let (mutation, variables) = (&mutation, &variables);
        Box::pin(async move { txn.execute_with_variables(mutation, variables).await })
    })
    .await
    .unwrap();
    if lifecycle == "running" {
        seed_background_receipt(node, &request, &doc_id, call).await;
    }
    gents::plugin_job::view::load_job_view(
        &ConfigAccess::local(node.clone()),
        None,
        gents::plugin_job::view::JobKey::Address {
            session_id: session.into(),
            tool_call_id: call.into(),
        },
    )
    .await
    .unwrap()
}

/// A pending `local-self` request authored through the production request
/// builder and signed by `node_did`'s registered identity, so readers that
/// verify the owning request's admission accept the rows seeded under it.
async fn seed_signed_request(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
    request_id: &str,
    created_at: &str,
) -> gents_protocol::row::AgentRequestRow {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    let create = gents::build_signed_request(
        gents::RequestSpec::new(
            RequestPurpose::Normal,
            gents::RequestIdentity {
                request_id: request_id.into(),
                node_did: node_did.into(),
                requester_did: None,
                agent_id: "test".into(),
                session_id: session_id.into(),
                content: "test".into(),
                execution_origin: gents::lifecycle::ExecutionOrigin::Interactive,
                created_at: created_at.into(),
            },
            AgentRequestAdmissionRecord::local_self(node_did),
        ),
        gents::RequestSigner::RegisteredTarget,
    )
    .await
    .unwrap();
    let created = node.execute(&create.graphql_mutation().unwrap()).await;
    ensure_no_errors(&created, "seed signed request").unwrap();
    let row = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            gents::graphql::escape_graphql_string(request_id),
            gents::SIGNED_REQUEST_FIELDS,
        ))
        .await;
    ensure_no_errors(&row, "seeded signed request row").unwrap();
    serde_json::from_value(row.data.unwrap()["AgentRequest"][0].clone()).unwrap()
}

#[cfg(unix)]
pub(super) async fn seed_test_agent_configuration(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
    default_agent_id: &str,
    model_name: &str,
    enabled: bool,
) {
    gents::ensure_node(node, node_did)
        .await
        .expect("seed test principal");
    gents::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "grok.test_agent_configuration",
        |txn| {
            Box::pin(async move {
                use gents::config_client::{
                    apply_desired_state_plan, read_desired_state_record_in_txn,
                    DesiredStateApplyDocument, DesiredStateApplyPlan,
                };
                use gents::Collection;

                let (_, mut principal) =
                    read_desired_state_record_in_txn(txn, Collection::Node, node_did, node_did)
                        .await?
                        .expect("seeded principal");
                principal["default_agent_id"] = default_agent_id.into();
                let backend_id = format!("{agent_id}-backend");
                let profile_id = format!("{agent_id}-inference");
                let values = [
                    (Collection::Node, principal),
                    (
                        Collection::InferenceBackend,
                        serde_json::json!({
                            "node_did": node_did,
                            "backend_id": backend_id,
                            "name": format!("{agent_id} test backend"),
                            "provider_kind": "OpenAiCompatible",
                            "openai_wire_api": "chat_completions",
                            "endpoint": "http://127.0.0.1:1/v1",
                            "auth": {"kind": "unauthenticated"},
                            "enabled": true
                        }),
                    ),
                    (
                        Collection::InferenceProfile,
                        serde_json::json!({
                            "node_did": node_did,
                            "profile_id": profile_id,
                            "backend_id": backend_id,
                            "model_name": model_name
                        }),
                    ),
                    (
                        Collection::Agent,
                        serde_json::json!({
                            "node_did": node_did,
                            "agent_id": agent_id,
                            "display_name": agent_id,
                            "inference_profile_id": profile_id,
                            "enabled": enabled
                        }),
                    ),
                ];
                let plan = DesiredStateApplyPlan::new(
                    values
                        .into_iter()
                        .map(|(collection, value)| DesiredStateApplyDocument {
                            collection,
                            add: value.clone(),
                            update: value,
                        })
                        .collect(),
                )?;
                apply_desired_state_plan(txn, &plan).await
            })
        },
    )
    .await
    .expect("seed canonical test agent configuration");
}
