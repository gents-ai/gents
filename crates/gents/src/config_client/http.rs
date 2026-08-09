use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use gents_protocol::graphql::{execute_graphql_async_authenticated_with_tx, GraphqlRequestOptions};
use reqwest::{Method, Response, Url};
use serde_json::Value;

use crate::identity::AgentIdentity;

/// Lifetime for command-scoped DefraDB HTTP identity tokens.
///
/// Tokens remain memory-only and are refreshed before expiry. The refresh
/// window keeps dashboard polling from repeatedly invoking Keychain, Secure
/// Enclave, or remote signers.
const BEARER_TOKEN_LIFETIME: Duration = Duration::from_secs(10 * 60);
const BEARER_TOKEN_REFRESH_AFTER: Duration = Duration::from_secs(8 * 60);
const BEARER_MINT_TIMEOUT: Duration = Duration::from_secs(30);

/// Central authenticated DefraDB HTTP boundary.
///
/// The bearer is intentionally private and its Debug implementation is
/// redacted. All GraphQL and transaction REST calls must flow through this
/// type so a retry, commit, or discard cannot accidentally become anonymous.
#[derive(Clone)]
pub struct AuthenticatedGraphql {
    endpoint: String,
    origin: Arc<str>,
    audience: Arc<str>,
    authenticated_did: Arc<str>,
    credential: GraphqlCredential,
    client: reqwest::Client,
}

#[derive(Clone)]
enum GraphqlCredential {
    Local {
        identity: Arc<dyn AgentIdentity>,
        cached: Arc<tokio::sync::Mutex<CachedBearer>>,
    },
    Static(Arc<str>),
}

struct CachedBearer {
    token: String,
    refresh_at: Instant,
}

impl fmt::Debug for AuthenticatedGraphql {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedGraphql")
            .field("endpoint", &self.endpoint)
            .field("credential", &"<redacted>")
            .finish()
    }
}

impl AuthenticatedGraphql {
    pub async fn new(
        endpoint: impl Into<String>,
        identity: Arc<dyn AgentIdentity>,
    ) -> Result<Self> {
        let endpoint = endpoint.into();
        let authenticated_did: Arc<str> = Arc::from(identity.did());
        let audience = defradb_http_audience(&endpoint)?;
        let token = tokio::time::timeout(
            BEARER_MINT_TIMEOUT,
            identity.defradb_bearer_token(&audience, BEARER_TOKEN_LIFETIME),
        )
        .await
        .with_context(|| {
            format!(
                "timed out minting DefraDB HTTP identity for {} as {}",
                audience,
                identity.did()
            )
        })?
        .with_context(|| {
            format!(
                "minting DefraDB HTTP identity for {} as {}",
                audience,
                identity.did()
            )
        })?;
        verify_token_for_identity(&token, &audience, identity.did())?;
        let endpoint_url = Url::parse(&endpoint)
            .with_context(|| format!("parsing DefraDB GraphQL endpoint {endpoint:?}"))?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building authenticated DefraDB HTTP client")?;
        Ok(Self {
            endpoint,
            origin: Arc::from(endpoint_url.origin().ascii_serialization()),
            audience: Arc::from(audience),
            authenticated_did,
            credential: GraphqlCredential::Local {
                identity,
                cached: Arc::new(tokio::sync::Mutex::new(CachedBearer {
                    token,
                    refresh_at: Instant::now() + BEARER_TOKEN_REFRESH_AFTER,
                })),
            },
            client,
        })
    }

    /// Construct from an already minted token after locally verifying the
    /// signature, lifetime, and exact endpoint audience. This supports clients
    /// that receive a token from an external identity provider without opening
    /// an unauthenticated escape hatch.
    pub fn from_verified_token(
        endpoint: impl Into<String>,
        token: String,
        expected_did: &str,
    ) -> Result<Self> {
        let endpoint = endpoint.into();
        let endpoint_url = Url::parse(&endpoint)
            .with_context(|| format!("parsing DefraDB GraphQL endpoint {endpoint:?}"))?;
        let origin = endpoint_url.origin().ascii_serialization();
        let audience = defradb_http_audience(&endpoint)?;
        verify_token_for_identity(&token, &audience, expected_did)?;
        verify_bounded_token_lifetime(&token)?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building authenticated DefraDB HTTP client")?;
        Ok(Self {
            endpoint,
            origin: Arc::from(origin),
            audience: Arc::from(audience),
            authenticated_did: Arc::from(expected_did),
            credential: GraphqlCredential::Static(Arc::from(token)),
            client,
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn authenticated_did(&self) -> &str {
        &self.authenticated_did
    }

    pub async fn execute(&self, query: &str, options: GraphqlRequestOptions) -> Result<Value> {
        let bearer_token = self.bearer_token().await?;
        self.execute_graphql_fail_closed(query, options, &bearer_token, None)
            .await
    }

    pub async fn execute_with_tx(
        &self,
        query: &str,
        options: GraphqlRequestOptions,
        txn_id: &str,
    ) -> Result<Value> {
        let bearer_token = self.bearer_token().await?;
        self.execute_graphql_fail_closed(query, options, &bearer_token, Some(txn_id))
            .await
    }

    pub async fn get(&self, url: impl reqwest::IntoUrl) -> Result<Response> {
        self.request(Method::GET, url).await
    }

    pub async fn post(&self, url: impl reqwest::IntoUrl) -> Result<Response> {
        self.request(Method::POST, url).await
    }

    pub async fn delete(&self, url: impl reqwest::IntoUrl) -> Result<Response> {
        self.request(Method::DELETE, url).await
    }

    pub async fn post_text(&self, url: impl reqwest::IntoUrl, body: String) -> Result<Response> {
        let url = self.checked_url(url)?;
        let bearer_token = self.bearer_token().await?;
        self.client
            .post(url)
            .bearer_auth(bearer_token)
            .header(reqwest::header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(body)
            .send()
            .await
            .context("sending authenticated DefraDB HTTP text request")
    }

    pub async fn patch_json(&self, url: impl reqwest::IntoUrl, body: &Value) -> Result<Response> {
        let url = self.checked_url(url)?;
        let bearer_token = self.bearer_token().await?;
        self.client
            .patch(url)
            .bearer_auth(bearer_token)
            .json(body)
            .send()
            .await
            .context("sending authenticated DefraDB HTTP JSON patch")
    }

    pub async fn post_json(&self, url: impl reqwest::IntoUrl, body: &Value) -> Result<Response> {
        let url = self.checked_url(url)?;
        let bearer_token = self.bearer_token().await?;
        self.client
            .post(url)
            .bearer_auth(bearer_token)
            .json(body)
            .send()
            .await
            .context("sending authenticated DefraDB HTTP JSON request")
    }

    async fn request(&self, method: Method, url: impl reqwest::IntoUrl) -> Result<Response> {
        let url = self.checked_url(url)?;
        let bearer_token = self.bearer_token().await?;
        self.client
            .request(method, url)
            .bearer_auth(bearer_token)
            .send()
            .await
            .context("sending authenticated DefraDB HTTP request")
    }

    fn checked_url(&self, url: impl reqwest::IntoUrl) -> Result<Url> {
        let url = url
            .into_url()
            .map_err(anyhow::Error::from)
            .context("parsing authenticated DefraDB request URL")?;
        let origin = url.origin().ascii_serialization();
        if origin != self.origin.as_ref() {
            anyhow::bail!(
                "refusing to send DefraDB identity bearer cross-origin: endpoint origin {}, target origin {origin}",
                self.origin
            );
        }
        Ok(url)
    }

    async fn bearer_token(&self) -> Result<String> {
        match &self.credential {
            GraphqlCredential::Local { identity, cached } => {
                let mut cached = cached.lock().await;
                if Instant::now() < cached.refresh_at {
                    return Ok(cached.token.clone());
                }

                let token = tokio::time::timeout(
                    BEARER_MINT_TIMEOUT,
                    identity.defradb_bearer_token(&self.audience, BEARER_TOKEN_LIFETIME),
                )
                .await
                .with_context(|| {
                    format!(
                        "timed out refreshing DefraDB HTTP identity for {} as {}",
                        self.audience,
                        identity.did()
                    )
                })?
                .with_context(|| {
                    format!(
                        "refreshing DefraDB HTTP identity for {} as {}",
                        self.audience,
                        identity.did()
                    )
                })?;
                verify_token_for_identity(&token, &self.audience, identity.did())?;
                *cached = CachedBearer {
                    token: token.clone(),
                    refresh_at: Instant::now() + BEARER_TOKEN_REFRESH_AFTER,
                };
                Ok(token)
            }
            GraphqlCredential::Static(token) => {
                verify_token_for_audience(token, &self.audience)?;
                Ok(token.to_string())
            }
        }
    }

    async fn execute_graphql_fail_closed(
        &self,
        document: &str,
        configured: GraphqlRequestOptions,
        bearer_token: &str,
        txn_id: Option<&str>,
    ) -> Result<Value> {
        if graphql_document_definitely_read_only(document) {
            return execute_graphql_async_authenticated_with_tx(
                &self.endpoint,
                document,
                configured,
                bearer_token,
                txn_id,
            )
            .await;
        }

        let transport_options = fail_closed_graphql_request_options(configured, document);
        for attempt in 0..configured.max_attempts.max(1) {
            match execute_graphql_async_authenticated_with_tx(
                &self.endpoint,
                document,
                transport_options,
                bearer_token,
                txn_id,
            )
            .await
            {
                Err(error)
                    if txn_id.is_none()
                        && explicit_graphql_conflict(&error)
                        && attempt + 1 < configured.max_attempts =>
                {
                    tracing::warn!(
                        attempt,
                        endpoint = %self.endpoint,
                        error = %error,
                        "retrying authenticated GraphQL mutation after explicit conflict"
                    );
                    tokio::time::sleep(
                        configured
                            .retry_backoff
                            .saturating_mul(attempt.saturating_add(1) as u32),
                    )
                    .await;
                }
                result => return result,
            }
        }
        unreachable!("bounded authenticated GraphQL retry loop returns")
    }
}

/// Restrict transparent HTTP retries to documents that are definitely reads.
///
/// Once a mutation reaches DefraDB, a timeout, retryable status, or malformed
/// response is commit-ambiguous: replaying it can mint a second physical
/// document. The boundary may issue a fresh attempt only after DefraDB returns
/// a parsed, explicit conflict response proving that attempt did not apply,
/// and never within the same still-open transaction.
/// Unknown GraphQL document shapes fail closed to one transport attempt as
/// well.
fn fail_closed_graphql_request_options(
    configured: GraphqlRequestOptions,
    document: &str,
) -> GraphqlRequestOptions {
    if graphql_document_definitely_read_only(document) {
        configured
    } else {
        GraphqlRequestOptions {
            max_attempts: 1,
            retry_backoff: Duration::ZERO,
            ..configured
        }
    }
}

fn graphql_document_definitely_read_only(document: &str) -> bool {
    let significant = graphql_significant_prefix(document);
    significant.starts_with('{') || graphql_operation_keyword(significant) == Some("query")
}

fn explicit_graphql_conflict(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.starts_with("graphql returned errors from ")
        && crate::retry::is_defradb_transaction_conflict_text(&message)
}

/// Return the first GraphQL operation keyword after ignored tokens. This is a
/// deliberately conservative classifier, not a full parser: comments, commas,
/// a UTF-8 BOM, and compact `mutation{...}` syntax are recognized, while a
/// fragment-first or otherwise unknown document is treated as a possible write.
fn graphql_operation_keyword(document: &str) -> Option<&str> {
    let remaining = graphql_significant_prefix(document);
    let keyword_len = remaining
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_alphanumeric() || *character == '_')
        .map(|(index, character)| index + character.len_utf8())
        .last()?;
    Some(&remaining[..keyword_len])
}

fn graphql_significant_prefix(document: &str) -> &str {
    let mut remaining = document;
    loop {
        remaining = remaining.trim_start_matches(|character: char| {
            character.is_whitespace() || character == ',' || character == '\u{feff}'
        });
        if let Some(comment) = remaining.strip_prefix('#') {
            remaining = comment
                .find(['\n', '\r'])
                .map(|end| &comment[end + 1..])
                .unwrap_or_default();
            continue;
        }
        break;
    }
    remaining
}

fn verify_token_for_audience(token: &str, audience: &str) -> Result<()> {
    let token_identity = identity::from_token(token.as_bytes())
        .map_err(anyhow::Error::from)
        .context("parsing DefraDB HTTP bearer token")?;
    identity::verify_auth_token(&token_identity, audience)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("verifying DefraDB HTTP bearer audience {audience}"))
}

fn verify_token_for_identity(token: &str, audience: &str, expected_did: &str) -> Result<()> {
    use identity::Identity as _;

    let token_identity = identity::from_token(token.as_bytes())
        .map_err(anyhow::Error::from)
        .context("parsing DefraDB HTTP bearer token")?;
    identity::verify_auth_token(&token_identity, audience)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("verifying DefraDB HTTP bearer audience {audience}"))?;
    let token_did = token_identity
        .did()
        .map_err(anyhow::Error::from)
        .context("deriving DefraDB HTTP bearer issuer DID")?;
    if token_did.to_string() != expected_did {
        anyhow::bail!(
            "DefraDB HTTP bearer issuer mismatch: identity reports {expected_did}, token is signed by {token_did}"
        );
    }
    Ok(())
}

fn verify_bounded_token_lifetime(token: &str) -> Result<()> {
    let token_identity = identity::from_token(token.as_bytes())
        .map_err(anyhow::Error::from)
        .context("parsing DefraDB HTTP bearer token")?;
    let claims = token_identity.claims();
    let lifetime = claims.exp.checked_sub(claims.iat).ok_or_else(|| {
        anyhow::anyhow!(
            "DefraDB HTTP bearer expires before it was issued: iat={}, exp={}",
            claims.iat,
            claims.exp
        )
    })?;
    if lifetime > BEARER_TOKEN_LIFETIME.as_secs() {
        anyhow::bail!(
            "DefraDB HTTP bearer lifetime {lifetime}s exceeds the maximum {}s",
            BEARER_TOKEN_LIFETIME.as_secs()
        );
    }
    Ok(())
}

/// Return the exact audience DefraDB derives from the HTTP `Host` header.
pub fn defradb_http_audience(endpoint: &str) -> Result<String> {
    let url = Url::parse(endpoint)
        .with_context(|| format!("parsing DefraDB GraphQL endpoint {endpoint:?}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("DefraDB GraphQL endpoint must use http or https: {endpoint}");
    }
    if url.host_str().is_none() {
        anyhow::bail!("DefraDB GraphQL endpoint has no host: {endpoint}");
    }
    if url.scheme() == "http" && !is_loopback_host(url.host_str().expect("host checked above")) {
        anyhow::bail!(
            "refusing to send DefraDB identity bearer over plaintext HTTP to non-loopback endpoint: {endpoint}"
        );
    }

    // Origin serialization matches reqwest's Host behavior: default ports are
    // omitted, explicit non-default ports are retained, IPv6 remains bracketed,
    // and DNS names are normalized/lowercased.
    let origin = url.origin().ascii_serialization();
    origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("DefraDB endpoint has an opaque HTTP origin: {endpoint}"))
}

fn is_loopback_host(host: &str) -> bool {
    let ip_literal = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || ip_literal
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use axum::extract::State;
    use axum::http::{header, HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde_json::json;

    use crate::identity::{AgentIdentity, KeyIdentity};

    use super::*;

    #[test]
    fn authenticated_transport_classifies_mutations_fail_closed() {
        let configured = GraphqlRequestOptions {
            timeout: Duration::from_secs(17),
            max_attempts: 5,
            retry_backoff: Duration::from_millis(100),
        };
        for document in [
            "mutation { create_Widget(input: {}) { _docID } }",
            "mutation{create_Widget(input:{}){_docID}}",
            "# generated request\nmutation NamedMutation { create_Widget(input: {}) { _docID } }",
            "\u{feff}, # first comment\r\n # second comment\n mutation{create_Widget(input:{}){_docID}}",
            "fragment WidgetFields on Widget { _docID } mutation Named { create_Widget(input: {}) { ...WidgetFields } }",
        ] {
            let options = fail_closed_graphql_request_options(configured, document);
            assert_eq!(options.timeout, configured.timeout, "{document:?}");
            assert_eq!(options.max_attempts, 1, "{document:?}");
            assert_eq!(options.retry_backoff, Duration::ZERO, "{document:?}");
        }

        for document in [
            "{ Widget { _docID } }",
            "query NamedQuery { Widget { _docID } }",
            "# mutation in a comment\n\u{feff}, query{Widget{_docID}}",
        ] {
            let options = fail_closed_graphql_request_options(configured, document);
            assert_eq!(
                options.max_attempts, configured.max_attempts,
                "{document:?}"
            );
            assert_eq!(
                options.retry_backoff, configured.retry_backoff,
                "{document:?}"
            );
        }
    }

    async fn count_invalid_json(
        State(attempts): State<Arc<AtomicUsize>>,
    ) -> ([(header::HeaderName, &'static str); 1], &'static str) {
        attempts.fetch_add(1, Ordering::SeqCst);
        ([(header::CONTENT_TYPE, "application/json")], "{")
    }

    #[tokio::test]
    async fn authenticated_transport_never_retries_ambiguous_mutations() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/api/v0/graphql", post(count_invalid_json))
            .with_state(attempts.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let key_dir = tempfile::tempdir().unwrap();
        let identity = Arc::new(
            KeyIdentity::load_or_create(key_dir.path().join("identity.key"), None).unwrap(),
        );
        let access =
            AuthenticatedGraphql::new(format!("http://{address}/api/v0/graphql"), identity)
                .await
                .unwrap();
        let configured = GraphqlRequestOptions {
            timeout: Duration::from_secs(5),
            max_attempts: 3,
            retry_backoff: Duration::ZERO,
        };

        for document in [
            "mutation{create_Widget(input:{}){_docID}}",
            "# generated\nmutation Named { update_Widget(input:{}){_docID}}",
            "\u{feff}, # comment\r\nmutation{delete_Widget(filter:{}){_docID}}",
        ] {
            attempts.store(0, Ordering::SeqCst);
            access.execute(document, configured).await.unwrap_err();
            assert_eq!(attempts.load(Ordering::SeqCst), 1, "{document:?}");
        }

        attempts.store(0, Ordering::SeqCst);
        access
            .execute_with_tx("mutation{create_Widget(input:{}){_docID}}", configured, "7")
            .await
            .unwrap_err();
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        attempts.store(0, Ordering::SeqCst);
        access
            .execute("query{Widget{_docID}}", configured)
            .await
            .unwrap_err();
        assert_eq!(attempts.load(Ordering::SeqCst), configured.max_attempts);
    }

    #[derive(Clone)]
    struct MutationErrorState {
        attempts: Arc<AtomicUsize>,
        first_error_message: Arc<str>,
    }

    async fn mutation_error_once_then_succeed(
        State(state): State<MutationErrorState>,
    ) -> Json<Value> {
        let attempt = state.attempts.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 {
            Json(json!({
                "errors": [{"message": state.first_error_message.as_ref()}]
            }))
        } else {
            Json(json!({"data": {"create_Widget": {"_docID": "widget-doc"}}}))
        }
    }

    async fn probe_authenticated_mutation_error(
        first_error_message: &str,
        txn_id: Option<&str>,
    ) -> (Result<Value>, usize) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/api/v0/graphql", post(mutation_error_once_then_succeed))
            .with_state(MutationErrorState {
                attempts: attempts.clone(),
                first_error_message: Arc::from(first_error_message),
            });
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let key_dir = tempfile::tempdir().unwrap();
        let identity = Arc::new(
            KeyIdentity::load_or_create(key_dir.path().join("identity.key"), None).unwrap(),
        );
        let access =
            AuthenticatedGraphql::new(format!("http://{address}/api/v0/graphql"), identity)
                .await
                .unwrap();
        let document = "mutation{create_Widget(input:{}){_docID}}";
        let options = GraphqlRequestOptions {
            timeout: Duration::from_secs(5),
            max_attempts: 3,
            retry_backoff: Duration::ZERO,
        };
        let result = match txn_id {
            Some(txn_id) => access.execute_with_tx(document, options, txn_id).await,
            None => access.execute(document, options).await,
        };

        (result, attempts.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn authenticated_transport_preserves_explicit_mutation_conflict_retries() {
        let (response, attempts) =
            probe_authenticated_mutation_error("DefraDB transaction conflict; please retry", None)
                .await;
        let response = response.unwrap();

        assert_eq!(attempts, 2);
        assert_eq!(
            response.pointer("/data/create_Widget/_docID"),
            Some(&Value::String("widget-doc".to_string()))
        );
    }

    #[tokio::test]
    async fn authenticated_transport_does_not_retry_conflict_on_open_transaction() {
        let message = "DefraDB transaction conflict; please retry";
        let (result, attempts) = probe_authenticated_mutation_error(message, Some("7")).await;
        let error = result.unwrap_err();

        assert_eq!(attempts, 1);
        assert!(error.to_string().contains(message), "{error:#}");
    }

    #[tokio::test]
    async fn authenticated_transport_does_not_retry_advisory_mutation_errors() {
        for message in ["please retry", "database is locked"] {
            let (result, attempts) = probe_authenticated_mutation_error(message, None).await;
            let error = result.unwrap_err();

            assert_eq!(attempts, 1, "{message:?}");
            assert!(error.to_string().contains(message), "{error:#}");
        }
    }

    #[test]
    fn audience_matches_http_host_rules() {
        assert_eq!(
            defradb_http_audience("http://LOCALHOST:9191/api/v0/graphql").unwrap(),
            "localhost:9191"
        );
        assert_eq!(
            defradb_http_audience("https://EXAMPLE.com:443/api/v0/graphql").unwrap(),
            "example.com"
        );
        assert_eq!(
            defradb_http_audience("http://[::1]:8080/api/v0/graphql").unwrap(),
            "[::1]:8080"
        );
    }

    #[test]
    fn bearer_transport_requires_tls_off_loopback() {
        assert!(defradb_http_audience("http://localhost:9191/api/v0/graphql").is_ok());
        assert!(defradb_http_audience("http://127.0.0.1:9191/api/v0/graphql").is_ok());
        assert!(defradb_http_audience("http://[::1]:9191/api/v0/graphql").is_ok());
        assert!(defradb_http_audience("https://defra.internal/api/v0/graphql").is_ok());

        let error = defradb_http_audience("http://defra.internal:9191/api/v0/graphql").unwrap_err();
        assert!(error.to_string().contains("plaintext HTTP"), "{error:#}");
    }

    #[derive(Clone)]
    struct CaptureState {
        audience: String,
        authenticated_requests: Arc<AtomicUsize>,
    }

    async fn capture_auth(
        State(state): State<CaptureState>,
        headers: HeaderMap,
    ) -> (StatusCode, Json<Value>) {
        let Some(token) = headers
            .get(reqwest::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
        else {
            return (StatusCode::UNAUTHORIZED, Json(json!({"error": "missing"})));
        };
        let Ok(identity) = identity::from_token(token.as_bytes()) else {
            return (StatusCode::FORBIDDEN, Json(json!({"error": "invalid"})));
        };
        if identity::verify_auth_token(&identity, &state.audience).is_err() {
            return (StatusCode::FORBIDDEN, Json(json!({"error": "audience"})));
        }
        state.authenticated_requests.fetch_add(1, Ordering::SeqCst);
        (StatusCode::OK, Json(json!({"data": {"ok": true}, "id": 7})))
    }

    #[tokio::test]
    async fn every_graphql_and_transaction_request_carries_a_valid_identity() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let audience = address.to_string();
        let authenticated_requests = Arc::new(AtomicUsize::new(0));
        let state = CaptureState {
            audience,
            authenticated_requests: authenticated_requests.clone(),
        };
        let app = Router::new()
            .route("/api/v0/graphql", post(capture_auth))
            .route("/api/v0/node/identity", get(capture_auth))
            .route("/api/v0/tx", post(capture_auth))
            .route("/api/v0/tx/{id}", post(capture_auth).delete(capture_auth))
            .with_state(state);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let key_dir = tempfile::tempdir().unwrap();
        let identity = Arc::new(
            KeyIdentity::load_or_create(key_dir.path().join("identity.key"), None).unwrap(),
        );
        let endpoint = format!("http://{address}/api/v0/graphql");
        let access = AuthenticatedGraphql::new(endpoint.clone(), identity)
            .await
            .unwrap();
        access
            .execute("{ __typename }", GraphqlRequestOptions::default())
            .await
            .unwrap();
        access
            .execute_with_tx("{ __typename }", GraphqlRequestOptions::default(), "7")
            .await
            .unwrap();
        access
            .get(format!("http://{address}/api/v0/node/identity"))
            .await
            .unwrap();
        access
            .post(format!("http://{address}/api/v0/tx"))
            .await
            .unwrap();
        access
            .delete(format!("http://{address}/api/v0/tx/7"))
            .await
            .unwrap();

        assert_eq!(authenticated_requests.load(Ordering::SeqCst), 5);
    }

    struct CannotMintIdentity;

    #[async_trait]
    impl AgentIdentity for CannotMintIdentity {
        fn did(&self) -> &str {
            "did:key:zUnavailable"
        }

        async fn sign(&self, _payload: &[u8]) -> Result<Vec<u8>> {
            anyhow::bail!("unavailable")
        }

        async fn verify(&self, _did: &str, _payload: &[u8], _signature: &[u8]) -> Result<bool> {
            Ok(false)
        }

        fn service_account(&self) -> Option<&crate::identity::ServiceAccount> {
            None
        }
    }

    struct CountingIdentity {
        inner: KeyIdentity,
        minted: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl AgentIdentity for CountingIdentity {
        fn did(&self) -> &str {
            self.inner.did()
        }

        async fn sign(&self, payload: &[u8]) -> Result<Vec<u8>> {
            self.inner.sign(payload).await
        }

        async fn verify(&self, did: &str, payload: &[u8], signature: &[u8]) -> Result<bool> {
            self.inner.verify(did, payload, signature).await
        }

        async fn defradb_bearer_token(&self, audience: &str, duration: Duration) -> Result<String> {
            self.minted.fetch_add(1, Ordering::SeqCst);
            self.inner.defradb_bearer_token(audience, duration).await
        }

        fn service_account(&self) -> Option<&crate::identity::ServiceAccount> {
            self.inner.service_account()
        }
    }

    #[tokio::test]
    async fn clones_share_a_bearer_until_the_refresh_window() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let authenticated_requests = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/api/v0/node/identity", get(capture_auth))
            .with_state(CaptureState {
                audience: address.to_string(),
                authenticated_requests: authenticated_requests.clone(),
            });
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let key_dir = tempfile::tempdir().unwrap();
        let minted = Arc::new(AtomicUsize::new(0));
        let identity = Arc::new(CountingIdentity {
            inner: KeyIdentity::load_or_create(key_dir.path().join("identity.key"), None).unwrap(),
            minted: minted.clone(),
        });
        let access =
            AuthenticatedGraphql::new(format!("http://{address}/api/v0/graphql"), identity)
                .await
                .unwrap();

        for client in [access.clone(), access.clone(), access] {
            client
                .get(format!("http://{address}/api/v0/node/identity"))
                .await
                .unwrap();
        }

        assert_eq!(authenticated_requests.load(Ordering::SeqCst), 3);
        assert_eq!(minted.load(Ordering::SeqCst), 1);
    }

    struct MismatchedIdentity {
        reported_did: String,
        issuer: KeyIdentity,
    }

    #[async_trait]
    impl AgentIdentity for MismatchedIdentity {
        fn did(&self) -> &str {
            &self.reported_did
        }

        async fn sign(&self, payload: &[u8]) -> Result<Vec<u8>> {
            self.issuer.sign(payload).await
        }

        async fn verify(&self, did: &str, payload: &[u8], signature: &[u8]) -> Result<bool> {
            self.issuer.verify(did, payload, signature).await
        }

        async fn defradb_bearer_token(&self, audience: &str, duration: Duration) -> Result<String> {
            self.issuer.defradb_bearer_token(audience, duration).await
        }

        fn service_account(&self) -> Option<&crate::identity::ServiceAccount> {
            None
        }
    }

    #[tokio::test]
    async fn construction_rejects_a_valid_token_from_the_wrong_principal() {
        let key_dir = tempfile::tempdir().unwrap();
        let issuer = KeyIdentity::load_or_create(key_dir.path().join("issuer.key"), None).unwrap();
        let error = AuthenticatedGraphql::new(
            "http://127.0.0.1:9191/api/v0/graphql",
            Arc::new(MismatchedIdentity {
                reported_did: "did:key:zReportedByAdapter".to_string(),
                issuer,
            }),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("issuer mismatch"), "{error:#}");
    }

    #[tokio::test]
    async fn construction_fails_closed_when_identity_cannot_mint_bearer() {
        let error = AuthenticatedGraphql::new(
            "http://127.0.0.1:9191/api/v0/graphql",
            Arc::new(CannotMintIdentity),
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("minting DefraDB HTTP identity"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn bearer_is_never_sent_cross_origin() {
        let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_address = target_listener.local_addr().unwrap();
        let received = Arc::new(AtomicUsize::new(0));
        let received_for_handler = received.clone();
        let target_app = Router::new().fallback(move || {
            let received = received_for_handler.clone();
            async move {
                received.fetch_add(1, Ordering::SeqCst);
                StatusCode::OK
            }
        });
        tokio::spawn(async move {
            axum::serve(target_listener, target_app).await.unwrap();
        });

        let key_dir = tempfile::tempdir().unwrap();
        let identity = Arc::new(
            KeyIdentity::load_or_create(key_dir.path().join("identity.key"), None).unwrap(),
        );
        let access = AuthenticatedGraphql::new("http://127.0.0.1:9191/api/v0/graphql", identity)
            .await
            .unwrap();
        let error = access
            .get(format!("http://{target_address}/steal"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cross-origin"), "{error:#}");
        tokio::task::yield_now().await;
        assert_eq!(received.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn authenticated_rest_client_never_follows_redirects() {
        let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_address = target_listener.local_addr().unwrap();
        let received = Arc::new(AtomicUsize::new(0));
        let received_for_handler = received.clone();
        let target_app = Router::new().fallback(move || {
            let received = received_for_handler.clone();
            async move {
                received.fetch_add(1, Ordering::SeqCst);
                StatusCode::OK
            }
        });
        tokio::spawn(async move { axum::serve(target_listener, target_app).await.unwrap() });

        let source_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_address = source_listener.local_addr().unwrap();
        let location = format!("http://{target_address}/capture");
        let source_app = Router::new().route(
            "/redirect",
            get(move || {
                let location = location.clone();
                async move {
                    (
                        StatusCode::TEMPORARY_REDIRECT,
                        [(reqwest::header::LOCATION, location)],
                    )
                }
            }),
        );
        tokio::spawn(async move { axum::serve(source_listener, source_app).await.unwrap() });

        let key_dir = tempfile::tempdir().unwrap();
        let identity = Arc::new(
            KeyIdentity::load_or_create(key_dir.path().join("identity.key"), None).unwrap(),
        );
        let access =
            AuthenticatedGraphql::new(format!("http://{source_address}/api/v0/graphql"), identity)
                .await
                .unwrap();
        let response = access
            .get(format!("http://{source_address}/redirect"))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        tokio::task::yield_now().await;
        assert_eq!(received.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn real_defradb_http_accepts_bound_identity_and_rejects_wrong_audience() {
        // Policyless collections may remain anonymously readable until ACP
        // policies are installed. The Gents-side invariant tested here is
        // narrower: its client always supplies the actor, Defra accepts that
        // actor for document reads/writes, and rejects a Host-mismatched JWT.
        let reserved = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reserved.local_addr().unwrap().port();
        drop(reserved);

        let key_dir = tempfile::tempdir().unwrap();
        let identity = Arc::new(
            KeyIdentity::load_or_create(key_dir.path().join("identity.key"), None).unwrap(),
        );
        let node = defra_node::EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::new(port))
            .build()
            .await
            .unwrap();
        node.add_schema("type AuthenticatedHttpProbe { value: String }")
            .await
            .unwrap();

        let endpoint = format!("http://127.0.0.1:{port}/api/v0/graphql");
        let raw_client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        for _ in 0..50 {
            if raw_client
                .get(format!("http://127.0.0.1:{port}/api/v0/node/identity"))
                .send()
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let wrong_audience = identity
            .defradb_bearer_token("127.0.0.1:1", Duration::from_secs(60))
            .await
            .unwrap();
        let wrong = raw_client
            .post(&endpoint)
            .bearer_auth(wrong_audience)
            .json(&json!({"query": "{ AuthenticatedHttpProbe { value } }"}))
            .send()
            .await
            .unwrap();
        assert!(
            !wrong.status().is_success(),
            "wrong-audience bearer unexpectedly succeeded"
        );

        let access = AuthenticatedGraphql::new(endpoint, identity).await.unwrap();
        let created = access
            .execute(
                r#"mutation { create_AuthenticatedHttpProbe(input: { value: "bound" }) { _docID } }"#,
                GraphqlRequestOptions::default(),
            )
            .await
            .unwrap();
        assert!(
            created
                .pointer("/data/create_AuthenticatedHttpProbe/_docID")
                .is_some()
                || created
                    .pointer("/data/create_AuthenticatedHttpProbe/0/_docID")
                    .is_some()
                || created
                    .pointer("/data/add_AuthenticatedHttpProbe/0/_docID")
                    .is_some(),
            "authenticated mutation returned no document: {created}"
        );
        let queried = access
            .execute(
                "{ AuthenticatedHttpProbe { value } }",
                GraphqlRequestOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            queried.pointer("/data/AuthenticatedHttpProbe/0/value"),
            Some(&Value::String("bound".to_string()))
        );

        node.shutdown().await;
    }

    #[test]
    fn supplied_invalid_token_is_rejected_before_any_request() {
        let error = AuthenticatedGraphql::from_verified_token(
            "http://127.0.0.1:9191/api/v0/graphql",
            "not-a-jwt".to_string(),
            "did:key:zExpected",
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("parsing DefraDB HTTP bearer token"));
    }

    #[tokio::test]
    async fn supplied_token_is_bound_to_expected_principal_and_short_lifetime() {
        let key_dir = tempfile::tempdir().unwrap();
        let identity =
            KeyIdentity::load_or_create(key_dir.path().join("identity.key"), None).unwrap();
        let audience = "127.0.0.1:9191";
        let endpoint = format!("http://{audience}/api/v0/graphql");

        let short = identity
            .defradb_bearer_token(audience, BEARER_TOKEN_LIFETIME)
            .await
            .unwrap();
        let wrong_principal = AuthenticatedGraphql::from_verified_token(
            endpoint.clone(),
            short,
            "did:key:zDifferentPrincipal",
        )
        .unwrap_err();
        assert!(
            wrong_principal.to_string().contains("issuer mismatch"),
            "{wrong_principal:#}"
        );

        let overlong = identity
            .defradb_bearer_token(audience, BEARER_TOKEN_LIFETIME + Duration::from_secs(1))
            .await
            .unwrap();
        let error = AuthenticatedGraphql::from_verified_token(endpoint, overlong, identity.did())
            .unwrap_err();
        assert!(
            error.to_string().contains("exceeds the maximum"),
            "{error:#}"
        );
    }
}
