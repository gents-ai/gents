use std::future::Future;
use std::sync::Arc;

use bytes::Bytes;
use chrono::Utc;
use gents_loop::account_usage::UsageSource;
use gents_loop::provider_limit::ProviderLimitHeaders;
use reqwest::StatusCode;
use rig::http_client::{
    self, HeaderMap, HttpClientExt, LazyBody, MultipartForm, Request, ReqwestClient, Response,
    StreamingResponse,
};
use rig::wasm_compat::WasmCompatSend;

use crate::inference::usage_observation::UsageReporter;

/// Rig's reqwest client, except that a non-success response keeps its
/// rate-limit headers: Rig's own implementation discards them before they can
/// reach `gents_loop::provider_limit`.
#[derive(Clone, Debug, Default)]
pub struct ProviderHttpClient {
    inner: ReqwestClient,
    usage: Option<Arc<UsageReporter>>,
}

impl ProviderHttpClient {
    pub fn new(inner: ReqwestClient) -> Self {
        Self { inner, usage: None }
    }

    pub(crate) fn with_usage(inner: ReqwestClient, reporter: Arc<UsageReporter>) -> Self {
        Self {
            inner,
            usage: Some(reporter),
        }
    }
}

/// Every transport error here is a reqwest error, mapped as
/// `instance_error(error.without_url())`: a provider URL can carry a key in
/// its query, and this error's text reaches failure reasons and logs.
fn instance_error<E>(error: E) -> http_client::Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    http_client::Error::Instance(Box::new(error))
}

fn observe(usage: &Option<Arc<UsageReporter>>, headers: &HeaderMap, source: UsageSource) {
    if let Some(usage) = usage {
        usage.observe(headers, source);
    }
}

pub(crate) fn rejected_response_error(
    status: StatusCode,
    headers: &HeaderMap,
    body: &str,
) -> http_client::Error {
    let annotated = ProviderLimitHeaders::from_headers(
        headers
            .iter()
            .filter_map(|(name, value)| Some((name.as_str(), value.to_str().ok()?))),
        Utc::now(),
    )
    .annotate(body);
    http_client::Error::InvalidStatusCodeWithMessage(status, annotated)
}

impl HttpClientExt for ProviderHttpClient {
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes>,
        U: WasmCompatSend + 'static,
    {
        let (parts, body) = req.into_parts();
        let body: Bytes = body.into();
        let request = self
            .inner
            .request(parts.method, parts.uri.to_string())
            .headers(parts.headers)
            .body(body);
        let usage = self.usage.clone();
        async move {
            let response = request
                .send()
                .await
                .map_err(|error| instance_error(error.without_url()))?;
            let status = response.status();
            if !status.is_success() {
                let headers = response.headers().clone();
                observe(&usage, &headers, UsageSource::Error);
                let body = response.text().await.unwrap_or_default();
                return Err(rejected_response_error(status, &headers, &body));
            }
            observe(&usage, response.headers(), UsageSource::Header);
            let mut builder = Response::builder().status(status);
            if let Some(headers) = builder.headers_mut() {
                *headers = response.headers().clone();
            }
            let body: LazyBody<U> = Box::pin(async move {
                let bytes = response
                    .bytes()
                    .await
                    .map_err(|error| instance_error(error.without_url()))?;
                Ok(U::from(bytes))
            });
            builder.body(body).map_err(http_client::Error::Protocol)
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
        HttpClientExt::send_multipart(&self.inner, req)
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes>,
    {
        let (parts, body) = req.into_parts();
        let body: Bytes = body.into();
        let request = self
            .inner
            .request(parts.method, parts.uri.to_string())
            .headers(parts.headers)
            .body(body)
            .build();
        let client = self.inner.clone();
        let usage = self.usage.clone();
        async move {
            let response = client
                .execute(request.map_err(|error| instance_error(error.without_url()))?)
                .await
                .map_err(|error| instance_error(error.without_url()))?;
            let status = response.status();
            if !status.is_success() {
                let headers = response.headers().clone();
                observe(&usage, &headers, UsageSource::Error);
                let body = response.text().await.unwrap_or_default();
                return Err(rejected_response_error(status, &headers, &body));
            }
            observe(&usage, response.headers(), UsageSource::Header);
            let mut builder = Response::builder()
                .status(status)
                .version(response.version());
            if let Some(headers) = builder.headers_mut() {
                *headers = response.headers().clone();
            }
            use futures::StreamExt;
            let stream: http_client::sse::BoxedStream = Box::pin(
                response
                    .bytes_stream()
                    .map(|chunk| chunk.map_err(|error| instance_error(error.without_url()))),
            );
            builder.body(stream).map_err(http_client::Error::Protocol)
        }
    }
}

#[cfg(test)]
#[path = "provider_http/tests.rs"]
pub(crate) mod tests;
