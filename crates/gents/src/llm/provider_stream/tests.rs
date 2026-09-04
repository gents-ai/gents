use super::*;
use bytes::Bytes;

fn response(chunks: Vec<http_client::Result<Bytes>>, status: u16) -> StreamingResponse {
    let body: rig::http_client::sse::BoxedStream = Box::pin(futures::stream::iter(chunks));
    http_client::Response::builder()
        .status(status)
        .header("x-test", "preserved")
        .body(body)
        .unwrap()
}

#[tokio::test]
async fn provider_stream_requires_explicit_terminal_across_all_chunk_boundaries() {
    for (protocol, payload) in [
        (ProviderStreamProtocol::ChatCompletions, "data: {\"choices\":[{\"delta\":{\"content\":\"héllo\"},\"finish_reason\":null}]}\r\n\r\ndata: [DONE]\r\n\r\n"),
        (ProviderStreamProtocol::ChatCompletions, "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n"),
        (ProviderStreamProtocol::ChatCompletions, "data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}]}\n\n"),
        (ProviderStreamProtocol::Responses, "event: response.completed\ndata: {\"type\":\"response.completed\"}\n\n"),
        (ProviderStreamProtocol::Anthropic, "event: message_stop\rdata: {\"type\":\"message_stop\"}\r\r"),
    ] {
        for split in 0..=payload.len() {
            let chunks = vec![Ok(Bytes::copy_from_slice(&payload.as_bytes()[..split])), Ok(Bytes::copy_from_slice(&payload.as_bytes()[split..]))];
            let guarded = guard_response(response(chunks, 200), Some(protocol));
            assert_eq!(guarded.headers()["x-test"], "preserved");
            let result = guarded.into_body().collect::<Vec<_>>().await;
            let actual: Vec<u8> = result.into_iter().flat_map(|item| item.expect("explicit terminal accepted")).collect();
            assert_eq!(actual, payload.as_bytes(), "split {split}");
        }
    }
}

#[tokio::test]
async fn provider_stream_partial_empty_and_unterminated_final_are_unexpected_eof() {
    for payload in [
        "",
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
        "data: [DONE]",
        "data: {\"choices\":[{\"delta\":{\"content\":\"[DONE]\"},\"finish_reason\":null}]}\n\n",
    ] {
        let guarded = guard_response(
            response(vec![Ok(Bytes::copy_from_slice(payload.as_bytes()))], 200),
            Some(ProviderStreamProtocol::ChatCompletions),
        );
        let mut body = guarded.into_body();
        assert_eq!(body.next().await.unwrap().unwrap(), payload.as_bytes());
        let http_client::Error::Instance(error) = body.next().await.unwrap().unwrap_err() else {
            panic!("expected EOF instance error")
        };
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        assert!(body.next().await.is_none());
    }
}

#[tokio::test]
async fn provider_stream_preserves_http_errors_and_non_completion_bodies() {
    for (status, protocol) in [
        (429, Some(ProviderStreamProtocol::ChatCompletions)),
        (200, None),
    ] {
        let guarded = guard_response(
            response(vec![Ok(Bytes::from_static(b"original error body"))], status),
            protocol,
        );
        assert_eq!(guarded.status().as_u16(), status);
        let result = guarded.into_body().collect::<Vec<_>>().await;
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].as_ref().unwrap(),
            b"original error body".as_slice()
        );
    }
    let error = http_client::Error::Instance(Box::new(std::io::Error::other(
        "original transport failure",
    )));
    let result = guard_response(
        response(vec![Err(error)], 200),
        Some(ProviderStreamProtocol::ChatCompletions),
    )
    .into_body()
    .collect::<Vec<_>>()
    .await;
    assert_eq!(result.len(), 1);
    assert!(result[0]
        .as_ref()
        .unwrap_err()
        .to_string()
        .contains("original transport failure"));
}

#[test]
fn provider_stream_bounds_event_buffering_and_recovers_at_next_event() {
    let mut events = TerminalEvents::new(ProviderStreamProtocol::ChatCompletions);
    for _ in 0..4 {
        events.feed(&vec![b'x'; MAX_EVENT_BYTES]);
    }
    assert!(events.line.len() <= MAX_EVENT_BYTES);
    assert!(events.data.len() <= MAX_EVENT_BYTES);
    assert!(!events.terminal);
    events.feed(b"\n\ndata: [DONE]\n\n");
    assert!(events.terminal);
}
#[derive(Clone, Debug, Default)]
struct FakeProviderHttp {
    terminal: bool,
}

impl rig::http_client::HttpClientExt for FakeProviderHttp {
    fn send<T, U>(
        &self,
        _req: http_client::Request<T>,
    ) -> impl std::future::Future<
        Output = http_client::Result<http_client::Response<http_client::LazyBody<U>>>,
    > + rig::wasm_compat::WasmCompatSend
           + 'static
    where
        T: Into<Bytes> + rig::wasm_compat::WasmCompatSend,
        U: From<Bytes> + rig::wasm_compat::WasmCompatSend + 'static,
    {
        async { Err(http_client::Error::StreamEnded) }
    }
    fn send_multipart<U>(
        &self,
        _req: http_client::Request<http_client::MultipartForm>,
    ) -> impl std::future::Future<
        Output = http_client::Result<http_client::Response<http_client::LazyBody<U>>>,
    > + rig::wasm_compat::WasmCompatSend
           + 'static
    where
        U: From<Bytes> + rig::wasm_compat::WasmCompatSend + 'static,
    {
        async { Err(http_client::Error::StreamEnded) }
    }
    fn send_streaming<T>(
        &self,
        _req: http_client::Request<T>,
    ) -> impl std::future::Future<Output = http_client::Result<StreamingResponse>>
           + rig::wasm_compat::WasmCompatSend
    where
        T: Into<Bytes>,
    {
        let terminal = self.terminal;
        async move {
            let mut chunks = vec![Ok(Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n"))];
            if terminal {
                chunks.push(Ok(Bytes::from_static(b"data: [DONE]\n\n")));
            }
            let mut response = response(chunks, 200);
            response.headers_mut().insert(
                "content-type",
                http_client::HeaderValue::from_static("text/event-stream"),
            );
            Ok(response)
        }
    }
}

async fn first_rig_terminal<H>(http: H) -> Result<(), String>
where
    H: Default + rig::http_client::HttpClientExt + std::fmt::Debug + Clone + Send + Sync + 'static,
{
    use rig::{client::CompletionClient, completion::CompletionModel};
    let client = crate::inference_http::build_openai_chat_completions_client(
        "test",
        "https://provider.invalid/v1",
        http,
    )
    .unwrap();
    let model = client.completion_model("test");
    let mut stream = model.completion_request("hello").stream().await.unwrap();
    while let Some(item) = stream.next().await {
        match item {
            Err(error) => return Err(error.to_string()),
            Ok(rig::streaming::StreamedAssistantContent::Final(_)) => return Ok(()),
            _ => {}
        }
    }
    panic!("rig stream produced neither error nor final response");
}

#[tokio::test]
async fn provider_stream_guard_prevents_rig_synthesizing_success_at_raw_http_eof() {
    // Reproduce the adapter behavior this seam must fence.
    assert!(first_rig_terminal(FakeProviderHttp { terminal: false })
        .await
        .is_ok());
    let guarded =
        crate::rendered_request::RenderedRequestCapturingHttpClient::new(FakeProviderHttp {
            terminal: false,
        });
    let error = first_rig_terminal(guarded).await.unwrap_err();
    assert!(
        error.contains("without an explicit protocol terminal event"),
        "{error}"
    );
    let guarded =
        crate::rendered_request::RenderedRequestCapturingHttpClient::new(FakeProviderHttp {
            terminal: true,
        });
    assert!(first_rig_terminal(guarded).await.is_ok());
}
