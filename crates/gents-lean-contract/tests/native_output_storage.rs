use crypto::Key;
use defra_core::{
    signing::{SigningConfig, SigningKeyType},
    RequestContext,
};
use defra_node::{Node as EmbeddedNode, QueryRequest};
use gents_protocol::output::{
    OutputSegment, OutputSource, OutputWriter, SegmentRun, StreamDeclaration, StreamPayload,
};
use identity::{Identity as _, RawIdentity};

/// A signed node actor: DefraDB enforces document ACP on every store, so the segment's
/// ownership policy is registered by a real identity before its collection binds it.
fn node_identity() -> String {
    let key = crypto::generate_ed25519().expect("node key");
    let identity = RawIdentity::from_ed25519(key.clone()).expect("node identity");
    let did = identity.did().expect("node DID").to_string();
    let public = identity.public_key_bytes();
    defra_core::signing::store_identity(
        &did,
        SigningConfig {
            key_type: SigningKeyType::Ed25519,
            private_key_bytes: key.raw().to_vec(),
            public_key_hex: hex::encode(&public),
            public_key_bytes: public,
            remote_signer: None,
            signing_authorization: None,
        },
    )
    .expect("the signing identity registers");
    did
}

#[tokio::test]
async fn canonical_segment_create_returns_physical_identity_and_exact_json() {
    // DefraDB can leave background tasks alive after shutdown, so the store outlives the node.
    let data = tempfile::tempdir().unwrap();
    let node = EmbeddedNode::builder()
        .data_path(data.path())
        .with_node_identity_did(node_identity())
        .build()
        .await
        .unwrap();
    let context =
        RequestContext::default().with_identity(node.node_identity_did().map(str::to_owned));
    let policy = node
        .add_dac_policy_with_counter_as(&context, gents_schemas::AGENT_OWNERSHIP_POLICY, 0)
        .await
        .unwrap();
    assert_eq!(policy, gents_schemas::AGENT_OWNERSHIP_POLICY_ID);
    node.add_collection(gents_schemas::AGENT_OUTPUT_SEGMENT)
        .await
        .unwrap();
    let segment = OutputSegment {
        node_did: "did:test:storage".into(),
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
    let response = node.execute_request(
        QueryRequest::new("mutation($input: AgentOutputSegmentMutationInputArg!) { create_AgentOutputSegment(input: $input) { _docID } }")
            .with_variables(serde_json::json!({"input": segment})),
    ).await;
    assert!(
        !response.has_errors(),
        "fixture create errors: {:?}",
        response.errors
    );
    let data = response.data.expect("create data");
    let rows = data["add_AgentOutputSegment"]
        .as_array()
        .unwrap_or_else(|| panic!("create response shape: {data}"));
    assert_eq!(rows.len(), 1);
    assert!(!rows[0]["_docID"].as_str().unwrap().is_empty());
    let response = node.execute("{ AgentOutputSegment { node_did requester_did session_id request_doc_id source writer ordinal runs payload close created_at } }").await;
    assert!(
        !response.has_errors(),
        "fixture read errors: {:?}",
        response.errors
    );
    let data = response.data.unwrap();
    let observed: OutputSegment =
        serde_json::from_value(data["AgentOutputSegment"][0].clone()).unwrap();
    assert_eq!(observed, segment);
    node.shutdown().await.expect("node shutdown");
    std::mem::forget(data);
}
