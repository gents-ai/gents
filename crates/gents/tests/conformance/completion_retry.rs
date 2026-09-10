use std::collections::BTreeSet;
use std::time::Duration;

use gents::agent::completion_retry::{
    failure_class, CompletionRetryPolicy, CompletionRetryState, FailureClass, MidStreamDirective,
    PreStreamDirective, RetryKind,
};
use gents::error::InferenceError;

use crate::lean_vocab_test::{
    assert_lean_contract_vocabulary_matches, lean_completion_retry_cases, LeanCompletionRetryCase,
    LeanContractVocabulary,
};

pub(super) fn completion_retry_lean_witness_cases_hold() {
    let cases = lean_completion_retry_cases();
    assert_eq!(
        cases.len(),
        22,
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
            "transport_ladder_progresses",
            "transport_exhausts_after_budget",
            "selected_delay_past_deadline_fails_fast",
            "deadline_behind_clock_fails_fast",
            "deterministic_400_skips_to_repair",
            "resample_budget_outlives_transport_ladder",
            "resample_exhausts_on_its_own_budget_then_repairs",
            "repair_second_time_illegal",
            "retract_with_effects_illegal",
            "close_turn_with_effects_legal",
            "reissue_with_open_effects_illegal",
            "rendered_never_two",
            "permanent_class_cannot_backoff",
            "unsatisfied_output_obligation_continues",
            "satisfied_output_obligation_completes",
            "dynamic_output_obligation_incomplete_continues",
            "dynamic_output_obligation_complete_closes",
            "dynamic_output_obligation_overfull_rejects",
            "dynamic_output_obligation_inconsistent_rejects",
            "trigger_output_obligation_inactive_interactive",
            "trigger_output_obligation_inactive_scheduled_control",
            "trigger_output_obligation_active_automated_trigger",
        ]),
        "CompletionRetry witness names drifted"
    );

    for case in cases {
        assert_eq!(case.domain, "completionRetry");
        match case.name.as_str() {
            "transport_ladder_progresses" => assert_transport_ladder_progresses(case),
            "transport_exhausts_after_budget" => assert_transport_exhausts_after_budget(case),
            "selected_delay_past_deadline_fails_fast" => {
                assert_selected_delay_past_deadline_fails_fast(case);
            }
            "deadline_behind_clock_fails_fast" => assert_deadline_behind_clock_fails_fast(case),
            "deterministic_400_skips_to_repair" => assert_deterministic_400_repairs(case),
            "resample_budget_outlives_transport_ladder" => {
                assert_resample_budget_outlives_ladder(case);
            }
            "resample_exhausts_on_its_own_budget_then_repairs" => {
                assert_resample_exhausts_on_its_own_budget(case);
            }
            "repair_second_time_illegal" => assert_repair_second_time_illegal(case),
            "retract_with_effects_illegal" => assert_retract_with_effects_illegal(case),
            "close_turn_with_effects_legal" => assert_close_turn_with_effects_legal(case),
            // These need full owned-loop traces; CompletionRetryState does not
            // own effect closure or rendered-response counts. Do not substitute
            // assertions on expected fixture fields for those observations.
            "reissue_with_open_effects_illegal" | "rendered_never_two" => {
                assert!(
                    case.rust_surface.starts_with("model_only"),
                    "{} needs a runtime consumer",
                    case.name
                );
            }
            "permanent_class_cannot_backoff" => assert_permanent_class_cannot_backoff(case),
            "unsatisfied_output_obligation_continues" => {
                let configured = output_obligation_config();
                assert!(configured[0].1.applies_to(true));
                assert_eq!(configured[0].1.minimum_writes, 1);
                assert_output_obligation_decision(case, configured[0].1.decision(0, None, true));
            }
            "satisfied_output_obligation_completes" => {
                assert_output_obligation_decision(
                    case,
                    output_obligation_config()[0].1.decision(1, None, true),
                );
            }
            "dynamic_output_obligation_incomplete_continues" => {
                assert_output_obligation_decision(
                    case,
                    output_obligation_config()[0].1.decision(2, Some(4), true),
                );
            }
            "dynamic_output_obligation_complete_closes" => {
                assert_output_obligation_decision(
                    case,
                    output_obligation_config()[0].1.decision(4, Some(4), true),
                );
            }
            "dynamic_output_obligation_overfull_rejects" => {
                assert_output_obligation_decision(
                    case,
                    output_obligation_config()[0].1.decision(5, Some(4), true),
                );
            }
            "dynamic_output_obligation_inconsistent_rejects" => {
                assert_output_obligation_decision(
                    case,
                    output_obligation_config()[0].1.decision(2, Some(4), false),
                );
            }
            "trigger_output_obligation_inactive_interactive" => {
                assert!(!output_obligation_config()[0].1.applies_to(false));
            }
            "trigger_output_obligation_inactive_scheduled_control" => {
                assert!(!output_obligation_config()[0].1.applies_to(false));
            }
            "trigger_output_obligation_active_automated_trigger" => {
                assert!(output_obligation_config()[0].1.applies_to(true));
            }
            other => panic!("unhandled CompletionRetry witness {other}"),
        }
    }
}

// Compare the observed production decision to the emitted outcome. This
// projects an enum only; it does not simulate the loop's persistence/turn state.
fn assert_output_obligation_decision(
    case: &LeanCompletionRetryCase,
    actual: gents::document_config::OutputObligationDecision,
) {
    use gents::document_config::OutputObligationDecision;
    let phase = match actual {
        OutputObligationDecision::Continue => "turn_closed",
        OutputObligationDecision::Complete => "turn_done",
        OutputObligationDecision::Reject => "failed_permanent",
    };
    assert_eq!(Some(phase), case.expected_phase.as_deref(), "{}", case.name);
}

fn output_obligation_config() -> Vec<(String, gents::document_config::WriteToolOutputObligation)> {
    vec![(
        "write_result".to_string(),
        gents::document_config::WriteToolOutputObligation {
            scope: gents::document_config::WriteToolOutputObligationScope::Trigger,
            minimum_writes: 1,
            expected_count_field: None,
        },
    )]
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
    assert_eq!(
        state.retry_count(),
        case.expected_transport_used.unwrap() as u32
    );
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
    assert_eq!(
        state.retry_count(),
        case.expected_transport_used.unwrap() as u32
    );
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
    assert_eq!(
        state.retry_count(),
        case.expected_resample_used.unwrap() as u32
    );
}

fn assert_resample_budget_outlives_ladder(case: &LeanCompletionRetryCase) {
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

fn assert_resample_exhausts_on_its_own_budget(case: &LeanCompletionRetryCase) {
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
    assert_eq!(
        state.retry_count(),
        case.expected_transport_used.unwrap() as u32
    );
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
