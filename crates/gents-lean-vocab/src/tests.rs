use std::collections::BTreeSet;
use std::sync::Arc;

use acp::{
    AcpStore, DocumentACP, DocumentPermission, Identity, LocalDocumentACP, MemoryAcpStore,
    RelationTuple, READER_RELATION,
};
use identity::Did;

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

const IDENTITY_PERMISSION_POLICY_ID: &str = "identity-permission-cases";
const IDENTITY_PERMISSION_RESOURCE_NAME: &str = "row";

async fn build_local_acp_from_lean_case(
    case: &LeanIdentityPermissionCase,
) -> anyhow::Result<LocalDocumentACP> {
    assert!(
        case.permission.ends_with(".read"),
        "case {:?}: only .read permission fixtures are supported by this ACP witness, got {:?}",
        case.name,
        case.permission
    );
    assert!(
        case.permission
            .starts_with(format!("row:{}:", case.row_owner).as_str()),
        "case {:?}: permission {:?} must be scoped to row owner {:?}",
        case.name,
        case.permission,
        case.row_owner
    );

    let store = Arc::new(MemoryAcpStore::new());
    let acp = LocalDocumentACP::new(store.clone());
    let row_owner = did_from_lean_case(&case.row_owner, case, "row_owner");

    acp.register_doc_object(
        &row_owner,
        IDENTITY_PERMISSION_POLICY_ID,
        IDENTITY_PERMISSION_RESOURCE_NAME,
        &case.row_owner,
    )
    .await?;

    let namespaced_resource =
        format!("{IDENTITY_PERMISSION_POLICY_ID}:{IDENTITY_PERMISSION_RESOURCE_NAME}");
    for grant in &case.grants {
        assert_eq!(
            grant.permission, case.permission,
            "case {:?}: grant {:?} targets a different permission than the row under test",
            case.name, grant
        );
        let node = did_from_lean_case(&grant.actor, case, "grant.actor");
        let tuple = RelationTuple::try_new(
            node,
            READER_RELATION,
            namespaced_resource.as_str(),
            case.row_owner.as_str(),
        )?;
        store.put_tuple(&tuple).await?;
    }

    Ok(acp)
}

fn did_from_lean_case(value: &str, case: &LeanIdentityPermissionCase, field: &str) -> Did {
    Did::new(value).unwrap_or_else(|error| {
        panic!(
            "case {:?}: {field} {:?} is not a valid DefraDB identity DID: {error}",
            case.name, value
        )
    })
}

#[tokio::test]
async fn resolved_identity_permission_cases_drive_defra_acp() -> anyhow::Result<()> {
    for case in lean_identity_permission_cases() {
        if case.expected_actor_did.is_none() || case.expected_peer_did.is_none() {
            continue;
        }
        let acp = build_local_acp_from_lean_case(case).await?;
        let actor = Identity::Authenticated(did_from_lean_case(
            case.expected_actor_did.as_deref().expect("resolved actor"),
            case,
            "expected_actor_did",
        ));
        let peer = Identity::Authenticated(did_from_lean_case(
            case.expected_peer_did.as_deref().expect("resolved peer"),
            case,
            "expected_peer_did",
        ));
        let actor_allowed = acp
            .check_doc_access(
                &actor,
                DocumentPermission::Read,
                IDENTITY_PERMISSION_POLICY_ID,
                IDENTITY_PERMISSION_RESOURCE_NAME,
                &case.row_owner,
            )
            .await?;
        let peer_allowed = acp
            .check_doc_access(
                &peer,
                DocumentPermission::Read,
                IDENTITY_PERMISSION_POLICY_ID,
                IDENTITY_PERMISSION_RESOURCE_NAME,
                &case.row_owner,
            )
            .await?;
        assert_eq!(
            actor_allowed, case.expected_actor_allowed,
            "{}: actor ACP permission",
            case.name
        );
        assert_eq!(
            peer_allowed, case.expected_peer_allowed,
            "{}: peer ACP permission",
            case.name
        );
        assert_eq!(
            actor_allowed == peer_allowed,
            case.expected_decisions_equal,
            "{}: ACP decision equality",
            case.name
        );
    }
    Ok(())
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
