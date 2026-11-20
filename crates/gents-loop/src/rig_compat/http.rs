use std::future::Future;

use bytes::Bytes;
use rig::http_client::HttpClientExt;

use crate::provider_call::ProviderCall;
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
/// Each send receives the [`ProviderCall`] it belongs to and passes it to the
/// transport beneath. `send` and `send_multipart` return a `'static` future
/// the provider client may poll from any task.
pub trait Transport: Send + Sync {
    fn send(
        &self,
        call: &ProviderCall,
        request: Request<Bytes>,
    ) -> impl Future<Output = Result<Response<LazyBody<Bytes>>>> + Send + 'static;

    fn send_multipart(
        &self,
        call: &ProviderCall,
        request: Request<MultipartForm>,
    ) -> impl Future<Output = Result<Response<LazyBody<Bytes>>>> + Send + 'static;

    fn send_streaming(
        &self,
        call: &ProviderCall,
        request: Request<Bytes>,
    ) -> impl Future<Output = Result<StreamingResponse>> + Send;
}

/// A [`Transport`] as Rig's HTTP client for one call.
#[derive(Clone, Debug, Default)]
pub struct Lifted<T> {
    transport: T,
    call: ProviderCall,
}

impl<T> Lifted<T> {
    pub fn new(transport: T, call: ProviderCall) -> Self {
        Self { transport, call }
    }
}

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
        let response = self.transport.send(&self.call, req.map(Into::into));
        async move { response.await.map(typed_body) }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        let response = self.transport.send_multipart(&self.call, req);
        async move { response.await.map(typed_body) }
    }

    fn send_streaming<B>(
        &self,
        req: Request<B>,
    ) -> impl Future<Output = Result<StreamingResponse>> + WasmCompatSend
    where
        B: Into<Bytes>,
    {
        self.transport
            .send_streaming(&self.call, req.map(Into::into))
    }
}

/// Rig's own clients, and anything else already speaking its client trait,
/// carry a stack as its innermost transport; they read nothing of the call.
impl<H> Transport for H
where
    H: HttpClientExt,
{
    fn send(
        &self,
        _call: &ProviderCall,
        request: Request<Bytes>,
    ) -> impl Future<Output = Result<Response<LazyBody<Bytes>>>> + Send + 'static {
        HttpClientExt::send::<Bytes, Bytes>(self, request)
    }

    fn send_multipart(
        &self,
        _call: &ProviderCall,
        request: Request<MultipartForm>,
    ) -> impl Future<Output = Result<Response<LazyBody<Bytes>>>> + Send + 'static {
        HttpClientExt::send_multipart::<Bytes>(self, request)
    }

    fn send_streaming(
        &self,
        _call: &ProviderCall,
        request: Request<Bytes>,
    ) -> impl Future<Output = Result<StreamingResponse>> + Send {
        HttpClientExt::send_streaming(self, request)
    }
}
