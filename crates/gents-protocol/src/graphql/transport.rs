use std::time::Duration;

#[cfg(feature = "native")]
use super::{GraphqlError, Result};

/// `timeout` bounds each attempt by the clock; `None` waits for the answer or
/// a connection failure. A store read takes `None`: a total timeout fails a
/// live server that is only slow on a loaded host
/// (`tla/CommitResponseWait.tla`, `ClientWindow`).
#[derive(Debug, Clone, Copy)]
pub struct GraphqlRequestOptions {
    pub timeout: Option<Duration>,
    pub max_attempts: usize,
    pub retry_backoff: Duration,
}

impl Default for GraphqlRequestOptions {
    fn default() -> Self {
        Self {
            timeout: None,
            max_attempts: 5,
            retry_backoff: Duration::from_millis(100),
        }
    }
}

/// How long a store client waits to establish a connection. A request that
/// never connected was never sent, so giving up decides nothing about its
/// outcome.
const STORE_CONNECT_LIMIT: Duration = Duration::from_secs(30);

/// TCP keepalive on store connections: idle time before the first probe, the
/// probe interval and the unanswered probes that end the connection. A live
/// host's kernel answers probes whatever its load, so they end only a
/// connection whose host is gone, which no reset reports
/// (`tla/CommitResponseWait.tla`, `NoKeepalive`).
const STORE_KEEPALIVE_IDLE: Duration = Duration::from_secs(15);
const STORE_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);
const STORE_KEEPALIVE_RETRIES: u32 = 3;

/// The builder of every HTTP client that talks to a DefraDB endpoint: no
/// total request timeout, a connect limit, keepalive probes and the pooled
/// connection idle limit.
#[cfg(feature = "native")]
pub fn store_http_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .connect_timeout(STORE_CONNECT_LIMIT)
        .tcp_keepalive(STORE_KEEPALIVE_IDLE)
        .tcp_keepalive_interval(STORE_KEEPALIVE_INTERVAL)
        .tcp_keepalive_retries(STORE_KEEPALIVE_RETRIES)
        .pool_idle_timeout(POOLED_CONNECTION_IDLE_LIMIT)
}

/// How long a pooled connection to a DefraDB HTTP server may sit idle before
/// a client stops reusing it. The server closes a keep-alive connection that
/// has sent no request for 30 seconds (hyper's HTTP/1 header read timeout,
/// armed by the timer DefraDB's `net::server` sets). A client whose connection
/// task has not yet observed that close would send its next request, a
/// transaction begin or another non-idempotent POST included, into the closed
/// connection and fail with an incomplete message. The pool compares this age
/// when it hands a connection out, so a shorter limit holds whether or not
/// that task has run since the server closed.
pub const POOLED_CONNECTION_IDLE_LIMIT: Duration = Duration::from_secs(20);

#[cfg(feature = "native")]
pub(super) async fn graphql_http_client() -> Result<&'static reqwest::Client> {
    static CLIENT: tokio::sync::OnceCell<reqwest::Client> = tokio::sync::OnceCell::const_new();
    CLIENT
        .get_or_try_init(|| async {
            // Native certificate loading can block; initialize once off the executor.
            tokio::task::spawn_blocking(|| store_http_client_builder().build())
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
    let mut request = client.post(graphql);
    if let Some(timeout) = options.timeout {
        request = request.timeout(timeout);
    }
    match request
        .json(&serde_json::json!({ "query": "{ __typename }" }))
        .send()
        .await
    {
        Ok(response) => {
            REQUEST_ENCODINGS.observe(
                graphql,
                names_gzip(response.headers().get(reqwest::header::ACCEPT_ENCODING)),
            );
            response.status().is_success()
                || matches!(
                    response.status(),
                    reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
                )
        }
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
    execute_graphql_async_with_tx(graphql, authorization, query, None, options, None).await
}

/// [`execute_graphql_async`] with GraphQL variables for the document.
#[cfg(feature = "native")]
pub async fn execute_graphql_with_variables_async(
    graphql: &str,
    authorization: Option<&str>,
    query: &str,
    variables: &serde_json::Value,
    options: GraphqlRequestOptions,
) -> Result<serde_json::Value> {
    ensure_query_document(query)?;
    execute_graphql_async_with_tx(
        graphql,
        authorization,
        query,
        Some(variables),
        options,
        None,
    )
    .await
}

#[cfg(feature = "native")]
pub(super) async fn execute_graphql_async_with_tx(
    graphql: &str,
    authorization: Option<&str>,
    query: &str,
    variables: Option<&serde_json::Value>,
    options: GraphqlRequestOptions,
    txn_id: Option<&str>,
) -> Result<serde_json::Value> {
    let client = graphql_http_client().await?;
    let mut last_error: Option<GraphqlError> = None;

    for attempt in 0..options.max_attempts.max(1) {
        let body = match variables {
            Some(variables) => serde_json::json!({ "query": query, "variables": variables }),
            None => serde_json::json!({ "query": query }),
        };
        let encoded = serde_json::to_vec(&body).unwrap_or_default();
        let mut request = client
            .post(graphql)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT_ENCODING, "gzip");
        request = match REQUEST_ENCODINGS
            .gzip_body(graphql, encoded.len())
            .then(|| gzip(&encoded))
            .flatten()
        {
            Some(compressed) => request
                .header(reqwest::header::CONTENT_ENCODING, "gzip")
                .body(compressed),
            None => request.body(encoded),
        };
        if let Some(timeout) = options.timeout {
            request = request.timeout(timeout);
        }
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
            Ok(response) => {
                REQUEST_ENCODINGS.observe(
                    graphql,
                    names_gzip(response.headers().get(reqwest::header::ACCEPT_ENCODING)),
                );
                response
            }
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
                    timeout_ms = options.timeout.map(|timeout| timeout.as_millis() as u64),
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

        if matches!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            if let Err(source) = response.error_for_status_ref() {
                return Err(GraphqlError::Refused {
                    graphql: graphql.to_owned(),
                    source,
                    code: refusal_code(response).await,
                });
            }
        }
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

        let gzipped = names_gzip(response.headers().get(reqwest::header::CONTENT_ENCODING));
        let value = match response.bytes().await {
            Ok(bytes) => decode_body(graphql, &bytes, gzipped)?,
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

/// The bound on the body of a refusal read for its `code`; a longer body is
/// no edge's answer.
#[cfg(feature = "native")]
const REFUSAL_BODY_LIMIT: usize = 4096;

#[cfg(feature = "native")]
async fn refusal_code(mut response: reqwest::Response) -> Option<String> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        body.extend_from_slice(&chunk);
        if body.len() > REFUSAL_BODY_LIMIT {
            return None;
        }
    }
    serde_json::from_slice::<serde_json::Value>(&body)
        .ok()?
        .get("code")?
        .as_str()
        .map(str::to_owned)
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
///
#[cfg(feature = "native")]
pub fn graphql_transport_error_in_chain(error: &(dyn std::error::Error + 'static)) -> bool {
    graphql_transport_source(error).is_some_and(graphql_transport_error_is_retryable)
}

/// External premise: an HTTP transport failure is a `reqwest::Error`, which
/// callers carry erased in the chain.
#[cfg(feature = "native")]
pub fn graphql_transport_source<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a reqwest::Error> {
    crate::error_chain::find_source::<reqwest::Error>(error)
}

#[cfg(feature = "native")]
fn scale_backoff(base: Duration, attempt: usize) -> Duration {
    let multiplier = attempt.saturating_add(1) as u32;
    base.saturating_mul(multiplier)
}

/// Request bodies larger than this many bytes travel gzipped to an endpoint
/// that accepts them (Lean `RequestEncoding.threshold`).
#[cfg(feature = "native")]
const GZIP_REQUEST_BODIES_OVER: usize = 8 * 1024;

/// What this client knows of each GraphQL endpoint: whether its latest answer
/// said it accepts gzip request bodies (`Accept-Encoding: gzip` on a response,
/// RFC 7694). An endpoint not yet heard from accepts nothing, so a
/// host that never says so, an older Gents host or a plain DefraDB endpoint,
/// never receives a gzipped body (Lean `RequestEncoding`).
#[cfg(feature = "native")]
#[derive(Debug, Default)]
pub(crate) struct RequestEncodings {
    accepting: std::sync::Mutex<rapidhash::RapidHashSet<String>>,
}

#[cfg(feature = "native")]
impl RequestEncodings {
    /// Records what an answer from `endpoint` said (Lean `remember`).
    pub(crate) fn observe(&self, endpoint: &str, accepts_gzip: bool) {
        let mut accepting = self
            .accepting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if accepts_gzip {
            if !accepting.contains(endpoint) {
                accepting.insert(endpoint.to_owned());
            }
        } else {
            accepting.remove(endpoint);
        }
    }

    /// Whether a body of `length` bytes goes to `endpoint` gzipped (Lean
    /// `gzipBody`).
    pub(crate) fn gzip_body(&self, endpoint: &str, length: usize) -> bool {
        length > GZIP_REQUEST_BODIES_OVER
            && self
                .accepting
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(endpoint)
    }
}

#[cfg(feature = "native")]
static REQUEST_ENCODINGS: std::sync::LazyLock<RequestEncodings> =
    std::sync::LazyLock::new(RequestEncodings::default);

/// Whether a `Content-Encoding` or `Accept-Encoding` value names gzip.
#[cfg(feature = "native")]
fn names_gzip(value: Option<&reqwest::header::HeaderValue>) -> bool {
    value
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.split(',').any(|coding| {
                coding
                    .split(';')
                    .next()
                    .is_some_and(|name| name.trim().eq_ignore_ascii_case("gzip"))
            })
        })
}

#[cfg(feature = "native")]
fn gzip(bytes: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write as _;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).ok()?;
    encoder.finish().ok()
}

/// The JSON of a response body, inflated first when the host gzipped it.
#[cfg(feature = "native")]
fn decode_body(graphql: &str, bytes: &[u8], gzipped: bool) -> Result<serde_json::Value> {
    let failed = |source: std::io::Error| GraphqlError::DecodeContent {
        graphql: graphql.to_owned(),
        source,
    };
    if !gzipped {
        return serde_json::from_slice(bytes).map_err(|error| failed(error.into()));
    }
    let mut inflated = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(bytes), &mut inflated)
        .map_err(failed)?;
    serde_json::from_slice(&inflated).map_err(|error| failed(error.into()))
}

#[cfg(all(test, feature = "native"))]
mod request_encoding_tests {
    use super::*;

    /// The client gzips exactly as the Lean model does
    /// (`RequestEncoding.Contracts.gzip`).
    #[test]
    fn request_encoding_matches_the_lean_model() {
        use gents_lean_vocab::families::request_encoding_contracts_gzip as family;
        family::replay(|input: &family::Input| {
            let encodings = RequestEncodings::default();
            for &accepts in &input.answers {
                encodings.observe("http://host/api/v0/graphql", accepts);
            }
            encodings.gzip_body("http://host/api/v0/graphql", input.length as usize)
        });
    }

    #[test]
    fn what_one_endpoint_said_does_not_reach_another() {
        let encodings = RequestEncodings::default();
        encodings.observe("http://new/api/v0/graphql", true);
        assert!(encodings.gzip_body("http://new/api/v0/graphql", 62_000));
        assert!(!encodings.gzip_body("http://old/api/v0/graphql", 62_000));
    }

    #[test]
    fn a_gzipped_answer_is_inflated() {
        let body = br#"{"data":{"Agent":[]}}"#;
        let value = decode_body("graphql", &gzip(body).unwrap(), true).unwrap();
        assert_eq!(value, serde_json::json!({"data": {"Agent": []}}));
        assert!(decode_body("graphql", body, true).is_err());
        assert_eq!(decode_body("graphql", body, false).unwrap(), value);
    }
}
