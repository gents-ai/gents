use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gents_protocol::message::Message;

use crate::backend_provider::BackendProviderKind;
use crate::completion_retry::CompletionRetryPolicy;
use crate::error::InferenceError;
use crate::loop_stream::{run_loop_stream, LoopConfig, LoopStreamItem, TaggedMessage};
use crate::openai_wire::OpenAiWireApi;
use crate::provider_input::ProviderInputCounter;
use crate::rendered_request::scope::{arming_sink, test_scope, CaptureScopeKind};
use crate::rendered_request::{RenderedRequestCaptureSink, RenderedRequestContext};
use crate::session_hook::NoopSessionHook;
use crate::test_support::doubles::{ScriptedWire, WireAttempt, WireModel};

const IDLE: Duration = Duration::from_secs(30);

fn config(retry_policy: CompletionRetryPolicy) -> LoopConfig {
    LoopConfig {
        provider_call: Default::default(),
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
        configuration_guard: None,
        tool_rebinding: None,
        on_rendered_request: Some(arming_sink(CaptureScopeKind::Inference)),
        turn_compactor: None,
        active_reduction_keys: Vec::new(),
        reduction_chain_keys: Vec::new(),
        initial_turn_index: 0,
        resume_from_history: false,
        context_window: 128_000,
        compaction_threshold: 0.75,
        context_calibration: Default::default(),
        retry_policy,
        provider_idle_timeout: Some(IDLE),
        deadline: None,
        max_turns: 4,
        max_invalid_tool_calls: crate::loop_stream::DEFAULT_MAX_INVALID_TOOL_CALLS,
        repeated_tool_failure_limit: crate::loop_stream::DEFAULT_REPEATED_TOOL_FAILURE_LIMIT,
        output_obligation_gate: None,
        folded_prompts: Vec::new(),
        authored_input: None,
        tool_context: crate::tool::ToolCallEnvelope::default(),
    }
}

fn context() -> RenderedRequestContext {
    RenderedRequestContext {
        request_doc_id: "doc-1".to_string(),
        request_commit_cid: "bafy-request-commit".to_string(),
        request_id: "req-1".to_string(),
        node_did: "did:key:agent".to_string(),
        requester_did: "did:key:requester".to_string(),
        agent_did: "behavior".to_string(),
        session_id: "session".to_string(),
        model_name: "configured-model".to_string(),
        provider_family: None,
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
    let observed = async move { let capture = test_scope(context(), accepting_sink());
        let stream = run_loop_stream::<_, NoopSessionHook>(
            model,
            None,
            TaggedMessage::unassociated(Message::user("hello")),
            Vec::new(),
            Arc::new(Vec::new()),
            {
                let mut config = config(policy);
                config.provider_call.capture = Some(capture);
                config
            },
        );
        futures::pin_mut!(stream);
        let mut observed = Vec::new();
        while let Some(item) = stream.next().await {
            let bodies_dropped = dropped.load(Ordering::SeqCst);
            match item {
                Ok(LoopStreamItem::Text(text)) => observed.push(Observed::Text(text)),
                Ok(LoopStreamItem::ToolCall { .. }) => observed.push(Observed::ToolCall),
                Ok(LoopStreamItem::Final { text }) => observed.push(Observed::Final(text)),
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
    }
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
