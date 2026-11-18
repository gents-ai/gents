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

fn assert_in_range(delay: Duration, low_ms: u64, high_ms: u64) {
    let ms = delay.as_millis() as u64;
    assert!(
        ms >= low_ms && ms <= high_ms,
        "expected delay in [{low_ms}, {high_ms}]ms, got {ms}ms"
    );
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
fn transport_ladder_progresses_then_fails() {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy::scheduled_default());
    let now = Utc::now();
    let error = transient("connection reset");

    let d1 = state.on_pre_stream_failure(&classified(&error, "connection reset"), now, None);
    match d1 {
        PreStreamDirective::RetryAfter { delay, kind } => {
            assert_eq!(kind, RetryKind::Transport);
            assert_in_range(delay, 3_750, 6_250); // 5s +/- 25%
        }
        other => panic!("expected RetryAfter, got {other:?}"),
    }
    assert_eq!(state.retry_count(), 1);

    let d2 = state.on_pre_stream_failure(&classified(&error, "connection reset"), now, None);
    match d2 {
        PreStreamDirective::RetryAfter { delay, kind } => {
            assert_eq!(kind, RetryKind::Transport);
            assert_in_range(delay, 22_500, 37_500); // 30s +/- 25%
        }
        other => panic!("expected RetryAfter, got {other:?}"),
    }

    let d3 = state.on_pre_stream_failure(&classified(&error, "connection reset"), now, None);
    match d3 {
        PreStreamDirective::RetryAfter { delay, kind } => {
            assert_eq!(kind, RetryKind::Transport);
            assert_in_range(delay, 90_000, 150_000); // 120s +/- 25%
        }
        other => panic!("expected RetryAfter, got {other:?}"),
    }
    assert_eq!(state.retry_count(), 3);

    let d4 = state.on_pre_stream_failure(&classified(&error, "connection reset"), now, None);
    match d4 {
        PreStreamDirective::Fail { reason } => {
            assert!(reason.contains("exhausted"), "reason: {reason}");
        }
        other => panic!("expected Fail, got {other:?}"),
    }
}

#[test]
fn rate_limited_uses_provider_hint_when_larger_than_ladder() {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy::scheduled_default());
    let now = Utc::now();
    let error = InferenceError::RateLimited {
        retry_after: Some(Duration::from_secs(90)),
    };
    match state.on_pre_stream_failure(&classified(&error, "rate limited"), now, None) {
        PreStreamDirective::RetryAfter { delay, kind } => {
            assert_eq!(kind, RetryKind::Transport);
            assert_in_range(delay, 67_500, 112_500);
        }
        other => panic!("expected RetryAfter, got {other:?}"),
    }
}

#[test]
fn fresh_parse_400_deadline_overshoot_fails_immediately_not_repair() {
    let policy = CompletionRetryPolicy {
        transport_backoff: vec![Duration::from_secs(30)],
        max_resample: 1,
        max_empty_resample: 1,
        allow_repair: true,
        account_hops: 0,
    };
    let mut state = CompletionRetryState::new(policy);
    let now = Utc::now();
    let error = transient("parse trouble");
    let text = parse_400_text("fresh-but-late");
    let deadline = now + chrono::Duration::seconds(5);

    match state.on_pre_stream_failure(&classified(&error, &text), now, Some(deadline)) {
        PreStreamDirective::Fail { reason } => {
            assert!(reason.to_lowercase().contains("deadline"), "{reason}");
        }
        other => panic!("expected Fail (not Repair), got {other:?}"),
    }
}

#[test]
fn internal_immediate_retracts_three_times_then_fails() {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy::internal_immediate());
    let now = Utc::now();

    let expected_ranges = [(100, 100), (750, 1_250), (1_500, 2_500)];
    for (low_ms, high_ms) in expected_ranges {
        match state.on_mid_stream_failure(false, now, None) {
            MidStreamDirective::RetractAndResample { delay } => {
                assert_in_range(delay, low_ms, high_ms);
            }
            other => panic!("expected RetractAndResample, got {other:?}"),
        }
    }
    match state.on_mid_stream_failure(false, now, None) {
        MidStreamDirective::Fail { reason } => assert!(reason.contains("exhausted")),
        other => panic!("expected Fail, got {other:?}"),
    }
}

#[test]
fn internal_immediate_is_deadline_aware() {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy::internal_immediate());
    let now = Utc::now();
    match state.on_mid_stream_failure(false, now, Some(now)) {
        MidStreamDirective::Fail { reason } => assert!(reason.contains("deadline")),
        other => panic!("expected Fail, got {other:?}"),
    }
}

#[test]
fn interactive_default_parse_400_goes_straight_to_repair() {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy::interactive_default());
    let now = Utc::now();
    let error = transient("parse trouble");
    let text = parse_400_text("interactive");

    assert_eq!(
        state.on_pre_stream_failure(&classified(&error, &text), now, None),
        PreStreamDirective::Repair
    );
}

#[test]
fn observed_retry_wake_is_deadline_bounded_and_inclusive() {
    let deadline = Utc::now();
    assert!(retry_wake_fits_deadline(deadline, Some(deadline)));
    assert!(!retry_wake_fits_deadline(
        deadline + chrono::Duration::nanoseconds(1),
        Some(deadline)
    ));
    assert!(retry_wake_fits_deadline(
        deadline + chrono::Duration::days(1),
        None
    ));
}

#[test]
fn usage_limit_fails_immediately_with_its_reset_time() {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy::scheduled_default());
    let limit = crate::provider_limit::UsageLimit {
        resets_at: chrono::DateTime::parse_from_rfc3339("2026-09-25T18:40:00Z")
            .ok()
            .map(|at| at.with_timezone(&Utc)),
        detail: "seat capped".into(),
    };
    let error = InferenceError::UsageLimited(limit);
    assert_eq!(class_of(&error, "seat capped"), FailureClass::Permanent);
    match state.on_pre_stream_failure(&classified(&error, "seat capped"), Utc::now(), None) {
        PreStreamDirective::Fail { reason } => assert_eq!(
            reason,
            "provider usage limit reached (resets at 2026-09-25T18:40:00Z): seat capped"
        ),
        other => panic!("expected Fail, got {other:?}"),
    }
    assert_eq!(state.retry_count(), 0);
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

fn account_policy(hops: u32) -> CompletionRetryPolicy {
    CompletionRetryPolicy {
        account_hops: hops,
        ..CompletionRetryPolicy::scheduled_default()
    }
}

#[test]
fn account_limited_report_selects_its_class() {
    let error = transient("limit");
    assert_eq!(
        failure_class(&account_limited(&error, "limit")),
        FailureClass::AccountLimited
    );
    assert_eq!(class_of(&error, "limit"), FailureClass::Transport);
}

#[test]
fn account_limit_reissues_at_once_within_hops_without_spending_transport() {
    let mut state = CompletionRetryState::new(account_policy(2));
    let error = transient("limit");
    for _ in 0..2 {
        assert_eq!(
            state.on_pre_stream_failure(&account_limited(&error, "limit"), Utc::now(), None),
            PreStreamDirective::RetryAfter {
                delay: Duration::ZERO,
                kind: RetryKind::Account,
            }
        );
    }
    assert_eq!(state.retry_count(), 0);
    assert!(matches!(
        state.on_pre_stream_failure(&account_limited(&error, "limit"), Utc::now(), None),
        PreStreamDirective::Fail { .. }
    ));
    assert!(matches!(
        state.on_pre_stream_failure(&classified(&error, "io"), Utc::now(), None),
        PreStreamDirective::RetryAfter {
            kind: RetryKind::Transport,
            ..
        }
    ));
}

#[test]
fn account_limit_with_no_hops_fails_with_the_usage_limit_reason() {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy::scheduled_default());
    let limit = crate::provider_limit::UsageLimit {
        resets_at: None,
        detail: "cap".into(),
    };
    let error = InferenceError::UsageLimited(limit.clone());
    match state.on_pre_stream_failure(&account_limited(&error, "cap"), Utc::now(), None) {
        PreStreamDirective::Fail { reason } => assert_eq!(reason, limit.to_string()),
        other => panic!("expected Fail, got {other:?}"),
    }
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

#[test]
fn empty_output_retries_independently_of_transport_and_resets_for_next_turn() {
    let now = Utc::now();
    let mut state = CompletionRetryState::new(CompletionRetryPolicy::interactive_default());
    assert!(matches!(
        state.on_mid_stream_failure(false, now, None),
        MidStreamDirective::RetractAndResample { .. }
    ));
    assert!(matches!(
        state.on_no_visible_output(now, None),
        MidStreamDirective::RetractAndResample { .. }
    ));
    assert!(matches!(
        state.on_no_visible_output(now, None),
        MidStreamDirective::Fail { .. }
    ));
    assert_eq!(state.retry_count(), 2);
    state.on_turn_accepted();
    assert_eq!(state.retry_count(), 2);
    assert!(matches!(
        state.on_no_visible_output(now, None),
        MidStreamDirective::RetractAndResample { .. }
    ));
    let mut no_retry = CompletionRetryState::new(CompletionRetryPolicy::no_retry());
    assert!(matches!(
        no_retry.on_no_visible_output(now, None),
        MidStreamDirective::Fail { .. }
    ));
    let mut deadline = CompletionRetryState::new(CompletionRetryPolicy::interactive_default());
    assert!(matches!(
        deadline.on_no_visible_output(now, Some(now)),
        MidStreamDirective::Fail { .. }
    ));
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
