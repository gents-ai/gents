use serde_json::Value;

pub(crate) fn node_builder() -> defra_node::NodeBuilder {
    use gents::NodeIdentity as _;

    let directory = tempfile::tempdir().unwrap();
    let identity =
        gents::KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
    defra_node::Node::builder().with_node_identity_did(identity.did())
}

pub(crate) async fn seed_unsent_xai_effort(
    owner: &str,
) -> gents::store::config_client::ConfigAccess {
    let node = std::sync::Arc::new(
        crate::shared::test_support::node_builder()
            .build()
            .await
            .unwrap(),
    );
    gents::ensure_runtime_schemas(&node).await.unwrap();
    gents::ensure_node(&node, owner).await.unwrap();
    let access = gents::store::config_client::ConfigAccess::local(node);
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
    let input = gents_protocol::graphql::graphql_input_literal(
        &serde_json::json!({"catalogs": catalogs, "probe_status":"unhealthy"}),
    )
    .unwrap();
    access.write("test.backend.observation", &format!("mutation {{ update_InferenceBackendMetadata(filter: {{backend_id: {{_eq: \"xai\"}}}}, input: {input}) {{_docID}} }}")).await.unwrap();
    access
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
    let node = std::sync::Arc::new(node_builder().build().await?);
    gents::ensure_runtime_schemas(&node).await?;
    let backend: gents::documents::InferenceBackend = serde_json::from_value(serde_json::json!({
        "node_did":"did:key:catalog-owner", "backend_id":"catalog-backend", "name":"Catalog",
        "provider_kind":"OpenAiCompatible", "endpoint":"http://127.0.0.1:1/v1",
        "auth":{"kind":"unauthenticated"}
    }))?;
    gents::ensure_node(&node, &backend.node_did).await?;
    let access = gents::ConfigAccess::local(node.clone());
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
