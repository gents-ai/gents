use gents_migration::{check_migration_compatibility, ensure_migrations, Error};
use gents_protocol::schemas;
mod common;
use common::fresh_node;

#[tokio::test]
async fn preflight_accepts_the_same_legacy_lineage_without_mutating_it() {
    let node = fresh_node().await;
    let previous = schemas::INFERENCE_PROFILE.replace("    service_tier: String\n", "");
    node.add_collection(&previous).await.unwrap();
    let before = node.get_all_collection_versions().await.unwrap();
    check_migration_compatibility(&before).unwrap();
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(node.get_all_collection_versions().await.unwrap()).unwrap()
    );
    ensure_migrations(node.as_ref()).await.unwrap();
    check_migration_compatibility(&node.get_all_collection_versions().await.unwrap()).unwrap();
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn preflight_and_startup_refuse_the_same_unknown_lineage() {
    let node = fresh_node().await;
    node.add_collection("type InferenceProfile { alien: String }")
        .await
        .unwrap();
    let before = node.get_all_collection_versions().await.unwrap();
    assert!(matches!(
        check_migration_compatibility(&before),
        Err(Error::UnknownLineage { .. })
    ));
    assert!(matches!(
        ensure_migrations(node.as_ref()).await,
        Err(Error::UnknownLineage { .. })
    ));
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(node.get_all_collection_versions().await.unwrap()).unwrap()
    );
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn startup_refuses_existing_unbound_provider_collections_before_changes() {
    let directive = format!(
        " @policy(id: \"{}\", resource: \"children\")",
        schemas::AGENT_OWNERSHIP_POLICY_ID
    );
    for (name, schema) in [
        (schemas::RENDERED_REQUEST_NAME, schemas::RENDERED_REQUEST),
        (
            schemas::RENDERED_REQUEST_BLOCK_NAME,
            schemas::RENDERED_REQUEST_BLOCK,
        ),
        (
            schemas::PROVIDER_CONTEXT_REDUCTION_NAME,
            schemas::PROVIDER_CONTEXT_REDUCTION,
        ),
        (schemas::COMPACTION_ENTRY_NAME, schemas::COMPACTION_ENTRY),
        (schemas::INFERENCE_CALL_NAME, schemas::INFERENCE_CALL),
        (schemas::GOAL_OBSERVATION_NAME, schemas::GOAL_OBSERVATION),
    ] {
        let node = fresh_node().await;
        assert!(
            schema.contains(&directive),
            "{name} must be bound in the catalog"
        );
        node.add_collection(&schema.replacen(&directive, "", 1))
            .await
            .unwrap();
        let before = node.get_all_collection_versions().await.unwrap();
        assert!(matches!(
            ensure_migrations(node.as_ref()).await,
            Err(Error::IncompatibleIdentityPolicy { collection }) if collection == name
        ));
        assert_eq!(
            serde_json::to_value(&before).unwrap(),
            serde_json::to_value(node.get_all_collection_versions().await.unwrap()).unwrap()
        );
        node.shutdown().await.expect("shut down test node");
    }
}

#[tokio::test]
async fn branchable_identity_collections_have_private_local_fallback_objects() {
    let node = fresh_node().await;
    ensure_migrations(node.as_ref()).await.unwrap();
    let runtime = identity::Did::new(node.node_identity_did().unwrap()).unwrap();
    for (name, resource) in schemas::IDENTITY_POLICY_BINDINGS
        .iter()
        .filter(|(name, _)| schemas::is_replicated_protocol_collection(name))
    {
        let collection = node.get_collection(name).unwrap().unwrap();
        assert!(collection.is_branchable);
        let acp = node.document_acp();
        assert!(acp
            .is_doc_registered(
                schemas::AGENT_OWNERSHIP_POLICY_ID,
                resource,
                &collection.collection_id
            )
            .await
            .unwrap());
        assert_eq!(
            acp.get_doc_owner(
                schemas::AGENT_OWNERSHIP_POLICY_ID,
                resource,
                &collection.collection_id
            )
            .await
            .unwrap(),
            Some(runtime.clone())
        );
    }
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn startup_refuses_unbound_transport_collections_before_mutation() {
    for (name, resource, schema) in [
        (
            schemas::NETWORK_NAME,
            "enrollment_records",
            schemas::NETWORK,
        ),
        (
            schemas::NETWORK_ENROLLMENT_REQUEST_NAME,
            "enrollment_records",
            schemas::NETWORK_ENROLLMENT_REQUEST,
        ),
        (
            schemas::NETWORK_ENROLLMENT_DECISION_NAME,
            "enrollment_records",
            schemas::NETWORK_ENROLLMENT_DECISION,
        ),
        (
            schemas::NETWORK_AUTHORIZATION_REVISION_NAME,
            "enrollment_records",
            schemas::NETWORK_AUTHORIZATION_REVISION,
        ),
        (
            schemas::NETWORK_ENROLLMENT_ROUTE_RECEIPT_NAME,
            "enrollment_records",
            schemas::NETWORK_ENROLLMENT_ROUTE_RECEIPT,
        ),
        (
            schemas::PEER_REGISTRY_NAME,
            "discovery",
            schemas::PEER_REGISTRY,
        ),
        (
            schemas::NODE_DIRECTORY_ENTRY_NAME,
            "scoped_discovery",
            schemas::NODE_DIRECTORY_ENTRY,
        ),
    ] {
        let directive = format!(
            " @policy(id: \"{}\", resource: \"{resource}\")",
            schemas::AGENT_OWNERSHIP_POLICY_ID
        );
        assert!(schema.contains(&directive));
        let node = fresh_node().await;
        node.add_collection(&schema.replacen(&directive, "", 1))
            .await
            .unwrap();
        let before = node.get_all_collection_versions().await.unwrap();
        assert!(
            matches!(ensure_migrations(node.as_ref()).await, Err(Error::IncompatibleIdentityPolicy { collection }) if collection == name)
        );
        assert_eq!(
            serde_json::to_value(&before).unwrap(),
            serde_json::to_value(node.get_all_collection_versions().await.unwrap()).unwrap()
        );
        node.shutdown().await.unwrap();
    }
}
