use defra_node::EmbeddedNode;
use gents::defra_node::QueryRequest;
use gents::graphql::single_mutation_document;
use gents::session::canonical_rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
#[cfg(unix)]
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSource, OutputWriter,
    PayloadPresentation, PayloadRef, PresentedPayload, ReasoningPart, SegmentRun, SourceClose,
    StreamDeclaration, StreamPayload,
};
use gents_protocol::output::{OutputSegment, TranscriptMessage};

#[cfg(unix)]
pub(in crate::commands) async fn seed_canonical_assistant_message(
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

pub(super) async fn insert_output_segment(node: &EmbeddedNode, segment: &OutputSegment) -> String {
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

pub(super) async fn insert_transcript_header(node: &EmbeddedNode, header: &TranscriptMessage) {
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

pub(in crate::commands) async fn next_request_sequence(
    node: &EmbeddedNode,
    request_doc_id: &str,
) -> u32 {
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
