use super::*;
use crate::rendered_request::scope::{
    ambient_arming_sink, scope_request, test_scope, CaptureScopeKind,
};
use crate::rendered_request::transport::RenderedRequestCapturingHttpClient;
use bytes::Bytes;
use futures::StreamExt;
use rig::client::CompletionClient;
use rig::http_client::{
    self, HttpClientExt, LazyBody, MultipartForm, Request, Response, StreamingResponse,
};
use rig::wasm_compat::WasmCompatSend;

#[derive(Clone, Debug, Default)]
struct ProviderWire {
    bodies: Arc<Mutex<std::collections::VecDeque<String>>>,
}

impl HttpClientExt for ProviderWire {
    fn send<T, U>(
        &self,
        _: Request<T>,
    ) -> impl std::future::Future<Output = http_client::Result<Response<LazyBody<U>>>>
           + WasmCompatSend
           + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        async { unreachable!("streaming provider") }
    }

    fn send_multipart<U>(
        &self,
        _: Request<MultipartForm>,
    ) -> impl std::future::Future<Output = http_client::Result<Response<LazyBody<U>>>>
           + WasmCompatSend
           + 'static
    where
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        async { unreachable!("streaming provider") }
    }

    fn send_streaming<T>(
        &self,
        _: Request<T>,
    ) -> impl std::future::Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes>,
    {
        let payload = self
            .bodies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected provider call");
        async move {
            let chunks = payload
                .as_bytes()
                .chunks(7)
                .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
                .collect::<Vec<_>>();
            let body: http_client::sse::BoxedStream = Box::pin(futures::stream::iter(chunks));
            Ok(Response::builder()
                .status(200)
                .header("content-type", "text/event-stream")
                .body(body)
                .unwrap())
        }
    }
}

fn tool_turn(reason: Option<&str>) -> String {
    let call = serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call","type":"function","function":{"name":"echo","arguments":"{\"text\":\"unfinished"}}]},"finish_reason":null}]});
    let finish = serde_json::json!({"choices":[{"delta":{},"finish_reason":reason}]});
    format!("data: {call}\n\ndata: {finish}\n\ndata: [DONE]\n\n")
}

fn capture_scope() -> Arc<crate::rendered_request::scope::RequestCaptureScope> {
    test_scope(
        crate::rendered_request::RenderedRequestContext {
            request_doc_id: "doc".into(),
            request_commit_cid: "commit".into(),
            request_id: "request".into(),
            node_did: "did:test:node".into(),
            requester_did: "did:test:requester".into(),
            agent_id: "agent".into(),
            session_id: "session".into(),
            model_name: "test-model".into(),
            provider_family: None,
        },
        Arc::new(|_| Box::pin(async { Ok(()) })),
    )
}

#[tokio::test]
async fn owned_loop_uses_each_turns_wire_finish_reason_for_tool_errors() {
    let wire = ProviderWire {
        bodies: Arc::new(Mutex::new([
            tool_turn(Some("length")),
            tool_turn(Some("stop")),
            tool_turn(None),
            "data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".into(),
        ].into())),
    };
    let client = rig::providers::openai::CompletionsClient::builder()
        .api_key("test")
        .base_url("https://provider.invalid/v1")
        .http_client(RenderedRequestCapturingHttpClient::new(wire.clone()))
        .build()
        .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(vec![Box::new(EchoTool {
        calls: calls.clone(),
        policy_allows: true,
    })]);
    let hook = RecordingHook::default();
    let mut config = test_loop_config();
    config.on_rendered_request = Some(ambient_arming_sink(CaptureScopeKind::Inference));
    scope_request(capture_scope(), async {
        let stream = crate::loop_stream::run_loop_stream(
            client.completion_model("test-model"),
            Some(hook.clone()),
            TaggedMessage::unassociated(Message::user("echo")),
            Vec::new(),
            tools,
            config,
        );
        futures::pin_mut!(stream);
        while let Some(item) = stream.next().await {
            use crate::loop_stream::{FinishOrIntake, LoopStreamItem};
            match item.unwrap() {
                LoopStreamItem::AuthoredInputReady { reply, .. }
                | LoopStreamItem::SteeringBoundary { reply } => reply.send(Ok(Vec::new())).unwrap(),
                LoopStreamItem::FinishOrIntake { reply, .. } => {
                    reply.send(Ok(FinishOrIntake::Finished)).unwrap()
                }
                _ => {}
            }
        }
    })
    .await;
    let outcomes = hook
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            RecordedCall::ToolResult { outcome, .. } => Some(outcome),
            _ => None,
        })
        .collect::<Vec<_>>();
    let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../../gents/tests/fixtures/tool_argument_failure.json"
    ))
    .unwrap();
    for (outcome, termination) in outcomes
        .iter()
        .zip(["token_limit", "completed", "unreported"])
    {
        let case = cases
            .iter()
            .find(|case| {
                case["termination"] == termination && case["arguments"] == "{\"text\":\"unfinished"
            })
            .unwrap();
        assert!(
            outcome.contains(&format!("({})", case["kind"].as_str().unwrap())),
            "{termination}: {outcome}"
        );
    }
    assert_eq!(outcomes.len(), 3);
    assert!(calls.lock().unwrap().is_empty());
    assert!(wire.bodies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn late_token_limit_from_superseded_attempt_does_not_change_retry() {
    use crate::provider_stream::{guard_response, ProviderStreamProtocol};
    use crate::rendered_request::scope::{arm, attempt_activity};
    use crate::rendered_request::{AssemblyBuildPath, AssemblyTrace};

    scope_request(capture_scope(), async {
        let trace =
            || AssemblyTrace::from_effective_messages(AssemblyBuildPath::Budgeted, Vec::new());
        arm(CaptureScopeKind::Inference, 0, 0, trace());
        let old = attempt_activity(0, 0).unwrap();
        let body: http_client::sse::BoxedStream = Box::pin(futures::stream::iter([Ok(
            Bytes::from_static(b"data: {\"choices\":[{\"finish_reason\":\"length\"}]}\n\n"),
        )]));
        let response = Response::builder().status(200).body(body).unwrap();
        let stale = guard_response(
            response,
            Some(ProviderStreamProtocol::ChatCompletions),
            Some(old.clone()),
        );
        arm(CaptureScopeKind::Inference, 0, 1, trace());
        let retry = attempt_activity(0, 1).unwrap();
        assert!(!retry.token_limit_reached());
        for chunk in stale.into_body().collect::<Vec<_>>().await {
            chunk.unwrap();
        }
        assert!(old.token_limit_reached());
        assert!(!retry.token_limit_reached());
        assert!(attempt_activity(0, 0).is_none());
    })
    .await;
}
