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

use bytes::Bytes;
use reqwest::header::HeaderName;
use rig::http_client::{
    self, HeaderMap, HeaderValue, HttpClientExt, LazyBody, MultipartForm, Request, ReqwestClient,
    Response, StreamingResponse,
};
use rig::wasm_compat::WasmCompatSend;

/// Header carrying the agent session id on outbound inference requests.
const SESSION_ID_HEADER: &str = "x-session-id";

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
