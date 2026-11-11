use super::*;
use gents_inference::llm::message::{Text, ToolResult, ToolResultContent, UserContent};
use gents_store::test_support::first_content;

#[expect(
    clippy::too_many_arguments,
    reason = "shared test helper with call sites in modules outside session"
)]
pub async fn import_history_observation(
    node: &std::sync::Arc<defra_node::Node>,
    request_id: &str,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    text: &str,
    message_key_suffix: &str,
    sequence: u32,
    tool_call: Option<(&str, &str)>,
) {
    use gents_protocol::output::rows::*;
    let segment = history_observation_segment(
        request_id,
        session_id,
        node_did,
        requester_did,
        text,
        message_key_suffix,
        tool_call,
    );
    let response = node
        .execute_request(
            defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                .with_variables(output_segment_create_variables(&segment).unwrap()),
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row =
        gents_store::graphql::single_mutation_document(&response, "create_AgentOutputSegment")
            .unwrap()
            .unwrap();
    let header = history_observation_header(
        request_id,
        session_id,
        node_did,
        requester_did,
        message_key_suffix,
        sequence,
        tool_call,
        row["_docID"].as_str().unwrap(),
        &segment.created_at,
    );
    let response = node
        .execute_request(
            defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                .with_variables(transcript_message_create_variables(&header).unwrap()),
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row = gents_store::graphql::single_mutation_document(&response, "create_AgentMessage")
        .unwrap()
        .unwrap();
    let (_, reconstructed) = load_canonical_message_from_node(
        node,
        row["_docID"].as_str().unwrap(),
        node_did,
        requester_did,
    )
    .await
    .unwrap();
    let expected = history_observation_message(text, tool_call);
    assert_eq!(reconstructed, expected);
}

fn history_observation_segment(
    request_id: &str,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    text: &str,
    message_key_suffix: &str,
    tool_call: Option<(&str, &str)>,
) -> gents_protocol::output::OutputSegment {
    use gents_protocol::output::*;
    OutputSegment {
        node_did: node_did.into(),
        requester_did: requester_did.map(str::to_owned),
        session_id: session_id.into(),
        request_doc_id: request_id.into(),
        source: match tool_call {
            Some((doc_id, _)) => OutputSource::ToolCall {
                tool_call_doc_id: doc_id.into(),
            },
            None => OutputSource::Authored {
                key: message_key_suffix.into(),
            },
        },
        writer: match tool_call {
            Some((doc_id, _)) => OutputWriter::ToolExecution {
                tool_call_doc_id: doc_id.into(),
            },
            None => OutputWriter::RequestExecution {
                execution_generation: "observed-generation".into(),
            },
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: text.len().try_into().unwrap(),
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: if tool_call.is_some() {
                    StreamPayload::ToolOutput
                } else {
                    StreamPayload::Text
                },
            }),
        }],
        payload: text.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![text.len() as u64],
        }),
        created_at: chrono::Utc::now().to_rfc3339(),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the shared history observation fixture's arguments"
)]
fn history_observation_header(
    request_id: &str,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    message_key_suffix: &str,
    sequence: u32,
    tool_call: Option<(&str, &str)>,
    close_doc_id: &str,
    now: &str,
) -> gents_protocol::output::TranscriptMessage {
    use gents_protocol::output::*;
    TranscriptMessage {
        native_request_id: None,
        message_key: message_key_suffix.into(),
        session_id: session_id.into(),
        node_did: node_did.into(),
        requester_did: requester_did.map(str::to_owned),
        request_doc_id: Some(request_id.into()),
        publication: match tool_call {
            Some((doc_id, _)) => MessagePublication::ToolDelivery {
                tool_call_doc_id: doc_id.into(),
            },
            None => MessagePublication::RequestExecution {
                execution_generation: "observed-generation".into(),
            },
        },
        outcome: OutputOutcome::Complete,
        sequence,
        role: MessageRole::User,
        native_id: None,
        blocks: vec![{
            let payload = PresentedPayload {
                output: PayloadRef {
                    close_doc_id: close_doc_id.into(),
                    stream: 0,
                },
                presentation: PayloadPresentation::Full,
            };
            match tool_call {
                Some((doc_id, call_id)) => MessageBlock::ToolResult {
                    tool_call_doc_id: doc_id.into(),
                    id: call_id.into(),
                    call_id: Some(call_id.into()),
                    parts: vec![ToolResultPart::Text { text: payload }],
                },
                None => MessageBlock::Text { text: payload },
            }
        }],
        created_at: now.into(),
    }
}

fn history_observation_message(text: &str, tool_call: Option<(&str, &str)>) -> Message {
    match tool_call {
        Some((_, call_id)) => Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                id: call_id.into(),
                call_id: Some(call_id.into()),
                content: vec![ToolResultContent::Text(Text { text: text.into() })],
            })],
        },
        None => Message::user(text),
    }
}

/// [`import_history_observation`] written and read back by `access`, so the
/// rows are owned by the actor (such as the session's Agent) that reads them.
#[expect(
    clippy::too_many_arguments,
    reason = "shared test helper with call sites in modules outside session"
)]
pub async fn import_history_observation_as(
    access: &gents_store::config_client::ConfigAccess,
    request_id: &str,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    text: &str,
    message_key_suffix: &str,
    sequence: u32,
    tool_call: Option<(&str, &str)>,
) {
    use gents_protocol::output::rows::*;
    let segment = history_observation_segment(
        request_id,
        session_id,
        node_did,
        requester_did,
        text,
        message_key_suffix,
        tool_call,
    );
    let header_doc = access
        .transact_write("test.history_observation", |txn| {
            let segment = &segment;
            Box::pin(async move {
                let response = txn
                    .execute_with_variables(
                        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                        &output_segment_create_variables(segment)?,
                    )
                    .await?;
                let close = gents_store::graphql::created_doc_id(&response, "AgentOutputSegment")?;
                let header = history_observation_header(
                    request_id,
                    session_id,
                    node_did,
                    requester_did,
                    message_key_suffix,
                    sequence,
                    tool_call,
                    &close,
                    &segment.created_at,
                );
                let response = txn
                    .execute_with_variables(
                        CREATE_AGENT_MESSAGE_MUTATION,
                        &transcript_message_create_variables(&header)?,
                    )
                    .await?;
                Ok::<_, anyhow::Error>(gents_store::graphql::created_doc_id(
                    &response,
                    "AgentMessage",
                )?)
            })
        })
        .await
        .unwrap();
    let (_, reconstructed) = load_canonical_message(access, &header_doc, node_did, requester_did)
        .await
        .unwrap();
    assert_eq!(reconstructed, history_observation_message(text, tool_call));
}

