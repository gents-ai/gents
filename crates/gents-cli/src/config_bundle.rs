use crate::config_writes::ConfigAccess;
use crate::{desired_state, shared::ConfigExportBundle, CONFIG_EXPORT_FORMAT};
use anyhow::{Context, Result};
use gents::store::graphql::escape_graphql_string;
use gents::{store::config_client::config_projection, Collection};
use serde_json::{Map, Value};
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
    let owner = node_did.to_owned();
    let access_mode = access.mode().to_owned();
    access
        .transact_write("export_owned_configuration", |txn| {
            let owner = owner.clone();
            let access_mode = access_mode.clone();
            Box::pin(async move {
                let mut config = Map::new();
                let mut has_documents = false;
                for collection in Collection::ALL {
                    let name = collection.graphql_type();
                    let (fields, _) = config_projection(collection, None)?;
                    let response = txn
                        .execute(&format!(
                            "{{{name}(filter:{{node_did:{{_eq:\"{}\"}}}}) {{{}}}}}",
                            escape_graphql_string(&owner),
                            fields.join(" ")
                        ))
                        .await?;
                    let rows = response
                        .get("data")
                        .and_then(|data| data.get(name))
                        .and_then(Value::as_array)
                        .with_context(|| format!("configuration query returned no {name} rows"))?;
                    let mut normalized = Vec::with_capacity(rows.len());
                    let mut identities = BTreeSet::new();
                    for row in rows {
                        let (_, document) = config_projection(collection, Some(row))?;
                        let document = document.context("canonical config projection missing")?;
                        anyhow::ensure!(
                            document.get("node_did").and_then(Value::as_str)
                                == Some(owner.as_str()),
                            "{name} query returned a foreign owner"
                        );
                        let id = document
                            .get(collection.unique_field())
                            .and_then(Value::as_str)
                            .context("configuration logical ID missing")?;
                        anyhow::ensure!(
                            identities.insert(id.to_owned()),
                            "ambiguous {name} logical ID {id:?} within owner"
                        );
                        normalized.push(document);
                    }
                    sort_document_rows(&mut normalized, collection.unique_field());
                    has_documents |= !normalized.is_empty();
                    if let Some(key) = collection.dir_name() {
                        if !normalized.is_empty() {
                            config.insert(key.to_owned(), Value::Array(normalized));
                        }
                    } else {
                        anyhow::ensure!(normalized.len() <= 1, "ambiguous configuration node");
                        if let Some(node) = normalized.pop() {
                            config.insert("node".into(), node);
                        }
                    }
                }
                if !config.contains_key("node") {
                    anyhow::ensure!(
                        !has_documents,
                        "owned configuration exists without its Node"
                    );
                    return Ok(None);
                }
                let config = serde_json::from_value(Value::Object(config))
                    .context("decode canonical configuration snapshot")?;
                Ok(Some(ConfigExportBundle {
                    format: CONFIG_EXPORT_FORMAT.into(),
                    node_did: owner,
                    exported_at: chrono::Utc::now().to_rfc3339(),
                    access_mode,
                    config,
                }))
            })
        })
        .await
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

pub(crate) fn sort_document_rows(rows: &mut [Value], key: &str) {
    rows.sort_by(|left, right| {
        let left_key = left.get(key).and_then(Value::as_str).unwrap_or_default();
        let right_key = right.get(key).and_then(Value::as_str).unwrap_or_default();
        left_key.cmp(right_key)
    });
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
