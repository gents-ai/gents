use crate::config_writes::{ConfigAccess, ConfigApplyTxn};
use crate::{desired_state, shared::ConfigExportBundle, CONFIG_EXPORT_FORMAT};
use anyhow::{Context, Result};
use gents_pack::pack::authoring::export::sort_document_rows;
use serde_json::Value;
use std::collections::BTreeSet;

pub(crate) async fn build_config_export_bundle(
    access: &ConfigAccess,
    node_did: &str,
) -> Result<ConfigExportBundle> {
    read_owned_config_bundle(access, node_did)
        .await?
        .context("configuration owner has no Node")
}

pub(crate) async fn build_desired_state_live_bundle(
    access: &ConfigAccess,
    desired: &desired_state::DesiredStateManifest,
) -> Result<Option<ConfigExportBundle>> {
    read_owned_config_bundle(access, &desired.node.node_did).await
}

async fn read_owned_config_bundle(
    access: &ConfigAccess,
    node_did: &str,
) -> Result<Option<ConfigExportBundle>> {
    anyhow::ensure!(!node_did.trim().is_empty(), "configuration owner is blank");
    let access_mode = access.mode().to_owned();
    // A snapshot read: offline client access admits only read-only
    // transactions, and export must work without a running server.
    access
        .transact_readonly("export_owned_configuration", |txn| {
            let access_mode = access_mode.clone();
            Box::pin(
                async move { read_owned_config_bundle_in_txn(txn, node_did, access_mode).await },
            )
        })
        .await
}

/// Read the owner's canonical configuration through the caller's transaction,
/// so a plan derived from it observes the same snapshot its writes commit to.
pub(crate) async fn read_owned_config_bundle_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    access_mode: String,
) -> Result<Option<ConfigExportBundle>> {
    Ok(
        gents_pack::pack::authoring::export::read_owned_config_in_txn(txn, node_did)
            .await?
            .map(|config| ConfigExportBundle {
                format: CONFIG_EXPORT_FORMAT.into(),
                node_did: node_did.to_owned(),
                exported_at: chrono::Utc::now().to_rfc3339(),
                access_mode,
                config,
            }),
    )
}

pub(crate) fn live_manifest_from_bundle(
    desired: &desired_state::DesiredStateManifest,
    live: &Option<ConfigExportBundle>,
) -> Result<(
    Option<desired_state::DesiredNode>,
    desired_state::DesiredStateManifest,
)> {
    if let Some(bundle) = live {
        let manifest = desired_state::manifest_from_export_bundle(bundle)?;
        Ok((Some(manifest.node.clone()), manifest))
    } else {
        let empty = serde_json::from_value(serde_json::json!({"node":desired.node}))?;
        Ok((None, empty))
    }
}

pub(crate) fn select_apply_collection_docs(
    docs: &[Value],
    unique_field: &str,
    collection_name: &str,
    diff: &desired_state::DesiredStateCollectionDiff,
) -> Result<Vec<Value>> {
    let requested_ids = diff
        .create
        .iter()
        .chain(diff.update.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    if requested_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut selected = docs
        .iter()
        .filter(|doc| {
            doc.get(unique_field)
                .and_then(Value::as_str)
                .is_some_and(|value| requested_ids.contains(value))
        })
        .cloned()
        .collect::<Vec<_>>();
    sort_document_rows(&mut selected, unique_field);

    let found_ids = selected
        .iter()
        .filter_map(|doc| doc.get(unique_field).and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();
    let missing_ids = requested_ids
        .difference(&found_ids)
        .cloned()
        .collect::<Vec<_>>();
    if !missing_ids.is_empty() {
        anyhow::bail!(
            "desired-state apply missing {collection_name} documents for ids: {}",
            missing_ids.join(", ")
        );
    }

    Ok(selected)
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn offline_client_access_exports_the_owned_configuration() {
        let node = std::sync::Arc::new(
            gents_host::test_support::fixture_node_builder()
                .build()
                .await
                .unwrap(),
        );
        gents_store::schema::ensure_runtime_schemas(&node)
            .await
            .unwrap();
        let owner = gents_host::test_support::publish_test_node(&node).await;
        // Offline client configuration access refuses every write transaction,
        // even for the runtime's own DID.
        let offline_client = gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            ::identity::Did::new(owner.as_str()).unwrap(),
        )
        .with_client_signed_configuration();
        let bundle = super::build_config_export_bundle(&offline_client, &owner)
            .await
            .unwrap();
        assert_eq!(bundle.node_did, owner);
        node.shutdown().await.unwrap();
    }
}
