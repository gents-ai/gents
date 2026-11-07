use anyhow::Result;
use gents::store::config_client::ConfigAccess;
use serde_json::{json, Value};

pub(super) async fn query_node_json(reader: &ConfigAccess, query: &str) -> Result<Value> {
    let response = reader.execute(query).await?;
    Ok(json!({
        "data": response.get("data").cloned().unwrap_or_else(|| json!({})),
    }))
}
