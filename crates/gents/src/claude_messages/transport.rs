//! Claude Messages over gents-claude's own HTTP/1.1 transport, so the bytes on
//! the wire are Claude Code 2.1.311's (Bun fetch): the request head as
//! `RequestHead` renders it, Bun's appended defaults, `Content-Length`, and the
//! padded gzip body above the size gate. [`ClaudeHttpClient`] sits under the
//! capture layer, which therefore records the request as built; padding and
//! gzip happen here, after capture. A 400 that the API answers with a JSON
//! parse failure latches gzip off for the process and is resent once plain.

use std::fmt;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

use bytes::Bytes;
use gents_claude::request::{self, RequestHead};
use gents_claude::transport::{
    gzip, proxy_from_env, Body, Client, ClientOptions, CompressError, ProxyError, Response, Target,
    TransportError,
};
use gents_loop::account_usage::UsageSource;
use rand::Rng;
use reqwest::header::HeaderName;
use reqwest::StatusCode;
use rig::http_client::Response as HttpResponse;
use rig::http_client::StreamingResponse;
use rig::http_client::{
    self, HeaderMap, HeaderValue, HttpClientExt, LazyBody, MultipartForm, Request, Uri,
};
use rig::wasm_compat::WasmCompatSend;
use serde_json::Value;
use tokio::sync::OnceCell;

use crate::provider_http::{observe, rejected_response_error};
use crate::rendered_request::RenderedRequestCapturingHttpClient;
use crate::usage_observation::UsageReporter;

/// The first-party host the proxy decision is made for; the client proxies every TLS target alike.
const FIRST_PARTY_HOST: &str = "api.anthropic.com";
/// Longest non-success body read, enough for any API error and bounded for a hostile peer.
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// Why a Claude Messages request could not be sent or answered.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ClaudeTransportError {
    #[error("the Claude request URL needs an http or https scheme and a host")]
    BadUri,
    #[error("the request header {0} is not valid text")]
    RequestHeader(String),
    #[error("the request body is not valid UTF-8")]
    BodyNotUtf8,
    #[error("the HTTP client could not be built")]
    ClientBuild(#[source] TransportError),
    #[error("the HTTP client setup task failed")]
    ClientTask(#[source] tokio::task::JoinError),
    #[error("the proxy setting is not usable: {0}")]
    Proxy(#[from] ProxyError),
    #[error("the request body could not be compressed: {0}")]
    Compress(#[from] CompressError),
    #[error("the compression task failed")]
    CompressTask(#[source] tokio::task::JoinError),
    #[error("the Claude request failed: {0}")]
    Transport(#[from] TransportError),
    #[error("the response header {0} is not valid")]
    ResponseHeader(String),
    #[error("Claude Messages does not send multipart requests")]
    Multipart,
}

impl From<ClaudeTransportError> for http_client::Error {
    fn from(error: ClaudeTransportError) -> Self {
        Self::Instance(Box::new(error))
    }
}

/// The process-wide client: one keep-alive pool and one proxy decision.
struct Shared {
    client: Client,
    proxied: bool,
}

static SHARED: OnceCell<Shared> = OnceCell::const_new();
static GZIP_LATCHED_OFF: LazyLock<Arc<AtomicBool>> = LazyLock::new(Arc::default);

fn env_pair(upper: &str, lower: &str) -> Option<String> {
    std::env::var(upper).or_else(|_| std::env::var(lower)).ok()
}

fn build_shared() -> Result<Shared, ClaudeTransportError> {
    let proxy = proxy_from_env(
        FIRST_PARTY_HOST,
        443,
        env_pair("HTTPS_PROXY", "https_proxy").as_deref(),
        env_pair("NO_PROXY", "no_proxy").as_deref(),
    )?;
    let proxied = proxy.is_some();
    let client = Client::new(ClientOptions {
        proxy,
        ..ClientOptions::default()
    })
    .map_err(ClaudeTransportError::ClientBuild)?;
    Ok(Shared { client, proxied })
}

/// `Client::new` loads the platform trust roots, which blocks, so it runs off the runtime workers.
async fn shared() -> Result<&'static Shared, ClaudeTransportError> {
    SHARED
        .get_or_try_init(|| async {
            tokio::task::spawn_blocking(build_shared)
                .await
                .map_err(ClaudeTransportError::ClientTask)?
        })
        .await
}

/// A request as the transport needs it: where it goes and the headers to render.
struct Outgoing {
    method: String,
    target: Target,
    /// The authority without userinfo, as the `Host` header carries it.
    host_header: String,
    path: String,
    headers: Vec<(String, String, bool)>,
}

impl Outgoing {
    fn new(method: &str, uri: &Uri, headers: &HeaderMap) -> Result<Self, ClaudeTransportError> {
        let tls = match uri.scheme_str() {
            Some("https") => true,
            Some("http") => false,
            _ => return Err(ClaudeTransportError::BadUri),
        };
        let host = uri
            .host()
            .map(|host| host.trim_matches(['[', ']']))
            .filter(|host| !host.is_empty())
            .ok_or(ClaudeTransportError::BadUri)?;
        let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
        let host_header = match uri.port_u16() {
            Some(port) => format!("{}:{port}", uri.host().unwrap_or(host)),
            None => uri.host().unwrap_or(host).to_owned(),
        };
        let path = uri
            .path_and_query()
            .map_or("/", |path| path.as_str())
            .to_owned();
        let headers = headers
            .iter()
            .map(|(name, value)| {
                let text = value
                    .to_str()
                    .map_err(|_| ClaudeTransportError::RequestHeader(name.to_string()))?;
                Ok((name.to_string(), text.to_owned(), value.is_sensitive()))
            })
            .collect::<Result<_, ClaudeTransportError>>()?;
        Ok(Self {
            method: method.to_owned(),
            target: Target {
                host: host.to_owned(),
                port,
                tls,
            },
            host_header,
            path,
            headers,
        })
    }
}

/// One answered request with its body still streaming.
struct Exchange {
    status: StatusCode,
    headers: HeaderMap,
    body: Body,
}

/// Claude Messages over the Bun-exact transport; observes usage headers like `ProviderHttpClient`.
#[derive(Clone)]
pub(crate) struct ClaudeHttpClient {
    usage: Option<Arc<UsageReporter>>,
    gzip_off: Arc<AtomicBool>,
    /// Connects here instead of the URL's host while keeping its `Host` and gzip decision.
    #[cfg(test)]
    route: Option<(String, u16)>,
}

impl fmt::Debug for ClaudeHttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeHttpClient").finish_non_exhaustive()
    }
}

impl ClaudeHttpClient {
    pub(crate) fn new(usage: Option<Arc<UsageReporter>>) -> Self {
        Self {
            usage,
            gzip_off: GZIP_LATCHED_OFF.clone(),
            #[cfg(test)]
            route: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_route(
        mut self,
        gzip_off: Arc<AtomicBool>,
        host: &str,
        port: u16,
    ) -> Self {
        self.gzip_off = gzip_off;
        self.route = Some((host.to_owned(), port));
        self
    }

    /// Sends once; `Ok(true)` in the second slot means the body went gzipped.
    async fn send_once(
        &self,
        shared: &Shared,
        outgoing: &Outgoing,
        body: &Bytes,
    ) -> Result<(Response, bool), ClaudeTransportError> {
        let text = std::str::from_utf8(body).map_err(|_| ClaudeTransportError::BodyNotUtf8)?;
        let proxied = shared.proxied && outgoing.target.tls;
        let gzipped = request::gzip_body(
            &outgoing.target.host,
            request::utf16_len(text),
            proxied,
            self.gzip_off.load(Ordering::Relaxed),
        );
        let wire = if gzipped {
            let plain = body.clone();
            let packed = tokio::task::spawn_blocking(move || {
                let mut rng = rand::rng();
                let len = request::padding_len(rng.random());
                let mut padded = plain.to_vec();
                request::pad_body(&mut padded, len, || rng.random_bool(0.5));
                gzip(&padded)
            })
            .await
            .map_err(ClaudeTransportError::CompressTask)??;
            Bytes::from(packed)
        } else {
            body.clone()
        };
        let head = RequestHead::new(
            &outgoing.method,
            &outgoing.path,
            &outgoing.host_header,
            outgoing
                .headers
                .iter()
                .map(|(name, value, sensitive)| (name.as_str(), value.as_str(), *sensitive)),
            gzipped,
        );
        #[cfg_attr(not(test), allow(unused_mut))]
        let mut target = outgoing.target.clone();
        #[cfg(test)]
        if let Some((host, port)) = &self.route {
            target.host.clone_from(host);
            target.port = *port;
        }
        let response = shared
            .client
            .send(&target, &head.encode(wire.len()), &wire)
            .await?;
        Ok((response, gzipped))
    }

    /// Sends the request, resending it plain once when the API rejects the gzip body, and
    /// maps a non-success answer to the provider-limit-annotated status error.
    fn execute(
        &self,
        method: String,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
    ) -> impl Future<Output = http_client::Result<Exchange>> + WasmCompatSend + 'static {
        let this = self.clone();
        async move {
            let outgoing = Outgoing::new(&method, &uri, &headers)?;
            let shared = shared().await?;
            loop {
                let (response, gzipped) = this.send_once(shared, &outgoing, &body).await?;
                let status = StatusCode::from_u16(response.status)
                    .map_err(|_| ClaudeTransportError::Transport(TransportError::MalformedHead))?;
                let map = header_map(&response.headers)?;
                if status.is_success() {
                    observe(&this.usage, &map, UsageSource::Header);
                    return Ok(Exchange {
                        status,
                        headers: map,
                        body: response.body,
                    });
                }
                let text = read_error_body(response.body).await;
                if gzipped && gzip_rejected(status, &map, &text) {
                    tracing::warn!("Claude API rejected the gzip request body, resending it plain");
                    this.gzip_off.store(true, Ordering::Relaxed);
                    continue;
                }
                observe(&this.usage, &map, UsageSource::Error);
                return Err(rejected_response_error(status, &map, &text));
            }
        }
    }
}

fn gzip_rejected(status: StatusCode, headers: &HeaderMap, body: &str) -> bool {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_default();
    request::gzip_rejected(
        status.as_u16(),
        headers.get("request-id").and_then(|id| id.to_str().ok()),
        headers.contains_key("cf-ray"),
        &message,
    )
}

fn header_map(headers: &[(String, String)]) -> Result<HeaderMap, ClaudeTransportError> {
    let mut map = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        let invalid = || ClaudeTransportError::ResponseHeader(name.clone());
        map.append(
            HeaderName::from_bytes(name.as_bytes()).map_err(|_| invalid())?,
            HeaderValue::from_str(value).map_err(|_| invalid())?,
        );
    }
    Ok(map)
}

/// A non-success body as text, truncated at [`MAX_ERROR_BODY_BYTES`]; a read error ends it early.
async fn read_error_body(mut body: Body) -> String {
    let mut bytes = Vec::new();
    while bytes.len() < MAX_ERROR_BODY_BYTES {
        match body.chunk().await {
            Ok(Some(chunk)) => bytes.extend_from_slice(&chunk),
            Ok(None) | Err(_) => break,
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn body_stream(body: Body) -> http_client::sse::BoxedStream {
    Box::pin(futures::stream::unfold(body, |mut body| async move {
        match body.chunk().await {
            Ok(Some(chunk)) => Some((Ok(chunk), body)),
            Ok(None) => None,
            Err(error) => Some((
                Err(http_client::Error::from(ClaudeTransportError::from(error))),
                body,
            )),
        }
    }))
}

impl HttpClientExt for ClaudeHttpClient {
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<HttpResponse<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        let (parts, body) = req.into_parts();
        let exchange = self.execute(
            parts.method.to_string(),
            parts.uri,
            parts.headers,
            body.into(),
        );
        async move {
            let Exchange {
                status,
                headers,
                mut body,
            } = exchange.await?;
            let mut builder = HttpResponse::builder().status(status);
            if let Some(map) = builder.headers_mut() {
                *map = headers;
            }
            let body: LazyBody<U> = Box::pin(async move {
                let mut bytes = Vec::new();
                while let Some(chunk) = body.chunk().await.map_err(ClaudeTransportError::from)? {
                    bytes.extend_from_slice(&chunk);
                }
                Ok(U::from(Bytes::from(bytes)))
            });
            builder.body(body).map_err(http_client::Error::Protocol)
        }
    }

    fn send_multipart<U>(
        &self,
        _req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<HttpResponse<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        std::future::ready(Err(ClaudeTransportError::Multipart.into()))
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes>,
    {
        let (parts, body) = req.into_parts();
        let exchange = self.execute(
            parts.method.to_string(),
            parts.uri,
            parts.headers,
            body.into(),
        );
        async move {
            let Exchange {
                status,
                headers,
                body,
            } = exchange.await?;
            let mut builder = HttpResponse::builder()
                .status(status)
                .version(reqwest::Version::HTTP_11);
            if let Some(map) = builder.headers_mut() {
                *map = headers;
            }
            builder
                .body(body_stream(body))
                .map_err(http_client::Error::Protocol)
        }
    }
}

/// The client of one send: capture outermost, so it records the request as built.
pub(super) fn capturing(
    fixture: Option<String>,
    live: ClaudeHttpClient,
) -> RenderedRequestCapturingHttpClient<MessagesTransport> {
    RenderedRequestCapturingHttpClient::new(MessagesTransport { fixture, live })
}

/// Serves a queued SSE fixture in tests; otherwise forwards to the Bun-exact client.
#[derive(Clone)]
pub(super) struct MessagesTransport {
    pub(super) fixture: Option<String>,
    pub(super) live: ClaudeHttpClient,
}

impl HttpClientExt for MessagesTransport {
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<HttpResponse<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        let inner = self.live.clone();
        let fixture = self.fixture.clone();
        let (parts, body) = req.into_parts();
        let body: Bytes = body.into();
        async move {
            if fixture.is_some() {
                let body: LazyBody<U> = Box::pin(async { Ok(U::from(Bytes::from_static(b"{}"))) });
                return Ok(HttpResponse::builder().status(200).body(body)?);
            }
            let req = Request::from_parts(parts, body);
            HttpClientExt::send::<Bytes, U>(&inner, req).await
        }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<HttpResponse<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        let inner = self.live.clone();
        async move { HttpClientExt::send_multipart::<U>(&inner, req).await }
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes>,
    {
        let inner = self.live.clone();
        let fixture = self.fixture.clone();
        let (parts, body) = req.into_parts();
        let body: Bytes = body.into();
        async move {
            if let Some(sse) = fixture {
                let stream: http_client::sse::BoxedStream =
                    Box::pin(futures::stream::iter([Ok(Bytes::from(sse))]));
                return Ok(HttpResponse::builder().status(200).body(stream)?);
            }
            let req = Request::from_parts(parts, body);
            HttpClientExt::send_streaming(&inner, req).await
        }
    }
}

impl fmt::Debug for MessagesTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MessagesTransport")
            .field("fixture", &self.fixture.is_some())
            .finish_non_exhaustive()
    }
}
