use super::common;
use crate::schema::catalog::ensure_runtime;

/// A retired collection leaves the baseline without a wipe or conversion; its
/// stored lineage and rows stay exactly as they were.
const RETIRED_IMAGE_DEMAND: &str = r#"type FileImageHydrationRequest @policy(id: "POLICY", resource: "configuration") {
    request_key: String @index(unique: true) @immutable
    peer_id: String @immutable
    requester_did: String @index @immutable
    node_did: String @index @immutable
    session_id: String @immutable
    request_doc_id: String @immutable
    tool_call_doc_id: String @immutable
    created_at: DateTime @immutable
    status: String @index
    status_detail: String
    observation_doc_id: String
    processed_at: DateTime
    outcome_signer_did: String
    outcome_signature: String
}"#;

#[tokio::test]
async fn retired_image_demand_rows_survive_startup() {
    let node = common::fresh_node().await;
    node.add_collection(
        &gents_protocol::schemas::HomePolicyIds::node_store().render(
            &RETIRED_IMAGE_DEMAND.replace(
                "POLICY",
                gents_protocol::schemas::HomePolicyIds::node_store()
                    .id(gents_protocol::schemas::BuiltInPolicy::Household),
            ),
        ),
    )
    .await
    .unwrap();
    let before = node
        .get_collection("FileImageHydrationRequest")
        .unwrap()
        .unwrap();
    let created = node
        .execute(
            r#"mutation {create_FileImageHydrationRequest(input: {
        request_key:"retained", peer_id:"peer", requester_did:"reader", node_did:"source",
        session_id:"session", request_doc_id:"request", tool_call_doc_id:"tool",
        status:"rejected", status_detail:"retained refusal", outcome_signature:"retained-signature"
    }) {_docID}}"#,
        )
        .await;
    assert!(!created.has_errors(), "{created:?}");
    let query = "{FileImageHydrationRequest {_docID request_key peer_id requester_did node_did session_id request_doc_id tool_call_doc_id status status_detail outcome_signature}}";
    let retained = node.execute(query).await.data.unwrap();
    ensure_runtime(&node).await.unwrap();
    let after = node
        .get_collection("FileImageHydrationRequest")
        .unwrap()
        .unwrap();
    assert_eq!(before.version_id, after.version_id);
    assert_eq!(before.collection_id, after.collection_id);
    assert_eq!(node.execute(query).await.data.unwrap(), retained);
    assert!(node
        .get_collection("FileInventoryImageRequest")
        .unwrap()
        .is_none());
    ensure_runtime(&node).await.unwrap();
    assert_eq!(
        node.get_collection("FileImageHydrationRequest")
            .unwrap()
            .unwrap()
            .version_id,
        before.version_id
    );
    node.shutdown().await.expect("shut down test node");
}
