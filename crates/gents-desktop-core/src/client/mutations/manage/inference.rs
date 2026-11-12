use anyhow::Result;
use gents_documents::InferenceBackend;
use gents_inference::operations::{self, OperationFailure};
use gents_protocol::operations::inference::{
    DocumentWrite, InferenceDocument, InferenceOperationError, WriteMode,
};
use gents_store::config_client::ConfigAccess;

pub(super) fn refusal(
    error: InferenceOperationError,
    document: InferenceDocument,
    verb: &'static str,
) -> anyhow::Error {
    let next = error.recovery(document, verb);
    crate::error::inference_operation_failure(error, next)
}

pub(super) fn operation_failure(
    failure: OperationFailure,
    document: InferenceDocument,
    verb: &'static str,
) -> anyhow::Error {
    match failure {
        OperationFailure::Operation(error) => refusal(error, document, verb),
        OperationFailure::Other(error) => error,
    }
}

pub(super) fn put_write(
    document: InferenceDocument,
    value: &serde_json::Value,
    mode: WriteMode,
) -> Result<(String, DocumentWrite)> {
    operations::whole_document_write(document, value, mode)
        .map_err(|error| refusal(error, document, "update"))
}

pub(super) async fn put_document(
    access: &ConfigAccess,
    document: InferenceDocument,
    value: &serde_json::Value,
) -> Result<()> {
    let (node_did, write) = put_write(document, value, WriteMode::Put)?;
    operations::write(access, &node_did, &write)
        .await
        .map_err(|error| refusal(error, document, "update"))?;
    Ok(())
}

pub(super) async fn remove_document(
    access: &ConfigAccess,
    document: InferenceDocument,
    node_did: &str,
    id: &str,
) -> Result<usize> {
    let receipt = operations::remove(access, node_did, document, id)
        .await
        .map_err(|error| refusal(error, document, "remove"))?;
    Ok(usize::from(receipt.removed))
}

pub async fn upsert_inference_backend_on(
    access: &ConfigAccess,
    document: &InferenceBackend,
) -> Result<()> {
    put_document(
        access,
        InferenceDocument::Backend,
        &serde_json::to_value(document)?,
    )
    .await
}

pub async fn delete_inference_backend_on(
    access: &ConfigAccess,
    node_did: &str,
    id: &str,
) -> Result<usize> {
    remove_document(access, InferenceDocument::Backend, node_did, id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::ServedRuntimeHome;
    use gents_documents::{BackendModelCatalog, InferenceProfile};
    use serde_json::json;

    async fn observation(
        access: &ConfigAccess,
        node_did: &str,
        backend_id: &str,
    ) -> Result<Option<gents_documents::InferenceBackendObservation>> {
        access
            .transact_readonly("test.backend.observation", |txn| {
                Box::pin(async move {
                    gents_inference::backends::registry::lookup_backend_observation_in_txn(
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
                    gents_inference::backends::registry::record_model_catalog_in_txn(
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
        let profile: InferenceProfile = serde_json::from_value(json!({
            "node_did":backend.node_did, "profile_id":"profile", "backend_id":"shared", "model_name":"model"
        }))?;
        gents_documents::write_inference_profile_document(
            &access,
            &profile,
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await?;
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
