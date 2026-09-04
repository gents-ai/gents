//! Shared bearer-auth HTTP transport for OAuth-backed inference providers.
//!
//! ChatGPT Codex and Grok/xAI OAuth both wrap a raw HTTP client with the same
//! shape: inject a fresh bearer on every request, invalidate it when the
//! provider rejects it, and normalize the streaming content-type. Before this
//! module the two providers carried ~150-line copies of that wrapper apiece,
//! differing only in which HTTP statuses count as a bearer rejection, what
//! provider-specific shaping the request body and response need, and whether
//! identity headers ride per-request or as client-build defaults.
//!
//! [`BearerAuthHttpClient`] is the single wrapper; [`IdentityHeaders`] is the
//! per-provider policy (a small `Default` marker type) that supplies those
//! differences. `chatgpt_codex` and `xai_grok_oauth` each keep their own
//! provider-specific body-patch/response-shaping free functions and instantiate
//! this wrapper through a policy type and a type alias so their existing call
//! sites (`ChatGptCodexHttpClient::new`, `.with_inner`, `.prepare_for_test`)
//! are unchanged.

use std::fmt;
use std::future::Future;
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use defra_node::EmbeddedNode;
use rig::http_client::{
    self, HeaderMap, HttpClientExt, LazyBody, MultipartForm, Request, ReqwestClient, Response,
    StreamingResponse,
};
use rig::wasm_compat::WasmCompatSend;

use crate::oauth_credential::{
    classify_oauth_auth_error, lookup_oauth_credential, shared_bearer, BearerSource,
    DbCredentialBearer, OAuthAuthProblem, OAuthCredential, OAuthProduct, OAuthRefreshKind,
};

/// Per-provider policy for [`BearerAuthHttpClient`]: which HTTP statuses mean
/// "the bearer is dead", plus the request/response shaping hooks each
/// provider needs. Every method has a passthrough default so a provider only
/// overrides what it actually differs on.
pub trait IdentityHeaders: Clone + Default + Send + Sync + 'static {
    /// HTTP status codes on which the current bearer must be invalidated and
    /// refreshed on the next request. Codex: 401 and 403. xAI: 401 only — its
    /// proxy uses 403 as a NotEntitled tier gate that no refresh fixes, and
    /// with rotating refresh tokens a force-refresh loop would burn a
    /// rotation per request.
    const REJECTION_STATUSES: &'static [u16];

    /// Merge provider identity headers into an outbound request, without
    /// overwriting anything already present. Default: none — a provider whose
    /// identity headers ride as client-build defaults (Codex, via
    /// `http_headers` on the rig client builder) never needs this.
    fn merge_identity_headers(&self, headers: &mut HeaderMap) -> http_client::Result<()> {
        let _ = headers;
        Ok(())
    }

    /// Provider-specific request-body shaping applied before auth/identity
    /// headers (Codex's `instructions` hoist, xAI's `store:false`). Default:
    /// unchanged.
    fn patch_request_body(&self, req: Request<Bytes>) -> Request<Bytes> {
        req
    }

    /// Buffered (non-streaming) send. Default: pass the response straight
    /// through. Codex overrides this to rewrite the SSE `/responses` body it
    /// receives into the buffered `response.completed` JSON rig expects.
    fn send_via<T, U>(
        &self,
        inner: &T,
        req: Request<Bytes>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Clone + HttpClientExt + 'static,
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        let inner = inner.clone();
        async move { HttpClientExt::send::<Bytes, U>(&inner, req).await }
    }

    /// Streaming-response post-process beyond the shared content-type fixup
    /// (xAI folds `usage` into the final SSE chunk). Default: unchanged.
    fn patch_streaming(&self, response: StreamingResponse) -> StreamingResponse {
        response
    }
}

fn bearer_rejection_status(error: &http_client::Error) -> Option<u16> {
    match error {
        http_client::Error::InvalidStatusCode(status)
        | http_client::Error::InvalidStatusCodeWithMessage(status, _) => Some(status.as_u16()),
        _ => None,
    }
}

/// Single owner of "does this HTTP error mean the bearer is dead" for every
/// OAuth-backed provider. `rejection_statuses` is the provider's
/// [`IdentityHeaders::REJECTION_STATUSES`].
pub fn is_bearer_rejection(rejection_statuses: &[u16], error: &http_client::Error) -> bool {
    bearer_rejection_status(error).is_some_and(|status| rejection_statuses.contains(&status))
}

/// Single owner of "the streaming response must be SSE" across OAuth
/// providers: only fills in `content-type` when the backend omitted it.
pub fn ensure_event_stream_content_type(headers: &mut HeaderMap) {
    if !headers.contains_key("content-type") {
        headers.insert(
            "content-type",
            http_client::HeaderValue::from_static("text/event-stream"),
        );
    }
}

/// HTTP client that injects a fresh OAuth bearer on every request, applies a
/// provider's [`IdentityHeaders`] policy, and invalidates the bearer when the
/// provider rejects it.
pub struct BearerAuthHttpClient<S: BearerSource, P: IdentityHeaders, T = ReqwestClient> {
    inner: T,
    bearer: Option<Arc<S>>,
    policy: P,
}

impl<S: BearerSource, P: IdentityHeaders, T: Clone> Clone for BearerAuthHttpClient<S, P, T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            bearer: self.bearer.clone(),
            policy: self.policy.clone(),
        }
    }
}

impl<S: BearerSource, P: IdentityHeaders, T: fmt::Debug> fmt::Debug
    for BearerAuthHttpClient<S, P, T>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BearerAuthHttpClient")
            .field("inner", &self.inner)
            .field("bearer_configured", &self.bearer.is_some())
            .finish()
    }
}

impl<S: BearerSource, P: IdentityHeaders, T: Default> Default for BearerAuthHttpClient<S, P, T> {
    fn default() -> Self {
        Self {
            inner: T::default(),
            bearer: None,
            policy: P::default(),
        }
    }
}

impl<S: BearerSource, P: IdentityHeaders> BearerAuthHttpClient<S, P, ReqwestClient> {
    pub fn new(bearer: Arc<S>) -> Self {
        Self {
            inner: ReqwestClient::default(),
            bearer: Some(bearer),
            policy: P::default(),
        }
    }
}

impl<S: BearerSource, P: IdentityHeaders, T> BearerAuthHttpClient<S, P, T> {
    pub fn with_inner(bearer: Arc<S>, inner: T) -> Self {
        Self {
            inner,
            bearer: Some(bearer),
            policy: P::default(),
        }
    }

    async fn fresh_auth_header(&self) -> http_client::Result<http_client::HeaderValue> {
        let bearer = self.bearer.as_ref().ok_or_else(|| {
            http_client::Error::Instance(
                anyhow::anyhow!("BearerAuthHttpClient used without a configured BearerSource")
                    .into(),
            )
        })?;
        let token = bearer
            .current_bearer()
            .await
            .map_err(|error| http_client::Error::Instance(error.into()))?;
        http_client::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|error| http_client::Error::Instance(anyhow::Error::from(error).into()))
    }

    async fn prepare(&self, req: Request<Bytes>) -> http_client::Result<Request<Bytes>> {
        let req = self.policy.patch_request_body(req);
        let value = self.fresh_auth_header().await?;
        let (mut parts, body) = req.into_parts();
        parts.headers.insert("authorization", value);
        self.policy.merge_identity_headers(&mut parts.headers)?;
        Ok(Request::from_parts(parts, body))
    }

    fn bearer_to_invalidate<X>(&self, result: &http_client::Result<X>) -> Option<Arc<S>> {
        match result {
            Err(error) if is_bearer_rejection(P::REJECTION_STATUSES, error) => self.bearer.clone(),
            _ => None,
        }
    }

    #[cfg(test)]
    pub async fn prepare_for_test(
        &self,
        req: Request<Bytes>,
    ) -> http_client::Result<Request<Bytes>> {
        self.prepare(req).await
    }
}

impl<S, P, T> HttpClientExt for BearerAuthHttpClient<S, P, T>
where
    S: BearerSource + 'static,
    P: IdentityHeaders,
    T: Clone + HttpClientExt + fmt::Debug + 'static,
{
    fn send<A, U>(
        &self,
        req: Request<A>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        A: Into<Bytes> + WasmCompatSend,
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        let inner = self.inner.clone();
        let this = self.clone();
        let (parts, body) = req.into_parts();
        let req = Request::from_parts(parts, body.into());
        async move {
            let req = this.prepare(req).await?;
            let result = this.policy.send_via(&inner, req).await;
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
            this.policy.merge_identity_headers(&mut parts.headers)?;
            let req = Request::from_parts(parts, body);
            let result = HttpClientExt::send_multipart(&inner, req).await;
            if let Some(bearer) = this.bearer_to_invalidate(&result) {
                bearer.invalidate().await;
            }
            result
        }
    }

    fn send_streaming<A>(
        &self,
        req: Request<A>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        A: Into<Bytes>,
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
            Ok(this.policy.patch_streaming(response))
        }
    }
}

/// Look up the `OAuthCredential` for `(agent_did, provider)` and mint a
/// shared, cached [`DbCredentialBearer`] against it. Single owner of the
/// lookup-or-missing-error-then-cache-bearer preamble both `chatgpt_codex`'s
/// and `xai_grok_oauth`'s client builders used to duplicate.
pub async fn bootstrap_oauth_client(
    node: Arc<EmbeddedNode>,
    agent_did: &str,
    provider: &str,
    refresh_kind: OAuthRefreshKind,
    product: OAuthProduct,
) -> Result<(Arc<DbCredentialBearer>, OAuthCredential)> {
    let credential = lookup_oauth_credential(node.as_ref(), agent_did, provider)
        .await
        .with_context(|| format!("loading OAuthCredential for agent {agent_did}"))?
        .ok_or_else(|| {
            anyhow::anyhow!(classify_oauth_auth_error(
                &product,
                agent_did,
                provider,
                &OAuthAuthProblem::Missing,
            ))
        })?;
    let credential_id = credential.credential_id.clone();
    let provider = provider.to_string();
    let bearer = shared_bearer(&credential_id, || {
        DbCredentialBearer::with_cache(
            node,
            agent_did,
            provider,
            credential_id.clone(),
            true,
            Some(credential.clone()),
            refresh_kind,
            product,
        )
    });
    Ok((bearer, credential))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone, Default)]
    struct TestPolicy;

    impl IdentityHeaders for TestPolicy {
        const REJECTION_STATUSES: &'static [u16] = &[401, 403];
    }

    struct CountingBearer {
        token: String,
        calls: AtomicUsize,
        invalidations: AtomicUsize,
    }

    impl CountingBearer {
        fn new(token: &str) -> Arc<Self> {
            Arc::new(Self {
                token: token.to_string(),
                calls: AtomicUsize::new(0),
                invalidations: AtomicUsize::new(0),
            })
        }
    }

    impl BearerSource for CountingBearer {
        async fn current_bearer(&self) -> Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.token.clone())
        }

        async fn invalidate(&self) {
            self.invalidations.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[derive(Clone, Debug)]
    struct StatusInjectingClient {
        status: u16,
    }

    impl HttpClientExt for StatusInjectingClient {
        fn send<T, U>(
            &self,
            _req: Request<T>,
        ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
        where
            T: Into<Bytes> + WasmCompatSend,
            U: From<Bytes> + WasmCompatSend + 'static,
        {
            let status = self.status;
            async move {
                if status >= 400 {
                    return Err(http_client::Error::InvalidStatusCodeWithMessage(
                        status.to_string().parse().expect("valid status"),
                        "injected".to_string(),
                    ));
                }
                let body: LazyBody<U> = Box::pin(async { Ok(U::from(Bytes::new())) });
                Response::builder()
                    .status(status)
                    .body(body)
                    .map_err(http_client::Error::Protocol)
            }
        }

        fn send_multipart<U>(
            &self,
            _req: Request<MultipartForm>,
        ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
        where
            U: From<Bytes> + WasmCompatSend + 'static,
        {
            std::future::ready(Err(http_client::Error::InvalidStatusCode(
                "501".parse().expect("valid status"),
            )))
        }

        fn send_streaming<T>(
            &self,
            _req: Request<T>,
        ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
        where
            T: Into<Bytes>,
        {
            std::future::ready(Err(http_client::Error::InvalidStatusCode(
                "501".parse().expect("valid status"),
            )))
        }
    }

    fn status_error(status: &str) -> http_client::Error {
        http_client::Error::InvalidStatusCodeWithMessage(
            status.parse().expect("valid status"),
            String::new(),
        )
    }

    #[test]
    fn rejection_statuses_are_provider_scoped() {
        assert!(is_bearer_rejection(&[401, 403], &status_error("401")));
        assert!(is_bearer_rejection(&[401, 403], &status_error("403")));
        assert!(!is_bearer_rejection(&[401], &status_error("403")));
        assert!(!is_bearer_rejection(&[401, 403], &status_error("500")));
    }

    #[test]
    fn event_stream_content_type_is_added_only_when_missing() {
        let mut missing = HeaderMap::new();
        ensure_event_stream_content_type(&mut missing);
        assert_eq!(
            missing
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );

        let mut present = HeaderMap::new();
        present.insert(
            "content-type",
            http_client::HeaderValue::from_static("application/json"),
        );
        ensure_event_stream_content_type(&mut present);
        assert_eq!(
            present
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("application/json"),
            "backend-supplied content type should not be overwritten"
        );
    }

    async fn send_through(status: u16) -> Arc<CountingBearer> {
        let bearer = CountingBearer::new("tok");
        let client = BearerAuthHttpClient::<CountingBearer, TestPolicy, _>::with_inner(
            bearer.clone(),
            StatusInjectingClient { status },
        );
        let req = Request::builder()
            .method("POST")
            .uri("https://example.com/v1/models")
            .body(Bytes::from_static(b"{}"))
            .unwrap();
        let _ = HttpClientExt::send::<Bytes, Bytes>(&client, req).await;
        bearer
    }

    #[tokio::test]
    async fn rejection_status_invalidates_the_bearer() {
        let bearer = send_through(401).await;
        assert_eq!(
            bearer.invalidations.load(Ordering::SeqCst),
            1,
            "a rejection status from the provider must invalidate the bearer"
        );
    }

    #[tokio::test]
    async fn non_rejection_status_leaves_the_bearer_intact() {
        let bearer = send_through(200).await;
        assert_eq!(
            bearer.invalidations.load(Ordering::SeqCst),
            0,
            "a successful response must not invalidate the bearer"
        );
    }

    #[tokio::test]
    async fn injects_fresh_bearer_on_each_request() {
        let bearer = CountingBearer::new("tok-123");
        let client = BearerAuthHttpClient::<_, TestPolicy>::new(bearer.clone());

        let req = Request::builder()
            .method("POST")
            .uri("https://example.com/v1/responses")
            .header("authorization", "Bearer STALE")
            .body(Bytes::from_static(b"{}"))
            .unwrap();

        let prepared = client.prepare_for_test(req).await.unwrap();
        let auth = prepared
            .headers()
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap();

        assert_eq!(auth, "Bearer tok-123", "stale bearer was replaced");
        assert_eq!(
            bearer.calls.load(Ordering::SeqCst),
            1,
            "refreshed once per request"
        );
    }
}
