use std::collections::BTreeSet;
use std::time::Duration;

use gents::agent::completion_retry::{
    failure_class, retry_wake_fits_deadline, CompletionRetryPolicy, CompletionRetryState,
    FailureClass, MidStreamDirective, PreStreamDirective, RetryKind,
};
use gents::error::{classify_completion_error, InferenceError};

use crate::lean_vocab_test::{
    assert_lean_contract_vocabulary_matches, lean_completion_retry_cases, LeanCompletionRetryCase,
    LeanContractVocabulary,
};

pub(super) fn completion_retry_lean_witness_cases_hold() {
    let cases = lean_completion_retry_cases();
    assert_eq!(
        cases.len(),
        20,
        "Lean should emit the finite CompletionRetry witness set"
    );
    assert_failure_class_bridge_matches_vocabulary();

    let names = cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        BTreeSet::from([
            "transport_failure_requires_retraction",
            "cannot_schedule_before_retraction",
            "uncommitted_retraction_is_rejected",
            "durable_retraction_is_observed",
            "transport_backoff_only_after_retraction",
            "parse_resample_only_after_retraction",
            "deterministic_parse_moves_to_repair",
            "accepted_publication_closes_retry",
            "accepted_failure_cannot_retract",
            "accepted_tool_failure_is_terminal_observation",
            "accepted_publication_cannot_schedule",
            "usage_is_charged_before_retraction",
            "late_usage_is_still_charged",
            "transport_budget_exhaustion_is_terminal_for_policy",
            "parse_budget_without_repair_is_exhausted",
            "retry_wake_past_deadline_is_exhausted",
            "repair_issue_consumes_its_only_capability",
            "second_repair_issue_is_rejected",
            "local_request_build_fails_permanently_without_retry",
            "retryable_transport_still_requires_retraction",
        ]),
        "CompletionRetry witness names drifted"
    );

    for case in cases {
        assert_eq!(case.domain, "completionRetry");
        if let Some(origin) = case.failure_origin.as_deref() {
            assert_failure_origin_bridge(case, origin);
        }
        if case.name == "retry_wake_past_deadline_is_exhausted" {
            assert!(case.legal);
            assert_eq!(case.expected_phase.as_deref(), Some("exhausted"));
            let deadline = chrono::DateTime::from_timestamp(
                case.pre_deadline.expect("modeled retry deadline"),
                0,
            )
            .unwrap();
            let observed = chrono::DateTime::from_timestamp(
                case.pre_scheduled_wake.expect("modeled retry wake"),
                0,
            )
            .unwrap();
            assert!(!retry_wake_fits_deadline(observed, Some(deadline)));
        }
        if case.name == "transport_backoff_only_after_retraction" {
            let observed = chrono::DateTime::from_timestamp(
                case.pre_scheduled_wake.expect("modeled retry wake"),
                0,
            )
            .unwrap();
            assert!(case.pre_deadline.is_none());
            assert!(retry_wake_fits_deadline(observed, None));
        }
    }

    // Native policy regressions remain independent implementation checks. They
    // intentionally do not pretend to execute the newer phaseful Lean machine.
    let native = &cases[0];
    assert_transport_ladder_progresses(native);
    assert_transport_exhausts_after_budget(native);
    assert_selected_delay_past_deadline_fails_fast(native);
    assert_deadline_behind_clock_fails_fast(native);
    assert_deterministic_400_repairs(native);
    assert_resample_budget_outlives_ladder(native);
    assert_resample_exhausts_on_its_own_budget(native);
    assert_repair_second_time_illegal(native);
    assert_retract_with_effects_illegal(native);
    assert_close_turn_with_effects_legal(native);
    assert_permanent_class_cannot_backoff(native);
}

fn assert_failure_origin_bridge(case: &LeanCompletionRetryCase, origin: &str) {
    let error = match origin {
        "local_request_build" => rig::agent::StreamingError::Completion(
            rig::completion::CompletionError::RequestError(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "malformed local request",
            ))),
        ),
        "retryable_transport" => rig::agent::StreamingError::Completion(
            rig::completion::CompletionError::ProviderError("connection reset".into()),
        ),
        other => panic!("unknown modeled failure origin {other}"),
    };
    let classified = classify_completion_error(&error);
    let class = failure_class(&classified, &error.to_string());
    assert_eq!(
        class_name(class),
        case.classified_failure
            .as_deref()
            .expect("modeled failure class"),
        "{}",
        case.name
    );
    let mut state = CompletionRetryState::new(scheduled_like_policy());
    let directive = state.on_pre_stream_failure(&classified, &error.to_string(), now(), None);
    match case.expected_phase.as_deref() {
        Some("failed_permanent") => {
            assert!(
                matches!(&directive, PreStreamDirective::Fail { .. }),
                "{}",
                case.name
            );
            assert_eq!(state.retry_count(), 0, "{}", case.name);
        }
        Some("retract_required") => {
            assert!(
                matches!(
                    &directive,
                    PreStreamDirective::RetryAfter {
                        kind: RetryKind::Transport,
                        ..
                    }
                ),
                "{}: {directive:?}",
                case.name
            );
        }
        other => panic!("unexpected modeled phase for {}: {other:?}", case.name),
    }
}

fn assert_failure_class_bridge_matches_vocabulary() {
    let parse_text = parse_400_text("bridge");
    let observed = [
        class_name(failure_class(&transient("temporary"), "temporary")),
        class_name(failure_class(&transient(&parse_text), &parse_text)),
        class_name(failure_class(
            &InferenceError::PermanentFailure {
                reason: "bad request".to_string(),
            },
            "bad request",
        )),
    ];
    assert_eq!(observed, ["transport", "parse_bad_request", "permanent"]);
    assert_lean_contract_vocabulary_matches(LeanContractVocabulary {
        domain: "CompletionRetryFailureClass",
        rust_source: "failure_class observations",
        rust_values: &observed,
    });
}

fn assert_transport_ladder_progresses(case: &LeanCompletionRetryCase) {
    let mut state = CompletionRetryState::new(scheduled_like_policy());
    match state.on_pre_stream_failure(
        &transient("connection reset"),
        "connection reset",
        now(),
        None,
    ) {
        PreStreamDirective::RetryAfter { kind, .. } => assert_eq!(kind, RetryKind::Transport),
        other => panic!(
            "expected transport RetryAfter for {}, got {other:?}",
            case.name
        ),
    }
    assert_eq!(state.retry_count(), 1);
}

fn assert_transport_exhausts_after_budget(case: &LeanCompletionRetryCase) {
    let mut state = CompletionRetryState::new(scheduled_like_policy());
    for _ in 0..3 {
        assert!(matches!(
            state.on_pre_stream_failure(
                &transient("connection reset"),
                "connection reset",
                now(),
                None
            ),
            PreStreamDirective::RetryAfter {
                kind: RetryKind::Transport,
                ..
            }
        ));
    }
    match state.on_pre_stream_failure(
        &transient("connection reset"),
        "connection reset",
        now(),
        None,
    ) {
        PreStreamDirective::Fail { reason } => assert!(reason.contains("exhausted")),
        other => panic!("expected exhausted Fail for {}, got {other:?}", case.name),
    }
    assert_eq!(state.retry_count(), 3);
}

fn assert_selected_delay_past_deadline_fails_fast(case: &LeanCompletionRetryCase) {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy {
        transport_backoff: vec![Duration::from_secs(30)],
        max_resample: 0,
        allow_repair: true,
    });
    let now = now();
    let deadline = now + chrono::Duration::seconds(10);
    match state.on_pre_stream_failure(
        &transient("connection reset"),
        "connection reset",
        now,
        Some(deadline),
    ) {
        PreStreamDirective::Fail { reason } => assert!(reason.to_lowercase().contains("deadline")),
        other => panic!("expected deadline Fail for {}, got {other:?}", case.name),
    }
    assert_eq!(state.retry_count(), 0);
}

fn assert_deadline_behind_clock_fails_fast(case: &LeanCompletionRetryCase) {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy {
        transport_backoff: vec![Duration::from_secs(5)],
        max_resample: 0,
        allow_repair: true,
    });
    let now = now();
    match state.on_pre_stream_failure(
        &transient("connection reset"),
        "connection reset",
        now,
        Some(now - chrono::Duration::seconds(1)),
    ) {
        PreStreamDirective::Fail { reason } => assert!(reason.to_lowercase().contains("deadline")),
        other => panic!(
            "expected expired-deadline Fail for {}, got {other:?}",
            case.name
        ),
    }
    assert_eq!(state.retry_count(), 0);
}

fn assert_deterministic_400_repairs(case: &LeanCompletionRetryCase) {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy {
        transport_backoff: vec![Duration::from_secs(5), Duration::from_secs(30)],
        max_resample: 2,
        allow_repair: true,
    });
    let text = parse_400_text("json-parse");
    assert!(matches!(
        state.on_pre_stream_failure(&transient("parse"), &text, now(), None),
        PreStreamDirective::RetryAfter {
            kind: RetryKind::Resample,
            ..
        }
    ));
    assert_eq!(
        state.on_pre_stream_failure(&transient("parse"), &text, now(), None),
        PreStreamDirective::Repair
    );
    assert_eq!(state.retry_count(), 1);
}

fn assert_resample_budget_outlives_ladder(_case: &LeanCompletionRetryCase) {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy {
        transport_backoff: vec![Duration::from_secs(5)],
        max_resample: 3,
        allow_repair: true,
    });

    for attempt in 0..3 {
        let text = parse_400_text(&format!("json-parse-{attempt}"));
        match state.on_pre_stream_failure(&transient("parse"), &text, now(), None) {
            PreStreamDirective::RetryAfter {
                kind: RetryKind::Resample,
                delay,
            } => {
                assert!(
                    delay <= Duration::from_secs(10),
                    "resample delay must pace from the ladder's last step, got {delay:?}"
                );
            }
            other => panic!(
                "resample {attempt} must proceed while the budget has room \
                 (the ladder is pacing, not budget), got {other:?}"
            ),
        }
    }
    assert_eq!(state.retry_count(), 3);
}

fn assert_resample_exhausts_on_its_own_budget(_case: &LeanCompletionRetryCase) {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy {
        transport_backoff: vec![Duration::from_secs(5)],
        max_resample: 2,
        allow_repair: true,
    });

    for attempt in 0..2 {
        let text = parse_400_text(&format!("json-parse-{attempt}"));
        assert!(
            matches!(
                state.on_pre_stream_failure(&transient("parse"), &text, now(), None),
                PreStreamDirective::RetryAfter {
                    kind: RetryKind::Resample,
                    ..
                }
            ),
            "resample {attempt} must proceed within the budget"
        );
    }

    let text = parse_400_text("json-parse-final");
    assert_eq!(
        state.on_pre_stream_failure(&transient("parse"), &text, now(), None),
        PreStreamDirective::Repair,
        "a spent resample budget must fall through to repair, never hard-fail"
    );
}

fn assert_repair_second_time_illegal(case: &LeanCompletionRetryCase) {
    let mut state = CompletionRetryState::new(CompletionRetryPolicy {
        transport_backoff: vec![Duration::from_secs(5)],
        max_resample: 0,
        allow_repair: true,
    });
    state.mark_repair_used();
    let text = parse_400_text("used");
    match state.on_pre_stream_failure(&transient("parse"), &text, now(), None) {
        PreStreamDirective::Fail { .. } => {}
        other => panic!("expected no second Repair for {}, got {other:?}", case.name),
    }
}

fn assert_retract_with_effects_illegal(case: &LeanCompletionRetryCase) {
    let mut state = CompletionRetryState::new(scheduled_like_policy());
    match state.on_mid_stream_failure(true, now(), None) {
        MidStreamDirective::CloseAndContinue { .. } => {}
        other => panic!(
            "effects=true must close-and-continue, never retract, for {}; got {other:?}",
            case.name
        ),
    }
}

fn assert_close_turn_with_effects_legal(case: &LeanCompletionRetryCase) {
    let mut state = CompletionRetryState::new(scheduled_like_policy());
    match state.on_mid_stream_failure(true, now(), None) {
        MidStreamDirective::CloseAndContinue { .. } => {}
        other => panic!("expected CloseAndContinue for {}, got {other:?}", case.name),
    }
    assert_eq!(state.retry_count(), 1);
}

fn assert_permanent_class_cannot_backoff(case: &LeanCompletionRetryCase) {
    let mut state = CompletionRetryState::new(scheduled_like_policy());
    match state.on_pre_stream_failure(
        &InferenceError::PermanentFailure {
            reason: "invalid request".to_string(),
        },
        "invalid request",
        now(),
        None,
    ) {
        PreStreamDirective::Fail { reason } => {
            assert!(reason.contains("permanent inference failure"));
        }
        other => panic!("expected permanent Fail for {}, got {other:?}", case.name),
    }
    assert_eq!(state.retry_count(), 0);
}

fn class_name(class: FailureClass) -> &'static str {
    match class {
        FailureClass::Transport => "transport",
        FailureClass::ParseBadRequest => "parse_bad_request",
        FailureClass::Permanent => "permanent",
    }
}

fn scheduled_like_policy() -> CompletionRetryPolicy {
    CompletionRetryPolicy {
        transport_backoff: vec![
            Duration::from_secs(5),
            Duration::from_secs(30),
            Duration::from_secs(120),
        ],
        max_resample: 1,
        allow_repair: true,
    }
}

fn transient(reason: &str) -> InferenceError {
    InferenceError::TransientFailure {
        reason: reason.to_string(),
    }
}

fn parse_400_text(tag: &str) -> String {
    format!("BadRequestError: Expecting ',' delimiter [{tag}]: line 1 column 5 (char 4)")
}

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}
