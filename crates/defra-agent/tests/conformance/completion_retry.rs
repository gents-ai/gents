//! CompletionRetry conformance home: consumes Lean-emitted witness rows and
//! checks the public Rust decision mirror used by the owned loop.

use std::collections::BTreeSet;
use std::time::Duration;

use defra_agent::agent::completion_retry::{
    failure_class, CompletionRetryPolicy, CompletionRetryState, FailureClass, MidStreamDirective,
    PreStreamDirective, RetryKind,
};
use defra_agent::error::InferenceError;

use crate::lean_vocab_test::{
    assert_lean_contract_vocabulary_matches, lean_completion_retry_cases, LeanCompletionRetryCase,
    LeanContractVocabulary,
};

pub(super) fn completion_retry_lean_witness_cases_hold() {
    let cases = lean_completion_retry_cases();
    assert_eq!(
        cases.len(),
        13,
        "Lean should emit the finite CompletionRetry witness set"
    );
    assert_lean_contract_vocabulary_matches(LeanContractVocabulary {
        domain: "CompletionRetryFailureClass",
        rust_source: "defra_agent::agent::completion_retry::FailureClass",
        rust_values: &["transport", "parse_bad_request", "permanent"],
    });
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
            "reissue_with_open_effects_illegal" => assert_model_only_open_effects_guard(case),
            "rendered_never_two" => assert_model_only_rendered_never_two(case),
            "permanent_class_cannot_backoff" => assert_permanent_class_cannot_backoff(case),
            other => panic!("unhandled CompletionRetry witness {other}"),
        }
    }
}

fn assert_failure_class_bridge_matches_vocabulary() {
    assert_eq!(
        class_name(failure_class(&transient("temporary"), "temporary")),
        "transport"
    );
    let parse_text = parse_400_text("bridge");
    assert_eq!(
        class_name(failure_class(&transient(&parse_text), &parse_text)),
        "parse_bad_request"
    );
    assert_eq!(
        class_name(failure_class(
            &InferenceError::PermanentFailure {
                reason: "bad request".to_string()
            },
            "bad request",
        )),
        "permanent"
    );
}

fn assert_transport_ladder_progresses(case: &LeanCompletionRetryCase) {
    assert_common(case, "pre_stream_fail", Some("transport"), true);
    assert_eq!(case.expected_phase.as_deref(), Some("backing_off"));
    assert_eq!(case.expected_transport_used, Some(1));

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
    assert_common(case, "pre_stream_fail", Some("transport"), true);
    assert_eq!(case.expected_phase.as_deref(), Some("exhausted"));
    assert_eq!(case.expected_transport_used, Some(3));

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
    assert_common(case, "pre_stream_fail", Some("transport"), true);
    assert_eq!(case.expected_phase.as_deref(), Some("exhausted"));
    assert_eq!(case.expected_transport_used, Some(0));

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
    assert_common(case, "pre_stream_fail", Some("transport"), true);
    assert_eq!(case.expected_phase.as_deref(), Some("exhausted"));
    assert_eq!(case.expected_transport_used, Some(0));

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
    assert_common(case, "pre_stream_fail", Some("parse_bad_request"), true);
    assert_eq!(case.expected_phase.as_deref(), Some("repairing"));
    assert_eq!(case.expected_resample_used, Some(1));
    assert_eq!(
        case.expected_last_parse_error.as_deref(),
        Some("json-parse")
    );

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

/// #653: the resample budget is independent of the transport ladder.
///
/// With a one-step ladder and a three-resample budget, Rust used to draw the
/// delay from `ladder[resampleUsed]`, get `None` on the second resample, and
/// hard-fail with "resample retry budget exhausted" — a lie (the budget had two
/// left) that also skipped repair. The ladder is pacing, not budget: it now
/// saturates at its last step and the budget is honored in full.
fn assert_resample_budget_outlives_ladder(case: &LeanCompletionRetryCase) {
    assert_common(case, "pre_stream_fail", Some("parse_bad_request"), true);
    assert_eq!(case.expected_phase.as_deref(), Some("backing_off"));
    assert_eq!(case.expected_resample_used, Some(2));

    let mut state = CompletionRetryState::new(CompletionRetryPolicy {
        // Ladder shorter than the resample budget — the #653 config.
        transport_backoff: vec![Duration::from_secs(5)],
        max_resample: 3,
        allow_repair: true,
    });

    // Three distinct parse errors: every one must resample, even though the
    // ladder only has a single step to offer.
    for attempt in 0..3 {
        let text = parse_400_text(&format!("json-parse-{attempt}"));
        match state.on_pre_stream_failure(&transient("parse"), &text, now(), None) {
            PreStreamDirective::RetryAfter {
                kind: RetryKind::Resample,
                delay,
            } => {
                // Saturated at the ladder's only (and therefore last) step.
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

/// The flip side: once the resample budget is genuinely spent, the next
/// parse-400 goes to repair — not to a hard fail.
fn assert_resample_exhausts_on_its_own_budget(case: &LeanCompletionRetryCase) {
    assert_common(case, "pre_stream_fail", Some("parse_bad_request"), true);
    assert_eq!(case.expected_phase.as_deref(), Some("repairing"));

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

    // Budget spent: repair is the recourse.
    let text = parse_400_text("json-parse-final");
    assert_eq!(
        state.on_pre_stream_failure(&transient("parse"), &text, now(), None),
        PreStreamDirective::Repair,
        "a spent resample budget must fall through to repair, never hard-fail"
    );
}

fn assert_repair_second_time_illegal(case: &LeanCompletionRetryCase) {
    assert_common(case, "repair_issue", None, false);
    assert_eq!(case.pre_phase, "repairing");
    assert_eq!(case.expected_phase, None);

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
    assert_common(case, "retract", None, false);
    assert_eq!(case.expected_phase, None);

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
    assert_common(case, "close_turn_then_continue", None, true);
    assert_eq!(case.intermediate_phase.as_deref(), Some("turn_closed"));
    assert_eq!(case.intermediate_rendered, Some(1));
    assert_eq!(case.expected_phase.as_deref(), Some("backing_off"));
    assert_eq!(case.expected_turn_index, Some(1));
    assert_eq!(case.expected_effects, Some(0));
    assert_eq!(case.expected_rendered, Some(0));
    assert_eq!(case.expected_transport_used, Some(1));

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

fn assert_model_only_open_effects_guard(case: &LeanCompletionRetryCase) {
    assert_common(case, "pre_stream_fail", Some("transport"), false);
    assert_eq!(case.expected_phase, None);
    assert_eq!(case.rust_surface, "model_only_open_effects_guard");
}

fn assert_model_only_rendered_never_two(case: &LeanCompletionRetryCase) {
    assert_common(case, "stream_ok", None, true);
    assert_eq!(case.expected_phase.as_deref(), Some("turn_done"));
    assert_eq!(case.expected_rendered, Some(1));
}

fn assert_permanent_class_cannot_backoff(case: &LeanCompletionRetryCase) {
    assert_common(case, "pre_stream_fail", Some("permanent"), true);
    assert_eq!(case.expected_phase.as_deref(), Some("failed_permanent"));
    assert_eq!(case.expected_transport_used, Some(0));

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

fn assert_common(
    case: &LeanCompletionRetryCase,
    action: &str,
    failure_class_name: Option<&str>,
    legal: bool,
) {
    assert_eq!(case.action, action);
    assert_eq!(case.failure_class.as_deref(), failure_class_name);
    assert_eq!(case.legal, legal);
    if case.selected_wake.is_some() {
        assert!(
            matches!(
                case.action.as_str(),
                "pre_stream_fail" | "retract" | "close_turn_then_continue"
            ),
            "{} selected a wake for an action that should not carry one",
            case.name
        );
    }
    if !legal {
        assert!(case.expected_transport_used.is_none());
        assert!(case.expected_resample_used.is_none());
        assert!(case.expected_repair_used.is_none());
    }
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
