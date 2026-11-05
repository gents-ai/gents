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

#[test]
fn generated_session_delivery_cases_match_the_lean_classification() {
    let contract: serde_json::Value =
        gents_lean_contract::load_contract_snapshot().expect("Lean contract snapshot");
    let cases = contract["client_session_delivery_cases"]
        .as_array()
        .expect("session delivery cases");
    let mut lifecycle_states = std::collections::BTreeSet::new();
    let mut outcomes = std::collections::BTreeSet::new();
    for case in cases {
        let submitted = match &case["submitted"] {
            serde_json::Value::Null => None,
            submitted => {
                let state = submitted["lifecycle_state"].as_str().expect("lifecycle");
                lifecycle_states.insert(state.to_owned());
                Some(SubmittedAttempt {
                    attempt: keyed_attempt(
                        "request",
                        None,
                        RequestLifecycleState::parse(state).expect("Lean lifecycle state"),
                        submitted["is_superseded"].as_bool().expect("superseded"),
                    ),
                    claimed: submitted["claimed"].as_bool().expect("claimed"),
                })
            }
        };
        let view = SessionDeliveryView {
            submitted,
            pairing: match case["pairing"].as_str() {
                Some("connected") => PairingSignal::Connected,
                Some("not_connected") => PairingSignal::NotConnected,
                other => panic!("unknown pairing {other:?}"),
            },
            session_row: match case["session_row"].as_str() {
                Some("absent") => SessionRowObservation::Absent,
                Some("canonical") => SessionRowObservation::Canonical,
                Some("duplicate") => SessionRowObservation::Duplicate,
                Some("out_of_scope") => SessionRowObservation::OutOfScope,
                other => panic!("unknown session row {other:?}"),
            },
        };
        let expected = match case["expected"].as_str() {
            Some("visible") => SessionDelivery::Visible,
            Some("accepted") => SessionDelivery::Accepted,
            Some("delivering") => SessionDelivery::Delivering,
            Some("not_connected") => SessionDelivery::NotConnected,
            Some("no_request") => SessionDelivery::NoRequest,
            Some("integrity_fault") => SessionDelivery::IntegrityFault,
            other => panic!("unknown delivery {other:?}"),
        };
        outcomes.insert(format!("{expected:?}"));
        assert_eq!(derive_session_delivery(&view), expected, "{case}");
    }
    assert_eq!(
        lifecycle_states,
        RequestLifecycleState::ALL
            .into_iter()
            .map(|state| state.as_str().to_owned())
            .collect(),
        "generated delivery cases must cover every lifecycle state"
    );
    assert_eq!(outcomes.len(), 6, "every delivery outcome is exercised");
}

#[test]
fn session_rows_outside_the_scope_or_duplicated_are_faults() {
    assert_eq!(
        SessionRowObservation::observe(0, true),
        SessionRowObservation::Absent
    );
    assert_eq!(
        SessionRowObservation::observe(1, true),
        SessionRowObservation::Canonical
    );
    assert_eq!(
        SessionRowObservation::observe(2, true),
        SessionRowObservation::Duplicate
    );
    assert_eq!(
        SessionRowObservation::observe(1, false),
        SessionRowObservation::OutOfScope
    );
}
