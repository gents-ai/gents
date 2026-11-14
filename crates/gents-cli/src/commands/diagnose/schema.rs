use anyhow::Result;
use serde_json::{json, Value};

use crate::config_writes::ConfigAccess;
use gents_store::collection::Collection;

/// Each collection's read is independent, so they run together: an unreachable endpoint costs
/// one read's retries, not one per collection.
pub(super) async fn diagnose_schema_presence(access: &ConfigAccess) -> Vec<Value> {
    let checks = gents_protocol::schemas::ALL_COLLECTION_NAMES
        .iter()
        .map(|collection| collection_presence(access, collection));
    futures_util::future::join_all(checks).await
}

async fn collection_presence(access: &ConfigAccess, collection: &str) -> Value {
    let required_for_config = Collection::ALL
        .iter()
        .any(|config| config.graphql_type() == collection);
    let query = format!(r#"{{ {collection}(limit: 1) {{ _docID }} }}"#);
    match access.execute(&query).await {
        Ok(_) => json!({
            "collection": collection,
            "required_for_config": required_for_config,
            "ok": true,
        }),
        Err(error) => json!({
            "collection": collection,
            "required_for_config": required_for_config,
            "ok": false,
            "error": error.to_string(),
        }),
    }
}

pub(super) async fn load_runtime_row(
    access: &ConfigAccess,
    node_did: &str,
) -> Result<Option<Value>> {
    access
        .read_view(&gents_store::views::runtime::NodeRuntimeList::new(node_did).limit(1))
        .await?
        .into_iter()
        .next()
        .map(serde_json::to_value)
        .transpose()
        .map_err(Into::into)
}
