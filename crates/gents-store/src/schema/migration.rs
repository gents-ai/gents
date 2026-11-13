use std::collections::BTreeMap;
use std::sync::Arc;

use defra_node::Node as EmbeddedNode;

use crate::config_client::ConfigClientError;

pub use gents_migration::{
    canonical_versions, check_migration_compatibility, ensure_migrations, ensure_migrations_arc,
    ensure_migrations_dynamic, ensure_migrations_with_registry, fixture_lens_wasm,
    non_canonical_lineages, non_canonical_store_lineages, predict_transform_id, BaselineCollection,
    BaselineCollectionOwned, CollectionExpectation, DynamicRegistry, Error as MigrationError,
    LensSpec, LensSpecOwned, LineageMismatch, MaterializationStats, MigrationReport, MigrationStep,
    MigrationStepOwned, Registry, DEFAULT_BASELINE, DEFAULT_REGISTRY, DEFAULT_STEPS,
};

#[derive(Debug, thiserror::Error)]
pub enum RuntimeMigrationError {
    #[error("ensure_migrations")]
    EnsureMigrations(#[source] MigrationError),
    #[error(transparent)]
    Config(#[from] ConfigClientError),
}

pub async fn ensure_all_runtime_migrations(
    node: Arc<EmbeddedNode>,
) -> Result<(), RuntimeMigrationError> {
    crate::config_client::ConfigAccess::local(node)
        .run_local_owned(|node| async move { ensure_migrations_borrowed(node.as_ref()).await })
        .await
}

pub(crate) async fn ensure_migrations_borrowed(
    node: &EmbeddedNode,
) -> Result<(), RuntimeMigrationError> {
    let report = ensure_migrations(node)
        .await
        .map_err(RuntimeMigrationError::EnsureMigrations)?;
    for conflict in &report.materialization.parked_unique_conflicts {
        warn_parked_unique_documents(node, conflict).await;
    }
    Ok(())
}

/// Names the documents behind a merge-parked unique-index conflict: of the
/// documents sharing one unique value tuple, the smallest docID keeps the
/// index entry and the rest stay readable by non-index scans.
async fn warn_parked_unique_documents(
    node: &EmbeddedNode,
    conflict: &gents_migration::ParkedUniqueConflict,
) {
    let collection = conflict.collection.as_str();
    if crate::graphql::validate_graphql_name(collection).is_err() {
        tracing::warn!(
            collection,
            "parked-doc diagnostics: unsafe collection identifier"
        );
        return;
    }
    for index in &conflict.unique_indexes {
        let fields = &index.fields;
        if fields.is_empty()
            || fields
                .iter()
                .any(|field| crate::graphql::validate_graphql_name(field).is_err())
        {
            continue;
        }
        let query = format!("{{ {collection} {{ _docID {} }} }}", fields.join(" "));
        let response = match crate::graphql::graphql_with_transaction_retry(
            node,
            &query,
            "parked unique conflict scan",
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(
                    collection,
                    index = %index.name,
                    error = %gents_protocol::error_chain::ErrorChain(&error),
                    "parked-doc diagnostics: scan failed"
                );
                continue;
            }
        };
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get(collection))
            .and_then(|rows| rows.as_array())
            .cloned()
            .unwrap_or_default();

        let mut by_values: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for row in &rows {
            let values: Vec<serde_json::Value> = fields
                .iter()
                .map(|field| row.get(field).cloned().unwrap_or(serde_json::Value::Null))
                .collect();
            // An all-null tuple is not unique-constrained.
            if values.iter().all(serde_json::Value::is_null) {
                continue;
            }
            let Some(doc_id) = row.get("_docID").and_then(|value| value.as_str()) else {
                continue;
            };
            let key = serde_json::to_string(&values).unwrap_or_default();
            by_values.entry(key).or_default().push(doc_id.to_string());
        }

        for (values, mut doc_ids) in by_values {
            if doc_ids.len() < 2 {
                continue;
            }
            doc_ids.sort();
            tracing::warn!(
                collection,
                index = %index.name,
                values = %values,
                winner_doc_id = %doc_ids[0],
                parked_doc_ids = ?&doc_ids[1..],
                "unique index conflict parked at boot: existing deterministic \
                 winner keeps the index entry; parked document(s) remain \
                 readable by non-index scans"
            );
        }
    }
}
