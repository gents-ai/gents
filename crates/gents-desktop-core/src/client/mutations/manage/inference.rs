use anyhow::Result;
use defra_node::EmbeddedNode;
use gents::collection::Collection;
use gents::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, ConfigAccess,
    DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::InferenceBackend;

pub async fn upsert_inference_backend(
    node: &EmbeddedNode,
    document: &InferenceBackend,
) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::InferenceBackend,
        add: value.clone(),
        update: value,
    }])?;
    ConfigAccess::transact_local(node, None, "desktop.inference_backend.save", |txn| {
        let plan = &plan;
        Box::pin(async move {
            apply_desired_state_plan(txn, plan).await?;
            Ok(())
        })
    })
    .await
}

pub async fn delete_inference_backend(
    node: &EmbeddedNode,
    agent_did: &str,
    id: &str,
) -> Result<usize> {
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
        Collection::InferenceBackend,
        agent_did.to_owned(),
        id.to_owned(),
    )])?;
    ConfigAccess::transact_local(node, None, "desktop.inference_backend.delete", |txn| {
        let plan = &plan;
        Box::pin(async move {
            let existed =
                read_desired_state_record_in_txn(txn, Collection::InferenceBackend, agent_did, id)
                    .await?
                    .is_some();
            apply_desired_state_plan(txn, plan).await?;
            Ok(usize::from(existed))
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::document_config::{BackendAuth, BackendModelCatalog, InferenceProfile};
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn backend_save_preserves_catalog_and_scoped_delete_rejects_retained_profile(
    ) -> Result<()> {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        gents::ensure_runtime_schemas(&node).await?;
        for owner in ["did:test:owner", "did:test:other"] {
            gents::ensure_agent_principal(&node, owner).await?;
        }
        let mut backend: InferenceBackend = serde_json::from_value(json!({
            "agent_did":"did:test:owner", "backend_id":"shared", "name":"Backend",
            "provider_kind":"OpenAiCompatible", "endpoint":"http://localhost:8000/v1",
            "auth":{"kind":"unauthenticated"}
        }))?;
        upsert_inference_backend(&node, &backend).await?;
        let mut other = backend.clone();
        other.agent_did = "did:test:other".into();
        upsert_inference_backend(&node, &other).await?;
        let catalog = BackendModelCatalog {
            agent_did: None,
            observed_at: "2026-09-10T00:00:00Z".into(),
            models: Vec::new(),
        };
        ConfigAccess::transact_local(&node, None, "test.catalog", |txn| {
            let backend = &backend;
            let catalog = catalog.clone();
            Box::pin(async move {
                gents::backend_registry::record_model_catalog_in_txn(txn, backend, catalog).await
            })
        })
        .await?;
        backend.name = "Renamed".into();
        upsert_inference_backend(&node, &backend).await?;
        let observation = gents::backend_registry::lookup_backend_observation(
            &node,
            &backend.agent_did,
            &backend.backend_id,
        )
        .await?;
        assert_eq!(observation.unwrap().catalogs, vec![catalog]);
        let mut invalid = backend.clone();
        invalid.auth = BackendAuth::ApiKey { key: " ".into() };
        assert!(upsert_inference_backend(&node, &invalid).await.is_err());
        let profile: InferenceProfile = serde_json::from_value(json!({
            "agent_did":backend.agent_did, "profile_id":"profile", "backend_id":"shared", "model_name":"model"
        }))?;
        gents::config_client::write_inference_profile_document(
            &ConfigAccess::Local(node.clone()),
            &profile,
        )
        .await?;
        assert!(
            delete_inference_backend(&node, &backend.agent_did, "shared")
                .await
                .is_err()
        );
        assert_eq!(
            delete_inference_backend(&node, &other.agent_did, "shared").await?,
            1
        );
        assert_eq!(
            delete_inference_backend(&node, &other.agent_did, "shared").await?,
            0
        );
        assert!(gents::backend_registry::lookup_backend_observation(
            &node,
            &backend.agent_did,
            "shared"
        )
        .await?
        .is_some());
        Ok(())
    }
}
