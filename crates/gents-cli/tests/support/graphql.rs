use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use gents::retry::{DEFRA_DB_CONFLICT_INITIAL_BACKOFF_MS, DEFRA_DB_CONFLICT_MAX_RETRIES};
use gents_protocol::graphql::{execute_graphql_async, GraphqlRequestOptions};
use serde_json::Value;

// Re-export the canonical escaper instead of duplicating it, so test-support
// escaping can never drift from production (audit/code-review finding).
pub use gents::graphql::escape_graphql_string;

pub async fn graphql_query(graphql: &str, query: &str) -> Result<Value> {
    execute_graphql_async(
        graphql,
        query,
        GraphqlRequestOptions {
            timeout: Duration::from_secs(30),
            max_attempts: DEFRA_DB_CONFLICT_MAX_RETRIES as usize + 1,
            retry_backoff: Duration::from_millis(DEFRA_DB_CONFLICT_INITIAL_BACKOFF_MS),
        },
    )
    .await
}

pub fn first_graphql_row<'a>(response: &'a Value, field: &str) -> Result<&'a Value> {
    response
        .pointer(&format!("/data/{field}"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .ok_or_else(|| anyhow!("missing {field} row in GraphQL response: {response}"))
}

pub async fn doc_id_for_selection(graphql: &str, selection_id: &str) -> Result<String> {
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                ToolSelection(filter: {{ selection_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    _docID
                }}
            }}"#,
            escape_graphql_string(selection_id),
        ),
    )
    .await?;
    first_graphql_row(&response, "ToolSelection")?
        .get("_docID")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("ToolSelection row missing _docID for {selection_id}"))
}

pub async fn exec(node: &gents::defra_node::EmbeddedNode, query: &str) -> Result<()> {
    let response = node.execute(query).await;
    if response.has_errors() {
        bail!("GraphQL mutation failed: {:?}", response.errors);
    }
    Ok(())
}
