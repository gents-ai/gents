use super::*;

fn keyed_attempt(
    request_id: &str,
    retry_parent_request: Option<&str>,
    lifecycle_state: RequestLifecycleState,
    is_superseded: bool,
) -> AttemptView {
    AttemptView {
        request: RequestSnapshot {
            request_id: request_id.into(),
            retry_parent_request: retry_parent_request.map(str::to_string),
            lifecycle_state,
            is_superseded,
        },
    }
}

#[test]
fn unordered_turn_resolves_the_single_unreferenced_tip() {
    let attempts = [
        keyed_attempt("r1", None, RequestLifecycleState::Completed, false),
        keyed_attempt("r2", Some("r1"), RequestLifecycleState::Failed, false),
        keyed_attempt("r3", Some("r2"), RequestLifecycleState::Processing, false),
    ];
    assert_eq!(derive_turn(&attempts), Some(ClientTurnState::Running));

    let reversed: Vec<_> = attempts.into_iter().rev().collect();
    assert_eq!(derive_turn(&reversed), Some(ClientTurnState::Running));
}

#[test]
fn unordered_turn_accepts_tip_whose_parent_is_in_scope() {
    let attempts = [
        keyed_attempt("r1", None, RequestLifecycleState::Completed, false),
        keyed_attempt("r2", Some("r1"), RequestLifecycleState::Completed, false),
    ];
    assert_eq!(derive_turn(&attempts), Some(ClientTurnState::Completed));
}

#[test]
fn unordered_turn_rejects_empty_and_duplicate_ids() {
    assert_eq!(derive_turn(&[]), None);

    let duplicated = [
        keyed_attempt("r1", None, RequestLifecycleState::Completed, false),
        keyed_attempt("r1", None, RequestLifecycleState::Completed, false),
    ];
    assert_eq!(derive_turn(&duplicated), None);
}

#[test]
fn unordered_turn_rejects_parent_cycle() {
    let cycle = [
        keyed_attempt("r1", Some("r2"), RequestLifecycleState::Completed, false),
        keyed_attempt("r2", Some("r1"), RequestLifecycleState::Completed, false),
    ];
    assert_eq!(derive_turn(&cycle), None);
}

#[test]
fn unordered_turn_rejects_ambiguous_tips() {
    let ambiguous = [
        keyed_attempt("r1", None, RequestLifecycleState::Completed, false),
        keyed_attempt("r2", None, RequestLifecycleState::Failed, false),
    ];
    assert_eq!(derive_turn(&ambiguous), None);
}
