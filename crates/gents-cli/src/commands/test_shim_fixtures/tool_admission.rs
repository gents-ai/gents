use std::sync::Arc;

use defra_node::EmbeddedNode;
use gents::session::canonical_rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
    StreamDeclaration, StreamPayload, TranscriptMessage,
};

use super::transcript::{insert_output_segment, insert_transcript_header, next_request_sequence};

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
