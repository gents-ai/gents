//! Native storage premises exercised without depending on the runtime's writers.
use defra_node::{EmbeddedNode, ExecuteRetryPolicy, QueryRequest};
use gents_protocol::output::{
    OutputSegment, OutputSource, OutputWriter, SegmentRun, StreamDeclaration, StreamPayload,
};

#[tokio::test]
async fn canonical_segment_create_returns_physical_identity_and_exact_json() {
    let node = EmbeddedNode::builder().build().await.unwrap();
    node.add_schema(gents_schemas::AGENT_OUTPUT_SEGMENT)
        .await
        .unwrap();
    let segment = OutputSegment {
        agent_did: "did:test:storage".into(),
        requester_did: None,
        session_id: "session".into(),
        request_doc_id: "request-doc".into(),
        source: OutputSource::Authored {
            key: "prompt".into(),
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: "generation".into(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: 3,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::Text,
            }),
        }],
        payload: "雪".into(),
        close: None,
        created_at: "2026-09-21T00:00:00Z".into(),
    };
    let response = node.execute_request_with_retry(
        QueryRequest::new("mutation($input: AgentOutputSegmentMutationInputArg!) { create_AgentOutputSegment(input: $input) { _docID } }")
            .with_variables(serde_json::json!({"input": segment})),
        ExecuteRetryPolicy::default(),
    ).await;
    assert!(
        !response.has_errors(),
        "fixture create errors: {:?}",
        response.errors
    );
    let data = response.data.expect("create data");
    // Pinned DefraDB normalizes CREATE to its ADD response name. Runtime
    // readers must use the shared mutation adapter, not hardcode create_.
    let rows = data["add_AgentOutputSegment"]
        .as_array()
        .unwrap_or_else(|| panic!("create response shape: {data}"));
    assert_eq!(rows.len(), 1);
    assert!(!rows[0]["_docID"].as_str().unwrap().is_empty());
    let response = node.execute("{ AgentOutputSegment { agent_did requester_did session_id request_doc_id source writer ordinal runs payload close created_at } }").await;
    assert!(
        !response.has_errors(),
        "fixture read errors: {:?}",
        response.errors
    );
    let data = response.data.unwrap();
    let observed: OutputSegment =
        serde_json::from_value(data["AgentOutputSegment"][0].clone()).unwrap();
    assert_eq!(observed, segment);
    node.shutdown().await;
}
