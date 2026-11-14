use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use gents_documents::{config_projection, PackConfig};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigApplyTxn;
use gents_store::graphql::escape_graphql_string;

/// The canonical documents `node_did` owns in the collections `include`
/// admits, read through the caller's transaction and sorted by logical ID.
pub(super) async fn read_owned_documents_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    include: &(dyn Fn(Collection) -> bool + Sync),
) -> Result<BTreeMap<Collection, Vec<Value>>> {
    anyhow::ensure!(!node_did.trim().is_empty(), "configuration owner is blank");
    let mut documents = BTreeMap::new();
    for collection in Collection::ALL.into_iter().filter(|kind| include(*kind)) {
        let name = collection.graphql_type();
        let rows = if collection == Collection::InferenceBackend {
            gents_documents::list_inference_backends_in_txn(txn, node_did)
                .await?
                .into_iter()
                .map(serde_json::to_value)
                .collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            let (fields, _) = config_projection(collection, None)?;
            let response = txn
                .execute(&format!(
                    "{{{name}(filter:{{node_did:{{_eq:\"{}\"}}}}) {{{}}}}}",
                    escape_graphql_string(node_did),
                    fields.join(" ")
                ))
                .await?;
            response
                .get("data")
                .and_then(|data| data.get(name))
                .and_then(Value::as_array)
                .with_context(|| format!("configuration query returned no {name} rows"))?
                .clone()
        };
        let mut normalized = Vec::with_capacity(rows.len());
        let mut identities = BTreeSet::new();
        for row in &rows {
            let (_, document) = config_projection(collection, Some(row))?;
            let document = document.context("canonical config projection missing")?;
            anyhow::ensure!(
                document.get("node_did").and_then(Value::as_str) == Some(node_did),
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
        documents.insert(collection, normalized);
    }
    Ok(documents)
}

/// The owner's whole canonical configuration through the caller's
/// transaction, so a plan derived from it observes the same snapshot its
/// writes commit to. `None` when the owner has no Node.
pub async fn read_owned_config_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
) -> Result<Option<PackConfig>> {
    let documents = read_owned_documents_in_txn(txn, node_did, &|_| true).await?;
    let mut config = Map::new();
    let mut has_documents = false;
    for (collection, mut rows) in documents {
        has_documents |= !rows.is_empty();
        if let Some(key) = collection.dir_name() {
            if !rows.is_empty() {
                config.insert(key.to_owned(), Value::Array(rows));
            }
        } else {
            anyhow::ensure!(rows.len() <= 1, "ambiguous configuration {collection}");
            if let Some(document) = rows.pop() {
                config.insert(collection.to_string(), document);
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
    serde_json::from_value(Value::Object(config))
        .context("decode canonical configuration snapshot")
        .map(Some)
}

pub fn sort_document_rows(rows: &mut [Value], key: &str) {
    rows.sort_by(|left, right| {
        let left_key = left.get(key).and_then(Value::as_str).unwrap_or_default();
        let right_key = right.get(key).and_then(Value::as_str).unwrap_or_default();
        left_key.cmp(right_key)
    });
}
