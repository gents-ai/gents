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
fn tight_interactive_deadline_rejects_first_retry_without_consuming_budget() {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy::interactive_default());
    let now = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let error = transient("HTTP status 503 before tight deadline");
    let deadline = now + chrono::Duration::seconds(1);

    match state.on_pre_stream_failure(&classified(&error, &error.to_string()), now, Some(deadline))
    {
        PreStreamDirective::Fail { reason } => assert!(reason.contains("request deadline")),
        other => panic!("expected deadline rejection before retry, got {other:?}"),
    }
    assert_eq!(state.retry_count(), 0);
}

#[test]
fn deadline_fail_fast_when_next_delay_overshoots() {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy::scheduled_default());
    let now = Utc::now();
    let error = transient("connection reset");

    let first = state.on_pre_stream_failure(&classified(&error, "connection reset"), now, None);
    assert!(matches!(first, PreStreamDirective::RetryAfter { .. }));

    let deadline = now + chrono::Duration::seconds(10);
    match state.on_pre_stream_failure(&classified(&error, "connection reset"), now, Some(deadline))
    {
        PreStreamDirective::Fail { reason } => {
            assert!(
                reason.to_lowercase().contains("deadline"),
                "reason should mention the deadline: {reason}"
            );
        }
        other => panic!("expected Fail, got {other:?}"),
    }
}

#[test]
fn deterministic_parse_400_skips_remaining_resample_budget_and_repairs() {
    let policy = CompletionRetryPolicy {
        transport_backoff: vec![Duration::from_secs(5), Duration::from_secs(30)],
        max_resample: 2,
        allow_repair: true,
    };
    let mut state = CompletionRetryState::new(policy);
    let now = Utc::now();
    let error = transient("parse trouble");
    let text = parse_400_text("same-error");

    let first = state.on_pre_stream_failure(&classified(&error, &text), now, None);
    match first {
        PreStreamDirective::RetryAfter { kind, .. } => assert_eq!(kind, RetryKind::Resample),
        other => panic!("expected RetryAfter(Resample), got {other:?}"),
    }
    assert_eq!(state.retry_count(), 1);

    let second = state.on_pre_stream_failure(&classified(&error, &text), now, None);
    assert_eq!(second, PreStreamDirective::Repair);
}

#[test]
fn repair_is_available_only_once() {
    let policy = CompletionRetryPolicy {
        transport_backoff: vec![Duration::from_secs(5)],
        max_resample: 0,
        allow_repair: true,
    };
    let mut state = CompletionRetryState::new(policy);
    let now = Utc::now();
    let error = transient("parse trouble");
    let text = parse_400_text("repeat");

    assert_eq!(
        state.on_pre_stream_failure(&classified(&error, &text), now, None),
        PreStreamDirective::Repair
    );
    state.mark_repair_used();

    match state.on_pre_stream_failure(&classified(&error, &text), now, None) {
        PreStreamDirective::Fail { .. } => {}
        other => panic!("expected Fail after repair used, got {other:?}"),
    }
}

#[test]
fn fresh_parse_400_deadline_overshoot_fails_immediately_not_repair() {
    let policy = CompletionRetryPolicy {
        transport_backoff: vec![Duration::from_secs(30)],
        max_resample: 1,
        allow_repair: true,
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
fn reasoning_rejection_repairs_once_then_fails() {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy {
        transport_backoff: vec![Duration::from_secs(1)],
        max_resample: 3,
        allow_repair: true,
    });
    let error = InferenceError::PermanentFailure {
        reason: "Invalid `signature` in `thinking` block".to_string(),
    };
    let text = error.to_string();
    assert!(matches!(
        state.on_pre_stream_failure(&classified(&error, &text), Utc::now(), None),
        PreStreamDirective::Repair
    ));
    state.mark_repair_used();
    assert!(matches!(
        state.on_pre_stream_failure(&classified(&error, &text), Utc::now(), None),
        PreStreamDirective::Fail { .. }
    ));
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
