//! The network terminal beneath every provider completion stack.

use std::future::Future;

use bytes::Bytes;
use chrono::Utc;
use gents_loop::provider_limit::ProviderLimitHeaders;
use reqwest::StatusCode;
use rig::http_client::{
    self, HeaderMap, HttpClientExt, LazyBody, MultipartForm, Request, ReqwestClient, Response,
    StreamingResponse,
};
use rig::wasm_compat::WasmCompatSend;

/// Rig's reqwest client, except that a non-success response keeps its
/// rate-limit headers.
///
/// Rig's own `HttpClientExt for reqwest::Client` turns a non-success status
/// into `InvalidStatusCodeWithMessage(status, body)` before it copies headers,
/// and only error text crosses Rig's provider and streaming boundaries. This
/// client returns the same variant with the body annotated by
/// [`ProviderLimitHeaders::annotate`], so
/// `gents_loop::provider_limit` can honor `Retry-After` and provider reset
/// headers.
#[derive(Clone, Debug, Default)]
pub struct ProviderHttpClient {
    inner: ReqwestClient,
}

impl ProviderHttpClient {
    pub fn new(inner: ReqwestClient) -> Self {
        Self { inner }
    }
}

fn instance_error<E: std::error::Error + Send + Sync + 'static>(error: E) -> http_client::Error {
    http_client::Error::Instance(Box::new(error))
}

/// The rejected-response error: Rig's variant, body plus header marker.
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
        async move {
            let response = request.send().await.map_err(instance_error)?;
            let status = response.status();
            if !status.is_success() {
                let headers = response.headers().clone();
                let body = response.text().await.unwrap_or_default();
                return Err(rejected_response_error(status, &headers, &body));
            }
            let mut builder = Response::builder().status(status);
            if let Some(headers) = builder.headers_mut() {
                *headers = response.headers().clone();
            }
            let body: LazyBody<U> = Box::pin(async move {
                let bytes = response.bytes().await.map_err(instance_error)?;
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
        async move {
            let response = client
                .execute(request.map_err(instance_error)?)
                .await
                .map_err(instance_error)?;
            let status = response.status();
            if !status.is_success() {
                let headers = response.headers().clone();
                let body = response.text().await.unwrap_or_default();
                return Err(rejected_response_error(status, &headers, &body));
            }
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
                    .map(|chunk| chunk.map_err(instance_error)),
            );
            builder.body(stream).map_err(http_client::Error::Protocol)
        }
    }
}

#[cfg(test)]
#[path = "provider_http/tests.rs"]
pub(crate) mod tests;
