use serde_json::Value;

pub(crate) fn node_builder() -> defra_node::NodeBuilder {
    use gents::NodeIdentity as _;

    let directory = tempfile::tempdir().unwrap();
    let identity =
        gents::KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
    defra_node::EmbeddedNode::builder().with_node_identity_did(identity.did())
}

pub(crate) async fn seed_unsent_xai_effort(owner: &str) -> gents::config_client::ConfigAccess {
    let node = std::sync::Arc::new(
        crate::shared::test_support::node_builder()
            .build()
            .await
            .unwrap(),
    );
    gents::ensure_runtime_schemas(&node).await.unwrap();
    for (collection, value) in [
        ("InferenceBackend", xai_effort_backend(owner)),
        ("InferenceProfile", xai_effort_profile(owner)),
    ] {
        let input = gents_protocol::graphql::graphql_input_literal(&value).unwrap();
        let result = node
            .execute(&format!(
                "mutation {{ create_{collection}(input: {input}) {{_docID}} }}"
            ))
            .await;
        assert!(!result.has_errors(), "{:?}", result.errors);
    }
    gents::config_client::ConfigAccess::local(node)
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
    let backend: gents::document_config::InferenceBackend =
        serde_json::from_value(serde_json::json!({
            "node_did":"did:key:catalog-owner", "backend_id":"catalog-backend", "name":"Catalog",
            "provider_kind":"OpenAiCompatible", "endpoint":"http://127.0.0.1:1/v1",
            "auth":{"kind":"unauthenticated"}
        }))?;
    gents::ensure_node(&node, &backend.node_did).await?;
    let access = gents::ConfigAccess::local(node.clone());
    gents::config_client::write_inference_backend_document(&access, &backend).await?;
    for observed_at in ["2026-10-28T01:00:00Z", "2026-10-28T02:00:00Z"] {
        let catalog: gents::document_config::BackendModelCatalog =
            serde_json::from_value(serde_json::json!({
                "node_did":null, "observed_at":observed_at,
                "models":[{"model_name":"retained-model"}]
            }))?;
        access
            .transact_write("test.catalog.refresh", |txn| {
                let backend = &backend;
                let catalog = catalog.clone();
                Box::pin(async move {
                    gents::backend_registry::record_model_catalog_in_txn(txn, backend, catalog)
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
    node.shutdown().await;
    Ok(())
}
