use std::time::Duration;

#[cfg(feature = "native")]
use super::{GraphqlError, Result};

#[derive(Debug, Clone, Copy)]
pub struct GraphqlRequestOptions {
    pub timeout: Duration,
    pub max_attempts: usize,
    pub retry_backoff: Duration,
}

impl Default for GraphqlRequestOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_attempts: 5,
            retry_backoff: Duration::from_millis(100),
        }
    }
}

#[cfg(feature = "native")]
pub(super) async fn graphql_http_client() -> Result<&'static reqwest::Client> {
    static CLIENT: tokio::sync::OnceCell<reqwest::Client> = tokio::sync::OnceCell::const_new();
    CLIENT
        .get_or_try_init(|| async {
            // Native certificate loading can block; initialize once off the executor.
            tokio::task::spawn_blocking(|| reqwest::Client::builder().build())
                .await?
                .map_err(GraphqlError::ClientBuild)
        })
        .await
}

#[cfg(feature = "native")]
pub async fn graphql_endpoint_available(graphql: &str, options: GraphqlRequestOptions) -> bool {
    let client = match graphql_http_client().await {
        Ok(client) => client,
        Err(_) => return false,
    };
    match client
        .post(graphql)
        .timeout(options.timeout)
        .json(&serde_json::json!({ "query": "{ __typename }" }))
        .send()
        .await
    {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    }
}

/// Execute a GraphQL read. Mutation documents are rejected before network I/O.
/// `authorization` is sent verbatim as the `Authorization` header.
#[cfg(feature = "native")]
pub async fn execute_graphql_async(
    graphql: &str,
    authorization: Option<&str>,
    query: &str,
    options: GraphqlRequestOptions,
) -> Result<serde_json::Value> {
    ensure_query_document(query)?;
    execute_graphql_async_with_tx(graphql, authorization, query, options, None).await
}

#[cfg(feature = "native")]
pub(super) async fn execute_graphql_async_with_tx(
    graphql: &str,
    authorization: Option<&str>,
    query: &str,
    options: GraphqlRequestOptions,
    txn_id: Option<&str>,
) -> Result<serde_json::Value> {
    let client = graphql_http_client().await?;
    let mut last_error: Option<GraphqlError> = None;

    for attempt in 0..options.max_attempts.max(1) {
        let mut request = client
            .post(graphql)
            .timeout(options.timeout)
            .json(&serde_json::json!({ "query": query }));
        if let Some(id) = txn_id {
            request = request.header("x-defradb-tx", id);
        }
        if let Some(authorization) = authorization {
            let mut value = reqwest::header::HeaderValue::from_str(authorization)
                .map_err(GraphqlError::AuthorizationHeader)?;
            value.set_sensitive(true);
            request = request.header(reqwest::header::AUTHORIZATION, value);
        }
        let response = request.send().await;
        let response = match response {
            Ok(response) => response,
            Err(error)
                if graphql_transport_error_is_retryable(&error)
                    && attempt + 1 < options.max_attempts =>
            {
                tracing::warn!(
                    attempt,
                    graphql,
                    error = %error,
                    cause = ?error,
                    is_timeout = error.is_timeout(),
                    is_connect = error.is_connect(),
                    timeout_ms = options.timeout.as_millis() as u64,
                    "retrying async GraphQL request after transport error"
                );
                last_error = Some(GraphqlError::Post {
                    graphql: graphql.to_owned(),
                    source: error,
                });
                tokio::time::sleep(scale_backoff(options.retry_backoff, attempt)).await;
                continue;
            }
            Err(error) => {
                return Err(GraphqlError::Post {
                    graphql: graphql.to_owned(),
                    source: error,
                });
            }
        };

        let response = match response.error_for_status() {
            Ok(response) => response,
            Err(error)
                if graphql_transport_error_is_retryable(&error)
                    && attempt + 1 < options.max_attempts =>
            {
                tracing::warn!(
                    attempt,
                    graphql,
                    error = %error,
                    "retrying async GraphQL request after response status error"
                );
                last_error = Some(GraphqlError::ReadResponse {
                    graphql: graphql.to_owned(),
                    source: error,
                });
                tokio::time::sleep(scale_backoff(options.retry_backoff, attempt)).await;
                continue;
            }
            Err(error) => {
                return Err(GraphqlError::ReadResponse {
                    graphql: graphql.to_owned(),
                    source: error,
                });
            }
        };

        let value = match response.json().await {
            Ok(value) => value,
            Err(error)
                if graphql_transport_error_is_retryable(&error)
                    && attempt + 1 < options.max_attempts =>
            {
                tracing::warn!(
                    attempt,
                    graphql,
                    error = %error,
                    "retrying async GraphQL request after decode error"
                );
                last_error = Some(GraphqlError::DecodeBody {
                    graphql: graphql.to_owned(),
                    source: error,
                });
                tokio::time::sleep(scale_backoff(options.retry_backoff, attempt)).await;
                continue;
            }
            Err(error) => {
                return Err(GraphqlError::DecodeBody {
                    graphql: graphql.to_owned(),
                    source: error,
                });
            }
        };

        // DefraDB's HTTP handler already retries auto-commit conflicts up to
        // its `max_txn_retries`; a conflict reaching here exhausted that loop.
        return finish_graphql_response(graphql, value);
    }

    Err(
        last_error.unwrap_or_else(|| GraphqlError::RetriesExhausted {
            graphql: graphql.to_owned(),
        }),
    )
}

#[cfg(feature = "native")]
fn ensure_query_document(document: &str) -> Result<()> {
    let document = document.trim_start();
    if document.starts_with('{') || document.starts_with("query") {
        Ok(())
    } else {
        Err(GraphqlError::QueryDocumentRequired)
    }
}

#[cfg(feature = "native")]
fn finish_graphql_response(graphql: &str, value: serde_json::Value) -> Result<serde_json::Value> {
    let errors = value
        .get("errors")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !errors.is_empty() {
        return Err(crate::database_error::GraphqlFailure::new(
            format!("from {graphql}"),
            serde_json::Value::Array(errors),
        )
        .into());
    }
    Ok(value)
}

/// External premise: reqwest classifies a connection the peer drops before
/// the response head as a request failure and one dropped mid-body as a body
/// or decode failure; its message carries only that kind and the URL.
#[cfg(feature = "native")]
fn graphql_transport_error_is_retryable(error: &reqwest::Error) -> bool {
    error.is_timeout()
        || error.is_connect()
        || error.is_request()
        || error.is_body()
        || error.is_decode()
}

/// Whether an error in its `source()` chain is retryable under the GraphQL transport policy.
#[cfg(feature = "native")]
pub fn graphql_error_is_retryable(error: &(dyn std::error::Error + 'static)) -> bool {
    if crate::database_error::is_storage_full(error) {
        return false;
    }
    graphql_transport_error_in_chain(error)
        || crate::error_chain::find_source::<GraphqlError>(error)
            .and_then(GraphqlError::database_failure)
            .or_else(|| {
                crate::error_chain::find_source::<crate::database_error::GraphqlFailure>(error)
            })
            .is_some_and(crate::database_error::GraphqlFailure::is_retryable_conflict)
}

/// Whether an error's `source()` chain wraps a retryable GraphQL transport failure.
///
/// Auto-commit reads must use this rather than [`graphql_error_is_retryable`]:
/// DefraDB's HTTP handler already retried their store conflicts.
#[cfg(feature = "native")]
pub fn graphql_transport_error_in_chain(error: &(dyn std::error::Error + 'static)) -> bool {
    crate::error_chain::find_source::<reqwest::Error>(error)
        .is_some_and(graphql_transport_error_is_retryable)
}

#[cfg(feature = "native")]
fn scale_backoff(base: Duration, attempt: usize) -> Duration {
    let multiplier = attempt.saturating_add(1) as u32;
    base.saturating_mul(multiplier)
}
