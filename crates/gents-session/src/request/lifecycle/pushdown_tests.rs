use gents_lean_vocab::{lean_pending_user_turn_cases, LeanPendingUserTurnCase};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_protocol::timeline::{has_durable_user_owner, DurableUserOwnerInput};

use crate::request::lifecycle::{REMOVED_REASON, REPLACED_REASON};

#[test]
fn pending_user_turn_cases_match_lean_table() {
    let cases: &[LeanPendingUserTurnCase] = lean_pending_user_turn_cases();
    assert_eq!(cases.len(), 4, "ownership cases should stay exhaustive");

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
        let rows = if case.retired_by_author {
            vec![
                (
                    RequestLifecycleState::Superseded,
                    REPLACED_REASON,
                    Some("replacement"),
                ),
                (RequestLifecycleState::Interrupted, REMOVED_REASON, None),
            ]
        } else {
            vec![(RequestLifecycleState::Pending, "", None)]
        };
        for (state, reason, superseded_by) in rows {
            let row = AgentRequestRow {
                request_id: "request-under-test".into(),
                lifecycle_state: Some(state),
                failure_reason: (!reason.is_empty()).then(|| reason.to_owned()),
                superseded_by_request: superseded_by.map(str::to_owned),
                ..AgentRequestRow::default()
            };
            assert_eq!(
                crate::request::lifecycle::retired_by_author(&row),
                case.retired_by_author,
                "case {:?} retirement drifted for a {state:?} row",
                case.name
            );
            let actual = !has_durable_user_owner(&messages, "request-under-test")
                && !crate::request::lifecycle::retired_by_author(&row);
            assert_eq!(
                actual, case.expect_pending_turn,
                "case {:?} pending projection drifted with {} unrelated turns",
                case.name, case.unrelated_user_turns
            );
        }
    }
}
