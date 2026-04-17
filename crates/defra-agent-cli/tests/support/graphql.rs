use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

pub fn escape_graphql_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

pub async fn graphql_query(graphql: &str, query: &str) -> Result<Value> {
    let response = reqwest::Client::new()
        .post(graphql)
        .json(&serde_json::json!({ "query": query }))
        .send()
        .await
        .with_context(|| format!("posting GraphQL to {graphql}"))?;
    let value: Value = response.json().await.context("decoding GraphQL response")?;
    if let Some(errors) = value.get("errors") {
        bail!("graphql returned errors: {errors}");
    }
    Ok(value)
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
