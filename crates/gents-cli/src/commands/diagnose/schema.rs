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
    use gents_store::graphql::escape_graphql_string;
    let query = format!(
        r#"{{
            NodeRuntime(
                filter: {{ node_did: {{ _eq: "{node_did}" }} }},
                limit: 1
            ) {{
                node_did
                reconcile_phase
                last_reconcile_result
                last_reconcile_error
                last_reconcile_completed_at
                updated_at
            }}
        }}"#,
        node_did = escape_graphql_string(node_did),
    );
    Ok(crate::graphql_rows(access, "NodeRuntime", &query)
        .await?
        .into_iter()
        .next())
}
