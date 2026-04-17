use super::*;

pub(crate) async fn query_has_row_by_unique_field(
    core: &ClientCore,
    root: &str,
    field: &str,
    value: &str,
) -> Result<bool> {
    let escaped_value = escape_graphql_string(value);
    let query = format!(
        r#"{{
            {root}(
                filter: {{ {field}: {{ _eq: "{escaped_value}" }} }},
                limit: 1
            ) {{
                {field}
            }}
        }}"#
    );
    let response = core.node().execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query {root}.{field}={value} failed: {}",
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get(root))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get(field))
        .and_then(Value::as_str)
        == Some(value))
}
