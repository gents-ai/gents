use anyhow::Result;
use gents_protocol::graphql::{
    extract_mutation_doc_id as shared_extract_mutation_doc_id,
    graphql_endpoint_available as shared_graphql_endpoint_available,
    graphql_input_literal as shared_graphql_input_literal, graphql_rows_from_response,
    graphql_string_list_literal as shared_graphql_string_list_literal,
    optional_bool_field as shared_optional_bool_field,
    optional_f64_field as shared_optional_f64_field,
    optional_i64_field as shared_optional_i64_field,
    optional_i64_list_field as shared_optional_i64_list_field,
    optional_string_field as shared_optional_string_field, GraphqlRequestOptions,
};
use serde_json::Value;

use crate::config_writes::ConfigAccess;

pub(crate) use gents::config_client::{graphql_api_base, graphql_diagnostic_hint, post_graphql};

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
    gents_protocol::graphql::is_collection_missing_error_message(
        collection_name,
        &error.to_string(),
    )
}

pub(crate) fn graphql_string_list_literal(values: &[String]) -> String {
    shared_graphql_string_list_literal(values)
}

pub(crate) fn graphql_input_literal(value: &Value) -> Result<String> {
    shared_graphql_input_literal(value)
}

pub(crate) fn extract_mutation_doc_id(response: &Value, collection_name: &str) -> Result<String> {
    shared_extract_mutation_doc_id(response, collection_name)
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

pub(crate) fn optional_i64_list_field(name: &str, value: Option<&[i64]>) -> Option<String> {
    shared_optional_i64_list_field(name, value)
}

pub(crate) fn optional_string_field(name: &str, value: Option<&str>) -> Option<String> {
    shared_optional_string_field(name, value)
}
