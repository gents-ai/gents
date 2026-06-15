//! HTTP client wrapper that tags outbound inference requests with the current
//! agent session id and trace context.
//!
//! Emitting `x-session-id` lets a load balancer in front of a multi-replica,
//! prefix-caching inference backend pin every turn of a conversation to the
//! same replica (sticky-session routing), keeping that replica's prefix cache
//! warm so only the new turn's delta is prefilled instead of paying the full
//! prefill tax on each hop. See issue #447.
//!
//! The session id is resolved per request from the admission task-local request
//! context, because the rig completion client is built once per behavior while
//! the session id varies per request. This mirrors the per-request injection
//! seam already used by [`crate::chatgpt_codex::ChatGptCodexHttpClient`].

use std::collections::HashMap;
use std::future::Future;

use anyhow::{Context, Result};
use bytes::Bytes;
use reqwest::header::HeaderName;
use rig::http_client::{
    self, HeaderMap, HeaderValue, HttpClientExt, LazyBody, MultipartForm, Request, ReqwestClient,
    Response, StreamingResponse,
};
use rig::wasm_compat::WasmCompatSend;

/// Header carrying the agent session id on outbound inference requests.
const SESSION_ID_HEADER: &str = "x-session-id";

/// Force the legacy Chat Completions client for OpenAI-compatible backends.
///
/// Default production behavior is the Responses API. The override keeps
/// compatibility with backends that do not serve `/v1/responses` and with test
/// mocks whose assertions are chat-format.
pub(crate) fn force_openai_chat_completions() -> bool {
    // defra-agent's own lib unit tests (compiled with `cfg(test)`) boot
    // in-process agents against Chat Completions mocks, so default to chat
    // there. Integration tests link defra-agent WITHOUT `cfg(test)`, so they
    // set the env explicitly; production + the real CLI binary default to the
    // Responses API.
    if cfg!(test) {
        return true;
    }
    openai_chat_completions_override_enabled(
        std::env::var("DEFRA_AGENT_OPENAI_CHAT_COMPLETIONS")
            .ok()
            .as_deref(),
    )
}

fn openai_chat_completions_override_enabled(value: Option<&str>) -> bool {
    matches!(value, Some("1") | Some("true"))
}

pub(crate) fn build_openai_responses_client<H>(
    api_key: &str,
    base_url: &str,
    http_client: H,
    http_headers: HeaderMap,
) -> Result<rig::providers::openai::Client<H>>
where
    H: Default + HttpClientExt,
{
    rig::providers::openai::Client::builder()
        .api_key(api_key)
        .base_url(base_url)
        .http_headers(http_headers)
        .http_client(http_client)
        .build()
        .context("building OpenAI Responses client")
}

pub(crate) fn build_openai_chat_completions_client<H>(
    api_key: &str,
    base_url: &str,
    http_client: H,
) -> Result<rig::providers::openai::CompletionsClient<H>>
where
    H: Default + HttpClientExt,
{
    rig::providers::openai::CompletionsClient::builder()
        .api_key(api_key)
        .base_url(base_url)
        .http_client(http_client)
        .build()
        .context("building OpenAI Chat Completions client")
}

/// A [`HttpClientExt`] that injects [`SESSION_ID_HEADER`] from the current
/// admission request context onto each outbound request, then delegates to the
/// inner reqwest client. When there is no active session context (e.g. one-shot
/// calls outside the daemon scope) the request is passed through unchanged.
#[derive(Clone, Debug, Default)]
pub struct SessionTaggingHttpClient {
    inner: ReqwestClient,
}

impl SessionTaggingHttpClient {
    fn tag<T>(req: Request<T>) -> Request<Bytes>
    where
        T: Into<Bytes>,
    {
        Self::tag_with_trace_context_headers(
            req,
            crate::runtime_trace::current_trace_context_headers(),
        )
    }

    fn tag_with_trace_context_headers<T>(
        req: Request<T>,
        trace_context_headers: HashMap<String, String>,
    ) -> Request<Bytes>
    where
        T: Into<Bytes>,
    {
        let (mut parts, body) = req.into_parts();
        Self::inject_headers(&mut parts.headers, trace_context_headers);
        Request::from_parts(parts, body.into())
    }

    fn tag_multipart(req: Request<MultipartForm>) -> Request<MultipartForm> {
        let (mut parts, body) = req.into_parts();
        Self::inject_headers(
            &mut parts.headers,
            crate::runtime_trace::current_trace_context_headers(),
        );
        Request::from_parts(parts, body)
    }

    fn inject_headers(headers: &mut HeaderMap, trace_context_headers: HashMap<String, String>) {
        if let Some(session_id) = crate::admission::current_session_id() {
            if let Ok(value) = HeaderValue::from_str(&session_id) {
                headers.insert(SESSION_ID_HEADER, value);
            }
        }
        Self::insert_trace_context_headers(headers, trace_context_headers);
    }

    fn insert_trace_context_headers(
        headers: &mut HeaderMap,
        trace_context_headers: HashMap<String, String>,
    ) {
        for (name, value) in trace_context_headers {
            let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else {
                continue;
            };
            let Ok(value) = HeaderValue::from_str(value.as_str()) else {
                continue;
            };
            headers.entry(name).or_insert(value);
        }
    }
}

impl HttpClientExt for SessionTaggingHttpClient {
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
        let req = Self::tag(req);
        async move { HttpClientExt::send(&inner, req).await }
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
        let req = Self::tag_multipart(req);
        async move { HttpClientExt::send_multipart(&inner, req).await }
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes>,
    {
        let inner = self.inner.clone();
        let req = Self::tag(req);
        async move { HttpClientExt::send_streaming(&inner, req).await }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_completions_override_parses_only_explicit_true_values() {
        assert!(openai_chat_completions_override_enabled(Some("1")));
        assert!(openai_chat_completions_override_enabled(Some("true")));
        assert!(!openai_chat_completions_override_enabled(Some("TRUE")));
        assert!(!openai_chat_completions_override_enabled(Some("0")));
        assert!(!openai_chat_completions_override_enabled(Some("false")));
        assert!(!openai_chat_completions_override_enabled(None));
    }

    #[test]
    fn openai_client_builders_accept_session_tagging_transport() {
        let _responses: rig::providers::openai::Client<SessionTaggingHttpClient> =
            build_openai_responses_client(
                "test-key",
                "http://example.test/v1",
                SessionTaggingHttpClient::default(),
                HeaderMap::default(),
            )
            .expect("Responses client should build");

        let _chat_completions: rig::providers::openai::CompletionsClient<SessionTaggingHttpClient> =
            build_openai_chat_completions_client(
                "test-key",
                "http://example.test/v1",
                SessionTaggingHttpClient::default(),
            )
            .expect("Chat Completions client should build");
    }

    #[test]
    fn tag_is_noop_without_session_context() {
        // Outside any admission scope there is no session id to attach.
        let req = Request::new(Bytes::from_static(b""));
        let tagged = SessionTaggingHttpClient::tag(req);
        assert!(
            !tagged.headers().contains_key(SESSION_ID_HEADER),
            "x-session-id must not be set when there is no active session context"
        );
    }

    #[test]
    fn tag_adds_valid_trace_context_headers() {
        let req = Request::new(Bytes::from_static(b""));
        let tagged = SessionTaggingHttpClient::tag_with_trace_context_headers(
            req,
            HashMap::from([
                (
                    "traceparent".to_string(),
                    "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
                ),
                ("tracestate".to_string(), "vendor=value".to_string()),
                ("bad header".to_string(), "ignored".to_string()),
                ("x-bad-value".to_string(), "bad\nvalue".to_string()),
            ]),
        );

        assert_eq!(
            tagged
                .headers()
                .get("traceparent")
                .and_then(|value| value.to_str().ok()),
            Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
        );
        assert_eq!(
            tagged
                .headers()
                .get("tracestate")
                .and_then(|value| value.to_str().ok()),
            Some("vendor=value")
        );
        assert!(!tagged.headers().contains_key("bad header"));
        assert!(!tagged.headers().contains_key("x-bad-value"));
    }
}
