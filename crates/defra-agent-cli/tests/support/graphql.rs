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
    // DefraDB surfaces optimistic-transaction races as a retryable
    // "transaction conflict. Please retry" error. Tests that seed documents
    // alongside a live runtime (which writes the same AgentRequest/AgentResponse
    // docs) can hit this transiently, so retry a bounded number of times on that
    // specific error. Any other GraphQL error fails fast and is not masked.
    const MAX_ATTEMPTS: usize = 8;
    let client = reqwest::Client::new();
    for attempt in 1..=MAX_ATTEMPTS {
        let response = client
            .post(graphql)
            .json(&serde_json::json!({ "query": query }))
            .send()
            .await
            .with_context(|| format!("posting GraphQL to {graphql}"))?;
        let value: Value = response.json().await.context("decoding GraphQL response")?;
        if let Some(errors) = value.get("errors") {
            let transient = errors.to_string().contains("transaction conflict");
            if transient && attempt < MAX_ATTEMPTS {
                tokio::time::sleep(std::time::Duration::from_millis(50 * attempt as u64)).await;
                continue;
            }
            bail!("graphql returned errors: {errors}");
        }
        return Ok(value);
    }
    unreachable!("graphql_query loop always returns or bails within MAX_ATTEMPTS")
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
