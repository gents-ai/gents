use anyhow::Result;
use gents_protocol::graphql::graphql_rows_from_response;
use serde_json::Value;

use crate::config_writes::ConfigAccess;

pub(crate) use gents_store::config_client::{graphql_api_base, post_graphql};

/// Availability probing is a single bounded request so a stale endpoint does
/// not indefinitely delay the caller's choice of access path.
pub(crate) async fn graphql_endpoint_available(graphql: &str) -> bool {
    gents_service::repair::serving_status(graphql)
        .await
        .is_some()
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
