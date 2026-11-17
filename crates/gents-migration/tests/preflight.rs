use crate::common;
use common::fresh_node;
use gents_migration::{check_migration_compatibility, ensure_migrations, Error};
use gents_protocol::schemas;

#[tokio::test]
async fn preflight_accepts_the_same_legacy_lineage_without_mutating_it() {
    let node = fresh_node().await;
    let previous = schemas::TASK.replace("    outcome_readers: JSON\n", "");
    assert_ne!(previous, schemas::TASK);
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
    // Bound to its identity policy, so its lineage is the only refusal reason.
    node.add_collection(&format!(
        r#"type InferenceProfile @policy(id: "{}", resource: "configuration") {{ alien: String }}"#,
        schemas::AGENT_OWNERSHIP_POLICY_ID
    ))
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

/// Pin the append result as well as the fresh SDL root: DefraDB field IDs must not move.
#[tokio::test]
async fn hidden_tool_names_append_preserves_existing_field_ids() {
    let node = fresh_node().await;
    let previous = schemas::TOOLS.replace("    hidden_tool_names: [String!]\n", "");
    assert_ne!(previous, schemas::TOOLS);
    node.add_collection(&previous).await.unwrap();
    let before = node.get_collection("Tools").unwrap().unwrap();
    let after = common::patch_collection(
        &node,
        "Tools",
        r#"[{"op":"add","path":"/Tools/Fields/-","value":{"Name":"hidden_tool_names","Kind":"[String!]"}}]"#,
    ).await;
    assert_eq!(
        before.version_id,
        "bafyreiay45qi2chkcus5wfycrsumq5vr6igrydc3ebsqqr2qrkb3qfgo6a"
    );
    assert_eq!(
        after.version_id,
        "bafyreictaobcfheh7hrsaek6p2i2hgnyeiy6ogtfetxc5c4ozhmqqcv4cy"
    );
    for field in &before.fields {
        assert_eq!(
            after
                .fields
                .iter()
                .find(|next| next.name == field.name)
                .unwrap()
                .id,
            field.id
        );
    }
    let fresh = fresh_node().await;
    fresh.add_collection(schemas::TOOLS).await.unwrap();
    let fresh_tools = fresh.get_collection("Tools").unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(&after.fields).unwrap(),
        serde_json::to_value(&fresh_tools.fields).unwrap()
    );
    fresh.shutdown().await.unwrap();
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn hidden_tool_names_upgrades_existing_tools_without_changing_grants() {
    let node = fresh_node().await;
    let previous = schemas::TOOLS.replace("    hidden_tool_names: [String!]\n", "");
    assert_ne!(previous, schemas::TOOLS);
    node.add_collection(&previous).await.unwrap();
    let created = node
        .execute(
            r#"mutation { create_Tools(input: {
        tools_id: "retained" node_did: "did:key:existing" display_name: "Retained grants"
        host: "{\"files\":{\"mode\":\"read\"}}"
        built_ins: "{\"schema\":true}" tags: ["retained"]
    }) { _docID } }"#,
        )
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let selection = "{ Tools { _docID tools_id node_did display_name host built_ins tags } }";
    let retained = node.execute(selection).await;
    assert!(!retained.has_errors(), "{:?}", retained.errors);
    assert_eq!(
        retained.data.as_ref().unwrap()["Tools"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let before = node.get_all_collection_versions().await.unwrap();
    check_migration_compatibility(&before).unwrap();
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(node.get_all_collection_versions().await.unwrap()).unwrap()
    );
    let report = ensure_migrations(node.as_ref()).await.unwrap();
    assert_eq!(report.steps_applied, 1);
    let after = node.execute(selection).await;
    assert!(!after.has_errors(), "{:?}", after.errors);
    assert_eq!(after.data, retained.data);
    let hidden = node.execute("{ Tools { hidden_tool_names } }").await;
    assert!(!hidden.has_errors(), "{:?}", hidden.errors);
    assert!(hidden.data.unwrap()["Tools"][0]["hidden_tool_names"].is_null());
    check_migration_compatibility(&node.get_all_collection_versions().await.unwrap()).unwrap();
    assert_eq!(
        ensure_migrations(node.as_ref())
            .await
            .unwrap()
            .steps_applied,
        0
    );
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn hidden_tool_names_unknown_older_tools_lineage_is_still_refused() {
    let node = fresh_node().await;
    let unknown = schemas::TOOLS
        .replace("    hidden_tool_names: [String!]\n", "")
        .replace("    self_config: JSON\n", "");
    node.add_collection(&unknown).await.unwrap();
    let before = node.get_all_collection_versions().await.unwrap();
    assert!(
        matches!(check_migration_compatibility(&before), Err(Error::UnknownLineage { collection, .. }) if collection == "Tools")
    );
    assert!(
        matches!(ensure_migrations(node.as_ref()).await, Err(Error::UnknownLineage { collection, .. }) if collection == "Tools")
    );
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(node.get_all_collection_versions().await.unwrap()).unwrap()
    );
    node.shutdown().await.unwrap();
}
