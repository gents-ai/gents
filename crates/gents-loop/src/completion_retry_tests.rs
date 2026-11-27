use std::time::Duration;

use chrono::Utc;

use super::*;

fn transient(reason: &str) -> InferenceError {
    InferenceError::TransientFailure {
        reason: reason.to_string(),
    }
}

/// A failure as the loop classifies it: the report reads the attempt text and
/// the error's own rendering, as the provider text reaches both.
fn classified(error: &InferenceError, text: &str) -> crate::error::ClassifiedFailure {
    crate::error::ClassifiedFailure {
        error: error.clone(),
        report: crate::error::ProviderReport::classify_failure_text(&format!("{text}\n{error}")),
        text: text.to_string(),
    }
}

fn class_of(error: &InferenceError, text: &str) -> FailureClass {
    failure_class(&classified(error, text))
}

fn parse_400_text(tag: &str) -> String {
    format!("BadRequestError: Expecting ',' delimiter [{tag}]: line 1 column 5 (char 4)")
}

#[test]
fn failure_class_parse_signature_overrides_variant() {
    let text = parse_400_text("override");
    assert_eq!(
        class_of(&transient(&text), &text),
        FailureClass::ParseBadRequest
    );
}

#[test]
fn failure_class_parse_signature_can_come_from_classified_reason() {
    let body = r#"{"object":"error","message":"BadRequestError: Error in processing prompt inputs: Expecting value: line 1 column 28 (char 27)","type":"BadRequestError","code":400}"#;
    let classified_reason =
        format!("ProviderError: Invalid status code 400 Bad Request with message: {body}");

    assert_eq!(
        class_of(&transient(&classified_reason), "completion request failed"),
        FailureClass::ParseBadRequest
    );
}

#[test]
fn failure_class_fail_closed_tool_use_is_parse_bad_request() {
    let text = "ProviderError: fail-closed: tool_use observed (bash_unrestricked)";
    assert_eq!(
        class_of(&transient(text), text),
        FailureClass::ParseBadRequest
    );
}

#[test]
fn failure_class_reasoning_rejections_are_their_own_class() {
    for text in [
        "400 invalid_request_error: messages.5.content.0: Invalid `signature` in `thinking` block",
        "invalid_request_error: messages.3.content.0: Invalid `data` in `redacted_thinking` block",
        "400 invalid_encrypted_content: The encrypted content for item rs_1 could not be verified.",
        "Encrypted content could not be decrypted",
        "400 invalid_request_error: messages.1.content.0.type: Expected `thinking` or `redacted_thinking`, but found `tool_use`. When `thinking` is enabled, a final `assistant` message must start with a thinking block",
    ] {
        assert_eq!(
            class_of(
                &InferenceError::PermanentFailure {
                    reason: text.to_string()
                },
                text
            ),
            FailureClass::ReasoningRejected,
            "{text}"
        );
    }
    let unrelated = "400 invalid_request_error: max_tokens is too large";
    assert_eq!(
        class_of(
            &InferenceError::PermanentFailure {
                reason: unrelated.to_string()
            },
            unrelated
        ),
        FailureClass::Permanent
    );
}

#[test]
fn permanent_failure_reason_carries_its_prefix_once() {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy::scheduled_default());
    let error = InferenceError::PermanentFailure {
        reason: "ProviderError: bad request".to_string(),
    };
    match state.on_pre_stream_failure(&classified(&error, "bad request"), Utc::now(), None) {
        PreStreamDirective::Fail { reason } => assert_eq!(
            reason,
            "permanent inference failure: ProviderError: bad request"
        ),
        other => panic!("expected Fail, got {other:?}"),
    }
}

fn account_limited(error: &InferenceError, text: &str) -> crate::error::ClassifiedFailure {
    let mut failure = classified(error, text);
    failure.report.account_limited = true;
    failure
}

#[test]
fn account_rotated_marker_reaches_the_classified_report() {
    use crate::error::{classify_loop_failure, CompletionFailure, LoopFailureCause};
    let message = crate::provider_limit::ProviderLimitHeaders {
        usage_exhausted: true,
        account_rotated: true,
        ..Default::default()
    }
    .annotate("Claude Messages HTTP 429 Too Many Requests (request-id -) body=usage limit");
    let cause = LoopFailureCause::Completion {
        failure: CompletionFailure::Provider(message.clone()),
        reason: message.clone(),
    };
    let failure = classify_loop_failure(&cause, &message);
    assert!(matches!(failure.error, InferenceError::UsageLimited(_)));
    assert_eq!(failure_class(&failure), FailureClass::AccountLimited);
    assert!(!failure.text.contains("provider-limit"));
}

#[derive(serde::Deserialize)]
struct RetryTrace {
    policy: RetryTracePolicy,
    now: i64,
    deadline: Option<i64>,
    steps: Vec<RetryTraceStep>,
    resample_delay_bound_ms: Option<u64>,
}

#[derive(serde::Deserialize)]
struct RetryTracePolicy {
    backoff: Vec<u64>,
    max_resample: u32,
    max_empty_resample: u32,
    allow_repair: bool,
    account_hops: u32,
}

#[derive(serde::Deserialize)]
struct RetryTraceStep {
    entry: String,
    failure: String,
    error: String,
    effects: bool,
}

#[derive(serde::Serialize)]
struct RetryTraceOutcome {
    kind: &'static str,
    retry_count: u32,
}

fn trace_failure(failure: &str, error: &str) -> crate::error::ClassifiedFailure {
    match failure {
        "transport" => classified(&transient(error), error),
        "parse_bad_request" => {
            let text = parse_400_text(error);
            classified(&transient("parse"), &text)
        }
        "reasoning_rejected" => {
            let text = format!(
                "400 invalid_request_error: messages.5.content.0: Invalid `signature` in `thinking` block [{error}]"
            );
            classified(
                &InferenceError::PermanentFailure {
                    reason: text.clone(),
                },
                &text,
            )
        }
        "context_overflow" => classified(
            &InferenceError::ContextLengthExceeded {
                reason: error.to_string(),
                input_tokens: Some(1_001_685),
                limit: Some(1_000_000),
            },
            error,
        ),
        "account_limited" => account_limited(
            &InferenceError::UsageLimited(crate::provider_limit::UsageLimit {
                resets_at: None,
                detail: error.to_string(),
                statement: Default::default(),
            }),
            error,
        ),
        "permanent" => classified(
            &InferenceError::PermanentFailure {
                reason: error.to_string(),
            },
            error,
        ),
        other => panic!("no modeled pre-stream failure class {other}"),
    }
}

fn pre_stream_kind(directive: PreStreamDirective) -> (&'static str, Duration) {
    match directive {
        PreStreamDirective::RetryAfter {
            delay,
            kind: RetryKind::Transport,
        } => ("retry_transport", delay),
        PreStreamDirective::RetryAfter {
            delay,
            kind: RetryKind::Resample,
        } => ("retry_resample", delay),
        PreStreamDirective::RetryAfter {
            delay,
            kind: RetryKind::Account,
        } => ("retry_account", delay),
        PreStreamDirective::Repair => ("repair", Duration::ZERO),
        PreStreamDirective::CompactAndRetry { .. } => ("compact_and_retry", Duration::ZERO),
        PreStreamDirective::Fail { .. } => ("fail", Duration::ZERO),
    }
}

fn mid_stream_kind(directive: MidStreamDirective) -> (&'static str, Duration) {
    match directive {
        MidStreamDirective::RetractAndResample { delay } => ("retract_and_resample", delay),
        MidStreamDirective::CloseAndContinue { delay } => ("close_and_continue", delay),
        MidStreamDirective::Fail { .. } => ("fail", Duration::ZERO),
    }
}

fn replay_retry_trace(trace: &RetryTrace) -> Vec<RetryTraceOutcome> {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy {
        transport_backoff: trace
            .policy
            .backoff
            .iter()
            .map(|seconds| Duration::from_secs(*seconds))
            .collect(),
        max_resample: trace.policy.max_resample,
        max_empty_resample: trace.policy.max_empty_resample,
        allow_repair: trace.policy.allow_repair,
        account_hops: trace.policy.account_hops,
        max_rate_limit_wait: Duration::MAX,
    });
    let mut now = chrono::DateTime::from_timestamp(trace.now, 0).expect("modeled clock");
    let deadline = trace
        .deadline
        .map(|deadline| chrono::DateTime::from_timestamp(deadline, 0).expect("modeled deadline"));
    let mut outcomes = Vec::new();
    for step in &trace.steps {
        let (kind, delay) = match step.entry.as_str() {
            "pre_stream" if step.failure == "parse_bad_request" => {
                let observed = pre_stream_kind(state.on_pre_stream_failure(
                    &trace_failure(&step.failure, &step.error),
                    now,
                    deadline,
                ));
                if let (Some(bound), "retry_resample") = (trace.resample_delay_bound_ms, observed.0)
                {
                    assert!(
                        observed.1 <= Duration::from_millis(bound),
                        "a resample paces from the ladder's last step: {:?} exceeds {bound} ms",
                        observed.1
                    );
                }
                observed
            }
            "pre_stream" => pre_stream_kind(state.on_pre_stream_failure(
                &trace_failure(&step.failure, &step.error),
                now,
                deadline,
            )),
            "mid_stream" => {
                mid_stream_kind(state.on_mid_stream_failure(step.effects, now, deadline))
            }
            "no_visible_output" => mid_stream_kind(state.on_no_visible_output(now, deadline)),
            "repair_issued" => {
                state.mark_repair_used();
                continue;
            }
            "compaction_issued" => {
                state.mark_compaction_used();
                continue;
            }
            other => panic!("no retry entry point {other}"),
        };
        now += chrono::Duration::from_std(delay).expect("retry delay in range");
        outcomes.push(RetryTraceOutcome {
            kind,
            retry_count: state.retry_count(),
        });
    }
    outcomes
}

#[test]
fn pre_stream_retry_traces_follow_the_lean_directive_projection() {
    gents_lean_vocab::families::completion_retry_contracts_pre_stream::replay::<
        String,
        String,
        String,
    >(|trace| {
        gents_lean_vocab::family::recode(&replay_retry_trace(&gents_lean_vocab::family::recode(
            trace,
        )))
    });
}

#[test]
fn mid_stream_retry_traces_follow_the_lean_directive_projection() {
    gents_lean_vocab::families::completion_retry_contracts_mid_stream::replay::<
        String,
        String,
        String,
    >(|trace| {
        gents_lean_vocab::family::recode(&replay_retry_trace(&gents_lean_vocab::family::recode(
            trace,
        )))
    });
}

#[test]
fn no_visible_output_retry_traces_follow_the_lean_directive_projection() {
    gents_lean_vocab::families::completion_retry_contracts_no_visible_output::replay::<
        String,
        String,
        String,
    >(|trace| {
        gents_lean_vocab::family::recode(&replay_retry_trace(&gents_lean_vocab::family::recode(
            trace,
        )))
    });
}

#[test]
fn provider_limit_verdicts_follow_the_executable_model() {
    use gents_lean_vocab::families::inference_provider_limit_verdicts as family;
    use gents_protocol::provider_limit::ProviderLimitVerdict;
    let at = |seconds: u64| {
        chrono::DateTime::<Utc>::UNIX_EPOCH
            + chrono::Duration::seconds(i64::try_from(seconds).unwrap())
    };
    family::replay::<ProviderLimitVerdict>(|input: &family::VerdictInput| {
        let now = at(input.now);
        let error = if input.usage_exhausted {
            InferenceError::UsageLimited(crate::provider_limit::UsageLimit {
                resets_at: input.reported_at.map(at),
                detail: String::new(),
                statement: Box::new(gents_protocol::provider_limit::ProviderLimitStatement {
                    window_minutes: input.statement.window_minutes,
                    tokens_used: input.statement.tokens_used,
                    tokens_limit: input.statement.tokens_limit,
                }),
            })
        } else {
            InferenceError::RateLimited {
                retry_after: input
                    .reported_at
                    .map(|wake| Duration::from_secs(wake.saturating_sub(input.now))),
            }
        };
        let cap = input.max_wait.map_or(Duration::MAX, Duration::from_secs);
        let recorded = provider_limit_verdict(&classified(&error, ""), now, cap)
            .expect("a throttle or a usage exhaustion is a provider limit")
            .recorded();
        let read = CompletionRetryState::new(CompletionRetryPolicy {
            max_rate_limit_wait: cap,
            ..CompletionRetryPolicy::scheduled_default()
        })
        .read_failure(classified(&error, ""), now);
        assert_eq!(
            failure_class(&read) == FailureClass::Permanent,
            recorded.verdict == ProviderLimitVerdict::UsageLimited,
            "the loop retries exactly the throttles the call records"
        );
        family::VerdictOutput {
            verdict: recorded.verdict,
            resets_at: recorded
                .resets_at
                .map(|reset| u64::try_from(reset.timestamp()).unwrap()),
            statement: family::Statement {
                window_minutes: recorded.statement.window_minutes,
                tokens_used: recorded.statement.tokens_used,
                tokens_limit: recorded.statement.tokens_limit,
            },
        }
    });
}

#[test]
fn provider_limit_verdict_names_follow_the_executable_model() {
    use gents_protocol::provider_limit::ProviderLimitVerdict;
    gents_lean_vocab::families::inference_call_provider_limit_verdict_vocabulary::replay(&[
        ProviderLimitVerdict::Throttled,
        ProviderLimitVerdict::UsageLimited,
    ]);
}
