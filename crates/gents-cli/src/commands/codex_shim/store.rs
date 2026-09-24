use std::sync::Arc;

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::defra_node::EmbeddedNode;
use gents::graphql::graphql_with_transaction_retry;
use serde_json::{json, Value};

/// Route shim reads and auto-committed writes through the runtime's bounded
/// DefraDB conflict retry so overlapping reconciliation stays transparent to
/// Codex clients.
pub(super) async fn query_node_json(node: &EmbeddedNode, query: &str) -> Result<Value> {
    let response = graphql_with_transaction_retry(node, query, "codex shim store").await?;
    Ok(json!({
        "data": response.data.unwrap_or_else(|| json!({})),
    }))
}

/// Route a mutation through the canonical committed-write owner.
pub(super) async fn write_committed(
    node: &Arc<EmbeddedNode>,
    operation: &'static str,
    mutation: &str,
) -> Result<Value> {
    ConfigAccess::Local(node.clone())
        .write(operation, mutation)
        .await
        .context("GENTS Codex shim mutation failed")
}
