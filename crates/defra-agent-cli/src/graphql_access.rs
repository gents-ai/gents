use anyhow::Result;
use defra_agent_protocol::graphql::{
    execute_graphql_async, extract_mutation_doc_id as shared_extract_mutation_doc_id,
    first_graphql_row as shared_first_graphql_row,
    graphql_bool_literal as shared_graphql_bool_literal,
    graphql_endpoint_available as shared_graphql_endpoint_available,
    graphql_input_literal as shared_graphql_input_literal, graphql_rows_from_response,
    graphql_string_list_literal as shared_graphql_string_list_literal,
    normalize_optional_rfc3339 as shared_normalize_optional_rfc3339,
    nullable_string_field as shared_nullable_string_field,
    optional_bool_field as shared_optional_bool_field,
    optional_f64_field as shared_optional_f64_field,
    optional_i64_field as shared_optional_i64_field,
    optional_string_field as shared_optional_string_field,
    string_list_field as shared_string_list_field, GraphqlRequestOptions,
};
use serde_json::Value;

use crate::config_writes::ConfigAccess;

pub(crate) async fn graphql_endpoint_available(graphql: &str) -> bool {
    shared_graphql_endpoint_available(
        graphql,
        GraphqlRequestOptions {
            timeout: std::time::Duration::from_secs(2),
            max_attempts: 1,
            retry_backoff: std::time::Duration::from_millis(50),
        },
    )
    .await
}

pub(crate) async fn graphql_rows(
    access: &ConfigAccess,
    collection_name: &str,
    query: &str,
) -> Result<Vec<Value>> {
    let response = access.execute(query).await?;
    Ok(graphql_rows_from_response(&response, collection_name))
}

pub(crate) async fn graphql_rows_or_empty_if_collection_missing(
    access: &ConfigAccess,
    collection_name: &str,
    query: &str,
) -> Result<Vec<Value>> {
    match graphql_rows(access, collection_name, query).await {
        Ok(rows) => Ok(rows),
        Err(error) if is_collection_missing_error(collection_name, &error) => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

pub(crate) fn is_collection_missing_error(collection_name: &str, error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains(collection_name)
        && (message.contains("collection not found") || message.contains("Cannot query field"))
}

pub(crate) fn graphql_string_list_literal(values: &[String]) -> String {
    shared_graphql_string_list_literal(values)
}

pub(crate) fn graphql_input_literal(value: &Value) -> Result<String> {
    shared_graphql_input_literal(value)
}

pub(crate) async fn post_graphql(graphql: &str, query: &str) -> Result<serde_json::Value> {
    execute_graphql_async(
        graphql,
        query,
        GraphqlRequestOptions {
            timeout: std::time::Duration::from_secs(30),
            max_attempts: 5,
            retry_backoff: std::time::Duration::from_millis(100),
        },
    )
    .await
    .map_err(|error| anyhow::anyhow!("{error}\n{}", graphql_diagnostic_hint(graphql)))
}

pub(crate) fn extract_mutation_doc_id(response: &Value, collection_name: &str) -> Result<String> {
    shared_extract_mutation_doc_id(response, collection_name)
}

pub(crate) fn nullable_string_field(name: &str, value: Option<&str>) -> String {
    shared_nullable_string_field(name, value)
}

pub(crate) fn graphql_bool_literal(value: bool) -> &'static str {
    shared_graphql_bool_literal(value)
}

pub(crate) fn normalize_optional_rfc3339(value: Option<&str>) -> Result<Option<String>> {
    shared_normalize_optional_rfc3339(value)
}

pub(crate) fn optional_i64_field(name: &str, value: Option<i64>) -> Option<String> {
    shared_optional_i64_field(name, value)
}

pub(crate) fn optional_f64_field(name: &str, value: Option<f64>) -> Option<String> {
    shared_optional_f64_field(name, value)
}

pub(crate) fn optional_bool_field(name: &str, value: Option<bool>) -> Option<String> {
    shared_optional_bool_field(name, value)
}

pub(crate) fn optional_string_field(name: &str, value: Option<&str>) -> Option<String> {
    shared_optional_string_field(name, value)
}

pub(crate) fn string_list_field(name: &str, values: &[String]) -> Option<String> {
    shared_string_list_field(name, values)
}

fn is_probably_local_graphql_endpoint(graphql: &str) -> bool {
    let graphql = graphql.trim();
    graphql.contains("127.0.0.1") || graphql.contains("localhost")
}

pub(crate) fn graphql_diagnostic_hint(graphql: &str) -> String {
    if is_probably_local_graphql_endpoint(graphql) {
        "Next:\n  1. If this home is not initialized, run `defra-agent init`\n  2. Start the runtime with `defra-agent server`\n  3. Inspect it with `defra-agent status`".to_string()
    } else {
        format!(
            "Next:\n  1. Verify the GraphQL endpoint {graphql}\n  2. Retry with `--graphql {graphql}` or point the command at the correct runtime"
        )
    }
}

pub(crate) fn first_graphql_row<'a>(
    response: &'a Value,
    collection_name: &str,
) -> Result<&'a Value> {
    shared_first_graphql_row(response, collection_name)
}
