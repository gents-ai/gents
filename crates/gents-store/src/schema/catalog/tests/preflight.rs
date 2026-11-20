use gents_protocol::schemas;

use super::common::fresh_node;
use crate::schema::catalog::{check_lineages, ensure_runtime, SchemaError};

/// A collection an earlier build registered before these fields existed is on
/// an older lineage: preflight and startup refuse it alike, and neither its
/// schema nor its rows change.
#[tokio::test]
async fn a_collection_from_before_its_current_fields_is_refused_without_conversion() {
    for (name, sdl, create, added) in [
        (
            schemas::TASK_NAME,
            schemas::TASK,
            r#"mutation {create_Task(input: {task_id: "retained", node_did: "owner"}) {_docID}}"#,
            &["outcome_readers"][..],
        ),
        (
            schemas::REPOSITORY_PLACEMENT_NAME,
            schemas::REPOSITORY_PLACEMENT,
            r#"mutation {create_RepositoryPlacement(input: {repository_id: "retained", node_did: "owner"}) {_docID}}"#,
            &["readers"][..],
        ),
        (
            schemas::TOOL_SERVICE_REGISTRY_NAME,
            schemas::TOOL_SERVICE_REGISTRY,
            r#"mutation {create_ToolServiceRegistry(input: {service_id: "retained", node_did: "owner", hostname: "host", mcp_port: 9000}) {_docID}}"#,
            &["endpoint_url"][..],
        ),
        (
            schemas::CALLBACK_RESULT_NAME,
            schemas::CALLBACK_RESULT,
            r#"mutation {create_CallbackResult(input: {result_id: "retained", owner_node_did: "owner", invocation_id: "inv"}) {_docID}}"#,
            &["handoff_id", "reply_session_id", "attempt"][..],
        ),
        (
            schemas::ISOLATED_WORKSPACE_NAME,
            schemas::ISOLATED_WORKSPACE,
            r#"mutation {create_IsolatedWorkspace(input: {workspace_id: "retained", owner_node_did: "owner"}) {_docID}}"#,
            &["handoff_id", "reply_session_id", "attempt"][..],
        ),
        (
            schemas::WORKSPACE_RECEIPT_NAME,
            schemas::WORKSPACE_RECEIPT,
            r#"mutation {create_WorkspaceReceipt(input: {receipt_id: "retained", workspace_id: "ws"}) {_docID}}"#,
            &["handoff_id", "reply_session_id", "attempt"][..],
        ),
        (
            schemas::PACK_INSTALLATION_NAME,
            schemas::PACK_INSTALLATION,
            r#"mutation {create_PackInstallation(input: {node_did: "owner", coordinate: "acme/retained"}) {_docID}}"#,
            &["source", "source_history", "variables"][..],
        ),
        (
            schemas::TRIGGER_NAME,
            schemas::TRIGGER,
            r#"mutation {create_Trigger(input: {trigger_id: "retained", node_did: "owner"}) {_docID}}"#,
            &["create_session", "max_fires_per_hour"][..],
        ),
    ] {
        let node = fresh_node().await;
        let older = sdl
            .lines()
            .filter(|line| {
                !added
                    .iter()
                    .any(|field| line.trim_start().starts_with(&format!("{field}:")))
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_ne!(older, sdl);
        node.add_collection(&older).await.unwrap();
        let created = node.execute(create).await;
        assert!(!created.has_errors(), "{created:?}");
        let query = format!("{{{name} {{_docID}}}}");
        let retained = node.execute(&query).await.data;
        let before = node.get_all_collection_versions().await.unwrap();
        assert!(
            matches!(check_lineages(&before), Err(SchemaError::UnknownLineage { ref collection, .. }) if collection == name),
            "{name}"
        );
        assert!(
            matches!(ensure_runtime(node.as_ref()).await, Err(SchemaError::UnknownLineage { ref collection, .. }) if collection == name),
            "{name}"
        );
        assert_eq!(
            serde_json::to_value(&before).unwrap(),
            serde_json::to_value(node.get_all_collection_versions().await.unwrap()).unwrap(),
            "{name}"
        );
        assert_eq!(node.execute(&query).await.data, retained, "{name}");
        assert!(node.get_collection(schemas::NODE_NAME).unwrap().is_none());
        node.shutdown().await.expect("shut down test node");
    }
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
        check_lineages(&before),
        Err(SchemaError::UnknownLineage { .. })
    ));
    assert!(matches!(
        ensure_runtime(node.as_ref()).await,
        Err(SchemaError::UnknownLineage { .. })
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
            ensure_runtime(node.as_ref()).await,
            Err(SchemaError::IncompatibleIdentityPolicy { collection }) if collection == name
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
    ensure_runtime(node.as_ref()).await.unwrap();
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
            matches!(ensure_runtime(node.as_ref()).await, Err(SchemaError::IncompatibleIdentityPolicy { collection }) if collection == name)
        );
        assert_eq!(
            serde_json::to_value(&before).unwrap(),
            serde_json::to_value(node.get_all_collection_versions().await.unwrap()).unwrap()
        );
        node.shutdown().await.unwrap();
    }
}
