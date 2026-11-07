use serde_json::Value;

pub(crate) fn node_builder() -> defra_node::NodeBuilder {
    use gents::NodeIdentity as _;

    let directory = tempfile::tempdir().unwrap();
    let identity =
        gents::KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
    defra_node::Node::builder().with_node_identity_did(identity.did())
}

/// A node whose runtime is the deterministic fixture signer
/// (`caused_sessions::fixtures::FIXTURE_DID`), whose key this process holds.
pub(crate) fn fixture_node_builder() -> defra_node::NodeBuilder {
    use gents::NodeIdentity as _;

    let signer = crate::caused_sessions::fixtures::fixture_signer();
    defra_node::Node::builder().with_node_identity_did(signer.did())
}

/// The fixture signer's managed Agent for one test: derived from the held
/// runtime root, so its signing key is registered in this process.
pub(crate) fn fixture_agent() -> gents_protocol::agent_identity::AgentIdentityBinding {
    gents::store::identity::provision_agent_identity(
        &crate::caused_sessions::fixtures::fixture_signer(),
    )
    .unwrap()
}

/// The fixture signer's managed Agent named `name`: the same name derives the
/// same Agent DID, so fixtures can name Agents across documents and reads.
pub(crate) fn fixture_agent_named(
    name: &str,
) -> gents_protocol::agent_identity::AgentIdentityBinding {
    use sha2::Digest as _;

    let creation_nonce = sha2::Sha256::digest(name.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let identity = gents::store::identity::derive_agent_identity(
        &crate::caused_sessions::fixtures::fixture_signer(),
        &creation_nonce,
    )
    .unwrap();
    gents_protocol::agent_identity::AgentIdentityBinding {
        agent_did: identity.did().to_owned(),
        creation_nonce,
    }
}

/// Publishes the fixture Node for `node`'s runtime as that runtime: local
/// writes are admitted only for the runtime or a managed Agent, and runtime
/// startup requires the Node to exist. Returns the Node's DID.
pub(crate) async fn publish_test_node(node: &std::sync::Arc<defra_node::Node>) -> String {
    let node_did = node
        .node_identity_did()
        .expect("fixture node has a runtime identity")
        .to_owned();
    publish_test_node_as(node, &node_did).await;
    node_did
}

/// [`publish_test_node`] for a fixture that names its Node's DID; the DID
/// must be the node's runtime.
pub(crate) async fn publish_test_node_as(node: &std::sync::Arc<defra_node::Node>, node_did: &str) {
    use gents::store::config_client::{
        apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };

    assert_eq!(
        node.node_identity_did(),
        Some(node_did),
        "the fixture Node is published by its own runtime"
    );
    if gents::load_node(node, node_did).await.unwrap().is_some() {
        return;
    }
    let value = serde_json::json!({"node_did": node_did});
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: gents::Collection::Node,
        add: value.clone(),
        update: value,
    }])
    .unwrap();
    ConfigAccess::local_as(node.clone(), ::identity::Did::new(node_did).unwrap())
        .transact_write("test.publish_node", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
        .unwrap();
}

/// Seeds an xAI backend whose profile effort will not be sent, as the
/// fixture runtime. Returns the runtime's access and its Node DID.
pub(crate) async fn seed_unsent_xai_effort() -> (gents::store::config_client::ConfigAccess, String)
{
    let node = std::sync::Arc::new(fixture_node_builder().build().await.unwrap());
    gents::ensure_runtime_schemas(&node).await.unwrap();
    let owner = publish_test_node(&node).await;
    let access = gents::store::config_client::ConfigAccess::local_as(
        node,
        ::identity::Did::new(owner.as_str()).unwrap(),
    );
    let owner = owner.as_str();
    let mut value = xai_effort_backend(owner);
    let catalogs = value.as_object_mut().unwrap().remove("catalogs").unwrap();
    value.as_object_mut().unwrap().remove("probe_status");
    let backend: gents::InferenceBackend = serde_json::from_value(value).unwrap();
    gents::store::config_client::write_inference_backend_document(&access, &backend)
        .await
        .unwrap();
    let input = gents_protocol::graphql::graphql_input_literal(&xai_effort_profile(owner)).unwrap();
    access
        .write(
            "test.profile.seed",
            &format!("mutation {{ create_InferenceProfile(input: {input}) {{_docID}} }}"),
        )
        .await
        .unwrap();
    let catalog: gents::documents::BackendModelCatalog =
        serde_json::from_value(catalogs["entries"][0].clone()).unwrap();
    access
        .transact_write("test.backend.catalog", |txn| {
            let backend = &backend;
            let catalog = catalog.clone();
            Box::pin(async move {
                gents::inference::backends::registry::record_model_catalog_in_txn(
                    txn, backend, catalog,
                )
                .await
            })
        })
        .await
        .unwrap();
    gents::inference::backends::registry::set_backend_probe_status_as(
        &access,
        owner,
        &backend.backend_id,
        "unhealthy",
    )
    .await
    .unwrap();
    let owner = owner.to_owned();
    (access, owner)
}

pub(crate) fn xai_effort_backend(owner: &str) -> Value {
    serde_json::json!({
        "node_did": owner, "backend_id": "xai", "name": "xAI",
        "provider_kind": "OpenAiCompatible", "openai_wire_api": "responses",
        "endpoint": "https://api.x.ai/v1", "enabled": true,
        "auth": {"kind": "environment", "variable": "XAI_API_KEY"},
        "probe_status": "unhealthy",
        "catalogs": {"entries": [{"node_did": null, "observed_at": "2026-01-01T00:00:00Z",
            "models": [{"model_name": "grok-4.20-0309-reasoning", "reasoning_efforts": []}]}]}
    })
}

pub(crate) fn xai_effort_profile(owner: &str) -> Value {
    serde_json::json!({
        "node_did": owner, "profile_id": "grok", "backend_id": "xai",
        "model_name": "grok-4.20-0309-reasoning", "reasoning_effort": "low"
    })
}

#[tokio::test]
async fn catalog_freshness_reaches_cli_observation_reader() -> anyhow::Result<()> {
    let node = std::sync::Arc::new(fixture_node_builder().build().await?);
    gents::ensure_runtime_schemas(&node).await?;
    let owner = publish_test_node(&node).await;
    let backend: gents::documents::InferenceBackend = serde_json::from_value(serde_json::json!({
        "node_did":owner, "backend_id":"catalog-backend", "name":"Catalog",
        "provider_kind":"OpenAiCompatible", "endpoint":"http://127.0.0.1:1/v1",
        "auth":{"kind":"unauthenticated"}
    }))?;
    let access = gents::ConfigAccess::local_as(node.clone(), ::identity::Did::new(owner)?);
    gents::store::config_client::write_inference_backend_document(&access, &backend).await?;
    for observed_at in ["2026-10-28T01:00:00Z", "2026-10-28T02:00:00Z"] {
        let catalog: gents::documents::BackendModelCatalog =
            serde_json::from_value(serde_json::json!({
                "node_did":null, "observed_at":observed_at,
                "models":[{"model_name":"retained-model"}]
            }))?;
        access
            .transact_write("test.catalog.refresh", |txn| {
                let backend = &backend;
                let catalog = catalog.clone();
                Box::pin(async move {
                    gents::inference::backends::registry::record_model_catalog_in_txn(
                        txn, backend, catalog,
                    )
                    .await
                })
            })
            .await?;
    }
    let observation =
        super::load_backend_observation(&access, &backend.node_did, &backend.backend_id).await?;
    assert_eq!(observation.catalogs[0].observed_at, "2026-10-28T02:00:00Z");
    assert_eq!(
        observation.catalogs[0].models[0].model_name,
        "retained-model"
    );
    node.shutdown().await.expect("node shutdown");
    Ok(())
}
