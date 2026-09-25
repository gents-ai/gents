//! The idle window measured through the real capturing transport: a scripted
//! wire sits under `RenderedRequestCapturingHttpClient`, and a test decoder
//! turns its Anthropic-shaped SSE into loop items the way a provider client
//! does, including events that decode to no item.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use gents_protocol::message::Message;
use rig::agent::MultiTurnStreamItem;
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::http_client::{
    self, HttpClientExt, LazyBody, MultipartForm, Request, Response, StreamingResponse,
};
use rig::streaming::{
    RawStreamingChoice, RawStreamingToolCall, StreamedAssistantContent, StreamingCompletionResponse,
};
use rig::wasm_compat::WasmCompatSend;

use crate::backend_provider::BackendProviderKind;
use crate::completion_retry::CompletionRetryPolicy;
use crate::error::InferenceError;
use crate::loop_stream::{run_loop_stream, LoopConfig, LoopStreamItem, TaggedMessage};
use crate::openai_wire::OpenAiWireApi;
use crate::provider_input::ProviderInputCounter;
use crate::rendered_request::scope::{
    ambient_arming_sink, scope_request, test_scope, CaptureScopeKind,
};
use crate::rendered_request::transport::RenderedRequestCapturingHttpClient;
use crate::rendered_request::{RenderedRequestCaptureSink, RenderedRequestContext};
use crate::session_hook::NoopSessionHook;

const IDLE: Duration = Duration::from_secs(30);

/// One provider attempt on the wire.
#[derive(Clone, Default)]
struct WireAttempt {
    /// Response headers never arrive.
    hold_headers: bool,
    /// `(delay before this event, SSE data JSON)`.
    events: Vec<(Duration, String)>,
    /// After `events`, keep the connection open and send nothing.
    hang: bool,
    /// Delay between the transport's terminal event and the decoder yielding
    /// `Final`: a durable finalization write under persist-before-publish.
    finalize_delay: Duration,
}

impl WireAttempt {
    fn event(mut self, after: Duration, data: serde_json::Value) -> Self {
        self.events.push((after, data.to_string()));
        self
    }

    fn text(self, after: Duration, text: &str) -> Self {
        self.event(
            after,
            serde_json::json!({"type": "text_delta", "text": text}),
        )
    }

    fn stop(self, after: Duration) -> Self {
        self.event(after, serde_json::json!({"type": "message_stop"}))
    }

    fn completes(text: &str) -> Self {
        Self::default()
            .text(Duration::ZERO, text)
            .stop(Duration::ZERO)
    }

    fn hangs(mut self) -> Self {
        self.hang = true;
        self
    }
}

struct DropCounter(Arc<AtomicUsize>);

impl Drop for DropCounter {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Clone, Debug, Default)]
struct ScriptedWire {
    attempts: Arc<Mutex<VecDeque<WireAttempt>>>,
    sends: Arc<AtomicUsize>,
    dropped_bodies: Arc<AtomicUsize>,
}

impl std::fmt::Debug for WireAttempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WireAttempt")
    }
}

impl ScriptedWire {
    fn new(attempts: Vec<WireAttempt>) -> Self {
        Self {
            attempts: Arc::new(Mutex::new(attempts.into())),
            ..Self::default()
        }
    }
}

impl HttpClientExt for ScriptedWire {
    fn send<T, U>(
        &self,
        _req: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        async { unreachable!("the owned loop only streams") }
    }

    fn send_multipart<U>(
        &self,
        _req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        async { unreachable!("the owned loop only streams") }
    }

    fn send_streaming<T>(
        &self,
        _req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes>,
    {
        self.sends.fetch_add(1, Ordering::SeqCst);
        let attempt = self
            .attempts
            .lock()
            .unwrap()
            .pop_front()
            .expect("provider called more times than scripted");
        let dropped = DropCounter(self.dropped_bodies.clone());
        async move {
            if attempt.hold_headers {
                let _dropped = dropped;
                std::future::pending::<()>().await;
                unreachable!("held headers never arrive");
            }
            let hang = attempt.hang;
            let body = async_stream::stream! {
                let _dropped = dropped;
                for (after, data) in attempt.events {
                    tokio::time::sleep(after).await;
                    yield Ok::<Bytes, http_client::Error>(Bytes::from(format!("data: {data}\n\n")));
                }
                if hang {
                    std::future::pending::<()>().await;
                }
            };
            let body: rig::http_client::sse::BoxedStream = Box::pin(body);
            Ok(Response::builder().status(200).body(body)?)
        }
    }
}

/// Eagerly awaits response headers inside `stream()`, as the Claude Messages
/// client does, then decodes: `text_delta` and `tool_use` become items,
/// `message_stop` becomes `Final`, every other event decodes to nothing.
#[derive(Clone)]
struct WireModel {
    client: RenderedRequestCapturingHttpClient<ScriptedWire>,
    finalize_delays: Arc<Mutex<VecDeque<Duration>>>,
}

impl WireModel {
    fn new(wire: &ScriptedWire, attempts: &[WireAttempt]) -> Self {
        Self {
            client: RenderedRequestCapturingHttpClient::new(wire.clone()),
            finalize_delays: Arc::new(Mutex::new(
                attempts
                    .iter()
                    .map(|attempt| attempt.finalize_delay)
                    .collect(),
            )),
        }
    }
}

#[allow(refining_impl_trait)]
impl CompletionModel for WireModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_client: &Self::Client, _model: impl Into<String>) -> Self {
        unreachable!("constructed directly")
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<()>, CompletionError> {
        unreachable!("the owned loop only ever calls stream()")
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<()>, CompletionError> {
        let finalize_delay = self
            .finalize_delays
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default();
        let body = serde_json::json!({
            "model": "wire-model",
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}],
        });
        let request = Request::builder()
            .method("POST")
            .uri("https://provider.test/v1/messages")
            .body(Bytes::from(body.to_string()))
            .expect("request");
        let response = self.client.send_streaming(request).await?;
        let mut body = response.into_body();
        let items = async_stream::stream! {
            let mut buffer = Vec::new();
            while let Some(chunk) = body.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        yield Err(CompletionError::HttpError(error));
                        return;
                    }
                };
                buffer.extend_from_slice(&chunk);
                while let Some(end) = buffer.windows(2).position(|window| window == b"\n\n") {
                    let event: Vec<u8> = buffer.drain(..end + 2).collect();
                    let data = std::str::from_utf8(&event).unwrap().trim();
                    let value: serde_json::Value =
                        serde_json::from_str(data.strip_prefix("data: ").unwrap()).unwrap();
                    match value["type"].as_str() {
                        Some("text_delta") => yield Ok(RawStreamingChoice::Message(
                            value["text"].as_str().unwrap().to_string(),
                        )),
                        Some("tool_use") => yield Ok(RawStreamingChoice::ToolCall(
                            RawStreamingToolCall {
                                id: "call_1".into(),
                                internal_call_id: "call_1".into(),
                                call_id: Some("call_1".into()),
                                name: "echo".into(),
                                arguments: serde_json::json!({"text": "hi"}),
                                signature: None,
                                additional_params: None,
                            },
                        )),
                        Some("message_stop") => {
                            tokio::time::sleep(finalize_delay).await;
                            yield Ok(RawStreamingChoice::FinalResponse(()));
                        }
                        _ => {}
                    }
                }
            }
        };
        Ok(StreamingCompletionResponse::stream(Box::pin(items)))
    }
}

fn config(retry_policy: CompletionRetryPolicy) -> LoopConfig {
    LoopConfig {
        replay: crate::loop_stream::LoopReplayInput::default(),
        provider_input_counter: Arc::new(ProviderInputCounter::new(
            BackendProviderKind::OpenAiCompatible,
            OpenAiWireApi::ChatCompletions,
            "test-model",
        )),
        preamble: None,
        context_message: None,
        temperature: None,
        max_tokens: None,
        aggregate_token_budget: None,
        additional_params: None,
        structured_output: None,
        tool_choice: None,
        on_rendered_request: Some(ambient_arming_sink(CaptureScopeKind::Inference)),
        turn_compactor: None,
        active_reduction_keys: Vec::new(),
        reduction_chain_keys: Vec::new(),
        initial_turn_index: 0,
        context_window: 128_000,
        compaction_threshold: 0.75,
        retry_policy,
        provider_idle_timeout: Some(IDLE),
        deadline: None,
        max_turns: 4,
        output_obligation_gate: None,
    }
}

fn context() -> RenderedRequestContext {
    RenderedRequestContext {
        request_doc_id: "doc-1".to_string(),
        request_commit_cid: "bafy-request-commit".to_string(),
        request_id: "req-1".to_string(),
        agent_did: "did:key:agent".to_string(),
        requester_did: "did:key:requester".to_string(),
        behavior_id: "behavior".to_string(),
        session_id: "session".to_string(),
        model_name: "configured-model".to_string(),
    }
}

fn accepting_sink() -> RenderedRequestCaptureSink {
    Arc::new(|_| Box::pin(async { Ok(()) }))
}

#[derive(Debug, PartialEq)]
enum Observed {
    Text(String),
    ToolCall,
    AttemptFailed {
        attempt: u32,
        timed_out: bool,
        will_retry: bool,
        /// Stalled attempt bodies already dropped when the failure surfaced.
        bodies_dropped: usize,
    },
    Retracted {
        attempt: u32,
        bodies_dropped: usize,
    },
    Final(String),
    Failed(String),
}

struct Run {
    observed: Vec<Observed>,
    elapsed: Duration,
    sends: usize,
}

async fn run(attempts: Vec<WireAttempt>, policy: CompletionRetryPolicy) -> Run {
    let wire = ScriptedWire::new(attempts.clone());
    let model = WireModel::new(&wire, &attempts);
    let dropped = wire.dropped_bodies.clone();
    let started = tokio::time::Instant::now();
    let observed = scope_request(test_scope(context(), accepting_sink()), async move {
        let stream = run_loop_stream::<_, NoopSessionHook>(
            model,
            None,
            TaggedMessage::unassociated(Message::user("hello")),
            Vec::new(),
            Arc::new(Vec::new()),
            config(policy),
        );
        futures::pin_mut!(stream);
        let mut observed = Vec::new();
        while let Some(item) = stream.next().await {
            let bodies_dropped = dropped.load(Ordering::SeqCst);
            match item {
                Ok(LoopStreamItem::Item(MultiTurnStreamItem::StreamAssistantItem(
                    StreamedAssistantContent::Text(text),
                ))) => observed.push(Observed::Text(text.text)),
                Ok(LoopStreamItem::Item(MultiTurnStreamItem::StreamAssistantItem(
                    StreamedAssistantContent::ToolCall { .. },
                ))) => observed.push(Observed::ToolCall),
                Ok(LoopStreamItem::Item(MultiTurnStreamItem::FinalResponse(response))) => {
                    observed.push(Observed::Final(response.response().to_string()))
                }
                Ok(LoopStreamItem::AttemptFailed {
                    attempt,
                    error,
                    will_retry,
                    ..
                }) => observed.push(Observed::AttemptFailed {
                    attempt,
                    timed_out: matches!(error, InferenceError::Timeout { timeout } if timeout == IDLE),
                    will_retry,
                    bodies_dropped,
                }),
                Ok(LoopStreamItem::TurnRetracted { attempt, .. }) => {
                    observed.push(Observed::Retracted {
                        attempt,
                        bodies_dropped,
                    })
                }
                Ok(_) => {}
                Err(error) => observed.push(Observed::Failed(error.to_string())),
            }
        }
        observed
    })
    .await;
    Run {
        observed,
        elapsed: started.elapsed(),
        sends: wire.sends.load(Ordering::SeqCst),
    }
}

fn recovered() -> [Observed; 2] {
    [
        Observed::Text("recovered".into()),
        Observed::Final("recovered".into()),
    ]
}

#[tokio::test(start_paused = true)]
async fn transport_events_without_items_keep_a_long_turn_alive() {
    let mut attempt = WireAttempt::default();
    for index in 0..10 {
        let event = if index % 2 == 0 {
            serde_json::json!({"type": "ping"})
        } else {
            serde_json::json!({"type": "thinking_delta", "thinking": "..."})
        };
        attempt = attempt.event(IDLE / 2, event);
    }
    let attempt = attempt.text(IDLE / 2, "answer").stop(Duration::ZERO);
    let run = run(vec![attempt], CompletionRetryPolicy::no_retry()).await;
    assert_eq!(
        run.observed,
        vec![
            Observed::Text("answer".into()),
            Observed::Final("answer".into())
        ]
    );
    assert_eq!(run.sends, 1);
    assert!(run.elapsed >= IDLE * 5, "{:?}", run.elapsed);
}

#[tokio::test(start_paused = true)]
async fn total_silence_after_headers_fails_the_attempt_and_retries() {
    let run = run(
        vec![
            WireAttempt::default().hangs(),
            WireAttempt::completes("recovered"),
        ],
        CompletionRetryPolicy::interactive_default(),
    )
    .await;
    let mut expected = vec![Observed::AttemptFailed {
        attempt: 0,
        timed_out: true,
        will_retry: true,
        bodies_dropped: 1,
    }];
    expected.extend(recovered());
    assert_eq!(run.observed, expected);
    assert_eq!(run.sends, 2);
    assert!(
        run.elapsed >= IDLE && run.elapsed < IDLE * 2,
        "{:?}",
        run.elapsed
    );
}

#[tokio::test(start_paused = true)]
async fn headers_that_never_arrive_fail_within_the_window() {
    let run = run(
        vec![
            WireAttempt {
                hold_headers: true,
                ..WireAttempt::default()
            },
            WireAttempt::completes("recovered"),
        ],
        CompletionRetryPolicy::interactive_default(),
    )
    .await;
    let mut expected = vec![Observed::AttemptFailed {
        attempt: 0,
        timed_out: true,
        will_retry: true,
        bodies_dropped: 1,
    }];
    expected.extend(recovered());
    assert_eq!(run.observed, expected);
    assert!(
        run.elapsed >= IDLE && run.elapsed < IDLE * 2,
        "{:?}",
        run.elapsed
    );
}

#[tokio::test(start_paused = true)]
async fn silence_after_partial_text_retracts_and_resamples() {
    let run = run(
        vec![
            WireAttempt::default()
                .text(Duration::ZERO, "partial")
                .hangs(),
            WireAttempt::completes("recovered"),
        ],
        CompletionRetryPolicy::interactive_default(),
    )
    .await;
    let mut expected = vec![
        Observed::Text("partial".into()),
        Observed::Retracted {
            attempt: 0,
            bodies_dropped: 1,
        },
    ];
    expected.extend(recovered());
    assert_eq!(run.observed, expected);
}

#[tokio::test(start_paused = true)]
async fn silence_after_a_complete_tool_call_retracts_without_dispatch() {
    let run = run(
        vec![
            WireAttempt::default()
                .event(Duration::ZERO, serde_json::json!({"type": "tool_use"}))
                .hangs(),
            WireAttempt::completes("recovered"),
        ],
        CompletionRetryPolicy::interactive_default(),
    )
    .await;
    let mut expected = vec![
        Observed::ToolCall,
        Observed::Retracted {
            attempt: 0,
            bodies_dropped: 1,
        },
    ];
    expected.extend(recovered());
    assert_eq!(run.observed, expected);
    assert_eq!(run.sends, 2);
}

#[tokio::test(start_paused = true)]
async fn slow_finalization_after_the_terminal_event_is_not_a_stall() {
    let attempt = WireAttempt {
        finalize_delay: IDLE * 3,
        ..WireAttempt::completes("done")
    };
    let run = run(vec![attempt], CompletionRetryPolicy::no_retry()).await;
    assert_eq!(
        run.observed,
        vec![
            Observed::Text("done".into()),
            Observed::Final("done".into())
        ]
    );
    assert!(run.elapsed >= IDLE * 3, "{:?}", run.elapsed);
}

#[tokio::test(start_paused = true)]
async fn repeated_silence_exhausts_the_retry_owner_instead_of_hanging() {
    let run = run(
        vec![
            WireAttempt::default().hangs(),
            WireAttempt::default()
                .text(Duration::ZERO, "partial")
                .hangs(),
        ],
        CompletionRetryPolicy::interactive_default(),
    )
    .await;
    assert_eq!(run.sends, 2);
    assert!(
        matches!(run.observed.last(), Some(Observed::Failed(reason)) if reason.contains("transport retry budget exhausted")),
        "{:?}",
        run.observed
    );
    assert!(
        run.elapsed >= IDLE * 2 && run.elapsed < IDLE * 3,
        "{:?}",
        run.elapsed
    );
}

#[tokio::test(start_paused = true)]
async fn sub_second_windows_are_reported_exactly() {
    let (error, _) = super::ProviderAttemptFailure::Stalled(super::ProviderStall {
        idle: Duration::from_millis(250),
        first_item: true,
    })
    .classify();
    assert_eq!(error.to_string(), "inference timed out after 250ms");
}
