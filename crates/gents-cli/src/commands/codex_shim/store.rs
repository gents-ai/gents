use std::sync::Arc;

use gents::config_client::{ConfigAccess, ConfigClientError};
use gents::defra_node::EmbeddedNode;
use gents::graphql::graphql_with_transaction_retry;
use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub(super) enum StoreError {
    #[error(transparent)]
    Query(#[from] ConfigClientError),
    #[error("GENTS Codex shim mutation failed")]
    Mutation(#[source] ConfigClientError),
}

type Result<T, E = StoreError> = std::result::Result<T, E>;

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
        .map_err(StoreError::Mutation)
}
