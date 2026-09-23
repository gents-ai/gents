use super::*;
use crate::lean_vocab_test::lean_request_execution_lease_cases;

#[test]
fn generated_request_execution_lease_contract_decodes_current_inventory() {
    let cases = lean_request_execution_lease_cases();
    assert_eq!(cases.len(), 48);

    let kinds: std::collections::BTreeSet<_> =
        cases.iter().map(|case| case.action.kind()).collect();
    assert_eq!(
        kinds,
        [
            "advance_time",
            "append_output",
            "authorize_producer_decision",
            "begin",
            "claim",
            "drop",
            "finalize",
            "no_op",
            "policy_revoke",
            "recover_dropped",
            "recover_dropped_and_fail",
            "recover_expired",
            "recover_expired_and_fail",
            "recover_expired_terminal",
            "renew",
            "socket_traffic",
        ]
        .into_iter()
        .collect(),
        "the generated action vocabulary changed; update the native adapter contract",
    );
    assert!(
        cases
            .iter()
            .any(|case| matches!(&case.action, LeanRequestExecutionAction::Renew { .. })),
        "the generated contract must retain explicit renewal coverage",
    );
}

#[test]
fn generated_provider_eof_cases_fence_production_policy() {
    let cases = crate::lean_vocab_test::lean_provider_eof_cases();
    assert_eq!(cases.len(), 2);
    for case in cases {
        assert_eq!(
            crate::lifecycle::execution_policy::provider_eof_is_failure(case.saw_explicit_final),
            case.expected_failure,
            "{case:?}",
        );
    }
}
