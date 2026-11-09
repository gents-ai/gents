use defra_node::Node as EmbeddedNode;
use gents::defra_node::QueryRequest;
use gents::store::graphql::single_mutation_document;
use gents_protocol::output::rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::output::{OutputSegment, TranscriptMessage};

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

pub(super) async fn next_request_sequence(node: &EmbeddedNode, request_doc_id: &str) -> u32 {
    let request_doc_id = gents::store::graphql::escape_graphql_string(request_doc_id);
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
    let session_id = gents::store::graphql::escape_graphql_string(
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
