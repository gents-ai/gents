//! HTTP client wrapper that tags outbound inference requests with the current
//! agent session id.
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

use std::future::Future;

use bytes::Bytes;
use rig::http_client::{
    self, HeaderValue, HttpClientExt, LazyBody, MultipartForm, Request, ReqwestClient, Response,
    StreamingResponse,
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
        let (mut parts, body) = req.into_parts();
        if let Some(session_id) = crate::admission::current_session_id() {
            if let Ok(value) = HeaderValue::from_str(&session_id) {
                parts.headers.insert(SESSION_ID_HEADER, value);
            }
        }
        Request::from_parts(parts, body.into())
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
}
