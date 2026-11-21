use gents_protocol::schemas::{
    BuiltInPolicy, HomePolicyIds, TOOL_FILE_CHANGE_POLICY_RESOURCE, TOOL_FILE_INVENTORY,
    TOOL_FILE_INVENTORY_NAME,
};

use super::common;
use crate::schema::catalog::{ensure_runtime, BASELINE};

#[tokio::test]
async fn inventory_metadata_has_its_pinned_fresh_schema() {
    let node = common::fresh_node().await;
    node.add_collection(
        &gents_protocol::schemas::HomePolicyIds::shared().render(TOOL_FILE_INVENTORY),
    )
    .await
    .unwrap();
    let collection = node
        .get_collection(TOOL_FILE_INVENTORY_NAME)
        .unwrap()
        .unwrap();
    let entry = BASELINE
        .iter()
        .find(|entry| entry.name == TOOL_FILE_INVENTORY_NAME)
        .unwrap();
    assert_eq!(collection.version_id, entry.expected_version.unwrap());
    let policy = collection.policy.unwrap();
    assert_eq!(
        policy.id,
        HomePolicyIds::shared().id(BuiltInPolicy::ToolFileChange)
    );
    assert_eq!(policy.resource_name, TOOL_FILE_CHANGE_POLICY_RESOURCE);
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn inventory_metadata_collection_fallback_stays_owned_by_its_node() {
    let node = common::fresh_node().await;
    ensure_runtime(&node).await.unwrap();
    for name in [
        TOOL_FILE_INVENTORY_NAME,
        gents_protocol::schemas::FILE_INVENTORY_REPLY_NAME,
        gents_protocol::schemas::FILE_INVENTORY_PROGRESS_NAME,
    ] {
        let collection = node.get_collection(name).unwrap().unwrap();
        let owner = node
            .document_acp()
            .get_doc_owner(
                HomePolicyIds::shared().id(BuiltInPolicy::ToolFileChange),
                TOOL_FILE_CHANGE_POLICY_RESOURCE,
                &collection.collection_id,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(owner.as_str(), node.node_identity_did().unwrap());
    }
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn inventory_metadata_refuses_an_unprotected_collection() {
    for (name, sdl) in [
        (TOOL_FILE_INVENTORY_NAME, TOOL_FILE_INVENTORY),
        (
            gents_protocol::schemas::FILE_INVENTORY_REPLY_NAME,
            gents_protocol::schemas::FILE_INVENTORY_REPLY,
        ),
        (
            gents_protocol::schemas::FILE_INVENTORY_PROGRESS_NAME,
            gents_protocol::schemas::FILE_INVENTORY_PROGRESS,
        ),
    ] {
        let node = common::fresh_node().await;
        let directive = format!(
            "@policy(id: \"{}\", resource: \"{}\")",
            BuiltInPolicy::ToolFileChange.placeholder(),
            TOOL_FILE_CHANGE_POLICY_RESOURCE
        );
        node.add_collection(
            &gents_protocol::schemas::HomePolicyIds::shared().render(&sdl.replace(&directive, "")),
        )
        .await
        .unwrap();
        let before = node.get_collection(name).unwrap().unwrap();
        assert!(before.policy.is_none());
        let error = ensure_runtime(&node).await.unwrap_err();
        assert!(
            error.to_string().contains("incompatible document policy"),
            "{error:#}"
        );
        let after = node.get_collection(name).unwrap().unwrap();
        assert_eq!(after.version_id, before.version_id);
        assert!(after.policy.is_none());
        node.shutdown().await.expect("shut down test node");
    }
}

#[tokio::test]
async fn inventory_reply_has_its_pinned_protected_local_schema() {
    use gents_protocol::schemas::{FILE_INVENTORY_REPLY, FILE_INVENTORY_REPLY_NAME};
    let node = common::fresh_node().await;
    node.add_collection(
        &gents_protocol::schemas::HomePolicyIds::shared().render(FILE_INVENTORY_REPLY),
    )
    .await
    .unwrap();
    let collection = node
        .get_collection(FILE_INVENTORY_REPLY_NAME)
        .unwrap()
        .unwrap();
    let entry = BASELINE
        .iter()
        .find(|entry| entry.name == FILE_INVENTORY_REPLY_NAME)
        .unwrap();
    assert_eq!(collection.version_id, entry.expected_version.unwrap());
    let policy = collection.policy.unwrap();
    assert_eq!(
        policy.id,
        HomePolicyIds::shared().id(BuiltInPolicy::ToolFileChange)
    );
    assert_eq!(policy.resource_name, TOOL_FILE_CHANGE_POLICY_RESOURCE);
    assert!(gents_protocol::schemas::is_local_only_collection(
        FILE_INVENTORY_REPLY_NAME
    ));
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn inventory_transport_has_its_pinned_fresh_schemas() {
    use gents_protocol::schemas::*;
    let node = common::fresh_node().await;
    let mut observed = Vec::new();
    for (name, sdl) in [(FILE_INVENTORY_REQUEST_NAME, FILE_INVENTORY_REQUEST)] {
        node.add_collection(&gents_protocol::schemas::HomePolicyIds::shared().render(sdl))
            .await
            .unwrap();
        let collection = node.get_collection(name).unwrap().unwrap();
        let entry = BASELINE.iter().find(|entry| entry.name == name).unwrap();
        observed.push((name, collection.version_id, entry.expected_version.unwrap()));
        assert!(!is_replicated_protocol_collection(name));
    }
    assert!(
        observed
            .iter()
            .all(|(_, actual, expected)| actual == expected),
        "{observed:?}"
    );
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn inventory_client_progress_has_its_pinned_protected_local_schema() {
    use gents_protocol::schemas::{FILE_INVENTORY_PROGRESS, FILE_INVENTORY_PROGRESS_NAME};
    let node = common::fresh_node().await;
    node.add_collection(
        &gents_protocol::schemas::HomePolicyIds::shared().render(FILE_INVENTORY_PROGRESS),
    )
    .await
    .unwrap();
    let collection = node
        .get_collection(FILE_INVENTORY_PROGRESS_NAME)
        .unwrap()
        .unwrap();
    let entry = BASELINE
        .iter()
        .find(|entry| entry.name == FILE_INVENTORY_PROGRESS_NAME)
        .unwrap();
    assert_eq!(collection.version_id, entry.expected_version.unwrap());
    let policy = collection.policy.unwrap();
    assert_eq!(
        policy.id,
        HomePolicyIds::shared().id(BuiltInPolicy::ToolFileChange)
    );
    assert_eq!(policy.resource_name, TOOL_FILE_CHANGE_POLICY_RESOURCE);
    assert!(gents_protocol::schemas::is_local_only_collection(
        FILE_INVENTORY_PROGRESS_NAME
    ));
    assert!(!gents_protocol::schemas::BRANCHABLE_COLLECTION_NAMES
        .contains(&FILE_INVENTORY_PROGRESS_NAME));
    node.shutdown().await.expect("shut down test node");
}

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
        &gents_protocol::schemas::HomePolicyIds::shared().render(
            &RETIRED_IMAGE_DEMAND.replace(
                "POLICY",
                gents_protocol::schemas::HomePolicyIds::shared()
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
