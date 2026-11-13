use std::collections::BTreeSet;

use super::*;

#[test]
fn reports_malformed_arm() {
    let lean = r#"
namespace Sample

def toDefraDB : Sample -> String
  | .one "one"

end Sample
"#;

    let error = parse_lean_to_defradb_values(lean, "Sample").unwrap_err();
    assert!(matches!(
        error,
        LeanVocabularyParseError::MalformedArm {
            reason: "missing `=>`",
            ..
        }
    ));
}

#[test]
fn pack_operator_wire_contract_cases_parse_through_typed_snapshot() {
    assert!(!lean_contract_snapshot().pack_operator_wire_cases.is_empty());
}

#[test]
fn lean_executable_contracts_cover_initial_domains() {
    let request_states: Vec<&str> = lean_contract_snapshot().family_vocabularies
        ["request_state_vocabulary"]["values"]
        .as_array()
        .expect("RequestState vocabulary values")
        .iter()
        .map(|state| state.as_str().expect("RequestState name"))
        .collect();
    assert_lifecycle_transition_cases_partition(
        "Request",
        &request_states,
        lean_request_transition_cases(),
    );
    assert_eq!(lean_contract_snapshot().runtime_reconcile_cases.len(), 14);
    assert_eq!(
        lean_contract_snapshot().request_transition_cases.len(),
        request_states.len().pow(2)
    );
    assert_eq!(
        lean_contract_snapshot()
            .persistence_failure_policy_cases
            .len(),
        2
    );
    assert_eq!(
        lean_contract_snapshot()
            .storage_observation_runtime_cases
            .len(),
        8
    );
    assert_eq!(lean_queue_deadline_cases().len(), 5);
    let sweep_ids = &lean_contract_snapshot().recovery_sweep_ids;
    assert!(!sweep_ids.is_empty(), "the model registers recovery sweeps");
    for sweep_id in sweep_ids {
        assert!(
            lean_recovery_sweep_cases()
                .iter()
                .any(|case| &case.sweep_id == sweep_id),
            "registered recovery sweep {sweep_id} has no emitted case"
        );
    }
    for case in lean_recovery_sweep_cases() {
        assert!(
            sweep_ids.contains(&case.sweep_id),
            "recovery sweep case {} names an unregistered sweep {}",
            case.name,
            case.sweep_id
        );
    }
    assert_eq!(lean_transcript_cases().len(), 11);
    assert_eq!(lean_composed_invariant_witnesses().len(), 4);
}
