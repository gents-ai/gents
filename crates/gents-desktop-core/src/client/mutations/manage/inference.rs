use anyhow::Result;
use gents::store::collection::Collection;
use gents::store::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents::InferenceBackend;

pub async fn upsert_inference_backend_on(
    access: &ConfigAccess,
    document: &InferenceBackend,
) -> Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::InferenceBackend,
        add: value.clone(),
        update: value,
    }])?;
    super::apply_plan(access, "desktop.inference_backend.save", plan).await
}

pub async fn delete_inference_backend_on(
    access: &ConfigAccess,
    node_did: &str,
    id: &str,
) -> Result<usize> {
    super::delete_scoped_document(
        access,
        "desktop.inference_backend.delete",
        Collection::InferenceBackend,
        node_did,
        id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::ServedRuntimeHome;
    use gents::documents::{BackendAuth, BackendModelCatalog, InferenceProfile};
    use serde_json::json;

    async fn observation(
        access: &ConfigAccess,
        node_did: &str,
        backend_id: &str,
    ) -> Result<Option<gents::documents::InferenceBackendObservation>> {
        access
            .transact_readonly("test.backend.observation", |txn| {
                Box::pin(async move {
                    gents::inference::backends::registry::lookup_backend_observation_in_txn(
                        txn, node_did, backend_id,
                    )
                    .await
                })
            })
            .await
    }

    #[tokio::test]
    async fn backend_save_preserves_catalog_and_scoped_delete_rejects_retained_profile(
    ) -> Result<()> {
        let runtime = ServedRuntimeHome::start().await;
        let access = runtime.user_access();
        let other_owner = runtime.publish_foreign_node().await;
        let mut backend: InferenceBackend = serde_json::from_value(json!({
            "node_did":runtime.node_did(), "backend_id":"shared", "name":"Backend",
            "provider_kind":"OpenAiCompatible", "endpoint":"http://localhost:8000/v1",
            "auth":{"kind":"unauthenticated"}
        }))?;
        upsert_inference_backend_on(&access, &backend).await?;
        let mut other = backend.clone();
        other.node_did = other_owner;
        upsert_inference_backend_on(&access, &other).await?;
        let catalog: BackendModelCatalog = serde_json::from_value(json!({
            "node_did": null,
            "observed_at": "2026-09-10T00:00:00Z",
            "models": [{"model_name": "model"}],
        }))?;
        // Catalog discovery is the runtime's own observation.
        runtime
            .runtime_access()
            .transact_write("test.catalog", |txn| {
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
        backend.name = "Renamed".into();
        upsert_inference_backend_on(&access, &backend).await?;
        let retained = observation(&access, &backend.node_did, &backend.backend_id).await?;
        assert_eq!(retained.unwrap().catalogs, vec![catalog]);
        let mut invalid = backend.clone();
        invalid.auth = BackendAuth::ApiKey { key: " ".into() };
        assert!(upsert_inference_backend_on(&access, &invalid)
            .await
            .is_err());
        let profile: InferenceProfile = serde_json::from_value(json!({
            "node_did":backend.node_did, "profile_id":"profile", "backend_id":"shared", "model_name":"model"
        }))?;
        gents::documents::write_inference_profile_document(&access, &profile).await?;
        assert!(
            delete_inference_backend_on(&access, &backend.node_did, "shared")
                .await
                .is_err()
        );
        assert_eq!(
            delete_inference_backend_on(&access, &other.node_did, "shared").await?,
            1
        );
        assert_eq!(
            delete_inference_backend_on(&access, &other.node_did, "shared").await?,
            0
        );
        assert!(observation(&access, &backend.node_did, "shared")
            .await?
            .is_some());
        runtime.shutdown().await;
        Ok(())
    }
}
