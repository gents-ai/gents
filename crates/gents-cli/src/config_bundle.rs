use crate::config_writes::ConfigAccess;
use crate::{desired_state, shared::ConfigExportBundle, CONFIG_EXPORT_FORMAT};
use gents::config_client::{config_projection, ConfigClientError, TransactionFailure};
use gents::graphql::escape_graphql_string;
use gents::Collection;
use serde_json::{Map, Value};
use std::collections::BTreeSet;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ConfigBundleError {
    #[error(transparent)]
    Config(#[from] ConfigClientError),
    #[error(transparent)]
    Projection(#[from] gents::config_client::DesiredStateError),
    #[error(transparent)]
    DesiredState(#[from] desired_state::convert::DesiredStateError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("desired-state apply missing {collection_name} documents for ids: {ids}")]
    MissingDocuments {
        collection_name: String,
        ids: String,
    },
    #[error("configuration owner is blank")]
    BlankOwner,
    #[error("configuration owner has no Node")]
    MissingNode,
    #[error("configuration query returned no {collection_name} rows")]
    MissingRows { collection_name: String },
    #[error("canonical config projection missing")]
    ProjectionMissing,
    #[error("{collection_name} query returned a foreign owner")]
    ForeignOwner { collection_name: String },
    #[error("configuration logical ID missing")]
    LogicalIdMissing,
    #[error("ambiguous {collection_name} logical ID {id:?} within owner")]
    AmbiguousLogicalId { collection_name: String, id: String },
    #[error("ambiguous configuration node")]
    AmbiguousNode,
    #[error("owned configuration exists without its Node")]
    OwnedWithoutNode,
    #[error("decode canonical configuration snapshot")]
    DecodeSnapshot(#[source] serde_json::Error),
}

type Result<T, E = ConfigBundleError> = std::result::Result<T, E>;

impl TransactionFailure for ConfigBundleError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Config(error) => Some(error),
            _ => None,
        }
    }
}

pub(crate) async fn build_config_export_bundle(
    access: &ConfigAccess,
    node_did: &str,
) -> Result<ConfigExportBundle> {
    read_owned_config_bundle(access, node_did)
        .await?
        .ok_or(ConfigBundleError::MissingNode)
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
    if node_did.trim().is_empty() {
        return Err(ConfigBundleError::BlankOwner);
    }
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
                        .ok_or_else(|| ConfigBundleError::MissingRows {
                            collection_name: name.to_owned(),
                        })?;
                    let mut normalized = Vec::with_capacity(rows.len());
                    let mut identities = BTreeSet::new();
                    for row in rows {
                        let (_, document) = config_projection(collection, Some(row))?;
                        let document = document.ok_or(ConfigBundleError::ProjectionMissing)?;
                        if document.get("node_did").and_then(Value::as_str) != Some(owner.as_str())
                        {
                            return Err(ConfigBundleError::ForeignOwner {
                                collection_name: name.to_owned(),
                            });
                        }
                        let id = document
                            .get(collection.unique_field())
                            .and_then(Value::as_str)
                            .ok_or(ConfigBundleError::LogicalIdMissing)?;
                        if !identities.insert(id.to_owned()) {
                            return Err(ConfigBundleError::AmbiguousLogicalId {
                                collection_name: name.to_owned(),
                                id: id.to_owned(),
                            });
                        }
                        normalized.push(document);
                    }
                    sort_document_rows(&mut normalized, collection.unique_field());
                    has_documents |= !normalized.is_empty();
                    if let Some(key) = collection.dir_name() {
                        if !normalized.is_empty() {
                            config.insert(key.to_owned(), Value::Array(normalized));
                        }
                    } else {
                        if normalized.len() > 1 {
                            return Err(ConfigBundleError::AmbiguousNode);
                        }
                        if let Some(node) = normalized.pop() {
                            config.insert("node".into(), node);
                        }
                    }
                }
                if !config.contains_key("node") {
                    if has_documents {
                        return Err(ConfigBundleError::OwnedWithoutNode);
                    }
                    return Ok(None);
                }
                let config = serde_json::from_value(Value::Object(config))
                    .map_err(ConfigBundleError::DecodeSnapshot)?;
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
) -> Result<
    (
        Option<desired_state::DesiredNode>,
        desired_state::DesiredStateManifest,
    ),
    ConfigBundleError,
> {
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
) -> Result<Vec<Value>, ConfigBundleError> {
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
        return Err(ConfigBundleError::MissingDocuments {
            collection_name: collection_name.to_owned(),
            ids: missing_ids.join(", "),
        });
    }

    Ok(selected)
}
