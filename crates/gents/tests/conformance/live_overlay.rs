use gents_protocol::timeline::{has_durable_user_owner, DurableUserOwnerInput};

use crate::lean_vocab_test::{lean_pending_user_turn_cases, LeanPendingUserTurnCase};

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
