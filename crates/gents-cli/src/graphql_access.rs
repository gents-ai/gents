use anyhow::Result;
use gents_protocol::graphql::{
    extract_mutation_doc_id as shared_extract_mutation_doc_id,
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

pub(crate) fn graphql_api_base(graphql: &str) -> Result<String> {
    graphql
        .trim()
        .strip_suffix("/graphql")
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            anyhow::anyhow!("expected GraphQL endpoint ending in /graphql, got {graphql}")
        })
}

pub(crate) async fn graphql_endpoint_available(graphql: &str) -> bool {
    // Availability must not issue an anonymous GraphQL query: doing so would
    // create a real data-layer read without an ACP actor. The node identity
    // utility endpoint is non-document health metadata and is intentionally
    // readable before authentication so callers can distinguish a live node
    // from an offline one before minting a Host-bound bearer.
    let Ok(api_base) = graphql_api_base(graphql) else {
        return false;
    };
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    client
        .get(format!("{api_base}/node/identity"))
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
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

pub(crate) async fn post_graphql(
    graphql: &gents::AuthenticatedGraphql,
    query: &str,
) -> Result<serde_json::Value> {
    graphql
        .execute(
            query,
            GraphqlRequestOptions {
                timeout: std::time::Duration::from_secs(30),
                max_attempts: 5,
                retry_backoff: std::time::Duration::from_millis(100),
            },
        )
        .await
        .map_err(|error| {
            anyhow::anyhow!("{error}\n{}", graphql_diagnostic_hint(graphql.endpoint()))
        })
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

fn is_probably_local_graphql_endpoint(graphql: &str) -> bool {
    let graphql = graphql.trim();
    graphql.contains("127.0.0.1") || graphql.contains("localhost")
}

pub(crate) fn graphql_diagnostic_hint(graphql: &str) -> String {
    if is_probably_local_graphql_endpoint(graphql) {
        "Next:\n  1. If this home is not initialized, run `gents init`\n  2. Start the runtime with `gents server`\n  3. Inspect it with `gents status`".to_string()
    } else {
        format!(
            "Next:\n  1. Verify the GraphQL endpoint {graphql}\n  2. Retry with `--graphql {graphql}` or point the command at the correct runtime"
        )
    }
}

#[cfg(test)]
pub(crate) async fn authenticated_test_graphql(
    graphql: impl Into<String>,
) -> gents::AuthenticatedGraphql {
    let graphql = graphql.into();
    let key_dir = tempfile::tempdir().expect("create authenticated GraphQL test key directory");
    let identity = std::sync::Arc::new(
        gents::KeyIdentity::load_or_create(key_dir.path().join("identity.key"), None)
            .expect("create authenticated GraphQL test identity"),
    );
    gents::AuthenticatedGraphql::new(graphql, identity)
        .await
        .expect("construct authenticated GraphQL test client")
}

#[cfg(test)]
pub(crate) fn authenticated_test_graphql_sync(
    graphql: impl Into<String>,
) -> gents::AuthenticatedGraphql {
    let graphql = graphql.into();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build authenticated GraphQL test runtime")
            .block_on(authenticated_test_graphql(graphql))
    })
    .join()
    .expect("authenticated GraphQL test client thread")
}
