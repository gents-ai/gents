use gents_protocol::schemas::{
    TOOL_FILE_CHANGE_POLICY_ID, TOOL_FILE_CHANGE_POLICY_RESOURCE, TOOL_FILE_INVENTORY,
    TOOL_FILE_INVENTORY_NAME,
};

mod common;

#[tokio::test]
async fn inventory_metadata_has_its_pinned_fresh_schema() {
    let node = common::fresh_node().await;
    node.add_schema(TOOL_FILE_INVENTORY).await.unwrap();
    let collection = node
        .get_collection(TOOL_FILE_INVENTORY_NAME)
        .unwrap()
        .unwrap();
    let entry = gents_migration::DEFAULT_BASELINE
        .iter()
        .find(|entry| entry.name == TOOL_FILE_INVENTORY_NAME)
        .unwrap();
    assert_eq!(collection.version_id, entry.expected_version.unwrap());
    let policy = collection.policy.unwrap();
    assert_eq!(policy.id, TOOL_FILE_CHANGE_POLICY_ID);
    assert_eq!(policy.resource_name, TOOL_FILE_CHANGE_POLICY_RESOURCE);
    node.shutdown().await;
}

#[tokio::test]
async fn inventory_metadata_collection_fallback_stays_owned_by_its_node() {
    let node = common::fresh_node().await;
    gents_migration::ensure_migrations(&node).await.unwrap();
    for name in [
        TOOL_FILE_INVENTORY_NAME,
        gents_protocol::schemas::FILE_INVENTORY_REPLY_NAME,
    ] {
        let collection = node.get_collection(name).unwrap().unwrap();
        let owner = node
            .document_acp()
            .get_doc_owner(
                TOOL_FILE_CHANGE_POLICY_ID,
                TOOL_FILE_CHANGE_POLICY_RESOURCE,
                &collection.collection_id,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(owner.as_str(), node.node_identity_did().unwrap());
    }
    node.shutdown().await;
}

#[tokio::test]
async fn inventory_metadata_refuses_an_unprotected_collection() {
    for (name, sdl) in [
        (TOOL_FILE_INVENTORY_NAME, TOOL_FILE_INVENTORY),
        (
            gents_protocol::schemas::FILE_INVENTORY_REPLY_NAME,
            gents_protocol::schemas::FILE_INVENTORY_REPLY,
        ),
    ] {
        let node = common::fresh_node().await;
        let directive = format!(
            "@policy(id: \"{}\", resource: \"{}\")",
            TOOL_FILE_CHANGE_POLICY_ID, TOOL_FILE_CHANGE_POLICY_RESOURCE
        );
        node.add_schema(&sdl.replace(&directive, "")).await.unwrap();
        let before = node.get_collection(name).unwrap().unwrap();
        assert!(before.policy.is_none());
        let error = gents_migration::ensure_migrations(&node).await.unwrap_err();
        assert!(
            error.to_string().contains("incompatible document policy"),
            "{error:#}"
        );
        let after = node.get_collection(name).unwrap().unwrap();
        assert_eq!(after.version_id, before.version_id);
        assert!(after.policy.is_none());
        node.shutdown().await;
    }
}

#[tokio::test]
async fn inventory_reply_has_its_pinned_protected_local_schema() {
    use gents_protocol::schemas::{FILE_INVENTORY_REPLY, FILE_INVENTORY_REPLY_NAME};
    let node = common::fresh_node().await;
    node.add_schema(FILE_INVENTORY_REPLY).await.unwrap();
    let collection = node
        .get_collection(FILE_INVENTORY_REPLY_NAME)
        .unwrap()
        .unwrap();
    let entry = gents_migration::DEFAULT_BASELINE
        .iter()
        .find(|entry| entry.name == FILE_INVENTORY_REPLY_NAME)
        .unwrap();
    assert_eq!(collection.version_id, entry.expected_version.unwrap());
    let policy = collection.policy.unwrap();
    assert_eq!(policy.id, TOOL_FILE_CHANGE_POLICY_ID);
    assert_eq!(policy.resource_name, TOOL_FILE_CHANGE_POLICY_RESOURCE);
    assert!(gents_protocol::schemas::is_local_only_collection(
        FILE_INVENTORY_REPLY_NAME
    ));
    node.shutdown().await;
}
