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
fn runtime_reconcile_state_machine_contract_is_complete() {
    assert_state_machine_contract_is_complete("RuntimeReconcile");
    assert_lean_transition_is_legal("RuntimeReconcile", "applying", "idle");
}

#[test]
fn pack_operator_wire_contract_cases_parse_through_typed_snapshot() {
    assert!(!lean_contract_snapshot().pack_operator_wire_cases.is_empty());
}

#[test]
fn lean_executable_contracts_cover_initial_domains() {
    for domain in [
        "Request",
        "Process",
        "Persistence.failClosed",
        "Persistence.failOpen",
        "StorageObservation.failClosed",
        "StorageObservation.failOpen",
        "RuntimeReconcile",
        "SessionRecovery",
        "InferenceCall",
    ] {
        assert_state_machine_contract_is_complete(domain);
    }

    assert_lean_transition_is_legal("RuntimeReconcile", "applying", "idle");
    assert_lean_transition_is_legal("RuntimeReconcile", "idle", "debouncing");
    assert_lean_transition_is_legal("Persistence.failClosed", "committing", "uncommitted");
    assert_lean_transition_is_legal("Persistence.failOpen", "committing", "lost");
    assert_lean_transition_is_legal("StorageObservation.failClosed", "noMutation", "inFlight");
    assert_lean_transition_is_legal(
        "StorageObservation.failClosed",
        "inFlight",
        "successAcknowledged",
    );
    assert_lean_transition_is_legal(
        "StorageObservation.failClosed",
        "inFlight",
        "mutationFailed",
    );
    assert_lean_transition_is_legal(
        "StorageObservation.failClosed",
        "mutationFailed",
        "noMutation",
    );
    assert_lean_transition_is_legal(
        "StorageObservation.failOpen",
        "mutationFailed",
        "lostAcknowledged",
    );
    assert_lean_transition_is_legal(
        "StorageObservation.failClosed",
        "successAcknowledged",
        "staleObserved",
    );
    assert_lean_transition_is_legal(
        "StorageObservation.failClosed",
        "successAcknowledged",
        "readVisible",
    );
    assert_lean_transition_is_legal(
        "StorageObservation.failClosed",
        "staleObserved",
        "readVisible",
    );
    assert_lean_transition_is_illegal(
        "StorageObservation.failClosed",
        "mutationFailed",
        "lostAcknowledged",
    );
    assert_lean_transition_is_illegal(
        "StorageObservation.failOpen",
        "mutationFailed",
        "noMutation",
    );
    assert_eq!(
        lean_vocabulary_values("SessionRecoveryLatestRequestState"),
        vec![
            "workspaceBindingPending",
            "pending",
            "claimed",
            "processing",
            "waitingForBackend",
            "resumingBackend",
            "completed",
            "failed",
            "superseded",
            "dead",
            "interrupted"
        ]
    );
    assert_lifecycle_transition_cases_partition(
        "Request",
        &lean_vocabulary_values("RequestState"),
        lean_request_transition_cases(),
    );
    assert_lean_transition_is_legal("SessionRecovery", "failed", "pending");
    assert_lean_transition_is_illegal("SessionRecovery", "dead", "pending");
    assert_lean_transition_is_illegal("SessionRecovery", "superseded", "pending");
    assert_lean_transition_is_illegal("SessionRecovery", "interrupted", "pending");
    assert_lean_transition_is_legal("InferenceCall", "queued", "running");
    assert_lean_transition_is_legal("InferenceCall", "running", "completed");
    assert_lean_transition_is_legal("InferenceCall", "running", "failed");
    assert_eq!(lean_contract_snapshot().runtime_reconcile_cases.len(), 14);
    assert_eq!(
        lean_contract_snapshot().request_transition_cases.len(),
        lean_vocabulary_values("RequestState").len().pow(2)
    );
    assert_eq!(lean_contract_snapshot().process_transition_cases.len(), 25);
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
    assert_eq!(
        lean_contract_snapshot().frontend_client_shell_case_count,
        lean_contract_snapshot().frontend_client_shell_cases.len()
    );
    assert_eq!(
        lean_contract_snapshot().frontend_client_shell_cases.len(),
        31
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
