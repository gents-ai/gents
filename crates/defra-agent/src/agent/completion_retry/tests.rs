use std::time::Duration;

use chrono::Utc;
use rand::rngs::StdRng;
use rand::SeedableRng;

use super::*;

fn transient(reason: &str) -> InferenceError {
    InferenceError::TransientFailure {
        reason: reason.to_string(),
    }
}

/// A provider message matching the vLLM tool-call json-parse-failure
/// signature (`error.rs::provider_message_is_tool_call_json_parse_failure`).
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
fn failure_class_maps_transport_variants() {
    assert_eq!(
        failure_class(
            &InferenceError::ModelUnreachable {
                endpoint: "x".into()
            },
            "unreachable"
        ),
        FailureClass::Transport
    );
    assert_eq!(
        failure_class(&transient("boom"), "boom"),
        FailureClass::Transport
    );
    assert_eq!(
        failure_class(&InferenceError::Timeout { timeout_secs: 30 }, "timeout"),
        FailureClass::Transport
    );
    assert_eq!(
        failure_class(
            &InferenceError::RateLimited {
                retry_after_secs: 60
            },
            "rate limited"
        ),
        FailureClass::Transport
    );
}

#[test]
fn failure_class_maps_permanent_variants() {
    assert_eq!(
        failure_class(
            &InferenceError::PermanentFailure {
                reason: "nope".into()
            },
            "nope"
        ),
        FailureClass::Permanent
    );
    assert_eq!(
        failure_class(
            &InferenceError::ContextLengthExceeded {
                reason: "too long".into()
            },
            "too long"
        ),
        FailureClass::Permanent
    );
    assert_eq!(
        failure_class(
            &InferenceError::RetriesExhausted {
                max_retries: 3,
                last_error: "x".into()
            },
            "x"
        ),
        FailureClass::Permanent
    );
}

#[test]
fn failure_class_parse_signature_overrides_variant() {
    // classify_completion_error currently produces TransientFailure for the
    // vLLM tool-call json-parse 400 (see error.rs doc comment); failure_class
    // must reclassify it via the error_text signature, not the variant.
    let text = parse_400_text("override");
    assert_eq!(
        failure_class(&transient(&text), &text),
        FailureClass::ParseBadRequest
    );
}

#[test]
fn jitter_is_deterministic_for_a_seeded_rng_and_within_25_percent() {
    let base = Duration::from_secs(10);
    let mut rng_a = StdRng::seed_from_u64(42);
    let mut rng_b = StdRng::seed_from_u64(42);
    let delay_a = jitter(base, &mut rng_a);
    let delay_b = jitter(base, &mut rng_b);
    assert_eq!(delay_a, delay_b, "same seed must produce same jitter");
    assert_in_range(delay_a, 7_500, 12_500);
}

#[test]
fn transport_ladder_progresses_then_fails() {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy::scheduled_default());
    let now = Utc::now();
    let error = transient("connection reset");

    let d1 = state.on_pre_stream_failure(&error, "connection reset", now, None);
    match d1 {
        PreStreamDirective::RetryAfter { delay, kind } => {
            assert_eq!(kind, RetryKind::Transport);
            assert_in_range(delay, 3_750, 6_250); // 5s +/- 25%
        }
        other => panic!("expected RetryAfter, got {other:?}"),
    }
    assert_eq!(state.retry_count(), 1);

    let d2 = state.on_pre_stream_failure(&error, "connection reset", now, None);
    match d2 {
        PreStreamDirective::RetryAfter { delay, kind } => {
            assert_eq!(kind, RetryKind::Transport);
            assert_in_range(delay, 22_500, 37_500); // 30s +/- 25%
        }
        other => panic!("expected RetryAfter, got {other:?}"),
    }

    let d3 = state.on_pre_stream_failure(&error, "connection reset", now, None);
    match d3 {
        PreStreamDirective::RetryAfter { delay, kind } => {
            assert_eq!(kind, RetryKind::Transport);
            assert_in_range(delay, 90_000, 150_000); // 120s +/- 25%
        }
        other => panic!("expected RetryAfter, got {other:?}"),
    }
    assert_eq!(state.retry_count(), 3);

    let d4 = state.on_pre_stream_failure(&error, "connection reset", now, None);
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
        retry_after_secs: 90,
    };
    match state.on_pre_stream_failure(&error, "rate limited", now, None) {
        PreStreamDirective::RetryAfter { delay, kind } => {
            assert_eq!(kind, RetryKind::Transport);
            // ladder[0] = 5s, provider hint = 90s -> max is 90s, +/- 25%.
            assert_in_range(delay, 67_500, 112_500);
        }
        other => panic!("expected RetryAfter, got {other:?}"),
    }
}

#[test]
fn rate_limited_keeps_ladder_delay_when_larger_than_hint() {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy::scheduled_default());
    let now = Utc::now();
    let error = InferenceError::RateLimited {
        retry_after_secs: 1,
    };
    match state.on_pre_stream_failure(&error, "rate limited", now, None) {
        PreStreamDirective::RetryAfter { delay, kind } => {
            assert_eq!(kind, RetryKind::Transport);
            // ladder[0] = 5s > hint 1s -> stays 5s +/- 25%.
            assert_in_range(delay, 3_750, 6_250);
        }
        other => panic!("expected RetryAfter, got {other:?}"),
    }
}

#[test]
fn deadline_fail_fast_when_next_delay_overshoots() {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy::scheduled_default());
    let now = Utc::now();
    let error = transient("connection reset");

    // Consume the first ladder entry (5s) so the next call needs 30s.
    let first = state.on_pre_stream_failure(&error, "connection reset", now, None);
    assert!(matches!(first, PreStreamDirective::RetryAfter { .. }));

    // Deadline is 10s out; even at -25% jitter, 30s's floor (22.5s) blows past it.
    let deadline = now + chrono::Duration::seconds(10);
    match state.on_pre_stream_failure(&error, "connection reset", now, Some(deadline)) {
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

    let first = state.on_pre_stream_failure(&error, &text, now, None);
    match first {
        PreStreamDirective::RetryAfter { kind, .. } => assert_eq!(kind, RetryKind::Resample),
        other => panic!("expected RetryAfter(Resample), got {other:?}"),
    }
    assert_eq!(state.retry_count(), 1);

    // Same error text again: deterministic, even though resample budget (2)
    // is not exhausted (resample_used == 1) — must go straight to Repair.
    let second = state.on_pre_stream_failure(&error, &text, now, None);
    assert_eq!(second, PreStreamDirective::Repair);
}

#[test]
fn repair_is_available_only_once() {
    // max_resample = 0 so any parse-400 immediately hits the
    // deterministic-or-budget-spent branch.
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
        state.on_pre_stream_failure(&error, &text, now, None),
        PreStreamDirective::Repair
    );
    state.mark_repair_used();

    match state.on_pre_stream_failure(&error, &text, now, None) {
        PreStreamDirective::Fail { .. } => {}
        other => panic!("expected Fail after repair used, got {other:?}"),
    }
}

#[test]
fn fresh_parse_400_deadline_overshoot_fails_immediately_not_repair() {
    // Budget is available (fresh error, resample room) but the deadline is
    // too close for the resample delay to fit — this must Fail, never
    // fall through to Repair (mirrors Lean parseExhaust vs repair guards).
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

    match state.on_pre_stream_failure(&error, &text, now, Some(deadline)) {
        PreStreamDirective::Fail { reason } => {
            assert!(reason.to_lowercase().contains("deadline"), "{reason}");
        }
        other => panic!("expected Fail (not Repair), got {other:?}"),
    }
}

#[test]
fn mid_stream_without_effects_retracts_and_resamples() {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy::scheduled_default());
    let now = Utc::now();
    match state.on_mid_stream_failure(false, now, None) {
        MidStreamDirective::RetractAndResample { delay } => {
            assert_in_range(delay, 3_750, 6_250);
        }
        other => panic!("expected RetractAndResample, got {other:?}"),
    }
    assert_eq!(state.retry_count(), 1);
}

#[test]
fn mid_stream_with_effects_closes_and_continues() {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy::scheduled_default());
    let now = Utc::now();
    match state.on_mid_stream_failure(true, now, None) {
        MidStreamDirective::CloseAndContinue { delay } => {
            assert_in_range(delay, 3_750, 6_250);
        }
        other => panic!("expected CloseAndContinue, got {other:?}"),
    }
    assert_eq!(state.retry_count(), 1);
}

#[test]
fn mid_stream_budget_exhausted_fails() {
    let policy = CompletionRetryPolicy {
        transport_backoff: vec![Duration::from_secs(2)],
        max_resample: 0,
        allow_repair: true,
    };
    let mut state = CompletionRetryState::new(policy);
    let now = Utc::now();
    assert!(matches!(
        state.on_mid_stream_failure(false, now, None),
        MidStreamDirective::RetractAndResample { .. }
    ));
    match state.on_mid_stream_failure(false, now, None) {
        MidStreamDirective::Fail { reason } => assert!(reason.contains("exhausted")),
        other => panic!("expected Fail, got {other:?}"),
    }
}

#[test]
fn interactive_default_allows_a_single_short_retry_then_fails() {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy::interactive_default());
    let now = Utc::now();
    let error = transient("connection reset");

    match state.on_pre_stream_failure(&error, "connection reset", now, None) {
        PreStreamDirective::RetryAfter { delay, kind } => {
            assert_eq!(kind, RetryKind::Transport);
            assert_in_range(delay, 1_500, 2_500); // 2s +/- 25%
        }
        other => panic!("expected RetryAfter, got {other:?}"),
    }

    match state.on_pre_stream_failure(&error, "connection reset", now, None) {
        PreStreamDirective::Fail { .. } => {}
        other => panic!("expected Fail, got {other:?}"),
    }
}

#[test]
fn default_for_origin_maps_interactive_and_scheduled() {
    use crate::lifecycle::ExecutionOrigin;

    let interactive = CompletionRetryPolicy::default_for_origin(ExecutionOrigin::Interactive);
    assert_eq!(interactive.transport_backoff, vec![Duration::from_secs(2)]);
    assert_eq!(interactive.max_resample, 0);
    assert!(interactive.allow_repair);

    let scheduled = CompletionRetryPolicy::default_for_origin(ExecutionOrigin::Scheduled);
    assert_eq!(
        scheduled.transport_backoff,
        vec![
            Duration::from_secs(5),
            Duration::from_secs(30),
            Duration::from_secs(120)
        ]
    );
    assert_eq!(scheduled.max_resample, 1);
    assert!(scheduled.allow_repair);
}

#[test]
fn interactive_default_parse_400_goes_straight_to_repair() {
    // max_resample = 0 in the interactive default: no resample room at all.
    let mut state = CompletionRetryState::new(CompletionRetryPolicy::interactive_default());
    let now = Utc::now();
    let error = transient("parse trouble");
    let text = parse_400_text("interactive");

    assert_eq!(
        state.on_pre_stream_failure(&error, &text, now, None),
        PreStreamDirective::Repair
    );
}
