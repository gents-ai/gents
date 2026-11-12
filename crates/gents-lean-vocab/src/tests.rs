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
fn decodes_pairing_reconcile_samples_with_stable_desired_resources() {
    let cases = lean_pairing_reconcile_cases();
    assert!(
        !cases.is_empty(),
        "Lean pairing_reconcile_cases must include multi-resource samples"
    );
    let names: BTreeSet<&str> = cases.iter().map(|case| case.name.as_str()).collect();
    assert_eq!(
        names.len(),
        cases.len(),
        "pairing reconcile case names must be unique"
    );
    for case in cases {
        assert_eq!(
            case.before.desired_replicators, case.after.desired_replicators,
            "pairing case {} must keep the desired resource set stable",
            case.name
        );
        assert!(
            !case.before.desired_replicators.is_empty(),
            "pairing case {} must target at least one resource",
            case.name
        );
    }
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
fn generated_root_admission_case_inventory_is_complete() {
    let cases = &crate::lean_contract_snapshot().root_admission_cases;
    assert!(!cases.is_empty());
    let mut names = BTreeSet::new();
    for case in cases {
        assert!(
            names.insert(case.name.as_str()),
            "duplicate case {}",
            case.name
        );
        assert_eq!(case.blank, case.authored.trim().is_empty(), "{}", case.name);
        assert!(
            matches!(case.operation.as_str(), "create" | "meet"),
            "unknown operation {} in {}",
            case.operation,
            case.name
        );
        assert!(
            !case.observation.trim().is_empty(),
            "{} omitted its filesystem observation",
            case.name
        );
    }
}

#[test]
fn generated_command_policy_cases_cover_policy_sandbox_and_env_contracts() {
    let forbidden = lean_command_policy_case("forbidden_prefix_wins_over_allowed_prefix_order");
    assert_eq!(forbidden.category, "forbidden_prefix");
    assert_eq!(forbidden.decision, "deny");
    assert_eq!(forbidden.denial_reason.as_deref(), Some("forbiddenPrefix"));
    assert_eq!(
        forbidden.matched_prefix.as_ref(),
        Some(&vec!["git".to_string()])
    );
    let second_forbidden = lean_command_policy_case("forbidden_prefix_second_configured_match");
    assert_eq!(
        second_forbidden.matched_prefix.as_ref(),
        Some(&vec!["git".to_string(), "diff".to_string()])
    );

    let allowed =
        lean_command_policy_case("allowed_prefix_required_precedes_network_and_allowlist");
    assert_eq!(allowed.decision, "deny");
    assert_eq!(
        allowed.denial_reason.as_deref(),
        Some("allowedPrefixRequired")
    );
    assert_eq!(
        allowed.denied_argv.as_ref(),
        Some(&vec!["curl".to_string(), "https://example.com".to_string()])
    );

    let configured =
        lean_command_policy_case("allowed_prefix_authorizes_read_only_diagnostic_command");
    assert_eq!(configured.category, "read_only_configured_prefix");
    assert_eq!(configured.decision, "allow");

    let configured_forbidden =
        lean_command_policy_case("forbidden_prefix_overrides_configured_read_only_diagnostic");
    assert_eq!(configured_forbidden.decision, "deny");
    assert_eq!(
        configured_forbidden.denial_reason.as_deref(),
        Some("forbiddenPrefix")
    );

    let curl = lean_command_policy_case("disabled_network_read_only_curl_denies_before_allowlist");
    assert_eq!(
        curl.denial_reason.as_deref(),
        Some("disabledNetworkCommand")
    );
    assert_eq!(curl.denied_command.as_deref(), Some("curl"));

    let key = lean_command_env_case("env_key_marker_dropped");
    assert_eq!(key.input_name, "OPENAI_API_KEY");
    assert_eq!(key.expected_output_value, None);

    let pager = lean_command_env_case("env_pager_forced_cat");
    assert_eq!(pager.expected_output_value.as_deref(), Some("cat"));
    let pager_absent = lean_command_env_case("env_pager_absent_still_forced_cat");
    assert_eq!(pager_absent.expected_output_value.as_deref(), Some("cat"));
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
    let follow_up_hooks = &lean_contract_snapshot().follow_up_hooks;
    assert!(
        !follow_up_hooks
            .iter()
            .any(|hook| hook.contains("RuntimeReconcile")),
        "RuntimeReconcile should be emitted as generated contract output, not a follow-up hook"
    );
    assert!(
        !follow_up_hooks
            .iter()
            .any(|hook| hook.contains("ToolExecution")),
        "ToolExecution should be emitted as generated contract output, not a follow-up hook"
    );
    assert!(
        !follow_up_hooks
            .iter()
            .any(|hook| hook.contains("CommandPolicy")),
        "CommandPolicy should be emitted as generated contract output, not a follow-up hook"
    );
    assert_eq!(lean_contract_snapshot().runtime_reconcile_cases.len(), 14);
    assert!(!lean_contract_snapshot().root_admission_cases.is_empty());
    assert_eq!(
        lean_contract_snapshot().request_transition_cases.len(),
        lean_vocabulary_values("RequestState").len().pow(2)
    );
    assert_eq!(lean_contract_snapshot().process_transition_cases.len(), 25);
    assert_eq!(lean_contract_snapshot().eval_outcome_cases.len(), 33);
    assert_eq!(
        lean_contract_snapshot().optimization_cases.decisions.len(),
        32
    );
    assert_eq!(lean_contract_snapshot().optimization_cases.gates.len(), 10);
    assert_eq!(lean_contract_snapshot().optimization_cases.costs.len(), 6);
    assert_eq!(
        lean_contract_snapshot()
            .optimization_cases
            .seed_coverage
            .len(),
        18
    );
    assert_eq!(lean_contract_snapshot().session_recovery_cases.len(), 16);
    assert_eq!(
        lean_contract_snapshot()
            .inference_slot_accounting_cases
            .len(),
        11
    );
    assert_eq!(
        lean_contract_snapshot().fleet_slot_accounting_cases.len(),
        5
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
    assert_eq!(
        lean_contract_snapshot()
            .backend_health_admission_cases
            .len(),
        7
    );
    assert_eq!(
        lean_contract_snapshot().frontend_client_shell_case_count,
        lean_contract_snapshot().frontend_client_shell_cases.len()
    );
    assert_eq!(
        lean_contract_snapshot().frontend_client_shell_cases.len(),
        31
    );
    assert_eq!(
        lean_contract_snapshot().desktop_client_shell_case_count,
        lean_contract_snapshot().desktop_client_shell_cases.len()
    );
    assert_eq!(
        lean_contract_snapshot().desktop_client_shell_cases.len(),
        28
    );
    assert_eq!(lean_contract_snapshot().tool_preflight_cases.len(), 9);
    assert_eq!(lean_contract_snapshot().tool_retry_cases.len(), 90);
    assert_eq!(lean_contract_snapshot().command_policy_cases.len(), 52);
    assert_eq!(lean_contract_snapshot().command_env_cases.len(), 18);
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

#[test]
fn pairing_reconcile_shutdown_boundary_preempts_in_flight_sweep() {
    let cases = lean_pairing_reconcile_shutdown_boundary_cases();
    assert_eq!(cases.len(), 1);
    let case = &cases[0];
    assert_eq!(
        case.name,
        "shutdown_preempts_in_flight_pairing_reconcile_sweep"
    );
    assert_eq!(case.supervisor, "pairingReconciler");
    assert_eq!(case.work_class, "p2pReconcileSweep");
    assert_eq!(case.boundary, "pairingReconcileSupervisorBoundary");
    assert_eq!(case.per_admin_call_timeout_ms, 10_000);
    assert!(case.cancellation_observed_inside_sweep);
    assert!(case.current_admin_future_dropped);
    assert!(case.remaining_peers_skipped);
    assert!(case.shutdown_join_bounded);
}

#[test]
fn pairing_reconcile_top_level_sweep_failure_is_nonterminal_and_retried() {
    let cases = lean_pairing_reconcile_sweep_retry_boundary_cases();
    assert_eq!(cases.len(), 1);
    let case = &cases[0];
    assert_eq!(
        case.name,
        "initial_top_level_sweep_failure_retries_without_terminating_reconciler"
    );
    assert_eq!(case.supervisor, "pairingReconciler");
    assert_eq!(case.work_class, "p2pReconcileSweep");
    assert_eq!(case.boundary, "pairingReconcileSupervisorBoundary");
    assert_eq!(case.failure_scope, "topLevelSweepEnumeration");
    assert!(!case.failure_terminal);
    assert_eq!(case.retry_trigger, "immediateFirstIntervalTick");
    assert!(case.cancellation_prioritized);
    assert!(case.convergence_retried);
}

/// The `OpeningRequest` rows decode strictly; L3 consumes them to drive
/// `opening_request`.
#[test]
fn generated_plugin_job_lineage_cases_decode_strictly() {
    let cases = lean_plugin_job_lineage_cases();
    assert_eq!(
        cases.len(),
        7,
        "Lean pins seven lineage rows in `lineage_regressions`"
    );
    let mut names = BTreeSet::new();
    for case in cases {
        assert!(names.insert(case.name.as_str()), "duplicate {}", case.name);
        let docs: BTreeSet<&str> = case.rows.iter().map(|r| r.doc_id.as_str()).collect();
        assert_eq!(
            docs.len(),
            case.rows.len(),
            "{}: duplicate _docID",
            case.name
        );
        assert!(!case.queries.is_empty(), "{}: no query", case.name);
        for query in &case.queries {
            if let Some(opening) = &query.expected_opening_doc_id {
                assert!(
                    docs.contains(opening.as_str()),
                    "{}: opening request {opening} is not a row",
                    case.name
                );
            }
            assert!(
                !query.listed_start || query.expected_opening_doc_id.is_some(),
                "{}: a listed start has an opening request",
                case.name
            );
        }
    }
}

#[test]
fn queued_steering_traces_are_derived_from_connected_owners() {
    let cases = lean_queued_steering_trace_cases();
    assert_eq!(
        cases.len(),
        9,
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
        assert!(
            case.accepted_input,
            "{} lost its signed admission",
            case.name
        );
        if matches!(case.lifecycle_state.as_str(), "failed" | "interrupted") {
            assert_eq!(
                case.queue_active, None,
                "{} left terminal work active",
                case.name
            );
        }
    }
}

#[test]
fn queued_steering_rejects_incoherent_claim_and_interrupted_publication() {
    let cases = lean_queued_steering_guard_cases();
    assert_eq!(cases.len(), 3);
    for case in cases {
        assert!(
            case.prefix_admitted,
            "case {:?} never reached its rejecting stage",
            case.name
        );
        assert!(
            !case.admitted,
            "case {:?} was unexpectedly admitted",
            case.name
        );
    }
}
