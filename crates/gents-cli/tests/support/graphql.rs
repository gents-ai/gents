use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

// Re-export the canonical escaper instead of duplicating it, so test-support
// escaping can never drift from production (audit/code-review finding).
pub use gents::graphql::escape_graphql_string;

pub async fn graphql_query(graphql: &str, query: &str) -> Result<Value> {
    let access = gents::config_client::ConfigAccess::Graphql(graphql.to_string());
    if query.trim_start().starts_with("mutation") {
        access.write("test.fixture", query).await
    } else {
        access.execute(query).await
    }
}

pub fn first_graphql_row<'a>(response: &'a Value, field: &str) -> Result<&'a Value> {
    response
        .pointer(&format!("/data/{field}"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .ok_or_else(|| anyhow!("missing {field} row in GraphQL response: {response}"))
}

pub fn doc_id_from_create(response: &Value, field: &str) -> Result<String> {
    response
        .pointer(&format!("/data/{field}/0/_docID"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .with_context(|| format!("missing _docID in {field} response: {response}"))
}

pub async fn doc_id_for_tools(graphql: &str, tools_id: &str) -> Result<String> {
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                Tools(filter: {{ tools_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    _docID
                }}
            }}"#,
            escape_graphql_string(tools_id),
        ),
    )
    .await?;
    first_graphql_row(&response, "Tools")?
        .get("_docID")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("Tools row missing _docID for {tools_id}"))
}

pub async fn exec(node: &gents::defra_node::EmbeddedNode, query: &str) -> Result<()> {
    let response = node.execute(query).await;
    if response.has_errors() {
        bail!("GraphQL mutation failed: {:?}", response.errors);
    }
    Ok(())
}
