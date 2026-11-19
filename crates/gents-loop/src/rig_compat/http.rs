use std::future::Future;

use bytes::Bytes;
use rig::http_client::HttpClientExt;
use rig::wasm_compat::WasmCompatSend;

pub use rig::http_client::sse::BoxedStream;
#[cfg(feature = "native")]
pub use rig::http_client::ReqwestClient;
pub use rig::http_client::{
    Error, HeaderMap, HeaderValue, LazyBody, MultipartForm, Request, Response, Result,
    StreamingResponse, Uri,
};

/// The HTTP exchange a provider client is built over, with request and
/// response bodies as bytes. A layer (capture, credentials, limit headers,
/// body rewriting) implements this over the transport beneath it, and
/// [`Lifted`] is the one place a stack becomes Rig's client.
///
/// `send` and `send_multipart` return a `'static` future the provider client
/// may poll from any task, so whatever a layer reads from the calling task it
/// reads before returning that future.
pub trait Transport: Send + Sync {
    fn send(
        &self,
        request: Request<Bytes>,
    ) -> impl Future<Output = Result<Response<LazyBody<Bytes>>>> + Send + 'static;

    fn send_multipart(
        &self,
        request: Request<MultipartForm>,
    ) -> impl Future<Output = Result<Response<LazyBody<Bytes>>>> + Send + 'static;

    fn send_streaming(
        &self,
        request: Request<Bytes>,
    ) -> impl Future<Output = Result<StreamingResponse>> + Send;
}

/// A [`Transport`] as Rig's HTTP client.
#[derive(Clone, Debug, Default)]
pub struct Lifted<T>(pub T);

fn typed_body<U>(response: Response<LazyBody<Bytes>>) -> Response<LazyBody<U>>
where
    U: From<Bytes> + WasmCompatSend + 'static,
{
    response.map(|body| -> LazyBody<U> { Box::pin(async move { Ok(U::from(body.await?)) }) })
}

impl<T> HttpClientExt for Lifted<T>
where
    T: Transport,
{
    fn send<B, U>(
        &self,
        req: Request<B>,
    ) -> impl Future<Output = Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        B: Into<Bytes> + WasmCompatSend,
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        let response = self.0.send(req.map(Into::into));
        async move { response.await.map(typed_body) }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        let response = self.0.send_multipart(req);
        async move { response.await.map(typed_body) }
    }

    fn send_streaming<B>(
        &self,
        req: Request<B>,
    ) -> impl Future<Output = Result<StreamingResponse>> + WasmCompatSend
    where
        B: Into<Bytes>,
    {
        self.0.send_streaming(req.map(Into::into))
    }
}

/// Rig's own clients, and anything else already speaking its client trait,
/// carry a stack as its innermost transport.
impl<H> Transport for H
where
    H: HttpClientExt,
{
    fn send(
        &self,
        request: Request<Bytes>,
    ) -> impl Future<Output = Result<Response<LazyBody<Bytes>>>> + Send + 'static {
        HttpClientExt::send::<Bytes, Bytes>(self, request)
    }

    fn send_multipart(
        &self,
        request: Request<MultipartForm>,
    ) -> impl Future<Output = Result<Response<LazyBody<Bytes>>>> + Send + 'static {
        HttpClientExt::send_multipart::<Bytes>(self, request)
    }

    fn send_streaming(
        &self,
        request: Request<Bytes>,
    ) -> impl Future<Output = Result<StreamingResponse>> + Send {
        HttpClientExt::send_streaming(self, request)
    }
}
