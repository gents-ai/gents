use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gents_protocol::message::Message;

use crate::completion_retry::CompletionRetryPolicy;
use crate::error::InferenceError;
use crate::loop_stream::{run_loop_stream, LoopConfig, LoopStreamItem, TaggedMessage};
use crate::rendered_request::scope::{arming_sink, CaptureScopeKind};
use crate::session_hook::NoopSessionHook;
use crate::test_support::doubles::{test_capture_scope, ScriptedWire, WireAttempt, WireModel};

const IDLE: Duration = Duration::from_secs(30);

fn config(retry_policy: CompletionRetryPolicy) -> LoopConfig {
    let mut config = crate::end_to_end_test::test_loop_config();
    config.preamble = None;
    config.on_rendered_request = Some(arming_sink(CaptureScopeKind::Inference));
    config.retry_policy = retry_policy;
    config.provider_idle_timeout = Some(IDLE);
    config.max_turns = 4;
    config.provider_call.capture = Some(test_capture_scope());
    config
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
    Run {
        observed,
        elapsed: started.elapsed(),
        sends: wire.sends.load(Ordering::SeqCst),
    }
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
async fn silence_after_or_before_headers_fails_within_idle_window_and_retries() {
    let expected = vec![
        Observed::AttemptFailed {
            attempt: 0,
            timed_out: true,
            will_retry: true,
            bodies_dropped: 1,
        },
        Observed::Text("recovered".into()),
        Observed::Final("recovered".into()),
    ];
    for (name, stalled) in [
        ("silence after headers", WireAttempt::default().hangs()),
        (
            "headers never arrive",
            WireAttempt {
                hold_headers: true,
                ..WireAttempt::default()
            },
        ),
    ] {
        let run = run(
            vec![stalled, WireAttempt::completes("recovered")],
            CompletionRetryPolicy::interactive_default(),
        )
        .await;
        assert_eq!(run.observed, expected, "{name}");
        assert_eq!(run.sends, 2, "{name}");
        assert!(
            run.elapsed >= IDLE && run.elapsed < IDLE * 2,
            "{name}: {:?}",
            run.elapsed
        );
    }
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
