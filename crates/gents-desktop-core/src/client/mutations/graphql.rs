use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use serde_json::Value;

pub(crate) async fn execute_mutation_response(
    node: &EmbeddedNode,
    mutation: &str,
    operation: &'static str,
) -> Result<Value> {
    gents::config_client::ConfigAccess::write_local(node, operation, mutation).await
}

pub(super) async fn execute_mutation(
    node: &EmbeddedNode,
    mutation: &str,
    operation: &'static str,
) -> Result<()> {
    execute_mutation_response(node, mutation, operation).await?;
    Ok(())
}

pub(super) fn normalize_required<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    normalize_optional_string(Some(value)).with_context(|| format!("{field} must not be empty"))
}

pub(super) fn normalize_optional_string(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

pub(super) use gents_protocol::graphql::escape_graphql_string;
