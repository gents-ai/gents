use anyhow::Result;
use serde_json::{json, Value};

use crate::config_writes::ConfigAccess;
use gents_store::collection::Collection;

pub(super) async fn diagnose_schema_presence(access: &ConfigAccess) -> Vec<Value> {
    let mut results = Vec::new();
    for collection in gents_protocol::schemas::ALL_COLLECTION_NAMES {
        let required_for_config = Collection::ALL
            .iter()
            .any(|config| config.graphql_type() == *collection);
        let query = format!(r#"{{ {collection}(limit: 1) {{ _docID }} }}"#);
        match access.execute(&query).await {
            Ok(_) => results.push(json!({
                "collection": collection,
                "required_for_config": required_for_config,
                "ok": true,
            })),
            Err(error) => results.push(json!({
                "collection": collection,
                "required_for_config": required_for_config,
                "ok": false,
                "error": error.to_string(),
            })),
        }
    }
    results
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
