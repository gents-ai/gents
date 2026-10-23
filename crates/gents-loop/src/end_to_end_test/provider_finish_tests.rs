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

#[expect(
    clippy::manual_async_fn,
    reason = "the trait requires a 'static future, which an async fn borrowing &self cannot provide"
)]
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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponsesArgs {
    text: String,
    #[allow(dead_code)]
    mode: Option<ResponsesMode>,
}

#[derive(serde::Deserialize)]
enum ResponsesMode {
    Normal,
}

struct ResponsesTool(Arc<Mutex<Vec<String>>>);

impl Tool for ResponsesTool {
    const NAME: &'static str = "echo";
    type Error = crate::tool::ToolError;
    type Args = ResponsesArgs;
    type Output = String;

    async fn definition(&self, _: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.into(),
            description: String::new(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["text"],
                "properties": {
                    "text": {"type": "string"},
                    "mode": {"type": "string", "enum": ["Normal"]}
                },
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<String, Self::Error> {
        self.0.lock().unwrap().push(args.text.clone());
        Ok(args.text.to_uppercase())
    }
}

fn responses_turn(
    kind: &str,
    reason: Option<&str>,
    arguments: Option<&str>,
    item_done: bool,
) -> String {
    let call = arguments.map(|arguments| {
        serde_json::json!({
            "type": "function_call",
            "id": "fc_1",
            "call_id": "call_1",
            "name": "echo",
            "arguments": arguments,
            "status": if kind == "response.incomplete" {"incomplete"} else {"completed"}
        })
    });
    let mut events = Vec::new();
    if let Some(call) = &call {
        let mut added = call.clone();
        added["arguments"] = serde_json::json!("");
        added["status"] = serde_json::json!("in_progress");
        events.push(serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "sequence_number": 1,
            "item": added
        }));
        events.push(serde_json::json!({
            "type": "response.function_call_arguments.delta",
            "output_index": 0,
            "sequence_number": 2,
            "item_id": "fc_1",
            "delta": arguments.unwrap()
        }));
        if item_done {
            events.push(serde_json::json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "sequence_number": 3,
                "item": call
            }));
        }
    } else {
        events.push(serde_json::json!({
            "type": "response.output_text.delta",
            "item_id": "msg_1",
            "output_index": 0,
            "content_index": 0,
            "sequence_number": 1,
            "delta": "done"
        }));
    }
    if !kind.is_empty() {
        let status = match kind {
            "response.incomplete" => "incomplete",
            "response.failed" => "failed",
            _ => "completed",
        };
        events.push(serde_json::json!({
            "type": kind,
            "sequence_number": 4,
            "response": {
                "id": "resp_1",
                "object": "response",
                "created_at": 1,
                "status": status,
                "error": null,
                "incomplete_details": reason.map(|reason| serde_json::json!({"reason": reason})),
                "instructions": null,
                "max_output_tokens": 20,
                "model": "test-model",
                "output": call.into_iter().collect::<Vec<_>>(),
                "usage": {
                    "input_tokens": 10,
                    "input_tokens_details": {"cached_tokens": 2},
                    "output_tokens": 5,
                    "output_tokens_details": {"reasoning_tokens": 1},
                    "total_tokens": 15
                }
            }
        }));
    }
    events
        .into_iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}

async fn responses_outcomes(turns: Vec<String>) -> (Vec<String>, Vec<String>, Option<String>) {
    let wire = ProviderWire {
        bodies: Arc::new(Mutex::new(turns.into())),
    };
    let client = rig::providers::openai::Client::builder()
        .api_key("test")
        .base_url("https://provider.invalid/v1")
        .http_client(RenderedRequestCapturingHttpClient::new(wire))
        .build()
        .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tools: Arc<Vec<Box<dyn ToolDyn>>> = Arc::new(vec![Box::new(ResponsesTool(calls.clone()))]);
    let hook = RecordingHook::default();
    let mut config = test_loop_config();
    config.retry_policy = crate::completion_retry::CompletionRetryPolicy::no_retry();
    config.provider_input_counter = Arc::new(crate::provider_input::ProviderInputCounter::new(
        BackendProviderKind::OpenAiCompatible,
        OpenAiWireApi::Responses,
        "test-model",
    ));
    config.on_rendered_request = Some(ambient_arming_sink(CaptureScopeKind::Inference));
    let mut accepted_turns = 0;
    let mut final_answers = 0;
    let error = scope_request(capture_scope(), async {
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
            let item = match item {
                Ok(item) => item,
                Err(error) => return Some(error.to_string()),
            };
            match item {
                LoopStreamItem::AuthoredInputReady { reply, .. }
                | LoopStreamItem::SteeringBoundary { reply } => reply.send(Ok(Vec::new())).unwrap(),
                LoopStreamItem::FinishOrIntake { reply, .. } => {
                    reply.send(Ok(FinishOrIntake::Finished)).unwrap()
                }
                LoopStreamItem::ProviderTurnReady { .. } => accepted_turns += 1,
                LoopStreamItem::Final { .. } => final_answers += 1,
                _ => {}
            }
        }
        None
    })
    .await;
    if error.is_some() {
        assert_eq!(accepted_turns, 0);
        assert_eq!(final_answers, 0);
    }
    let outcomes = hook
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            RecordedCall::ToolResult { outcome, .. } => Some(outcome),
            _ => None,
        })
        .collect();
    let executed = calls.lock().unwrap().clone();
    (outcomes, executed, error)
}

#[tokio::test]
async fn responses_token_limit_arguments_reach_owned_dispatch() {
    let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../../gents/tests/fixtures/tool_argument_failure.json"
    ))
    .unwrap();
    for case in cases
        .iter()
        .filter(|case| case["termination"] == "token_limit")
    {
        for item_done in [false, true] {
            let (outcomes, calls, error) = responses_outcomes(vec![
                responses_turn(
                    "response.incomplete",
                    Some("max_output_tokens"),
                    case["arguments"].as_str(),
                    item_done,
                ),
                responses_turn("response.completed", None, None, false),
            ])
            .await;
            assert!(error.is_none(), "{case} done={item_done}: {error:?}");
            assert!(calls.is_empty());
            assert_eq!(outcomes.len(), 1);
            assert!(
                outcomes[0].contains(&format!("({})", case["kind"].as_str().unwrap())),
                "{case}: {outcomes:?}"
            );
        }
    }
}

#[tokio::test]
async fn responses_token_limit_does_not_change_valid_calls_or_later_turns() {
    let (outcomes, calls, error) = responses_outcomes(vec![
        responses_turn(
            "response.incomplete",
            Some("max_output_tokens"),
            Some(r#"{"text":"ok"}"#),
            false,
        ),
        responses_turn(
            "response.completed",
            None,
            Some(r#"{"text":"unfinished"#),
            true,
        ),
        responses_turn("response.completed", None, None, false),
    ])
    .await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(calls, ["ok"]);
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes[0].contains("OK"));
    let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../../gents/tests/fixtures/tool_argument_failure.json"
    ))
    .unwrap();
    let case = cases
        .iter()
        .find(|case| {
            case["termination"] == "completed" && case["arguments"] == r#"{"text":"unfinished"#
        })
        .unwrap();
    assert!(
        outcomes[1].contains(&format!("({})", case["kind"].as_str().unwrap())),
        "{:?}",
        outcomes
    );
}

#[tokio::test]
async fn responses_other_incomplete_reasons_and_missing_terminal_remain_errors() {
    for reason in [
        Some("content_filter"),
        Some("max_messages"),
        Some("steered"),
        Some("unknown"),
        None,
    ] {
        for item_done in [false, true] {
            let (outcomes, calls, error) = responses_outcomes(vec![responses_turn(
                "response.incomplete",
                reason,
                Some(r#"{"text":"ok"}"#),
                item_done,
            )])
            .await;
            assert!(error.is_some(), "{reason:?} done={item_done}");
            assert!(outcomes.is_empty());
            assert!(calls.is_empty());
        }
    }
    let undelimited = responses_turn(
        "response.incomplete",
        Some("max_output_tokens"),
        Some(r#"{"text":"ok"}"#),
        true,
    )
    .trim_end_matches('\n')
    .to_owned();
    for response in [
        responses_turn("response.failed", None, Some(r#"{"text":"ok"}"#), true),
        responses_turn("", None, Some(r#"{"text":"ok"}"#), true),
        undelimited,
    ] {
        let (outcomes, calls, error) = responses_outcomes(vec![response]).await;
        assert!(error.is_some());
        assert!(outcomes.is_empty());
        assert!(calls.is_empty());
    }
}
