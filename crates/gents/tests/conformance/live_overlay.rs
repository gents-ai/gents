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
    assert_eq!(cases.len(), 2);
    let interrupted = &cases[0];
    assert_eq!(interrupted.lifecycle_state, "interrupted");
    assert!(interrupted.admission_visible);
    assert_eq!(interrupted.canonical_authored_count, 0);
    assert_eq!(interrupted.request_id, 11);
    assert_eq!(interrupted.request_doc_id, 101);
    assert_eq!(interrupted.content_token, 501);
    let published = &cases[1];
    assert_eq!(published.lifecycle_state, "processing");
    assert!(!published.admission_visible);
    assert_eq!(published.canonical_authored_count, 1);
    assert_eq!(published.request_id, interrupted.request_id);
    assert_eq!(published.request_doc_id, interrupted.request_doc_id);
    assert_eq!(published.content_token, interrupted.content_token);
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
