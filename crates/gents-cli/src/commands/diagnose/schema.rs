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
    use gents_protocol::document_query::{
        fields::node_runtime as fields, Filter, Query, Selection,
    };
    let rendered = Query::select(
        Selection::new()
            .field(fields::NODE_DID)
            .field(fields::RECONCILE_PHASE)
            .field(fields::LAST_RECONCILE_RESULT)
            .field(fields::LAST_RECONCILE_ERROR)
            .field(fields::LAST_RECONCILE_COMPLETED_AT)
            .field(fields::UPDATED_AT),
    )
    .filter(Filter::eq(fields::NODE_DID, node_did))
    .limit(1)
    .render();
    Ok(gents_protocol::graphql::graphql_rows_from_response(
        &access.read(&rendered).await?,
        "NodeRuntime",
    )
    .into_iter()
    .next())
}
