use anyhow::{bail, Context, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;
use serde_json::{json, Value};

const REMOTE_MUTATION_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub(super) async fn execute_mutation(
    node: &EmbeddedNode,
    mutation: &str,
    operation: &str,
) -> Result<()> {
    let response = node.execute(mutation).await;
    if response.has_errors() {
        bail!(
            "{operation} failed: {}",
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(())
}

pub(super) async fn execute_remote_mutation(
    graphql: &str,
    mutation: &str,
    operation: &str,
) -> Result<()> {
    execute_remote_mutation_response(graphql, mutation, operation)
        .await
        .map(|_| ())
}

pub(super) async fn execute_remote_delete_mutation(
    graphql: &str,
    mutation: &str,
    operation: &str,
    response_field: &str,
) -> Result<usize> {
    let response = execute_remote_mutation_response(graphql, mutation, operation).await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get(response_field))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0))
}

async fn execute_remote_mutation_response(
    graphql: &str,
    mutation: &str,
    operation: &str,
) -> Result<RemoteGraphqlMutationResponse> {
    let client = reqwest::Client::builder()
        .timeout(REMOTE_MUTATION_HTTP_TIMEOUT)
        .build()
        .context("building remote GraphQL mutation HTTP client")?;
    let response = client
        .post(graphql)
        .json(&json!({ "query": mutation }))
        .send()
        .await
        .with_context(|| format!("sending {operation} mutation to {graphql}"))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .with_context(|| format!("reading {operation} mutation response from {graphql}"))?;
    if !status.is_success() {
        bail!(
            "{operation} mutation to {graphql} failed with {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    let response: RemoteGraphqlMutationResponse = serde_json::from_slice(&body)
        .with_context(|| format!("decoding {operation} mutation response from {graphql}"))?;
    if let Some(errors) = response.errors.as_ref() {
        if !errors.is_empty() {
            bail!("{operation} mutation to {graphql} returned errors: {errors:?}");
        }
    }
    Ok(response)
}

#[derive(Debug, Deserialize)]
struct RemoteGraphqlMutationResponse {
    #[allow(dead_code)]
    data: Option<Value>,
    errors: Option<Vec<Value>>,
}

pub(super) fn join_fields(fields: &[Option<String>]) -> String {
    fields
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join(",\n                    ")
}

pub(super) fn graphql_string_field(name: &str, value: Option<&str>) -> String {
    match normalize_optional_string(value) {
        Some(value) => format!(r#"{name}: "{}""#, escape_graphql_string(value)),
        None => format!("{name}: null"),
    }
}

pub(super) fn graphql_string_list_field(name: &str, values: &[String]) -> String {
    // Empty lists serialize as `null`, never `[]`: a bare `[]` literal is typed
    // by DefraDB as JsonArray and corrupts a NillableStringArray column (create
    // stores JsonArray, later updates fail re-validation). Matches the
    // `graphql_string_field` null idiom and the protocol-level `string_list_field` fix.
    if values.is_empty() {
        return format!("{name}: null");
    }
    let values = values
        .iter()
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name}: [{values}]")
}

pub(super) fn graphql_optional_int_list_field(name: &str, values: Option<&[i64]>) -> String {
    let Some(values) = values else {
        return format!("{name}: null");
    };
    if values.is_empty() {
        return format!("{name}: null");
    }
    let values = values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{name}: [{values}]")
}

pub(super) fn graphql_optional_bool_field(name: &str, value: Option<bool>) -> String {
    match value {
        Some(value) => format!("{name}: {value}"),
        None => format!("{name}: null"),
    }
}

pub(super) fn graphql_optional_int_field(name: &str, value: Option<i64>) -> String {
    match value {
        Some(value) => format!("{name}: {value}"),
        None => format!("{name}: null"),
    }
}

pub(super) fn graphql_optional_float_field(name: &str, value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{name}: {value}"),
        None => format!("{name}: null"),
    }
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

pub(super) fn escape_graphql_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}
