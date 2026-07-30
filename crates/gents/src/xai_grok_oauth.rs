//! Grok / xAI SuperGrok subscription OAuth provider (subscription proxy path).

use std::future::Future;
use std::sync::Arc;
use std::{fmt, fmt::Formatter};

use anyhow::{Context, Result};
use bytes::Bytes;
use defra_node::EmbeddedNode;
use rig::http_client::{
    self, HeaderMap, HeaderValue, HttpClientExt, LazyBody, MultipartForm, Request, ReqwestClient,
    Response, StreamingResponse,
};
use rig::wasm_compat::WasmCompatSend;
use serde_json::Value;

use crate::oauth_credential::{
    classify_oauth_auth_error, lookup_oauth_credential, shared_bearer, BearerSource,
    DbCredentialBearer, OAuthAuthProblem, OAuthRefreshKind, XAI_OAUTH_PRODUCT,
};

pub const XAI_OAUTH_PROVIDER: &str = "xai-oauth";

/// Subscription inference proxy (not the metered developer API).
pub const XAI_GROK_OAUTH_BASE_URL: &str = "https://cli-chat-proxy.grok.com/v1";

const GROK_CLIENT_VERSION: &str = "0.2.93";
const GROK_CLIENT_VERSION_ENV: &str = "GENTS_XAI_GROK_CLIENT_VERSION";

pub fn default_backend_endpoint() -> &'static str {
    XAI_GROK_OAUTH_BASE_URL
}

pub fn default_model_name() -> &'static str {
    "grok-4.5"
}

pub fn normalize_endpoint(endpoint: &str) -> String {
    let trimmed = endpoint.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        default_backend_endpoint().to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn normalize_provider(provider: &str) -> String {
    let provider = provider.trim();
    if provider.is_empty() {
        XAI_OAUTH_PROVIDER.to_string()
    } else {
        provider.to_string()
    }
}

pub fn classify_xai_auth_error(
    agent_did: &str,
    provider: &str,
    problem: &OAuthAuthProblem,
) -> String {
    classify_oauth_auth_error(&XAI_OAUTH_PRODUCT, agent_did, provider, problem)
}

pub fn grok_client_version() -> String {
    std::env::var(GROK_CLIENT_VERSION_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| GROK_CLIENT_VERSION.to_string())
}

/// Headers the Grok CLI chat proxy uses to recognize subscription clients.
pub fn build_xai_grok_oauth_headers() -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Accept",
        HeaderValue::from_static("text/event-stream, application/json"),
    );
    headers.insert("x-xai-token-auth", HeaderValue::from_static("xai-grok-cli"));
    headers.insert(
        "x-grok-client-identifier",
        HeaderValue::from_static("grok-shell"),
    );
    headers.insert(
        "x-grok-client-version",
        HeaderValue::from_str(&grok_client_version())
            .context("Grok client version could not be encoded as an HTTP header")?,
    );
    headers.insert("User-Agent", HeaderValue::from_static("xai-grok-cli"));
    Ok(headers)
}

fn bearer_rejection_status(error: &http_client::Error) -> Option<u16> {
    match error {
        http_client::Error::InvalidStatusCode(status)
        | http_client::Error::InvalidStatusCodeWithMessage(status, _) => Some(status.as_u16()),
        _ => None,
    }
}

fn is_bearer_rejection(error: &http_client::Error) -> bool {
    matches!(bearer_rejection_status(error), Some(401) | Some(403))
}

/// HTTP client that injects a fresh OAuth bearer and lightly shapes Responses bodies.
pub struct XaiGrokOAuthHttpClient<S: BearerSource, H = ReqwestClient> {
    inner: H,
    bearer: Option<Arc<S>>,
}

impl<S: BearerSource, H: Clone> Clone for XaiGrokOAuthHttpClient<S, H> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            bearer: self.bearer.clone(),
        }
    }
}

impl<S: BearerSource, H: fmt::Debug> fmt::Debug for XaiGrokOAuthHttpClient<S, H> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("XaiGrokOAuthHttpClient")
            .field("inner", &self.inner)
            .field("bearer_configured", &self.bearer.is_some())
            .finish()
    }
}

impl<S: BearerSource, H: Default> Default for XaiGrokOAuthHttpClient<S, H> {
    fn default() -> Self {
        Self {
            inner: H::default(),
            bearer: None,
        }
    }
}

impl<S: BearerSource> XaiGrokOAuthHttpClient<S, ReqwestClient> {
    pub fn new(bearer: Arc<S>) -> Self {
        Self {
            inner: ReqwestClient::default(),
            bearer: Some(bearer),
        }
    }
}

impl<S: BearerSource, H> XaiGrokOAuthHttpClient<S, H> {
    pub fn with_inner(bearer: Arc<S>, inner: H) -> Self {
        Self {
            inner,
            bearer: Some(bearer),
        }
    }

    async fn fresh_auth_header(&self) -> http_client::Result<HeaderValue> {
        let bearer = self.bearer.as_ref().ok_or_else(|| {
            http_client::Error::Instance(
                anyhow::anyhow!("XaiGrokOAuthHttpClient used without a configured BearerSource")
                    .into(),
            )
        })?;
        let token = bearer
            .current_bearer()
            .await
            .map_err(|error| http_client::Error::Instance(error.into()))?;
        HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|error| http_client::Error::Instance(anyhow::Error::from(error).into()))
    }

    async fn prepare(&self, req: Request<Bytes>) -> http_client::Result<Request<Bytes>> {
        let req = Self::patch_responses_body(req);
        let value = self.fresh_auth_header().await?;
        let (mut parts, body) = req.into_parts();
        parts.headers.insert("authorization", value);
        Ok(Request::from_parts(parts, body))
    }

    fn bearer_to_invalidate<X>(&self, result: &http_client::Result<X>) -> Option<Arc<S>> {
        match result {
            Err(error) if is_bearer_rejection(error) => self.bearer.clone(),
            _ => None,
        }
    }

    fn patch_responses_body(req: Request<Bytes>) -> Request<Bytes> {
        let (parts, body) = req.into_parts();
        let mut body = body;
        if parts.uri.path().ends_with("/responses") {
            if let Some(patched) = patch_store_false(&body) {
                body = patched;
            }
        }
        Request::from_parts(parts, body)
    }

    #[cfg(test)]
    pub async fn prepare_for_test(
        &self,
        req: Request<Bytes>,
    ) -> http_client::Result<Request<Bytes>> {
        self.prepare(req).await
    }
}

impl<S, H> HttpClientExt for XaiGrokOAuthHttpClient<S, H>
where
    S: BearerSource + 'static,
    H: Clone + HttpClientExt + fmt::Debug + 'static,
{
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        let inner = self.inner.clone();
        let this = self.clone();
        let (parts, body) = req.into_parts();
        let req = Request::from_parts(parts, body.into());
        async move {
            let req = this.prepare(req).await?;
            let result = HttpClientExt::send::<Bytes, U>(&inner, req).await;
            if let Some(bearer) = this.bearer_to_invalidate(&result) {
                bearer.invalidate().await;
            }
            result
        }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        let inner = self.inner.clone();
        let this = self.clone();
        async move {
            let value = this.fresh_auth_header().await?;
            let (mut parts, body) = req.into_parts();
            parts.headers.insert("authorization", value);
            let req = Request::from_parts(parts, body);
            let result = HttpClientExt::send_multipart(&inner, req).await;
            if let Some(bearer) = this.bearer_to_invalidate(&result) {
                bearer.invalidate().await;
            }
            result
        }
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes>,
    {
        let inner = self.inner.clone();
        let this = self.clone();
        let (parts, body) = req.into_parts();
        let req = Request::from_parts(parts, body.into());
        async move {
            let req = this.prepare(req).await?;
            let result = HttpClientExt::send_streaming(&inner, req).await;
            if let Some(bearer) = this.bearer_to_invalidate(&result) {
                bearer.invalidate().await;
            }
            let mut response = result?;
            ensure_event_stream_content_type(response.headers_mut());
            Ok(response)
        }
    }
}

fn ensure_event_stream_content_type(headers: &mut HeaderMap) {
    if !headers.contains_key("content-type") {
        headers.insert(
            "content-type",
            HeaderValue::from_static("text/event-stream"),
        );
    }
}

fn patch_store_false(body: &[u8]) -> Option<Bytes> {
    let mut value = serde_json::from_slice::<Value>(body).ok()?;
    let mut changed = false;
    if value.get("store").is_none() {
        value["store"] = Value::Bool(false);
        changed = true;
    }
    if !changed {
        return None;
    }
    serde_json::to_vec(&value).ok().map(Bytes::from)
}

pub async fn build_responses_client(
    node: Arc<EmbeddedNode>,
    agent_did: &str,
    endpoint: &str,
) -> Result<rig::providers::openai::Client<XaiGrokOAuthHttpClient<DbCredentialBearer>>> {
    let provider = XAI_OAUTH_PROVIDER;
    let credential = lookup_oauth_credential(node.as_ref(), agent_did, provider)
        .await
        .with_context(|| format!("loading OAuthCredential for agent {agent_did}"))?
        .ok_or_else(|| {
            anyhow::anyhow!(classify_xai_auth_error(
                agent_did,
                provider,
                &OAuthAuthProblem::Missing,
            ))
        })?;
    let headers = build_xai_grok_oauth_headers()?;
    let endpoint = normalize_endpoint(endpoint);
    let credential_id = credential.credential_id.clone();
    let bearer = shared_bearer(&credential_id, || {
        DbCredentialBearer::with_cache(
            node,
            agent_did,
            provider,
            credential_id.clone(),
            true,
            Some(credential.clone()),
            OAuthRefreshKind::Xai,
            XAI_OAUTH_PRODUCT,
        )
    });
    let http = XaiGrokOAuthHttpClient::new(bearer);
    crate::inference_http::build_openai_responses_client(
        "xai-oauth-managed",
        &endpoint,
        http,
        headers,
    )
    .context("building Grok OAuth Responses client")
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingBearer {
        token: String,
        calls: AtomicUsize,
    }

    impl CountingBearer {
        fn new(token: &str) -> Arc<Self> {
            Arc::new(Self {
                token: token.to_string(),
                calls: AtomicUsize::new(0),
            })
        }
    }

    impl BearerSource for CountingBearer {
        async fn current_bearer(&self) -> Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.token.clone())
        }
    }

    #[test]
    fn headers_advertise_cli_identity() {
        let headers = build_xai_grok_oauth_headers().unwrap();
        assert_eq!(
            headers
                .get("x-xai-token-auth")
                .and_then(|value| value.to_str().ok()),
            Some("xai-grok-cli")
        );
        assert_eq!(
            headers
                .get("x-grok-client-identifier")
                .and_then(|value| value.to_str().ok()),
            Some("grok-shell")
        );
        assert_eq!(
            headers
                .get("x-grok-client-version")
                .and_then(|value| value.to_str().ok()),
            Some(grok_client_version().as_str())
        );
        assert_eq!(
            headers
                .get("user-agent")
                .and_then(|value| value.to_str().ok()),
            Some("xai-grok-cli")
        );
    }

    #[tokio::test]
    async fn injects_fresh_bearer() {
        let bearer = CountingBearer::new("tok-xyz");
        let client = XaiGrokOAuthHttpClient::new(bearer.clone());
        let req = Request::builder()
            .method("POST")
            .uri("https://cli-chat-proxy.grok.com/v1/responses")
            .header("authorization", "Bearer STALE")
            .body(Bytes::from_static(br#"{"model":"grok-4.5"}"#))
            .unwrap();
        let prepared = client.prepare_for_test(req).await.unwrap();
        assert_eq!(
            prepared
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer tok-xyz")
        );
        let body: Value = serde_json::from_slice(prepared.body()).unwrap();
        assert_eq!(body.get("store").and_then(Value::as_bool), Some(false));
    }

    #[test]
    fn classify_missing_points_at_grok_login() {
        let msg = classify_xai_auth_error(
            "did:key:zAgent",
            XAI_OAUTH_PROVIDER,
            &OAuthAuthProblem::Missing,
        );
        assert!(msg.contains("gents grok-login"), "{msg}");
    }

    #[test]
    fn patch_store_false_is_idempotent_when_present() {
        let body = serde_json::json!({"store": true, "model": "grok-4.5"});
        assert!(patch_store_false(&serde_json::to_vec(&body).unwrap()).is_none());
    }
}
