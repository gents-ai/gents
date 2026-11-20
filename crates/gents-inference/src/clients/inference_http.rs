//! HTTP client wrapper that tags outbound inference requests with the current
//! agent session id and trace context. `x-session-id` lets a load balancer in
//! front of a multi-replica, prefix-caching inference backend pin every turn
//! of a conversation to the same replica (sticky-session routing), keeping its
//! prefix cache warm; the session id comes from the admission of the call
//! each request belongs to, because the provider's reusable parts are built
//! once per agent while the session id varies per request.

use rapidhash::RapidHashMap;
use std::future::Future;

use crate::llm::rig_compat::http::{
    self as http_client, HeaderMap, HeaderValue, LazyBody, MultipartForm, Request, ReqwestClient,
    Response, StreamingResponse, Transport,
};
use bytes::Bytes;
use gents_loop::provider_call::ProviderCall;
use reqwest::header::HeaderName;

const SESSION_ID_HEADER: &str = "x-session-id";

#[derive(Clone, Debug, Default)]
pub struct ResponsesNormalizingHttpClient<H = ReqwestClient> {
    inner: H,
    service_tier: Option<crate::resolved::ServiceTier>,
}

impl<H> ResponsesNormalizingHttpClient<H> {
    pub fn new(inner: H) -> Self {
        Self {
            inner,
            service_tier: None,
        }
    }

    pub fn with_service_tier(mut self, tier: Option<crate::resolved::ServiceTier>) -> Self {
        self.service_tier = tier;
        self
    }

    fn normalize_json_body(&self, req: Request<Bytes>) -> Request<Bytes> {
        let (parts, mut body) = req.into_parts();
        if parts.uri.path().ends_with("/responses") {
            if let Some(normalized) = normalize_responses_body(&body, self.service_tier) {
                body = normalized;
            }
        }
        Request::from_parts(parts, body)
    }
}

fn normalize_responses_body(
    body: &[u8],
    tier: Option<crate::resolved::ServiceTier>,
) -> Option<Bytes> {
    let mut value = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    gents_loop::responses_normalize::normalize_responses_assistant_items(&mut value);
    if let Some(tier) = tier {
        tier.apply(&mut value).ok()?;
    }
    serde_json::to_vec(&value).ok().map(Bytes::from)
}

impl<H> Transport for ResponsesNormalizingHttpClient<H>
where
    H: Clone + Transport + 'static,
{
    fn send(
        &self,
        call: &ProviderCall,
        req: Request<Bytes>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<Bytes>>>> + Send + 'static {
        let inner = self.inner.clone();
        let call = call.clone();
        let req = self.normalize_json_body(req);
        async move { Transport::send(&inner, &call, req).await }
    }

    fn send_multipart(
        &self,
        call: &ProviderCall,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<Bytes>>>> + Send + 'static {
        let inner = self.inner.clone();
        let call = call.clone();
        async move { Transport::send_multipart(&inner, &call, req).await }
    }

    fn send_streaming(
        &self,
        call: &ProviderCall,
        req: Request<Bytes>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + Send {
        let inner = self.inner.clone();
        let call = call.clone();
        let req = self.normalize_json_body(req);
        async move { Transport::send_streaming(&inner, &call, req).await }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SessionTaggingHttpClient<H = ReqwestClient> {
    inner: H,
}

impl<H> SessionTaggingHttpClient<H> {
    pub fn new(inner: H) -> Self {
        Self { inner }
    }

    fn tag(call: &ProviderCall, req: Request<Bytes>) -> Request<Bytes> {
        Self::tag_with_trace_context_headers(
            call,
            req,
            crate::trace_context::current_trace_context_headers(),
        )
    }

    fn tag_with_trace_context_headers(
        call: &ProviderCall,
        mut req: Request<Bytes>,
        trace_context_headers: RapidHashMap<String, String>,
    ) -> Request<Bytes> {
        Self::inject_headers(call, req.headers_mut(), trace_context_headers);
        req
    }

    fn tag_multipart(call: &ProviderCall, req: Request<MultipartForm>) -> Request<MultipartForm> {
        let (mut parts, body) = req.into_parts();
        Self::inject_headers(
            call,
            &mut parts.headers,
            crate::trace_context::current_trace_context_headers(),
        );
        Request::from_parts(parts, body)
    }

    fn inject_headers(
        call: &ProviderCall,
        headers: &mut HeaderMap,
        trace_context_headers: RapidHashMap<String, String>,
    ) {
        if let Some(admission) = &call.admission {
            if let Ok(value) = HeaderValue::from_str(admission.session_id()) {
                headers.insert(SESSION_ID_HEADER, value);
            }
        }
        Self::insert_trace_context_headers(headers, trace_context_headers);
    }

    fn insert_trace_context_headers(
        headers: &mut HeaderMap,
        trace_context_headers: RapidHashMap<String, String>,
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

impl<H> Transport for SessionTaggingHttpClient<H>
where
    H: Clone + Transport + 'static,
{
    fn send(
        &self,
        call: &ProviderCall,
        req: Request<Bytes>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<Bytes>>>> + Send + 'static {
        let inner = self.inner.clone();
        let call = call.clone();
        let req = Self::tag(&call, req);
        async move { Transport::send(&inner, &call, req).await }
    }

    fn send_multipart(
        &self,
        call: &ProviderCall,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<Bytes>>>> + Send + 'static {
        let inner = self.inner.clone();
        let call = call.clone();
        let req = Self::tag_multipart(&call, req);
        async move { Transport::send_multipart(&inner, &call, req).await }
    }

    fn send_streaming(
        &self,
        call: &ProviderCall,
        req: Request<Bytes>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + Send {
        let inner = self.inner.clone();
        let call = call.clone();
        let req = Self::tag(&call, req);
        async move { Transport::send_streaming(&inner, &call, req).await }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug, Default)]
    struct CapturingHttpClient {
        bodies: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    impl CapturingHttpClient {
        fn bodies(&self) -> Vec<serde_json::Value> {
            self.bodies.lock().expect("capture lock").clone()
        }
    }

    impl Transport for CapturingHttpClient {
        fn send(
            &self,
            _call: &ProviderCall,
            req: Request<Bytes>,
        ) -> impl Future<Output = http_client::Result<Response<LazyBody<Bytes>>>> + Send + 'static
        {
            let bodies = Arc::clone(&self.bodies);
            let body = req.into_body();
            async move {
                bodies
                    .lock()
                    .expect("capture lock")
                    .push(serde_json::from_slice(&body).expect("captured JSON body"));
                let body: LazyBody<Bytes> = Box::pin(async { Ok(Bytes::from_static(b"{}")) });
                Ok(Response::builder().status(200).body(body)?)
            }
        }

        fn send_multipart(
            &self,
            _call: &ProviderCall,
            req: Request<MultipartForm>,
        ) -> impl Future<Output = http_client::Result<Response<LazyBody<Bytes>>>> + Send + 'static
        {
            let _ = req;
            async move {
                let body: LazyBody<Bytes> = Box::pin(async { Ok(Bytes::from_static(b"{}")) });
                Ok(Response::builder().status(200).body(body)?)
            }
        }

        fn send_streaming(
            &self,
            _call: &ProviderCall,
            req: Request<Bytes>,
        ) -> impl Future<Output = http_client::Result<StreamingResponse>> + Send {
            let bodies = Arc::clone(&self.bodies);
            let body = req.into_body();
            async move {
                bodies
                    .lock()
                    .expect("capture lock")
                    .push(serde_json::from_slice(&body).expect("captured JSON body"));
                let stream: http_client::BoxedStream =
                    Box::pin(futures::stream::empty::<http_client::Result<Bytes>>());
                Ok(Response::builder().status(200).body(stream)?)
            }
        }
    }

    #[tokio::test]
    async fn responses_normalizing_client_normalizes_send_body() {
        let inner = CapturingHttpClient::default();
        let client = ResponsesNormalizingHttpClient::new(inner.clone());
        let req = Request::builder()
            .uri("http://example.test/v1/responses")
            .body(Bytes::from_static(
                br#"{"input":[{"role":"assistant","content":[{"type":"output_text","text":"hi"}]}]}"#,
            ))
            .expect("request");

        let _ = Transport::send(&client, &Default::default(), req)
            .await
            .expect("send");

        let bodies = inner.bodies();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0]["input"][0]["type"], "message");
        assert_eq!(bodies[0]["input"][0]["id"], "msg_gents_0");
        assert_eq!(bodies[0]["input"][0]["status"], "completed");
        assert_eq!(
            bodies[0]["input"][0]["content"][0]["annotations"],
            serde_json::json!([])
        );
    }

    #[tokio::test]
    async fn responses_normalizing_client_normalizes_streaming_body() {
        let inner = CapturingHttpClient::default();
        let client = ResponsesNormalizingHttpClient::new(inner.clone());
        let req = Request::builder()
            .uri("http://example.test/v1/responses")
            .body(Bytes::from_static(
                br#"{"input":[{"role":"assistant","content":[{"type":"output_text","text":"hi"}]}]}"#,
            ))
            .expect("request");

        let _ = Transport::send_streaming(&client, &Default::default(), req)
            .await
            .expect("send streaming");

        let bodies = inner.bodies();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0]["input"][0]["type"], "message");
        assert_eq!(bodies[0]["input"][0]["id"], "msg_gents_0");
        assert_eq!(bodies[0]["input"][0]["status"], "completed");
        assert_eq!(
            bodies[0]["input"][0]["content"][0]["annotations"],
            serde_json::json!([])
        );
    }

    #[tokio::test]
    async fn responses_normalizing_client_leaves_other_paths_unchanged() {
        let inner = CapturingHttpClient::default();
        let client = ResponsesNormalizingHttpClient::new(inner.clone());
        let original = serde_json::json!({
            "input": [{
                "role": "assistant",
                "content": [{"type": "output_text", "text": "hi"}]
            }]
        });
        let req = Request::builder()
            .uri("http://example.test/v1/chat/completions")
            .body(Bytes::from(
                serde_json::to_vec(&original).expect("serialize"),
            ))
            .expect("request");

        let _ = Transport::send(&client, &Default::default(), req)
            .await
            .expect("send");

        assert_eq!(inner.bodies(), vec![original]);
    }

    #[test]
    fn tag_is_noop_without_session_context() {
        let req = Request::new(Bytes::from_static(b""));
        let tagged = SessionTaggingHttpClient::<ReqwestClient>::tag(&ProviderCall::default(), req);
        assert!(
            !tagged.headers().contains_key(SESSION_ID_HEADER),
            "x-session-id must not be set when there is no active session context"
        );
    }

    #[test]
    fn tag_adds_valid_trace_context_headers() {
        let req = Request::new(Bytes::from_static(b""));
        let tagged = SessionTaggingHttpClient::<ReqwestClient>::tag_with_trace_context_headers(
            &ProviderCall::default(),
            req,
            RapidHashMap::from_iter([
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
