use gents_protocol::timeline::{has_durable_user_owner, DurableUserOwnerInput};

use crate::lean_vocab_test::{lean_pending_user_turn_cases, LeanPendingUserTurnCase};
use crate::lean_vocab_test::{lean_queued_steering_guard_cases, lean_queued_steering_trace_cases};

#[test]
fn pending_user_turn_cases_match_lean_table() {
    let cases: &[LeanPendingUserTurnCase] = lean_pending_user_turn_cases();
    assert_eq!(cases.len(), 3, "ownership cases should stay exhaustive");

    for case in cases {
        let mut messages = (0..case.unrelated_user_turns)
            .map(|_| DurableUserOwnerInput {
                request_id: Some("unrelated-request"),
                is_user: true,
                has_visible_content: true,
                runtime_control: false,
            })
            .collect::<Vec<_>>();
        messages.push(DurableUserOwnerInput {
            request_id: Some("request-under-test"),
            is_user: true,
            has_visible_content: true,
            runtime_control: true,
        });
        if case.has_durable_user_owner {
            messages.push(DurableUserOwnerInput {
                request_id: Some("request-under-test"),
                is_user: true,
                has_visible_content: true,
                runtime_control: false,
            });
        }
        let actual = !has_durable_user_owner(&messages, "request-under-test");
        assert_eq!(
            actual, case.expect_pending_turn,
            "case {:?} pending projection drifted with {} unrelated turns",
            case.name, case.unrelated_user_turns
        );
    }
}

#[test]
fn queued_steering_traces_are_derived_from_connected_owners() {
    let cases = lean_queued_steering_trace_cases();
    assert_eq!(
        cases.len(),
        8,
        "the connected owner traces must be exported"
    );
    for case in cases {
        assert_eq!(case.request_id, case.entry.request_id, "{}", case.name);
        assert_eq!(
            case.capture.request_doc_id, case.request_doc_id,
            "{}",
            case.name
        );
        assert!(
            !case.actions.is_empty(),
            "{} has no generated script",
            case.name
        );
    }
}

#[test]
fn queued_steering_rejects_incoherent_claim_and_interrupted_publication() {
    let cases = lean_queued_steering_guard_cases();
    assert_eq!(cases.len(), 2);
    for case in cases {
        assert!(
            !case.admitted,
            "case {:?} was unexpectedly admitted",
            case.name
        );
    }
}
