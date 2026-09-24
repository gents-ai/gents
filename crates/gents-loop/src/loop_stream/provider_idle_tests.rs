use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use gents_protocol::message::Message;
use rig::agent::MultiTurnStreamItem;
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::streaming::{RawStreamingChoice, StreamedAssistantContent, StreamingCompletionResponse};

use crate::backend_provider::BackendProviderKind;
use crate::completion_retry::CompletionRetryPolicy;
use crate::error::InferenceError;
use crate::loop_stream::{run_loop_stream, LoopConfig, LoopStreamItem};
use crate::openai_wire::OpenAiWireApi;
use crate::provider_input::ProviderInputCounter;
use crate::session_hook::NoopSessionHook;

const IDLE: Duration = Duration::from_secs(30);

/// One provider attempt: the choices it emits, then whether it stalls with
/// the connection held open instead of ending.
#[derive(Clone)]
struct Attempt {
    choices: Vec<RawStreamingChoice<()>>,
    stall: bool,
}

impl Attempt {
    fn stalls_after(choices: Vec<RawStreamingChoice<()>>) -> Self {
        Self {
            choices,
            stall: true,
        }
    }

    fn completes(text: &str) -> Self {
        Self {
            choices: vec![
                RawStreamingChoice::Message(text.to_string()),
                RawStreamingChoice::FinalResponse(()),
            ],
            stall: false,
        }
    }
}

#[derive(Clone)]
struct StallingModel {
    attempts: Arc<Mutex<Vec<Attempt>>>,
    calls: Arc<AtomicUsize>,
}

impl StallingModel {
    fn new(attempts: Vec<Attempt>) -> Self {
        Self {
            attempts: Arc::new(Mutex::new(attempts.into_iter().rev().collect())),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[allow(refining_impl_trait)]
impl CompletionModel for StallingModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_client: &Self::Client, _model: impl Into<String>) -> Self {
        Self::new(Vec::new())
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
        self.calls.fetch_add(1, Ordering::SeqCst);
        let attempt = self
            .attempts
            .lock()
            .unwrap()
            .pop()
            .expect("provider called more times than scripted");
        let emitted = futures::stream::iter(attempt.choices.into_iter().map(Ok));
        let tail = if attempt.stall {
            futures::stream::pending().boxed()
        } else {
            futures::stream::empty().boxed()
        };
        Ok(StreamingCompletionResponse::stream(Box::pin(
            emitted.chain(tail),
        )))
    }
}

fn config(retry_policy: CompletionRetryPolicy) -> LoopConfig {
    LoopConfig {
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
        on_rendered_request: None,
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

#[derive(Debug, PartialEq)]
enum Observed {
    Text(String),
    AttemptFailed {
        attempt: u32,
        timed_out: bool,
        will_retry: bool,
    },
    Retracted {
        attempt: u32,
    },
    Final(String),
    Failed(String),
}

async fn run(model: StallingModel, policy: CompletionRetryPolicy) -> (Vec<Observed>, Duration) {
    let started = tokio::time::Instant::now();
    let stream = run_loop_stream::<_, NoopSessionHook>(
        model,
        None,
        Message::user("hello"),
        Vec::new(),
        Arc::new(Vec::new()),
        config(policy),
    );
    futures::pin_mut!(stream);
    let mut observed = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(LoopStreamItem::Item(MultiTurnStreamItem::StreamAssistantItem(
                StreamedAssistantContent::Text(text),
            ))) => observed.push(Observed::Text(text.text)),
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
                timed_out: matches!(error, InferenceError::Timeout { timeout_secs } if timeout_secs == IDLE.as_secs()),
                will_retry,
            }),
            Ok(LoopStreamItem::TurnRetracted { attempt, .. }) => {
                observed.push(Observed::Retracted { attempt })
            }
            Ok(_) => {}
            Err(error) => observed.push(Observed::Failed(error.to_string())),
        }
    }
    (observed, started.elapsed())
}

#[tokio::test(start_paused = true)]
async fn silent_stream_before_first_item_fails_the_attempt_and_retries() {
    let model = StallingModel::new(vec![
        Attempt::stalls_after(Vec::new()),
        Attempt::completes("recovered"),
    ]);
    let calls = model.calls.clone();
    let (observed, elapsed) = run(model, CompletionRetryPolicy::interactive_default()).await;
    assert_eq!(
        observed,
        vec![
            Observed::AttemptFailed {
                attempt: 0,
                timed_out: true,
                will_retry: true,
            },
            Observed::Text("recovered".into()),
            Observed::Final("recovered".into()),
        ]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(elapsed >= IDLE && elapsed < IDLE * 2, "{elapsed:?}");
}

#[tokio::test(start_paused = true)]
async fn silence_between_items_retracts_the_partial_turn_and_resamples() {
    let model = StallingModel::new(vec![
        Attempt::stalls_after(vec![RawStreamingChoice::Message("partial".into())]),
        Attempt::completes("recovered"),
    ]);
    let calls = model.calls.clone();
    let (observed, elapsed) = run(model, CompletionRetryPolicy::interactive_default()).await;
    assert_eq!(
        observed,
        vec![
            Observed::Text("partial".into()),
            Observed::Retracted { attempt: 0 },
            Observed::Text("recovered".into()),
            Observed::Final("recovered".into()),
        ]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(elapsed >= IDLE && elapsed < IDLE * 2, "{elapsed:?}");
}

#[tokio::test(start_paused = true)]
async fn repeated_silence_exhausts_the_retry_owner_instead_of_hanging() {
    let model = StallingModel::new(vec![
        Attempt::stalls_after(Vec::new()),
        Attempt::stalls_after(vec![RawStreamingChoice::Message("partial".into())]),
    ]);
    let calls = model.calls.clone();
    let (observed, elapsed) = run(model, CompletionRetryPolicy::interactive_default()).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        &observed[..2],
        &[
            Observed::AttemptFailed {
                attempt: 0,
                timed_out: true,
                will_retry: true,
            },
            Observed::Text("partial".into()),
        ]
    );
    assert!(
        matches!(observed.last(), Some(Observed::Failed(reason)) if reason.contains("transport retry budget exhausted")),
        "{observed:?}"
    );
    assert!(elapsed >= IDLE * 2 && elapsed < IDLE * 3, "{elapsed:?}");
}

#[tokio::test(start_paused = true)]
async fn stall_without_retry_budget_fails_on_first_expiry() {
    let model = StallingModel::new(vec![Attempt::stalls_after(Vec::new())]);
    let (observed, elapsed) = run(model, CompletionRetryPolicy::no_retry()).await;
    assert_eq!(
        observed[0],
        Observed::AttemptFailed {
            attempt: 0,
            timed_out: true,
            will_retry: false,
        }
    );
    assert!(matches!(observed.last(), Some(Observed::Failed(_))));
    assert!(elapsed >= IDLE && elapsed < IDLE + Duration::from_secs(1));
}
