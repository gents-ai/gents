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
        24,
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
            "provider_stream_malformed_requires_retraction",
            "provider_reasoning_rejection_requires_retraction",
            "reasoning_rejection_moves_directly_to_repair",
            "reasoning_rejection_after_repair_is_exhausted",
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
        "provider_stream_malformed" => {
            use gents::claude_messages::{
                parse_messages_sse, parse_messages_sse_typed, MessagesParseError,
                ThinkingParseCause,
            };
            let sse = "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n";
            assert_eq!(
                parse_messages_sse_typed(sse, &Default::default())
                    .expect_err("open provider thinking must fail at EOF"),
                MessagesParseError::MalformedThinking {
                    cause: ThinkingParseCause::IncompleteBlock,
                },
            );
            let completion = parse_messages_sse(sse, &Default::default())
                .expect_err("provider truncation must reach the Rig boundary");
            assert!(
                matches!(
                    &completion,
                    rig::completion::CompletionError::ResponseError(_)
                ),
                "provider stream parse failure must not be a local RequestError: {completion}"
            );
            rig::agent::StreamingError::Completion(completion)
        }
        "provider_reasoning_rejected" => {
            rig::agent::StreamingError::Completion(rig::completion::CompletionError::ProviderError(
                "400 invalid_request_error: messages.5.content.0: Invalid `signature` in \
                 `thinking` block. The block is bound to a different conversation."
                    .into(),
            ))
        }
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
        Some("retract_required") if origin == "provider_reasoning_rejected" => {
            assert!(
                matches!(&directive, PreStreamDirective::Repair),
                "{}: a rejected replay goes straight to the one repair: {directive:?}",
                case.name
            );
            state.mark_repair_used();
            assert!(
                matches!(
                    state.on_pre_stream_failure(&classified, &error.to_string(), now(), None),
                    PreStreamDirective::Fail { .. }
                ),
                "{}: a second rejection after the repair fails",
                case.name
            );
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
            if origin == "provider_stream_malformed" {
                // A preview item before EOF takes the native mid-stream
                // branch. Lean's `.streaming` means an in-flight attempt,
                // not a claim that an item has already been observed.
                let mut parser = gents::claude_messages::MessagesSseState::new(Default::default());
                let preview_sse = "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"partial\"}}\n\n";
                let mut saw_preview = false;
                for line in preview_sse.lines() {
                    for event in parser.push_line(line).expect("provider preview") {
                        saw_preview |= matches!(
                            event,
                            rig::streaming::RawStreamingChoice::ReasoningDelta { .. }
                        );
                    }
                }
                assert!(
                    saw_preview,
                    "the mid-stream branch needs a real preview item"
                );
                let eof = parser
                    .finish()
                    .expect_err("provider truncated after preview");
                assert!(
                    matches!(&eof, rig::completion::CompletionError::ResponseError(_)),
                    "truncated provider stream must stay transient after a preview: {eof}"
                );
                let mut state = CompletionRetryState::new(scheduled_like_policy());
                assert!(matches!(
                    state.on_mid_stream_failure(false, now(), None),
                    MidStreamDirective::RetractAndResample { .. }
                ));
            }
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
                reason: "Invalid `signature` in `thinking` block".to_string(),
            },
            "Invalid `signature` in `thinking` block",
        )),
        class_name(failure_class(
            &InferenceError::PermanentFailure {
                reason: "bad request".to_string(),
            },
            "bad request",
        )),
    ];
    assert_eq!(
        observed,
        [
            "transport",
            "parse_bad_request",
            "reasoning_rejected",
            "permanent"
        ]
    );
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
        FailureClass::ReasoningRejected => "reasoning_rejected",
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
