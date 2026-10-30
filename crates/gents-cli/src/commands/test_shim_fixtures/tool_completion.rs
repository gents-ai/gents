use defra_node::EmbeddedNode;
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
    StreamDeclaration, StreamPayload, TranscriptMessage,
};

use super::transcript::{insert_output_segment, insert_transcript_header, next_request_sequence};

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
pub(in crate::commands) async fn complete_canonical_tool_call(
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
pub(in crate::commands) async fn seed_canonical_live_tool_output(
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
pub(in crate::commands) async fn complete_canonical_spawned_process_output(
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
