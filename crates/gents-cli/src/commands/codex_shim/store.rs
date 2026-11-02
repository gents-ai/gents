use std::sync::Arc;

use anyhow::{Context, Result};
use gents::defra_node::EmbeddedNode;
use gents::store::config_client::ConfigAccess;
use gents::store::graphql::graphql_with_transaction_retry;
use serde_json::{json, Value};

pub(super) async fn query_node_json(node: &EmbeddedNode, query: &str) -> Result<Value> {
    let response = graphql_with_transaction_retry(node, query, "codex shim store").await?;
    Ok(json!({
        "data": response.data.unwrap_or_else(|| json!({})),
    }))
}

pub(super) async fn write_committed(
    node: &Arc<EmbeddedNode>,
    operation: &'static str,
    mutation: &str,
) -> Result<Value> {
    ConfigAccess::local(node.clone())
        .write(operation, mutation)
        .await
        .context("GENTS Codex shim mutation failed")
}
