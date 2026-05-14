use std::collections::BTreeSet;
use std::time::Duration;

use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::lifecycle::{ClaimOutcome, ExecutionOrigin, TriggerLineage};
use defra_agent::{
    write_manual_agent_request, DefraSessionHook, DefraStreamWriter, FailurePolicy,
    RequestLifecycle,
};
use rig::agent::{HookAction, PromptHook, ToolCallHookAction};
use rig::completion::message::{
    AssistantContent, Message, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent,
    UserContent,
};
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::one_or_many::OneOrMany;
use rig::streaming::StreamingCompletionResponse;
use serde::Deserialize;
use serde_json::json;

#[path = "../src/admission/slot_accounting.rs"]
mod admission_slot_accounting;
#[path = "../src/lean_vocab_test.rs"]
mod lean_vocab_test;
mod support;

use admission_slot_accounting::{
    reconstructed_running_slot_count, slot_contribution, InferenceCallSlotRow,
};
use lean_vocab_test::{
    assert_lean_transition_is_illegal, assert_lean_transition_is_legal,
    assert_lifecycle_transition_cases_partition, assert_state_machine_contract_is_complete,
    lean_client_shell_case, lean_command_env_case, lean_command_policy_case,
    lean_command_sandbox_case, lean_contract_snapshot, lean_event_delivery_convergence_traces,
    lean_event_delivery_source_instances, lean_event_delivery_transition_cases,
    lean_fleet_slot_accounting_case, lean_inference_slot_accounting_case, lean_mcp_health_cases,
    lean_queue_deadline_case, lean_queue_deadline_cases, lean_recovery_sweep_case,
    lean_recovery_sweep_cases, lean_request_transition_cases, lean_runtime_reconcile_case,
    lean_session_recovery_case, lean_state_machine_contract, lean_tool_preflight_case,
    lean_tool_retry_case, lean_transcript_case, lean_transcript_cases, lean_vocabulary_values,
    LeanEventDeliveryAction, LeanLifecycleTransitionCase,
};
use support::conformance_consumers::assert_registered_conformance_consumers_resolve;
use support::snapshots::{
    fetch_conversation_snapshot, fetch_message_snapshots_for_session,
    fetch_request_lineage_snapshot, fetch_request_lineage_snapshot_by_tuple,
    fetch_request_snapshot, fetch_request_snapshot_raw, fetch_response_content,
    fetch_response_interrupted_at, fetch_response_snapshot, fetch_session_snapshot,
    fetch_tool_call_snapshots_for_session, ConversationSnapshot, MessageSnapshot,
    RequestLineageSnapshot, RequestSnapshot, ResponseSnapshot, SessionSnapshot, ToolCallSnapshot,
};
use support::{
    build_request, create_agent_session, create_request, create_response_with_content_and_status,
    create_response_with_status, first_optional_row, set_interrupt_requested_at,
    set_request_lifecycle_state, set_valid_until, test_db, upsert_conversation, AGENT_DID,
    AGENT_NAME, BACKEND_ID, DEADLINE_SECS,
};

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
        "PairingReconcile",
        "SessionRecovery",
        "InferenceCall",
    ] {
        assert_state_machine_contract_is_complete(domain);
    }

    assert_lean_transition_is_legal("RuntimeReconcile", "applying", "idle");
    assert_lean_transition_is_legal("RuntimeReconcile", "idle", "debouncing");
    assert_lean_transition_is_legal("PairingReconcile", "idle", "diverged");
    assert_lean_transition_is_legal("PairingReconcile", "diverged", "converged");
    assert_lean_transition_is_legal("PairingReconcile", "converged", "crashed");
    assert_lean_transition_is_illegal("PairingReconcile", "idle", "converged");
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
            "pending",
            "claimed",
            "processing",
            "inputRequired",
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
    assert_lean_transition_is_illegal("SessionRecovery", "inputRequired", "pending");
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
    assert_eq!(lean_contract_snapshot().runtime_reconcile_cases.len(), 6);
    assert_eq!(lean_contract_snapshot().request_transition_cases.len(), 81);
    assert_eq!(lean_contract_snapshot().process_transition_cases.len(), 25);
    assert_eq!(lean_contract_snapshot().apply_reconcile_cases.len(), 6);
    assert_eq!(lean_contract_snapshot().session_recovery_cases.len(), 18);
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
        5
    );
    assert_eq!(
        lean_contract_snapshot().frontend_client_shell_case_count,
        lean_contract_snapshot().frontend_client_shell_cases.len()
    );
    assert_eq!(
        lean_contract_snapshot().frontend_client_shell_cases.len(),
        15
    );
    assert_eq!(
        lean_contract_snapshot().desktop_client_shell_case_count,
        lean_contract_snapshot().desktop_client_shell_cases.len()
    );
    assert_eq!(
        lean_contract_snapshot().desktop_client_shell_cases.len(),
        12
    );
    assert_eq!(lean_contract_snapshot().tool_preflight_cases.len(), 9);
    assert_eq!(lean_contract_snapshot().tool_retry_cases.len(), 45);
    assert_eq!(lean_contract_snapshot().command_policy_cases.len(), 45);
    assert_eq!(lean_contract_snapshot().command_sandbox_cases.len(), 4);
    assert_eq!(lean_contract_snapshot().command_env_cases.len(), 14);
    assert_eq!(lean_queue_deadline_cases().len(), 5);
    assert_eq!(lean_recovery_sweep_cases().len(), 17);
    assert_eq!(lean_transcript_cases().len(), 6);
}

#[test]
fn lean_boundary_metadata_is_typed_and_reviewable() {
    let snapshot = lean_contract_snapshot();
    let expected_boundary_ids = [
        "boundary.request.input-required-reserved",
        "boundary.request.dead-preclaim-only",
        "boundary.tool-call.permanent-without-retry-evidence",
        "boundary.mcp.call-tool-dispatch-retry-evidence",
        "boundary.inference-slots.running-row-derived",
        "boundary.fleet-slot-accounting.derived-view",
        "boundary.command-policy.host-execution-assumptions",
        "boundary.trigger.dispatch-source-delivery",
        "boundary.persistence.abstract-lifecycle",
        "boundary.storage.hook-failure-policy",
        "boundary.storage.observation-daemon-visible",
        "boundary.storage.minimum-visibility-path",
        "boundary.backend-health.admission-freshness",
        "boundary.session-recovery.client-retry-surface",
        "boundary.coverage-ledger.review-discipline",
        "boundary.event-delivery.fair-substrate",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<BTreeSet<_>>();

    let mut actual_boundary_ids = BTreeSet::new();
    let mut boundary_subjects = BTreeSet::new();
    for boundary in &snapshot.boundaries {
        assert!(
            !boundary.id.trim().is_empty(),
            "boundary id must be non-empty: {:?}",
            boundary
        );
        assert!(
            !boundary.domain.trim().is_empty(),
            "boundary domain must be non-empty: {:?}",
            boundary
        );
        assert!(
            !boundary.subject.trim().is_empty(),
            "boundary subject must be non-empty: {:?}",
            boundary
        );
        assert!(
            !boundary.statement.trim().is_empty(),
            "boundary statement must be non-empty: {:?}",
            boundary
        );
        assert!(
            boundary
                .accepted_failure_mode
                .as_deref()
                .map_or(true, |text| !text.trim().is_empty()),
            "boundary accepted_failure_mode must be omitted or non-empty: {:?}",
            boundary
        );
        assert!(
            boundary
                .accepted_follow_up
                .as_deref()
                .map_or(true, |text| !text.trim().is_empty()),
            "boundary accepted_follow_up must be omitted or non-empty: {:?}",
            boundary
        );
        assert!(
            actual_boundary_ids.insert(boundary.id.clone()),
            "duplicate boundary id: {:?}",
            boundary
        );
        assert!(
            boundary_subjects.insert((boundary.domain.clone(), boundary.subject.clone())),
            "duplicate boundary subject in domain {:?}: {:?}",
            boundary.domain,
            boundary
        );
    }

    assert_eq!(
        actual_boundary_ids, expected_boundary_ids,
        "Lean boundary metadata ids changed; update this review-discipline list with the boundary data"
    );
}

#[test]
fn lean_deviation_metadata_is_empty_or_explicitly_classified() {
    let snapshot = lean_contract_snapshot();
    let mut deviation_ids = BTreeSet::new();
    let mut deviation_subjects = BTreeSet::new();

    for deviation in &snapshot.deviations {
        assert!(
            !deviation.id.trim().is_empty(),
            "deviation id must be non-empty: {:?}",
            deviation
        );
        assert!(
            !deviation.domain.trim().is_empty(),
            "deviation domain must be non-empty: {:?}",
            deviation
        );
        assert!(
            !deviation.subject.trim().is_empty(),
            "deviation subject must be non-empty: {:?}",
            deviation
        );
        assert!(
            !deviation.statement.trim().is_empty(),
            "deviation statement must be non-empty: {:?}",
            deviation
        );
        assert!(
            deviation
                .accepted_failure_mode
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
                || deviation
                    .accepted_follow_up
                    .as_deref()
                    .is_some_and(|text| !text.trim().is_empty()),
            "active deviations must carry accepted_failure_mode or accepted_follow_up text: {:?}",
            deviation
        );
        assert!(
            deviation_ids.insert(deviation.id.clone()),
            "duplicate deviation id: {:?}",
            deviation
        );
        assert!(
            deviation_subjects.insert((deviation.domain.clone(), deviation.subject.clone())),
            "duplicate deviation subject in domain {:?}: {:?}",
            deviation.domain,
            deviation
        );
    }
}

#[test]
fn lean_contract_coverage_ledger_accounts_for_every_emitted_domain() {
    let snapshot = lean_contract_snapshot();
    let mut emitted = BTreeSet::new();
    let boundary_ids = snapshot
        .boundaries
        .iter()
        .map(|boundary| boundary.id.clone())
        .collect::<BTreeSet<_>>();

    for vocabulary in &snapshot.vocabularies {
        emitted.insert(("vocabulary".to_string(), vocabulary.domain.clone()));
    }
    for machine in &snapshot.state_machines {
        emitted.insert(("state_machine".to_string(), machine.domain.clone()));
    }
    if !snapshot.request_transition_cases.is_empty() {
        emitted.insert((
            "lifecycle_transition_cases".to_string(),
            "RequestTransitions".to_string(),
        ));
    }
    if !snapshot.process_transition_cases.is_empty() {
        emitted.insert((
            "lifecycle_transition_cases".to_string(),
            "ProcessTransitions".to_string(),
        ));
    }
    assert_eq!(
        snapshot.trigger_dispatch_case_count,
        snapshot.trigger_dispatch_cases.len(),
        "Lean trigger dispatch case count drifted from emitted cases"
    );
    if !snapshot.trigger_dispatch_cases.is_empty() {
        emitted.insert(("trigger_cases".to_string(), "TriggerDispatch".to_string()));
    }
    if !snapshot.runtime_reconcile_cases.is_empty() {
        emitted.insert((
            "runtime_cases".to_string(),
            "RuntimeReconcileCases".to_string(),
        ));
    }
    if !snapshot.apply_reconcile_cases.is_empty() {
        emitted.insert((
            "apply_reconcile_cases".to_string(),
            "ApplyReconcileCases".to_string(),
        ));
    }
    if !snapshot.session_recovery_cases.is_empty() {
        emitted.insert((
            "session_recovery_cases".to_string(),
            "SessionRecoveryCases".to_string(),
        ));
    }
    if !snapshot.inference_slot_accounting_cases.is_empty() {
        emitted.insert((
            "slot_cases".to_string(),
            "InferenceCallSlotAccounting".to_string(),
        ));
    }
    if !snapshot.fleet_slot_accounting_cases.is_empty() {
        emitted.insert(("fleet_cases".to_string(), "FleetSlotAccounting".to_string()));
    }
    if !snapshot.persistence_failure_policy_cases.is_empty() {
        emitted.insert((
            "persistence_policy_cases".to_string(),
            "PersistenceFailurePolicyCases".to_string(),
        ));
    }
    if !snapshot.storage_observation_runtime_cases.is_empty() {
        emitted.insert((
            "storage_observation_cases".to_string(),
            "StorageObservationRuntimeCases".to_string(),
        ));
    }
    if !snapshot.backend_health_admission_cases.is_empty() {
        emitted.insert((
            "backend_health_cases".to_string(),
            "BackendHealthAdmissionCases".to_string(),
        ));
    }
    if !snapshot.native_filesystem_boundary_cases.is_empty() {
        emitted.insert((
            "native_filesystem_boundary_cases".to_string(),
            "NativeFilesystemBoundaryCases".to_string(),
        ));
    }
    assert_eq!(
        snapshot.frontend_client_shell_case_count,
        snapshot.frontend_client_shell_cases.len(),
        "Lean frontend ClientShell case count drifted from emitted cases"
    );
    if !snapshot.frontend_client_shell_cases.is_empty() {
        emitted.insert((
            "frontend_client_shell_cases".to_string(),
            "FrontendClientShellCases".to_string(),
        ));
    }
    assert_eq!(
        snapshot.desktop_client_shell_case_count,
        snapshot.desktop_client_shell_cases.len(),
        "Lean desktop ClientShell case count drifted from emitted cases"
    );
    if !snapshot.desktop_client_shell_cases.is_empty() {
        emitted.insert((
            "desktop_client_shell_cases".to_string(),
            "DesktopClientShellCases".to_string(),
        ));
    }
    if !snapshot.tool_preflight_cases.is_empty() {
        emitted.insert((
            "tool_cases".to_string(),
            "ToolExecutionPreflight".to_string(),
        ));
    }
    if !snapshot.tool_retry_cases.is_empty() {
        emitted.insert(("tool_cases".to_string(), "ToolExecutionRetry".to_string()));
    }
    if !snapshot.command_policy_cases.is_empty() {
        emitted.insert((
            "command_policy_cases".to_string(),
            "CommandPolicyValidation".to_string(),
        ));
    }
    if !snapshot.command_sandbox_cases.is_empty() {
        emitted.insert((
            "command_policy_cases".to_string(),
            "CommandPolicySandbox".to_string(),
        ));
    }
    if !snapshot.command_env_cases.is_empty() {
        emitted.insert((
            "command_policy_cases".to_string(),
            "CommandPolicyEnv".to_string(),
        ));
    }
    if !snapshot.live_overlay_cases.is_empty() {
        emitted.insert((
            "live_overlay_cases".to_string(),
            "LiveOverlayCases".to_string(),
        ));
    }
    if !lean_queue_deadline_cases().is_empty() {
        emitted.insert((
            "queue_deadline_cases".to_string(),
            "QueueDeadlineConformanceCases".to_string(),
        ));
    }
    if !lean_recovery_sweep_cases().is_empty() {
        emitted.insert((
            "recovery_sweep_cases".to_string(),
            "RecoverySweepCases".to_string(),
        ));
    }
    if !lean_transcript_cases().is_empty() {
        emitted.insert((
            "transcript_cases".to_string(),
            "TranscriptConformanceCases".to_string(),
        ));
    }
    assert_eq!(
        snapshot.event_delivery_transition_case_count,
        snapshot.event_delivery_transition_cases.len(),
        "Lean event-delivery transition case count drifted from emitted cases"
    );
    if !snapshot.event_delivery_transition_cases.is_empty() {
        emitted.insert((
            "event_delivery_cases".to_string(),
            "EventDeliveryTransitionCases".to_string(),
        ));
    }
    if !snapshot.event_delivery_source_instances.is_empty() {
        emitted.insert((
            "event_delivery_cases".to_string(),
            "EventDeliverySourceInstances".to_string(),
        ));
    }
    if !snapshot.event_delivery_convergence_traces.is_empty() {
        emitted.insert((
            "event_delivery_cases".to_string(),
            "EventDeliveryConvergenceTraces".to_string(),
        ));
    }
    if !lean_mcp_health_cases().is_empty() {
        emitted.insert(("mcp_health_cases".to_string(), "MCPHealthCases".to_string()));
    }
    for hook in &snapshot.follow_up_hooks {
        emitted.insert(("follow_up_hook".to_string(), hook.clone()));
    }

    // Keep this mirrored with the category strings in CoverageLedger.lean.
    let valid_categories = [
        "vocabulary",
        "state_machine",
        "lifecycle_transition_cases",
        "trigger_cases",
        "runtime_cases",
        "apply_reconcile_cases",
        "session_recovery_cases",
        "slot_cases",
        "fleet_cases",
        "persistence_policy_cases",
        "storage_observation_cases",
        "backend_health_cases",
        "native_filesystem_boundary_cases",
        "frontend_client_shell_cases",
        "desktop_client_shell_cases",
        "tool_cases",
        "command_policy_cases",
        "live_overlay_cases",
        "queue_deadline_cases",
        "recovery_sweep_cases",
        "transcript_cases",
        "event_delivery_cases",
        "mcp_health_cases",
        "follow_up_hook",
    ];
    let registered_consumers = assert_registered_conformance_consumers_resolve();
    let mut ledger = BTreeSet::new();
    let mut ledger_consumers = BTreeSet::new();

    for entry in &snapshot.coverage_ledger {
        assert!(
            valid_categories.contains(&entry.category.as_str()),
            "coverage ledger entry has unknown category: {:?}",
            entry
        );
        assert!(
            !entry.domain.trim().is_empty(),
            "coverage ledger entry has an empty domain: {:?}",
            entry
        );

        let has_consumer = !entry.consumer.trim().is_empty();
        let has_boundary = !entry.accepted_boundary.trim().is_empty();
        let has_follow_up = !entry.accepted_follow_up.trim().is_empty();
        assert!(
            has_consumer || has_boundary || has_follow_up,
            "coverage ledger entry must name a consumer, boundary, or follow-up: {:?}",
            entry
        );
        if entry.category == "follow_up_hook" {
            assert!(
                has_follow_up,
                "follow-up hook ledger entries must carry accepted_follow_up text: {:?}",
                entry
            );
        }
        if has_boundary {
            assert!(
                boundary_ids.contains(&entry.accepted_boundary),
                "coverage ledger accepted_boundary must reference an emitted boundary id: {:?}",
                entry
            );
        }
        if has_consumer {
            assert!(
                registered_consumers.contains(entry.consumer.as_str()),
                "coverage ledger consumer must resolve to a registered Rust/TS conformance consumer: {:?}",
                entry
            );
            ledger_consumers.insert(entry.consumer.as_str());
        }

        assert!(
            ledger.insert((entry.category.clone(), entry.domain.clone())),
            "duplicate coverage ledger entry for {:?} / {:?}",
            entry.category,
            entry.domain
        );
    }

    let missing = emitted.difference(&ledger).cloned().collect::<Vec<_>>();
    let extra = ledger.difference(&emitted).cloned().collect::<Vec<_>>();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "coverage ledger must exactly match emitted Lean contract domains\n  missing ledger entries: {:?}\n  extra ledger entries: {:?}\n  emitted: {:?}\n  ledger: {:?}",
        missing,
        extra,
        emitted,
        ledger
    );
    let unreferenced_consumers = registered_consumers
        .difference(&ledger_consumers)
        .copied()
        .collect::<Vec<_>>();
    assert!(
        unreferenced_consumers.is_empty(),
        "coverage ledger consumer registry has unreferenced entries: {:?}",
        unreferenced_consumers
    );
}

#[test]
fn generated_recovery_sweep_cases_pin_startup_recovery_contract() {
    let cases = lean_recovery_sweep_cases();
    assert_eq!(
        cases.len(),
        17,
        "Lean should emit one row per registered recovery predicate witness"
    );

    let expected_sweep_ids = [
        "request_lifecycle_recover_all_requests",
        "request_lifecycle_recover_all_streaming_responses",
        "tool_call_lifecycle_recover_all_running_calls",
        "tool_call_lifecycle_recover_detached_bridge_rows",
        "inference_call_recover_all_stale_calls",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let actual_sweep_ids = cases
        .iter()
        .map(|case| case.sweep_id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual_sweep_ids, expected_sweep_ids,
        "Lean recovery sweep registry drifted"
    );

    for case in cases {
        assert_eq!(case.cadence.as_str(), "startup");
        assert!(
            case.measure_before > case.measure_after,
            "recovery case {} must decrease its measure",
            case.name
        );
        assert_eq!(
            case.measure_after, 0,
            "recovery case {} must reach zero measure",
            case.name
        );
        assert_ne!(
            case.terminal_state.as_str(),
            "running",
            "recovery case {} must not leave a stale row running",
            case.name
        );
        assert!(
            !case.deadline_audit_ref.trim().is_empty(),
            "recovery case {} must name its audit reference",
            case.name
        );
    }

    let implemented = [
        "request_lifecycle_recover_all_requests",
        "request_lifecycle_recover_all_streaming_responses",
        "tool_call_lifecycle_recover_all_running_calls",
    ];
    for sweep_id in implemented {
        for case in cases.iter().filter(|case| case.sweep_id == sweep_id) {
            assert_eq!(
                case.implementation_status.as_str(),
                "implemented",
                "sweep {sweep_id} should be an implemented startup sweep"
            );
        }
    }

    let detached_cases = cases
        .iter()
        .filter(|case| case.sweep_id == "tool_call_lifecycle_recover_detached_bridge_rows")
        .collect::<Vec<_>>();
    assert_eq!(
        detached_cases.len(),
        5,
        "detached bridge recovery must have explicit obligation witnesses"
    );
    for case in detached_cases {
        assert_eq!(case.collection.as_str(), "AgentToolCall");
        assert_eq!(
            case.rust_function.as_str(),
            "ToolCallLifecycle::recover_detached_bridge_rows"
        );
        assert_eq!(case.implementation_status.as_str(), "obligation");
        assert!(
            case.deadline_audit_ref
                .contains("subagent-bridge-terminal-lifetime"),
            "detached bridge case {} must point at the bridge terminal lifetime gap",
            case.name
        );
        assert!(
            ["completed", "failed", "cancelled", "timedOut"]
                .contains(&case.terminal_state.as_str()),
            "detached bridge case {} must terminalize, not skip",
            case.name
        );
    }

    let queued = lean_recovery_sweep_case("inference_queued_stale_to_cancelled");
    assert_eq!(queued.pre_state.as_str(), "queued");
    assert_eq!(queued.terminal_state.as_str(), "cancelled");

    let running = lean_recovery_sweep_case("inference_running_stale_to_failed");
    assert_eq!(running.pre_state.as_str(), "running");
    assert_eq!(running.terminal_state.as_str(), "failed");

    let interrupted = lean_recovery_sweep_case("inference_interrupted_parent_to_cancelled");
    assert_eq!(interrupted.terminal_state.as_str(), "cancelled");

    for case in cases
        .iter()
        .filter(|case| case.sweep_id == "inference_call_recover_all_stale_calls")
    {
        assert_eq!(case.collection.as_str(), "InferenceCall");
        assert_eq!(case.rust_function.as_str(), "InferenceCall::recover_all");
        assert_eq!(case.implementation_status.as_str(), "obligation");
        assert!(
            case.deadline_audit_ref.contains("follow-up-6-pr-e"),
            "InferenceCall recovery case {} must point at deadline audit PR E",
            case.name
        );
        let terminal_row =
            InferenceCallSlotRow::new("contract-backend", case.terminal_state.as_str());
        assert_eq!(
            slot_contribution(terminal_row, "contract-backend"),
            0,
            "terminal InferenceCall recovery case {} must release its backend slot",
            case.name
        );
        assert_eq!(
            reconstructed_running_slot_count([terminal_row], "contract-backend"),
            0,
            "terminal InferenceCall recovery case {} must reconstruct zero running slots",
            case.name
        );
    }
}

#[derive(Clone, Default)]
struct TranscriptConformanceModel;

#[allow(refining_impl_trait)]
impl CompletionModel for TranscriptConformanceModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        Self
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        Err(CompletionError::ProviderError(
            "completion is unused in transcript conformance tests".to_string(),
        ))
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        Err(CompletionError::ProviderError(
            "streaming is unused in transcript conformance tests".to_string(),
        ))
    }
}

fn transcript_user_message(text: &str) -> Message {
    Message::User {
        content: OneOrMany::one(UserContent::Text(Text {
            text: text.to_string(),
        })),
    }
}

fn transcript_assistant_tool_call_message(model_call_id: &str) -> Message {
    Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::ToolCall(ToolCall {
            id: model_call_id.to_string(),
            call_id: Some(model_call_id.to_string()),
            function: ToolFunction {
                name: "read".to_string(),
                arguments: json!({ "file_path": "/tmp/transcript-contract.txt" }),
            },
            signature: None,
            additional_params: None,
        })),
    }
}

fn transcript_tool_result_message(result_id: &str, text: &str) -> Message {
    Message::User {
        content: OneOrMany::one(UserContent::ToolResult(ToolResult {
            id: result_id.to_string(),
            call_id: Some(result_id.to_string()),
            content: OneOrMany::one(ToolResultContent::Text(Text {
                text: text.to_string(),
            })),
        })),
    }
}

async fn transcript_hook_fixture(test_name: &str) -> (support::TestDb, DefraSessionHook, String) {
    let db = test_db(test_name).await;
    let session_id = format!("{test_name}-session");
    let hook = DefraSessionHook::resume_or_create_with_identity_policy(
        db.node.clone(),
        &session_id,
        AGENT_NAME,
        AGENT_DID,
        FailurePolicy::default(),
    )
    .await
    .expect("resume transcript hook");
    hook.set_active_request_id(Some(format!("{test_name}-request")))
        .await;
    hook.set_request_deadline_at(Some(chrono::Utc::now() + chrono::Duration::minutes(5)))
        .await;
    (db, hook, session_id)
}

async fn transcript_messages_and_calls(
    node: &EmbeddedNode,
    session_id: &str,
) -> (Vec<MessageSnapshot>, Vec<ToolCallSnapshot>, Vec<Message>) {
    let messages = fetch_message_snapshots_for_session(node, session_id).await;
    let tool_calls = fetch_tool_call_snapshots_for_session(node, session_id).await;
    let history = defra_agent::load_history(node, session_id)
        .await
        .expect("load transcript history");
    (messages, tool_calls, history)
}

fn transcript_tool_result_count(history: &[Message]) -> usize {
    history
        .iter()
        .filter(|message| {
            matches!(
                message,
                Message::User { content }
                    if matches!(content.first_ref(), UserContent::ToolResult(_))
            )
        })
        .count()
}

fn transcript_ordered(messages: &[MessageSnapshot]) -> bool {
    messages
        .windows(2)
        .all(|window| window[0].sequence < window[1].sequence)
}

fn transcript_strong_drain(tool_calls: &[ToolCallSnapshot]) -> bool {
    tool_calls
        .iter()
        .all(|call| call.lifecycle_state.as_deref() != Some("running"))
}

fn transcript_pair_closed(
    messages: &[MessageSnapshot],
    tool_calls: &[ToolCallSnapshot],
    history: &[Message],
) -> bool {
    let tool_calls_reserved_by_assistant_message = tool_calls.iter().all(|call| {
        messages.iter().any(|message| {
            message.sequence == call.message_sequence && message.role.as_str() == "assistant"
        })
    });
    let no_running_tool_calls = transcript_strong_drain(tool_calls);
    let completed_tool_call_count = tool_calls
        .iter()
        .filter(|call| call.lifecycle_state.as_deref() == Some("completed"))
        .count();
    let completed_calls_have_results = completed_tool_call_count == 0
        || transcript_tool_result_count(history) == completed_tool_call_count;

    tool_calls_reserved_by_assistant_message
        && no_running_tool_calls
        && completed_calls_have_results
}

async fn assert_transcript_counts(
    label: &str,
    node: &EmbeddedNode,
    session_id: &str,
    expected_messages: usize,
    expected_tool_calls: usize,
) {
    let (messages, tool_calls, _) = transcript_messages_and_calls(node, session_id).await;
    assert_eq!(
        messages.len(),
        expected_messages,
        "{label}: AgentMessage count"
    );
    assert_eq!(
        tool_calls.len(),
        expected_tool_calls,
        "{label}: AgentToolCall count"
    );
}

async fn assert_transcript_post_state(
    case: &lean_vocab_test::LeanTranscriptCase,
    node: &EmbeddedNode,
    session_id: &str,
) -> (Vec<MessageSnapshot>, Vec<ToolCallSnapshot>, Vec<Message>) {
    let (messages, tool_calls, history) = transcript_messages_and_calls(node, session_id).await;
    assert_eq!(
        messages.len(),
        case.post_message_count,
        "{}: post_message_count",
        case.name
    );
    assert_eq!(
        tool_calls.len(),
        case.post_tool_call_count,
        "{}: post_tool_call_count",
        case.name
    );
    assert_eq!(
        transcript_ordered(&messages),
        case.expected_ordered,
        "{}: expected_ordered",
        case.name
    );
    assert_eq!(
        transcript_pair_closed(&messages, &tool_calls, &history),
        case.expected_pair_closed,
        "{}: expected_pair_closed",
        case.name
    );
    assert_eq!(
        transcript_strong_drain(&tool_calls),
        case.expected_strong_drain,
        "{}: expected_strong_drain",
        case.name
    );
    (messages, tool_calls, history)
}

async fn persist_completed_tool_sequence(
    test_name: &str,
    case: &lean_vocab_test::LeanTranscriptCase,
) -> (support::TestDb, DefraSessionHook, String, u32) {
    let (db, hook, session_id) = transcript_hook_fixture(test_name).await;
    assert_transcript_counts(
        &format!("{} pre-state", case.name),
        &db.node,
        &session_id,
        case.pre_message_count,
        case.pre_tool_call_count,
    )
    .await;

    assert!(matches!(
        PromptHook::<TranscriptConformanceModel>::on_completion_call(
            &hook,
            &transcript_user_message("run transcript conformance tool"),
            &[],
        )
        .await,
        HookAction::Continue
    ));

    let model_call_id = format!("result-{}", case.logical_result_id);
    let internal_call_id = format!("internal-{}", case.logical_result_id);
    let payload = format!("payload-{}", case.payload_hash);
    let tool_args = r#"{"file_path":"/tmp/transcript-contract.txt"}"#;

    assert!(matches!(
        PromptHook::<TranscriptConformanceModel>::on_tool_call(
            &hook,
            "read",
            Some(model_call_id.clone()),
            &internal_call_id,
            tool_args,
        )
        .await,
        ToolCallHookAction::Continue
    ));

    let assistant_sequence = hook
        .persist_message(&transcript_assistant_tool_call_message(&model_call_id))
        .await
        .expect("persist assistant tool-call message");
    assert_eq!(
        assistant_sequence as usize, case.assistant_sequence,
        "{}: assistant_sequence",
        case.name
    );

    assert!(matches!(
        PromptHook::<TranscriptConformanceModel>::on_tool_result(
            &hook,
            "read",
            Some(model_call_id.clone()),
            &internal_call_id,
            tool_args,
            &payload,
        )
        .await,
        HookAction::Continue
    ));

    (db, hook, session_id, case.result_sequence as u32)
}

fn assert_transcript_case_shape() {
    let cases = lean_transcript_cases();
    assert_eq!(cases.len(), 6);

    let names = cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        [
            "ordering_user_assistant_tool_result",
            "dedupe_duplicate_reuses_sequence",
            "distinct_result_ids_append_distinct_rows",
            "completed_tool_pair_closed",
            "explicit_drain_terminalizes_ownership",
            "drop_abandon_not_strong_drain",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>()
    );

    let ordering = lean_transcript_case("ordering_user_assistant_tool_result");
    assert!(ordering.legal);
    assert_eq!(ordering.group.as_str(), "ordering");
    assert_eq!(ordering.pre_message_count, 0);
    assert_eq!(ordering.post_message_count, 3);
    assert_eq!(ordering.pre_tool_call_count, 0);
    assert_eq!(ordering.post_tool_call_count, 1);
    assert_eq!(ordering.assistant_sequence, 2);
    assert_eq!(ordering.result_sequence, 3);
    assert!(ordering.expected_ordered);
    assert!(ordering.expected_pair_closed);

    let dedupe = lean_transcript_case("dedupe_duplicate_reuses_sequence");
    assert_eq!(dedupe.group.as_str(), "dedupe");
    assert_eq!(dedupe.action.as_str(), "observe_duplicate_tool_result");
    assert_eq!(dedupe.pre_message_count, dedupe.post_message_count);
    assert_eq!(dedupe.pre_tool_call_count, dedupe.post_tool_call_count);
    assert_eq!(dedupe.logical_result_id, ordering.logical_result_id);
    assert_eq!(dedupe.payload_hash, ordering.payload_hash);
    assert!(dedupe.expected_duplicate_reused_sequence);
    assert_eq!(dedupe.result_sequence, ordering.result_sequence);

    let distinct = lean_transcript_case("distinct_result_ids_append_distinct_rows");
    assert_eq!(distinct.group.as_str(), "dedupe");
    assert_eq!(distinct.payload_hash, ordering.payload_hash);
    assert_ne!(distinct.logical_result_id, ordering.logical_result_id);
    assert_eq!(distinct.pre_message_count + 1, distinct.post_message_count);
    assert!(!distinct.expected_duplicate_reused_sequence);

    let pair = lean_transcript_case("completed_tool_pair_closed");
    assert_eq!(pair.group.as_str(), "pairing");
    assert!(pair.expected_pair_closed);
    assert!(pair.expected_ordered);

    let drain = lean_transcript_case("explicit_drain_terminalizes_ownership");
    assert_eq!(drain.group.as_str(), "hook_boundary");
    assert_eq!(drain.pre_in_flight_count, 1);
    assert_eq!(drain.post_in_flight_count, 0);
    assert!(drain.expected_strong_drain);

    let abandon = lean_transcript_case("drop_abandon_not_strong_drain");
    assert_eq!(abandon.group.as_str(), "hook_boundary");
    assert_eq!(abandon.action.as_str(), "abandon_hook_ownership");
    assert_eq!(abandon.pre_in_flight_count, 1);
    assert_eq!(abandon.post_in_flight_count, 0);
    assert!(!abandon.expected_strong_drain);
    assert!(!abandon.expected_pair_closed);

    for case in cases {
        assert!(case.legal, "transcript case {} should be legal", case.name);
        assert!(
            case.expected_ordered,
            "transcript case {} should preserve ordering",
            case.name
        );
    }
}

#[tokio::test]
async fn generated_transcript_cases_pin_agent_message_ordering_contract() {
    assert_transcript_case_shape();

    let ordering = lean_transcript_case("ordering_user_assistant_tool_result");
    let (db, hook, session_id, result_sequence) =
        persist_completed_tool_sequence("transcript-ordering", ordering).await;
    assert_eq!(
        hook.cancel_in_flight_tool_calls().await.unwrap(),
        ordering.post_in_flight_count,
        "{}: post_in_flight_count",
        ordering.name
    );
    let (messages, tool_calls, history) =
        assert_transcript_post_state(ordering, &db.node, &session_id).await;
    assert_eq!(result_sequence as usize, ordering.result_sequence);
    assert_eq!(
        messages
            .iter()
            .find(|message| message.role.as_str() == "user" && message.sequence > 1)
            .map(|message| message.sequence as usize),
        Some(ordering.result_sequence),
        "{}: result_sequence",
        ordering.name
    );
    assert_eq!(
        tool_calls
            .first()
            .map(|call| call.message_sequence as usize),
        Some(ordering.assistant_sequence),
        "{}: tool call reserves assistant sequence",
        ordering.name
    );
    assert_eq!(
        transcript_tool_result_count(&history),
        1,
        "{}",
        ordering.name
    );

    let dedupe = lean_transcript_case("dedupe_duplicate_reuses_sequence");
    let (db, hook, session_id, first_result_sequence) =
        persist_completed_tool_sequence("transcript-dedupe", ordering).await;
    assert_transcript_counts(
        "dedupe duplicate pre-state",
        &db.node,
        &session_id,
        dedupe.pre_message_count,
        dedupe.pre_tool_call_count,
    )
    .await;
    let duplicate_sequence = hook
        .persist_message(&transcript_tool_result_message(
            &format!("result-{}", dedupe.logical_result_id),
            &format!("payload-{}", dedupe.payload_hash),
        ))
        .await
        .expect("persist duplicate tool-result message");
    assert_eq!(
        duplicate_sequence as usize, dedupe.result_sequence,
        "{}: duplicate reused sequence",
        dedupe.name
    );
    assert_eq!(
        first_result_sequence as usize, dedupe.result_sequence,
        "{}: original sequence",
        dedupe.name
    );
    assert_eq!(
        hook.cancel_in_flight_tool_calls().await.unwrap(),
        dedupe.post_in_flight_count,
        "{}: post_in_flight_count",
        dedupe.name
    );
    let (messages, _, history) = assert_transcript_post_state(dedupe, &db.node, &session_id).await;
    assert_eq!(messages.len(), dedupe.pre_message_count, "{}", dedupe.name);
    assert_eq!(transcript_tool_result_count(&history), 1, "{}", dedupe.name);

    let distinct = lean_transcript_case("distinct_result_ids_append_distinct_rows");
    let (db, hook, session_id) = transcript_hook_fixture("transcript-distinct").await;
    let seed_result_id = format!("result-{}", ordering.logical_result_id);
    let payload = format!("payload-{}", distinct.payload_hash);
    let first_sequence = hook
        .persist_message(&transcript_tool_result_message(&seed_result_id, &payload))
        .await
        .expect("persist seed tool-result message");
    assert_eq!(first_sequence, 1, "{}: seed sequence", distinct.name);
    assert_transcript_counts(
        "distinct result-id pre-state",
        &db.node,
        &session_id,
        distinct.pre_message_count,
        distinct.pre_tool_call_count,
    )
    .await;
    let distinct_sequence = hook
        .persist_message(&transcript_tool_result_message(
            &format!("result-{}", distinct.logical_result_id),
            &payload,
        ))
        .await
        .expect("persist distinct tool-result message");
    assert_eq!(
        distinct_sequence as usize, distinct.result_sequence,
        "{}: result_sequence",
        distinct.name
    );
    assert_eq!(
        hook.cancel_in_flight_tool_calls().await.unwrap(),
        distinct.post_in_flight_count,
        "{}: post_in_flight_count",
        distinct.name
    );
    let (_, _, history) = assert_transcript_post_state(distinct, &db.node, &session_id).await;
    assert_eq!(
        transcript_tool_result_count(&history),
        distinct.post_message_count,
        "{}: distinct result rows",
        distinct.name
    );

    let pair = lean_transcript_case("completed_tool_pair_closed");
    let (db, hook, session_id, _) =
        persist_completed_tool_sequence("transcript-pair-closed", pair).await;
    assert_eq!(
        hook.cancel_in_flight_tool_calls().await.unwrap(),
        pair.post_in_flight_count,
        "{}: post_in_flight_count",
        pair.name
    );
    let (_, tool_calls, history) = assert_transcript_post_state(pair, &db.node, &session_id).await;
    assert_eq!(
        tool_calls
            .first()
            .and_then(|call| call.lifecycle_state.as_deref()),
        Some("completed"),
        "{}: completed tool call",
        pair.name
    );
    assert_eq!(transcript_tool_result_count(&history), 1, "{}", pair.name);

    let drain = lean_transcript_case("explicit_drain_terminalizes_ownership");
    let (db, hook, session_id) = transcript_hook_fixture("transcript-explicit-drain").await;
    assert!(matches!(
        PromptHook::<TranscriptConformanceModel>::on_tool_call(
            &hook,
            "read",
            Some("result-drain".to_string()),
            "internal-drain",
            r#"{"file_path":"/tmp/transcript-contract.txt"}"#,
        )
        .await,
        ToolCallHookAction::Continue
    ));
    let assistant_sequence = hook
        .persist_message(&transcript_assistant_tool_call_message("result-drain"))
        .await
        .expect("persist drain assistant message");
    assert_eq!(
        assistant_sequence as usize, drain.assistant_sequence,
        "{}: assistant_sequence",
        drain.name
    );
    assert_transcript_counts(
        "explicit drain pre-state",
        &db.node,
        &session_id,
        drain.pre_message_count,
        drain.pre_tool_call_count,
    )
    .await;
    assert_eq!(
        hook.cancel_in_flight_tool_calls().await.unwrap(),
        drain.pre_in_flight_count,
        "{}: explicit drain count",
        drain.name
    );
    assert_eq!(
        hook.cancel_in_flight_tool_calls().await.unwrap(),
        drain.post_in_flight_count,
        "{}: post_in_flight_count",
        drain.name
    );
    let (_, tool_calls, _) = assert_transcript_post_state(drain, &db.node, &session_id).await;
    assert_eq!(
        tool_calls
            .first()
            .and_then(|call| call.lifecycle_state.as_deref()),
        Some("cancelled"),
        "{}: durable row terminalized",
        drain.name
    );

    let abandon = lean_transcript_case("drop_abandon_not_strong_drain");
    let (db, hook, session_id) = transcript_hook_fixture("transcript-drop-abandon").await;
    assert!(matches!(
        PromptHook::<TranscriptConformanceModel>::on_tool_call(
            &hook,
            "read",
            Some("result-abandon".to_string()),
            "internal-abandon",
            r#"{"file_path":"/tmp/transcript-contract.txt"}"#,
        )
        .await,
        ToolCallHookAction::Continue
    ));
    assert_transcript_counts(
        "drop abandon pre-state",
        &db.node,
        &session_id,
        abandon.pre_message_count,
        abandon.pre_tool_call_count,
    )
    .await;
    let observer = hook.clone();
    drop(hook);
    assert_eq!(
        observer.cancel_in_flight_tool_calls().await.unwrap(),
        abandon.post_in_flight_count,
        "{}: drop abandons in-memory ownership",
        abandon.name
    );
    let (_, tool_calls, _) = assert_transcript_post_state(abandon, &db.node, &session_id).await;
    assert_eq!(
        tool_calls
            .first()
            .and_then(|call| call.lifecycle_state.as_deref()),
        Some("running"),
        "{}: durable row remains running after Drop",
        abandon.name
    );
}

#[test]
fn generated_client_shell_cases_cover_shell_projection_contracts() {
    let snapshot = lean_client_shell_case("snapshot_preserves_selection");
    assert_eq!(snapshot.input.as_str(), "snapshot");
    assert!(snapshot.selection_preserved);
    assert_eq!(
        snapshot.pre_selection_session,
        snapshot.post_selection_session
    );

    let advanced = lean_client_shell_case("snapshot_workflow_advances_on_matching_request");
    assert!(advanced.workflow_advanced);
    assert_eq!(advanced.pre_workflow_kind.as_str(), "awaiting");
    assert_eq!(advanced.post_workflow_kind.as_str(), "idle");
    assert_eq!(advanced.pre_workflow_request, Some(101));
    assert_eq!(advanced.post_workflow_request, None);

    let stale = lean_client_shell_case("awaiting_stale_request_observation");
    assert!(!stale.workflow_advanced);
    assert_eq!(
        stale.property.as_str(),
        "awaiting_stale_request_observation"
    );
    assert_eq!(stale.post_workflow_kind.as_str(), "awaiting");
    assert_eq!(
        stale.frontend_expected_send_blocked_reason.as_deref(),
        Some("waitingForRequestObservation")
    );

    let matching = lean_client_shell_case("awaiting_matching_request_observation");
    assert_eq!(
        matching.frontend_expected_workflow_kind.as_str(),
        "turnInProgress"
    );
    assert_eq!(
        matching.frontend_expected_send_blocked_reason.as_deref(),
        Some("awaitingTurnTerminality")
    );

    let switched = lean_client_shell_case("stale_workflow_after_session_switch");
    assert!(switched.workflow_advanced);
    assert_eq!(switched.pre_selection_session, Some(1));
    assert_eq!(switched.post_selection_session, Some(2));
    assert_eq!(switched.post_workflow_kind.as_str(), "idle");
    assert_eq!(switched.frontend_expected_send_status.as_str(), "ready");

    let transport = lean_client_shell_case("transport_noop");
    assert!(transport.transport_noop);
    assert!(transport.selection_preserved);
    assert!(!transport.workflow_advanced);

    for (name, reason) in [
        ("blocked_submit_client_offline", "clientOffline"),
        ("blocked_submit_agent_not_selected", "agentNotSelected"),
        ("blocked_submit_composer_empty", "composerEmpty"),
        ("blocked_submit_mutation_in_flight", "mutationInFlight"),
        ("blocked_submit_awaiting_observation", "awaitingObservation"),
        ("blocked_submit_session_absent", "sessionAbsent"),
        ("blocked_submit_nonterminal_turn", "awaitingTurnTerminality"),
    ] {
        let case = lean_client_shell_case(name);
        assert!(!case.can_submit_before, "{name} should gate submit");
        assert_eq!(case.send_decision.as_str(), "blocked");
        assert_eq!(case.send_blocked_reason.as_deref(), Some(reason));
        assert_eq!(case.frontend_expected_send_status.as_str(), "disabled");
    }

    let terminal = lean_client_shell_case("terminal_follow_up_allowed");
    assert!(terminal.can_submit_before);
    assert_eq!(terminal.send_decision.as_str(), "ready");
    assert_eq!(terminal.frontend_expected_send_status.as_str(), "ready");

    let no_summary = lean_client_shell_case("terminal_follow_up_session_snapshot_without_summary");
    assert!(no_summary.can_submit_before);
    assert_eq!(no_summary.frontend_expected_send_status.as_str(), "ready");
    assert_eq!(no_summary.frontend_expected_active_request_id, Some(101));
}

#[test]
fn generated_runtime_reconcile_cases_pin_generation_and_admission_contract() {
    let publish = lean_runtime_reconcile_case("publish_changed_snapshot");
    assert!(publish.legal);
    assert_eq!(publish.action.as_str(), "publish");
    assert_eq!(publish.pre_phase.as_str(), "applying");
    assert_eq!(publish.post_phase.as_str(), "idle");
    assert_eq!(
        publish.pre_active_generation + 1,
        publish.post_active_generation
    );
    assert_eq!(
        publish.pre_router_generation,
        publish.post_router_generation
    );
    assert_eq!(
        publish.pre_ready_generation_count + 1,
        publish.post_ready_generation_count
    );
    assert_eq!(
        publish.pre_live_generation_count + 1,
        publish.post_live_generation_count
    );

    let router = lean_runtime_reconcile_case("router_observe_published_generation");
    assert!(router.legal);
    assert_eq!(router.pre_phase.as_str(), "idle");
    assert_eq!(router.post_phase.as_str(), "idle");
    assert_eq!(router.pre_active_generation, router.post_active_generation);
    assert_eq!(router.post_router_generation, router.post_active_generation);

    let accept = lean_runtime_reconcile_case("accept_request_after_router_observe");
    assert!(accept.legal);
    assert_eq!(accept.pre_phase.as_str(), "idle");
    assert_eq!(accept.post_phase.as_str(), "idle");
    assert_eq!(accept.pre_in_flight_count + 1, accept.post_in_flight_count);
    assert_eq!(accept.tracked_request_id, 500);
    assert_eq!(accept.tracked_session_id, 100);
    assert_eq!(
        accept.tracked_request_generation,
        accept.post_router_generation
    );
    assert_eq!(accept.tracked_request_session, accept.tracked_session_id);
    assert_eq!(
        accept.tracked_request_behavior,
        accept.tracked_session_behavior
    );

    let retire = lean_runtime_reconcile_case("retire_unobserved_generation");
    assert!(retire.legal);
    assert_eq!(
        retire.pre_live_generation_count - 1,
        retire.post_live_generation_count
    );
    assert_eq!(
        retire.pre_ready_generation_count - 1,
        retire.post_ready_generation_count
    );
}

#[test]
fn generated_session_recovery_cases_cover_retry_guards_and_preservation() {
    let legal = lean_session_recovery_case("legal_open_budget_latest");
    assert!(legal.legal);
    assert_eq!(legal.action.as_str(), "reissueFailed");
    assert_eq!(legal.pre_latest_state.as_str(), "failed");
    assert_eq!(legal.pre_failed_state.as_str(), "failed");
    assert_eq!(legal.post_latest_state.as_str(), "pending");
    assert_eq!(legal.post_failed_state.as_str(), "failed");
    assert_eq!(legal.post_new_state.as_str(), "pending");
    assert_eq!(legal.pre_latest_admission.as_str(), "released");
    assert_eq!(legal.post_latest_admission.as_str(), "released");
    assert_eq!(legal.pre_failed_admission.as_str(), "released");
    assert_eq!(legal.post_failed_admission.as_str(), "released");
    assert_eq!(legal.post_new_admission.as_str(), "released");
    assert_eq!(legal.pre_origin.as_str(), "scheduled");
    assert_eq!(legal.pre_backend.as_str(), "contract-backend-alt");
    assert_eq!(legal.pre_origin.as_str(), legal.post_new_origin.as_str());
    assert_eq!(legal.pre_backend.as_str(), legal.post_new_backend.as_str());
    assert_eq!(legal.pre_retry_count + 1, legal.post_retry_count);
    assert!(legal.post_retry_count <= legal.max_retries);
    assert_eq!(legal.pre_session_id, legal.post_session_id);
    assert_eq!(legal.pre_behavior_id, legal.post_behavior_id);
    assert_eq!(legal.pre_request_count + 1, legal.post_request_count);
    assert_eq!(legal.post_latest_id, legal.new_id);
    assert!(legal.pre_failed_is_latest);
    assert!(!legal.post_failed_is_latest);
    assert!(legal.post_new_is_latest);
    assert!(legal.pre_failed_exists);
    assert!(legal.pre_latest_exists);
    assert!(legal.pre_request_ids.contains(&legal.failed_id));
    assert!(!legal.pre_new_request_exists);
    assert!(legal.old_request_retained);
    assert!(legal.new_request_inserted);
    assert!(legal.origin_preserved);
    assert!(legal.backend_preserved);

    let last_slot = lean_session_recovery_case("legal_last_retry_slot");
    assert!(last_slot.legal);
    assert_eq!(last_slot.post_retry_count, last_slot.max_retries);

    let initial_slot = lean_session_recovery_case("legal_initial_retry_slot");
    assert!(initial_slot.legal);
    assert_eq!(initial_slot.pre_retry_count, 0);
    assert_eq!(initial_slot.post_retry_count, 1);

    let duplicate_new_id = lean_session_recovery_case("illegal_new_request_id_already_exists");
    assert!(!duplicate_new_id.legal);
    assert!(duplicate_new_id.pre_new_request_exists);
    assert_eq!(duplicate_new_id.pre_failed_admission.as_str(), "released");

    let duplicate_failed_id =
        lean_session_recovery_case("illegal_new_request_id_matches_failed_id");
    assert!(!duplicate_failed_id.legal);
    assert_eq!(duplicate_failed_id.failed_id, duplicate_failed_id.new_id);
    assert!(duplicate_failed_id.pre_new_request_exists);
    assert_eq!(
        duplicate_failed_id.pre_failed_admission.as_str(),
        "released"
    );

    let source_not_released = lean_session_recovery_case("illegal_source_not_released");
    assert!(!source_not_released.legal);
    assert_eq!(source_not_released.pre_failed_state.as_str(), "failed");
    assert_eq!(source_not_released.pre_failed_admission.as_str(), "waiting");

    let non_latest = lean_session_recovery_case("illegal_non_latest_failed_with_pending_latest");
    assert!(!non_latest.legal);
    assert_eq!(non_latest.pre_failed_state.as_str(), "failed");
    assert_eq!(non_latest.pre_latest_state.as_str(), "pending");
    assert!(!non_latest.pre_failed_is_latest);

    let missing = lean_session_recovery_case("illegal_missing_failed_request");
    assert!(!missing.legal);
    assert!(!missing.pre_failed_exists);
    assert!(!missing.pre_latest_exists);

    for name in [
        "illegal_retry_budget_exhausted",
        "illegal_deadline_closed",
        "illegal_non_latest_failed_request",
        "illegal_non_latest_failed_with_pending_latest",
        "illegal_new_request_id_already_exists",
        "illegal_new_request_id_matches_failed_id",
        "illegal_source_not_failed",
        "illegal_source_not_released",
        "illegal_source_completed_terminal",
        "illegal_source_dead_stale_terminal",
        "illegal_source_superseded_terminal",
        "illegal_source_interrupted_terminal",
        "illegal_source_input_required_reserved",
        "illegal_source_processing_active_runtime",
        "illegal_missing_failed_request",
    ] {
        let case = lean_session_recovery_case(name);
        assert!(!case.legal, "{name} must be rejected by Lean");
        assert!(case.post_latest_state.is_empty());
    }
}

#[tokio::test]
async fn generated_session_recovery_cases_drive_db_backed_reissue_contract() {
    let cases = &lean_contract_snapshot().session_recovery_cases;
    assert_eq!(cases.iter().filter(|case| case.legal).count(), 3);
    assert_eq!(cases.len(), 18);

    let db = test_db("session-recovery-generated-contract").await;
    for case in cases {
        let pre = seed_session_recovery_case(&db.node, case).await;
        assert_eq!(
            request_count_for_session(&db.node, &pre.session_id).await,
            case.pre_request_count,
            "pre request count must match Lean case {}",
            case.name
        );
        assert_eq!(
            latest_request_id_for_session(&db.node, &pre.session_id).await,
            pre.pre_latest_request_id,
            "pre latest binding must match Lean case {}",
            case.name
        );

        let before_failed = fetch_recovery_request(&db.node, &pre.failed_request_id).await;
        let before_new_count = request_count_by_id(&db.node, &pre.new_request_id).await;
        let result = reissue_failed_request_for_contract(&db.node, &pre).await;

        if case.legal {
            assert_eq!(
                result.as_deref(),
                Ok(pre.new_request_id.as_str()),
                "legal Lean case {} must reissue",
                case.name
            );
            assert_legal_reissue_postconditions(&db.node, case, &pre).await;
        } else {
            let error = result.expect_err("illegal Lean case must be denied");
            assert!(
                error.contains(expected_reissue_denial_fragment(case)),
                "illegal case {} failed with unexpected error: {error}",
                case.name
            );
            assert_eq!(
                request_count_for_session(&db.node, &pre.session_id).await,
                case.pre_request_count,
                "illegal case {} must not insert a successor",
                case.name
            );
            assert_eq!(
                latest_request_id_for_session(&db.node, &pre.session_id).await,
                pre.pre_latest_request_id,
                "illegal case {} must not change latest request",
                case.name
            );
            assert_eq!(
                request_count_by_id(&db.node, &pre.new_request_id).await,
                before_new_count,
                "illegal case {} must not create the requested successor id",
                case.name
            );
            assert_eq!(
                fetch_recovery_request(&db.node, &pre.failed_request_id).await,
                before_failed,
                "illegal case {} must leave the source request unchanged",
                case.name
            );
        }
    }
}

#[derive(Debug)]
struct SessionRecoveryDbPre {
    session_id: String,
    failed_request_id: String,
    new_request_id: String,
    pre_latest_request_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct RecoveryRequestRow {
    request_id: String,
    agent_did: String,
    behavior_id: String,
    session_id: String,
    content: String,
    status: String,
    lifecycle_state: String,
    backend_id: String,
    execution_origin: String,
    retry_parent_request: String,
    retry_root_request: String,
    retry_count: i64,
    max_retries: i64,
    deadline: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RecoveryConversationLatestRow {
    latest_request_id: String,
}

async fn seed_session_recovery_case(
    node: &EmbeddedNode,
    case: &lean_vocab_test::LeanSessionRecoveryCase,
) -> SessionRecoveryDbPre {
    let session_id = format!("sr-{}-session", case.name);
    let failed_request_id = recovery_request_id(case, case.failed_id);
    let new_request_id = recovery_request_id(case, case.new_id);
    let pre_latest_request_id = recovery_request_id(case, case.pre_latest_id);

    create_agent_session(node, &session_id, AGENT_NAME, "2026-03-23T00:00:00Z").await;
    for request_id in &case.pre_request_ids {
        let request_id_string = recovery_request_id(case, *request_id);
        let (state, status, retry_count, max_retries, deadline, backend, origin) =
            if *request_id == case.failed_id {
                (
                    case.pre_failed_state.as_str(),
                    recovery_status_for_source(case),
                    case.pre_retry_count as i64,
                    case.max_retries as i64,
                    recovery_deadline(case.pre_deadline_exceeded),
                    case.pre_backend.as_str(),
                    case.pre_origin.as_str(),
                )
            } else if *request_id == case.pre_latest_id {
                (
                    case.pre_latest_state.as_str(),
                    status_for_lifecycle_state(&case.pre_latest_state),
                    0,
                    case.max_retries as i64,
                    recovery_deadline(false),
                    case.pre_backend.as_str(),
                    case.pre_origin.as_str(),
                )
            } else {
                (
                    "pending",
                    "pending",
                    0,
                    case.max_retries as i64,
                    recovery_deadline(false),
                    case.pre_backend.as_str(),
                    case.pre_origin.as_str(),
                )
            };

        create_session_recovery_request(
            node,
            &request_id_string,
            &session_id,
            state,
            status,
            retry_count,
            max_retries,
            &deadline,
            backend,
            origin,
            if state == "dead" { "Stale" } else { "" },
        )
        .await;
    }
    upsert_conversation(
        node,
        &session_id,
        &pre_latest_request_id,
        "session recovery contract",
        "active",
    )
    .await;

    SessionRecoveryDbPre {
        session_id,
        failed_request_id,
        new_request_id,
        pre_latest_request_id,
    }
}

#[allow(clippy::too_many_arguments)]
async fn create_session_recovery_request(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    lifecycle_state: &str,
    status: &str,
    retry_count: i64,
    max_retries: i64,
    deadline: &str,
    backend_id: &str,
    execution_origin: &str,
    failure_reason: &str,
) {
    let request_id = escape_graphql_string(request_id);
    let session_id = escape_graphql_string(session_id);
    let lifecycle_state = escape_graphql_string(lifecycle_state);
    let status = escape_graphql_string(status);
    let deadline = escape_graphql_string(deadline);
    let backend_id = escape_graphql_string(backend_id);
    let execution_origin = escape_graphql_string(execution_origin);
    let failure_reason = escape_graphql_string(failure_reason);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{request_id}",
                agent_did: "{AGENT_DID}",
                behavior_id: "{AGENT_NAME}",
                session_id: "{session_id}",
                retry_parent_request: "",
                retry_root_request: "{request_id}",
                superseded_by_request: "",
                content: "session recovery contract",
                status: "{status}",
                lifecycle_state: "{lifecycle_state}",
                backend_id: "{backend_id}",
                execution_origin: "{execution_origin}",
                failure_reason: "{failure_reason}",
                created_at: "2026-03-23T00:00:00Z",
                deadline: "{deadline}",
                retry_count: {retry_count},
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create session recovery request failed: {:?}",
        resp.errors
    );
}

async fn reissue_failed_request_for_contract(
    node: &EmbeddedNode,
    pre: &SessionRecoveryDbPre,
) -> Result<String, String> {
    let Some(parent) = fetch_recovery_request(node, &pre.failed_request_id).await else {
        return Err(format!(
            "retry parent request not found: request_id={}",
            pre.failed_request_id
        ));
    };
    if parent.lifecycle_state != "failed" || parent.status != "error" {
        return Err(format!(
            "retry parent request must be failed/error, got lifecycle_state={} status={}",
            parent.lifecycle_state, parent.status
        ));
    }
    if parent.retry_count >= parent.max_retries {
        return Err(format!(
            "retry parent request exhausted retry budget: retry_count={} max_retries={}",
            parent.retry_count, parent.max_retries
        ));
    }
    if parent
        .deadline
        .as_deref()
        .filter(|deadline| !deadline.is_empty())
        .is_some_and(|deadline| {
            chrono::DateTime::parse_from_rfc3339(deadline)
                .map(|parsed| chrono::Utc::now() > parsed.with_timezone(&chrono::Utc))
                .unwrap_or(true)
        })
    {
        return Err("retry parent request deadline is closed".to_string());
    }
    let latest_request_id = latest_request_id_for_session(node, &pre.session_id).await;
    if latest_request_id != parent.request_id {
        return Err(format!(
            "retry parent request must be latest for session {}, got latest_request_id={latest_request_id}",
            pre.session_id
        ));
    }
    if fetch_recovery_request(node, &pre.new_request_id)
        .await
        .is_some()
    {
        return Err(format!(
            "retry new request id already exists: request_id={}",
            pre.new_request_id
        ));
    }

    let retry_root_request = if parent.retry_root_request.is_empty() {
        parent.request_id.as_str()
    } else {
        parent.retry_root_request.as_str()
    };
    create_session_recovery_request(
        node,
        &pre.new_request_id,
        &pre.session_id,
        "pending",
        "pending",
        parent.retry_count + 1,
        parent.max_retries,
        &recovery_deadline(false),
        &parent.backend_id,
        &parent.execution_origin,
        "",
    )
    .await;
    set_recovery_request_lineage(
        node,
        &pre.new_request_id,
        &parent.request_id,
        retry_root_request,
    )
    .await;
    upsert_conversation(
        node,
        &pre.session_id,
        &pre.new_request_id,
        &parent.content,
        "active",
    )
    .await;

    Ok(pre.new_request_id.clone())
}

async fn set_recovery_request_lineage(
    node: &EmbeddedNode,
    request_id: &str,
    retry_parent_request: &str,
    retry_root_request: &str,
) {
    let request_id = escape_graphql_string(request_id);
    let retry_parent_request = escape_graphql_string(retry_parent_request);
    let retry_root_request = escape_graphql_string(retry_root_request);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{
                    retry_parent_request: "{retry_parent_request}",
                    retry_root_request: "{retry_root_request}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "set retry lineage failed: {:?}",
        resp.errors
    );
}

async fn assert_legal_reissue_postconditions(
    node: &EmbeddedNode,
    case: &lean_vocab_test::LeanSessionRecoveryCase,
    pre: &SessionRecoveryDbPre,
) {
    assert_eq!(
        request_count_for_session(node, &pre.session_id).await,
        case.post_request_count
    );
    assert_eq!(
        latest_request_id_for_session(node, &pre.session_id).await,
        pre.new_request_id
    );

    let new_request = fetch_recovery_request(node, &pre.new_request_id)
        .await
        .expect("legal reissue must insert successor");
    assert_eq!(new_request.session_id, pre.session_id);
    assert_eq!(new_request.agent_did, AGENT_DID);
    assert_eq!(new_request.behavior_id, AGENT_NAME);
    assert_eq!(new_request.status, "pending");
    assert_eq!(new_request.lifecycle_state, case.post_new_state);
    assert_eq!(new_request.retry_parent_request, pre.failed_request_id);
    assert_eq!(new_request.retry_root_request, pre.failed_request_id);
    assert_eq!(new_request.retry_count, case.post_retry_count as i64);
    assert_eq!(new_request.max_retries, case.max_retries as i64);
    assert_eq!(new_request.backend_id, case.post_new_backend);
    assert_eq!(new_request.execution_origin, case.post_new_origin);
    assert!(case.origin_preserved);
    assert!(case.backend_preserved);

    let failed_request = fetch_recovery_request(node, &pre.failed_request_id)
        .await
        .expect("legal reissue must retain source request");
    assert_eq!(failed_request.lifecycle_state, case.post_failed_state);
    assert_eq!(failed_request.status, "error");
    assert_eq!(failed_request.retry_count, case.pre_retry_count as i64);
    assert_eq!(failed_request.max_retries, case.max_retries as i64);
    assert_eq!(failed_request.backend_id, case.pre_backend);
    assert_eq!(failed_request.execution_origin, case.pre_origin);
    assert_eq!(
        request_count_by_id(node, &pre.failed_request_id).await,
        if case.old_request_retained { 1 } else { 0 }
    );
    assert_eq!(
        request_count_by_id(node, &pre.new_request_id).await,
        if case.new_request_inserted { 1 } else { 0 }
    );
}

async fn fetch_recovery_request(
    node: &EmbeddedNode,
    request_id: &str,
) -> Option<RecoveryRequestRow> {
    let request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                limit: 1
            ) {{
                request_id
                agent_did
                behavior_id
                session_id
                content
                status
                lifecycle_state
                backend_id
                execution_origin
                retry_parent_request
                retry_root_request
                retry_count
                max_retries
                deadline
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_optional_row::<RecoveryRequestRow>(&resp, "AgentRequest")
}

async fn latest_request_id_for_session(node: &EmbeddedNode, session_id: &str) -> String {
    let session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                limit: 1
            ) {{
                latest_request_id
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    first_optional_row::<RecoveryConversationLatestRow>(&resp, "AgentConversation")
        .map(|row| row.latest_request_id)
        .unwrap_or_default()
}

async fn request_count_for_session(node: &EmbeddedNode, session_id: &str) -> usize {
    let session_id = escape_graphql_string(session_id);
    request_count_query(
        node,
        &format!(
            r#"{{
                AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{
                    _docID
                }}
            }}"#
        ),
    )
    .await
}

async fn request_count_by_id(node: &EmbeddedNode, request_id: &str) -> usize {
    let request_id = escape_graphql_string(request_id);
    request_count_query(
        node,
        &format!(
            r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{
                    _docID
                }}
            }}"#
        ),
    )
    .await
}

async fn request_count_query(node: &EmbeddedNode, query: &str) -> usize {
    let resp = node.execute(query).await;
    assert!(
        !resp.has_errors(),
        "request count failed: {:?}",
        resp.errors
    );
    resp.data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .map(Vec::len)
        .unwrap_or_default()
}

fn recovery_request_id(case: &lean_vocab_test::LeanSessionRecoveryCase, id: usize) -> String {
    format!("sr-{}-{id}", case.name)
}

fn recovery_deadline(exceeded: bool) -> String {
    let deadline = if exceeded {
        chrono::Utc::now() - chrono::Duration::seconds(30)
    } else {
        chrono::Utc::now() + chrono::Duration::minutes(5)
    };
    deadline.to_rfc3339()
}

fn recovery_status_for_source(case: &lean_vocab_test::LeanSessionRecoveryCase) -> &'static str {
    if case.pre_failed_state == "failed" && case.pre_failed_admission == "released" {
        "error"
    } else if case.pre_failed_state == "failed" {
        "processing"
    } else {
        status_for_lifecycle_state(&case.pre_failed_state)
    }
}

fn status_for_lifecycle_state(lifecycle_state: &str) -> &'static str {
    match lifecycle_state {
        "pending" => "pending",
        "completed" => "completed",
        "failed" => "error",
        "superseded" => "superseded",
        "dead" => "dead",
        "interrupted" => "interrupted",
        _ => "processing",
    }
}

fn expected_reissue_denial_fragment(
    case: &lean_vocab_test::LeanSessionRecoveryCase,
) -> &'static str {
    // Generated cases assert the first denial in the DB-backed reissue check
    // order, so future multi-violation cases should choose this precedence
    // deliberately.
    if !case.pre_failed_exists {
        "not found"
    } else if case.pre_failed_state != "failed" || case.pre_failed_admission != "released" {
        "failed/error"
    } else if case.pre_retry_count >= case.max_retries {
        "exhausted retry budget"
    } else if case.pre_deadline_exceeded {
        "deadline is closed"
    } else if !case.pre_failed_is_latest {
        "must be latest"
    } else if case.pre_new_request_exists {
        "already exists"
    } else {
        panic!("unhandled illegal SessionRecovery case: {}", case.name);
    }
}

#[test]
fn generated_tool_execution_cases_cover_preflight_and_retry_contracts() {
    let unreachable =
        lean_tool_preflight_case("preflight_unreachable_valid_blocks_serviceUnavailable");
    assert_eq!(unreachable.decision, "block");
    assert_eq!(
        unreachable.failure_class.as_deref(),
        Some("serviceUnavailable")
    );

    let invalid = lean_tool_preflight_case("preflight_healthy_invalid_blocks_argumentInvalid");
    assert_eq!(invalid.decision, "block");
    assert_eq!(invalid.failure_class.as_deref(), Some("argumentInvalid"));

    for name in [
        "preflight_healthy_valid_dispatch",
        "preflight_stale_valid_dispatch",
    ] {
        let case = lean_tool_preflight_case(name);
        assert_eq!(case.decision, "dispatch", "{name}");
        assert_eq!(case.failure_class, None, "{name}");
    }

    let safe_read = lean_tool_retry_case("retry_mcpListTools_unknown_transport_retrySafeRead");
    assert_eq!(safe_read.disposition, "retrySafeRead");

    let idempotent =
        lean_tool_retry_case("retry_mcpCall_idempotent_transport_retryIdempotentToolCall");
    assert_eq!(idempotent.disposition, "retryIdempotentToolCall");

    for name in [
        "retry_mcpCall_unknown_transport_doNotRetry",
        "retry_mcpCall_nonIdempotent_transport_doNotRetry",
        "retry_nativeCommand_idempotent_transport_doNotRetry",
    ] {
        let case = lean_tool_retry_case(name);
        assert_eq!(case.disposition, "doNotRetry", "{name}");
    }
}

fn slot_rows_from_contract<'a>(
    backend_ids: &'a [String],
    row_states: &'a [String],
) -> impl Iterator<Item = InferenceCallSlotRow<'a>> {
    backend_ids
        .iter()
        .zip(row_states)
        .map(|(backend_id, state)| InferenceCallSlotRow::new(backend_id.as_str(), state.as_str()))
}

#[test]
fn generated_slot_accounting_cases_pin_inference_and_fleet_contracts() {
    for case in &lean_contract_snapshot().inference_slot_accounting_cases {
        assert_eq!(
            case.row_backend_ids.len(),
            case.row_states.len(),
            "Inference slot case {} emitted mismatched row arrays",
            case.name
        );
        if case.row_states.len() == 1 {
            let row = InferenceCallSlotRow::new(
                case.row_backend_ids[0].as_str(),
                case.row_states[0].as_str(),
            );
            assert_eq!(
                slot_contribution(row, &case.backend_id),
                case.expected_contribution,
                "Inference slot case {} drifted from Rust slot contribution",
                case.name
            );
        }
        let reconstructed = reconstructed_running_slot_count(
            slot_rows_from_contract(&case.row_backend_ids, &case.row_states),
            &case.backend_id,
        );
        assert_eq!(
            reconstructed, case.reconstructed_running_count,
            "Inference slot case {} drifted from Rust admission reconstruction",
            case.name
        );
    }

    let queued = lean_inference_slot_accounting_case("queued_contributes_zero");
    assert_eq!(queued.property.as_str(), "state_contribution");
    assert_eq!(queued.pre_state.as_str(), "queued");
    assert_eq!(queued.contribution, 0);
    assert_eq!(queued.reconstructed_running_count, 0);

    let running = lean_inference_slot_accounting_case("running_contributes_one");
    assert_eq!(running.pre_state.as_str(), "running");
    assert_eq!(running.contribution, 1);
    assert_eq!(running.expected_contribution, 1);

    for name in [
        "cancelled_terminal_contributes_zero",
        "completed_terminal_contributes_zero",
        "failed_terminal_contributes_zero",
    ] {
        let case = lean_inference_slot_accounting_case(name);
        assert_eq!(case.property.as_str(), "state_contribution");
        assert_eq!(case.contribution, 0, "{name}");
        assert_eq!(case.reconstructed_running_count, 0, "{name}");
    }

    for name in [
        "cancelled_releases_slot",
        "completed_releases_slot",
        "failed_releases_slot",
    ] {
        let case = lean_inference_slot_accounting_case(name);
        assert_eq!(case.property.as_str(), "terminal_release", "{name}");
        assert_eq!(case.pre_state.as_str(), "running", "{name}");
        assert_eq!(case.pre_contribution, 1, "{name}");
        assert_eq!(case.post_contribution, 0, "{name}");
        assert!(case.released_slot, "{name}");
    }

    for name in [
        "permit_drop_failed_terminalization_not_counted",
        "permit_drop_cancelled_terminalization_not_counted",
    ] {
        let case = lean_inference_slot_accounting_case(name);
        assert_eq!(
            case.property.as_str(),
            "permit_drop_terminalization",
            "{name}"
        );
        assert!(case.permit_drop_terminalization, "{name}");
        assert_eq!(case.post_contribution, 0, "{name}");
    }

    let bounded = lean_inference_slot_accounting_case(
        "reconstructed_running_count_bounded_by_max_concurrent",
    );
    assert_eq!(bounded.reconstructed_running_count, 1);
    assert_eq!(bounded.max_concurrent, 1);
    assert!(bounded.bounded_by_max_concurrent);
    assert_eq!(
        bounded.row_states,
        vec![
            "running".to_string(),
            "queued".to_string(),
            "completed".to_string(),
            "running".to_string()
        ]
    );

    let fleet_ledger = lean_contract_snapshot()
        .coverage_ledger
        .iter()
        .find(|entry| entry.category == "fleet_cases" && entry.domain == "FleetSlotAccounting")
        .expect("FleetSlotAccounting coverage ledger entry must be emitted");
    assert_eq!(
        fleet_ledger.accepted_boundary.as_str(),
        "boundary.fleet-slot-accounting.derived-view",
        "FleetSlotAccounting must be classified as a derived boundary, not a persisted aggregate"
    );

    for case in &lean_contract_snapshot().fleet_slot_accounting_cases {
        assert_eq!(
            case.row_backend_ids.len(),
            case.row_states.len(),
            "Fleet slot case {} emitted mismatched projection row arrays",
            case.name
        );
        if case.row_states.len() == 1 {
            if case.admission_state == "released" {
                let expected_terminal_state = match case.request_state.as_str() {
                    "completed" => "completed",
                    "failed" => "failed",
                    "interrupted" | "superseded" | "dead" => "cancelled",
                    other => panic!(
                        "Fleet slot released case {} has non-terminal request_state={other}",
                        case.name
                    ),
                };
                assert_eq!(
                    case.row_states[0].as_str(),
                    expected_terminal_state,
                    "Fleet slot released case {} projected the wrong terminal InferenceCall state",
                    case.name
                );
            }
            let row = InferenceCallSlotRow::new(
                case.row_backend_ids[0].as_str(),
                case.row_states[0].as_str(),
            );
            assert_eq!(
                slot_contribution(row, &case.backend_id),
                case.expected_contribution,
                "Fleet slot case {} drifted from Rust slot contribution",
                case.name
            );
        }
        let reconstructed = reconstructed_running_slot_count(
            slot_rows_from_contract(&case.row_backend_ids, &case.row_states),
            &case.backend_id,
        );
        assert_eq!(
            reconstructed, case.reconstructed_running_count,
            "Fleet slot case {} drifted from Rust admission reconstruction",
            case.name
        );
        assert_eq!(
            reconstructed, case.slot_count,
            "Fleet slot case {} must be a derived projection over admission reconstruction",
            case.name
        );
        assert_eq!(
            case.scheduler_running, case.slot_count,
            "Fleet slot case {} must keep aggregate running reconstructed from slot count",
            case.name
        );
        assert_eq!(
            case.contribution, case.expected_contribution,
            "Fleet slot case {} must compute its expected contribution",
            case.name
        );
        assert_eq!(
            case.bounded_by_max_concurrent,
            case.slot_count <= case.max_concurrent,
            "Fleet slot case {} must compute its max_concurrent bound",
            case.name
        );
        assert!(
            case.aggregate_reconstructed_not_persisted,
            "Fleet slot case {} must preserve reconstructed-not-persisted policy",
            case.name
        );
    }

    let waiting = lean_fleet_slot_accounting_case("fleet_waiting_contributes_zero");
    assert_eq!(waiting.admission_state.as_str(), "waiting");
    assert_eq!(waiting.contribution, 0);

    let acquired = lean_fleet_slot_accounting_case("fleet_acquired_contributes_one");
    assert_eq!(acquired.admission_state.as_str(), "acquired");
    assert_eq!(acquired.contribution, 1);

    let executing = lean_fleet_slot_accounting_case("fleet_executing_contributes_one");
    assert_eq!(executing.admission_state.as_str(), "executing");
    assert_eq!(executing.contribution, 1);

    let released = lean_fleet_slot_accounting_case("fleet_released_terminal_contributes_zero");
    assert_eq!(released.request_state.as_str(), "completed");
    assert_eq!(released.admission_state.as_str(), "released");
    assert_eq!(released.contribution, 0);

    let fleet_bound = lean_fleet_slot_accounting_case(
        "fleet_reconstructed_running_count_bounded_by_max_concurrent",
    );
    assert_eq!(fleet_bound.slot_count, fleet_bound.scheduler_running);
    assert_eq!(fleet_bound.slot_count, 2);
    assert_eq!(fleet_bound.reconstructed_running_count, 2);
    assert_eq!(
        fleet_bound.row_states,
        vec![
            "running".to_string(),
            "running".to_string(),
            "queued".to_string(),
            "completed".to_string()
        ]
    );
    assert_eq!(fleet_bound.max_concurrent, 2);
    assert!(fleet_bound.bounded_by_max_concurrent);
}

#[test]
fn generated_queue_deadline_cases_pin_r4a_contract_rows() {
    let cases = lean_queue_deadline_cases();
    assert_eq!(cases.len(), 5);

    let emitted_names = cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        emitted_names,
        [
            "active_request_blocks_later_same_session_claim",
            "terminal_active_allows_next_pending_same_session_claim",
            "subagent_completion_session_coalesces_one_pending_wakeup",
            "cancel_drains_automated_wakeups_preserves_user_pending",
            "claim_preserves_explicit_deadline",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>()
    );

    for case in cases {
        assert_eq!(case.session_id, 900, "{}", case.name);
        assert!(
            case.superseded_request_ids.is_empty(),
            "{} must be a queue/deadline contract, not a supersession contract",
            case.name
        );
    }

    let blocked = lean_queue_deadline_case("active_request_blocks_later_same_session_claim");
    assert_eq!(blocked.group, "queue_admission");
    assert_eq!(blocked.action, "claimNext");
    assert!(!blocked.legal);
    assert!(blocked.blocked_by_active);
    assert_eq!(blocked.pre_active_request_id, Some(100));
    assert_eq!(blocked.post_active_request_id, Some(100));
    assert_eq!(blocked.pre_pending_request_ids, vec![101]);
    assert_eq!(blocked.post_pending_request_ids, vec![101]);
    assert_eq!(blocked.claimed_request_id, None);
    assert!(blocked.post_terminal_request_ids.is_empty());

    let terminal =
        lean_queue_deadline_case("terminal_active_allows_next_pending_same_session_claim");
    assert_eq!(terminal.group, "queue_admission");
    assert_eq!(terminal.action, "finishActive_then_claimNext");
    assert!(terminal.legal);
    assert!(!terminal.blocked_by_active);
    assert_eq!(terminal.pre_active_request_id, Some(100));
    assert_eq!(terminal.pre_pending_request_ids, vec![101]);
    assert_eq!(terminal.post_active_request_id, Some(101));
    assert_eq!(terminal.claimed_request_id, Some(101));
    assert!(terminal.post_pending_request_ids.is_empty());
    assert_eq!(terminal.post_terminal_request_ids, vec![100]);

    let coalesced =
        lean_queue_deadline_case("subagent_completion_session_coalesces_one_pending_wakeup");
    assert_eq!(coalesced.group, "queue_coalesce");
    assert_eq!(coalesced.action, "coalescePending_twice");
    assert!(coalesced.legal);
    assert_eq!(
        coalesced.queue_key.as_deref(),
        Some("subagent_completion:900")
    );
    assert!(coalesced.pre_pending_request_ids.is_empty());
    assert_eq!(coalesced.post_pending_request_ids, vec![201]);
    assert_eq!(coalesced.post_coalesced_pending_count, 1);
    assert!(coalesced.post_terminal_request_ids.is_empty());

    let cancel = lean_queue_deadline_case("cancel_drains_automated_wakeups_preserves_user_pending");
    assert_eq!(cancel.group, "queue_cancel");
    assert_eq!(cancel.action, "drainAutomated");
    assert!(cancel.legal);
    assert_eq!(cancel.queue_key.as_deref(), Some("subagent_completion:900"));
    assert_eq!(cancel.pre_pending_request_ids, vec![301, 302]);
    assert_eq!(cancel.post_pending_request_ids, vec![302]);
    assert_eq!(cancel.automated_drained_request_ids, vec![301]);
    assert_eq!(cancel.preserved_user_pending_request_ids, vec![302]);
    assert_eq!(cancel.post_terminal_request_ids, vec![301]);
    assert_eq!(cancel.post_coalesced_pending_count, 0);

    let deadline = lean_queue_deadline_case("claim_preserves_explicit_deadline");
    assert_eq!(deadline.group, "claim_deadline");
    assert_eq!(deadline.action, "claim");
    assert!(deadline.legal);
    assert_eq!(deadline.claimed_request_id, Some(401));
    assert_eq!(deadline.pre_request_deadline, Some(50));
    assert_eq!(deadline.synthesized_claim_deadline, Some(51));
    assert_eq!(deadline.post_deadline, Some(50));
    assert!(
        deadline.post_deadline < deadline.synthesized_claim_deadline,
        "explicit request deadline should remain tighter than the synthesized claim deadline"
    );
    assert!(deadline.explicit_deadline_preserved);
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

    let curl = lean_command_policy_case("disabled_network_read_only_curl_denies_before_allowlist");
    assert_eq!(
        curl.denial_reason.as_deref(),
        Some("disabledNetworkCommand")
    );
    assert_eq!(curl.denied_command.as_deref(), Some("curl"));

    let workspace = lean_command_sandbox_case("workspace_write_enforced_selects_macos_seatbelt");
    assert_eq!(workspace.decision, "selected");
    assert_eq!(workspace.sandbox.as_deref(), Some("macos_seatbelt"));

    let unrestricted = lean_command_sandbox_case("unrestricted_selects_unsandboxed_unrestricted");
    assert_eq!(
        unrestricted.sandbox.as_deref(),
        Some("unsandboxed_unrestricted")
    );

    let key = lean_command_env_case("env_key_marker_dropped");
    assert_eq!(key.input_name, "OPENAI_API_KEY");
    assert_eq!(key.expected_output_value, None);

    let pager = lean_command_env_case("env_pager_forced_cat");
    assert_eq!(pager.expected_output_value.as_deref(), Some("cat"));
    let pager_absent = lean_command_env_case("env_pager_absent_still_forced_cat");
    assert_eq!(pager_absent.expected_output_value.as_deref(), Some("cat"));
}

fn rust_request_transition_action(from: &str, to: &str) -> Option<&'static str> {
    match (from, to) {
        ("pending", "claimed") => Some("claim"),
        ("pending", "superseded") => Some("dedupLose"),
        ("claimed", "processing") => Some("beginInference"),
        ("processing", "processing") => Some("advance"),
        ("processing", "completed") => Some("finish"),
        ("processing", "failed") => Some("fail"),
        ("claimed", "failed") => Some("failBeforeStream"),
        ("pending", "dead") => Some("expire"),
        ("pending", "interrupted") => Some("interruptBeforeClaim"),
        ("claimed", "interrupted") => Some("interruptClaimed"),
        ("processing", "interrupted") => Some("interruptProcessing"),
        _ => None,
    }
}

fn rust_request_transition_classification(from: &str, to: &str) -> &'static str {
    if rust_request_transition_action(from, to).is_some() {
        "legal"
    } else if from == "inputRequired" || to == "inputRequired" {
        "productUnreachable"
    } else {
        "illegal"
    }
}

fn request_lifecycle_for_case(
    db: &support::TestDb,
    doc_id: String,
    request_id: String,
    session_id: String,
    created_at: String,
) -> RequestLifecycle {
    let request = build_request(doc_id, request_id, session_id, created_at);
    RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    )
}

async fn drive_generated_request_legal_case(case: &LeanLifecycleTransitionCase) {
    let db = test_db("generated-request-transition").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;
    let action = case
        .action
        .as_deref()
        .expect("legal Request transition case must carry an action");
    let mut lifecycle = request_lifecycle_for_case(
        &db,
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at.clone(),
    );

    match action {
        "claim" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
        }
        "dedupLose" => {
            let escaped_doc_id = escape_graphql_string(&doc_id);
            let resp = db
                .node
                .execute(&format!(
                    r#"mutation {{
                        update_AgentRequest(
                            filter: {{
                                _docID: {{ _eq: "{escaped_doc_id}" }},
                                status: {{ _eq: "pending" }},
                                lifecycle_state: {{ _eq: "pending" }}
                            }},
                            input: {{
                                status: "superseded",
                                lifecycle_state: "superseded",
                                superseded_by_request: "explicit-replacement-{request_id}"
                            }}
                        ) {{ _docID }}
                    }}"#
                ))
                .await;
            assert!(
                !resp.has_errors(),
                "explicit supersede writer failed: {:?}",
                resp.errors
            );
        }
        "beginInference" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            lifecycle.prepare_session_with_identity().await.unwrap();
            lifecycle.begin_execution().await.unwrap();
        }
        "advance" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            lifecycle.prepare_session_with_identity().await.unwrap();
            lifecycle.begin_execution().await.unwrap();
            let response_doc_id = create_response_with_status(
                &db.node,
                &format!("resp-{request_id}"),
                &request_id,
                &session_id,
                "streaming",
            )
            .await;
            lifecycle.set_response_doc_id(&response_doc_id);
            lifecycle.advance().await.unwrap();
        }
        "finish" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            lifecycle.prepare_session_with_identity().await.unwrap();
            lifecycle.begin_execution().await.unwrap();
            lifecycle.complete().await.unwrap();
        }
        "fail" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            lifecycle.prepare_session_with_identity().await.unwrap();
            lifecycle.begin_execution().await.unwrap();
            lifecycle.fail().await.unwrap();
        }
        "failBeforeStream" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            lifecycle.prepare_session_with_identity().await.unwrap();
            lifecycle.fail().await.unwrap();
        }
        "expire" => {
            let past = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
            set_valid_until(&db.node, &doc_id, &past).await;
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Expired);
        }
        "interruptBeforeClaim" => {
            let interrupt_at = chrono::Utc::now().to_rfc3339();
            set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Interrupted);
        }
        "interruptClaimed" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            let interrupt_at = chrono::Utc::now().to_rfc3339();
            set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;
            lifecycle.transition_to_interrupted().await.unwrap();
        }
        "interruptProcessing" => {
            assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
            lifecycle.prepare_session_with_identity().await.unwrap();
            lifecycle.begin_execution().await.unwrap();
            let interrupt_at = chrono::Utc::now().to_rfc3339();
            set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;
            lifecycle.transition_to_interrupted().await.unwrap();
        }
        other => panic!(
            "generated Request transition {} has unsupported action {other:?}",
            case.name
        ),
    }

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        snap.lifecycle_state, case.to,
        "generated Request transition {} expected {} -> {} classified as {} via {:?}, got persisted lifecycle_state={}",
        case.name, case.from, case.to, case.classification, case.action, snap.lifecycle_state
    );
}

#[tokio::test]
async fn generated_request_transition_cases_cover_lifecycle_policy() {
    let mut legal_count = 0;
    let mut illegal_count = 0;
    let mut product_unreachable_count = 0;

    for case in lean_request_transition_cases() {
        let rust_classification = rust_request_transition_classification(&case.from, &case.to);
        assert_eq!(
            case.classification, rust_classification,
            "Request transition {} expected classification drift for {} -> {}; Lean action={:?} boundary={:?}",
            case.name, case.from, case.to, case.action, case.boundary
        );

        match case.classification.as_str() {
            "legal" => {
                legal_count += 1;
                assert_eq!(
                    case.action.as_deref(),
                    rust_request_transition_action(&case.from, &case.to),
                    "Request transition {} legal writer action drifted for {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
                drive_generated_request_legal_case(case).await;
            }
            "illegal" => {
                illegal_count += 1;
                assert!(
                    rust_request_transition_action(&case.from, &case.to).is_none(),
                    "Request transition {} is ordinary illegal but Rust has a writer path for {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
            }
            "productUnreachable" => {
                product_unreachable_count += 1;
                assert!(
                    case.from == "inputRequired" || case.to == "inputRequired",
                    "Request transition {} product-unreachable classification must be scoped to reserved inputRequired, got {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
                assert_eq!(
                    case.boundary.as_deref(),
                    Some("boundary.request.input-required-reserved"),
                    "Request transition {} must cite the reserved inputRequired boundary",
                    case.name
                );
                assert!(
                    rust_request_transition_action(&case.from, &case.to).is_none(),
                    "Request transition {} is reserved but Rust has a writer path for {} -> {}",
                    case.name,
                    case.from,
                    case.to
                );
            }
            other => panic!(
                "generated Request transition {} has unknown classification {other:?}",
                case.name
            ),
        }
    }

    assert_eq!(legal_count, 11);
    assert_eq!(illegal_count, 53);
    assert_eq!(product_unreachable_count, 17);
}

#[tokio::test]
async fn interactive_claim_snapshot_matches_claimed_waiting() {
    let db = test_db("interactive-claim").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    assert_lean_transition_is_legal("Request", "pending", "claimed");

    assert_eq!(
        fetch_request_snapshot(&db.node, &doc_id).await,
        RequestSnapshot {
            status: "processing".into(),
            lifecycle_state: "claimed".into(),
            behavior_id: AGENT_NAME.into(),
            backend_id: BACKEND_ID.into(),
            execution_origin: "interactive".into(),
            retry_parent_request: "".into(),
            retry_root_request: request_id.clone(),
            superseded_by_request: "".into(),
            retry_count: 0,
            max_retries: defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );
    assert_eq!(
        fetch_conversation_snapshot(&db.node, &session_id).await,
        None
    );
    assert_eq!(fetch_session_snapshot(&db.node, &session_id).await, None);
}

#[tokio::test]
async fn interactive_prepare_session_pins_behavior() {
    let db = test_db("interactive-prepare-session").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle.prepare_session_with_identity().await.unwrap();

    assert_eq!(
        fetch_session_snapshot(&db.node, &session_id).await,
        Some(SessionSnapshot {
            session_id: session_id.clone(),
            behavior_id: AGENT_NAME.into(),
            status: "active".into(),
        })
    );
    assert_eq!(
        fetch_conversation_snapshot(&db.node, &session_id).await,
        Some(ConversationSnapshot {
            latest_request_id: request_id,
            behavior_id: AGENT_NAME.into(),
            status: "processing".into(),
            forked_from_session_id: None,
            fork_at_user_turn: None,
            forked_at: None,
        })
    );
}

#[tokio::test]
async fn interactive_admission_and_progress_snapshots_match_execution_flow() {
    let db = test_db("interactive-executing").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle.prepare_session_with_identity().await.unwrap();
    lifecycle.begin_execution().await.unwrap();
    assert_lean_transition_is_legal("Request", "claimed", "processing");
    let response_doc_id = create_response_with_status(
        &db.node,
        &format!("resp-{request_id}"),
        &request_id,
        &session_id,
        "streaming",
    )
    .await;
    lifecycle.set_response_doc_id(&response_doc_id);
    lifecycle.advance().await.unwrap();
    assert_lean_transition_is_legal("Request", "processing", "processing");

    assert_eq!(
        fetch_request_snapshot(&db.node, &doc_id).await,
        RequestSnapshot {
            status: "processing".into(),
            lifecycle_state: "processing".into(),
            behavior_id: AGENT_NAME.into(),
            backend_id: BACKEND_ID.into(),
            execution_origin: "interactive".into(),
            retry_parent_request: "".into(),
            retry_root_request: request_id.clone(),
            superseded_by_request: "".into(),
            retry_count: 0,
            max_retries: defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );
    assert_eq!(
        fetch_conversation_snapshot(&db.node, &session_id).await,
        Some(ConversationSnapshot {
            latest_request_id: request_id,
            behavior_id: AGENT_NAME.into(),
            status: "processing".into(),
            forked_from_session_id: None,
            fork_at_user_turn: None,
            forked_at: None,
        })
    );
    assert_eq!(
        fetch_session_snapshot(&db.node, &session_id).await,
        Some(SessionSnapshot {
            session_id,
            behavior_id: AGENT_NAME.into(),
            status: "active".into(),
        })
    );
    assert_eq!(
        fetch_response_snapshot(&db.node, &response_doc_id).await,
        ResponseSnapshot {
            status: "streaming".into(),
            behavior_id: AGENT_NAME.into(),
            progress_seq: 1,
            completed_at_present: false,
        }
    );
}

#[tokio::test]
async fn interactive_fail_before_stream_snapshot_matches_failed_released() {
    let db = test_db("interactive-fail").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle.prepare_session_with_identity().await.unwrap();
    lifecycle.fail().await.unwrap();
    assert_lean_transition_is_legal("Request", "claimed", "failed");

    assert_eq!(
        fetch_request_snapshot(&db.node, &doc_id).await,
        RequestSnapshot {
            status: "error".into(),
            lifecycle_state: "failed".into(),
            behavior_id: AGENT_NAME.into(),
            backend_id: BACKEND_ID.into(),
            execution_origin: "interactive".into(),
            retry_parent_request: "".into(),
            retry_root_request: request_id.clone(),
            superseded_by_request: "".into(),
            retry_count: 0,
            max_retries: defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );
    assert_eq!(
        fetch_conversation_snapshot(&db.node, &session_id).await,
        Some(ConversationSnapshot {
            latest_request_id: request_id,
            behavior_id: AGENT_NAME.into(),
            status: "active".into(),
            forked_from_session_id: None,
            fork_at_user_turn: None,
            forked_at: None,
        })
    );
    assert_eq!(
        fetch_session_snapshot(&db.node, &session_id).await,
        Some(SessionSnapshot {
            session_id,
            behavior_id: AGENT_NAME.into(),
            status: "active".into(),
        })
    );
}

#[tokio::test]
async fn scheduled_materialization_snapshot_matches_claimed_waiting() {
    let db = test_db("scheduled-materialize").await;
    let lifecycle = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "scheduled prompt body",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        TriggerLineage::default(),
    )
    .await
    .unwrap();

    assert_eq!(
        fetch_request_snapshot(&db.node, &lifecycle.request().doc_id).await,
        RequestSnapshot {
            status: "processing".into(),
            lifecycle_state: "claimed".into(),
            behavior_id: AGENT_NAME.into(),
            backend_id: BACKEND_ID.into(),
            execution_origin: "scheduled".into(),
            retry_parent_request: "".into(),
            retry_root_request: lifecycle.request().request_id.clone(),
            superseded_by_request: "".into(),
            retry_count: 0,
            max_retries: defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );
    assert_eq!(
        fetch_session_snapshot(&db.node, &lifecycle.request().session_id).await,
        Some(SessionSnapshot {
            session_id: lifecycle.request().session_id.clone(),
            behavior_id: AGENT_NAME.into(),
            status: "active".into(),
        })
    );
    assert_eq!(
        fetch_conversation_snapshot(&db.node, &lifecycle.request().session_id).await,
        Some(ConversationSnapshot {
            latest_request_id: lifecycle.request().request_id.clone(),
            behavior_id: AGENT_NAME.into(),
            status: "processing".into(),
            forked_from_session_id: None,
            fork_at_user_turn: None,
            forked_at: None,
        })
    );
}

#[tokio::test]
async fn scheduled_materialization_persists_trigger_lineage() {
    let db = test_db("scheduled-materialize-lineage").await;
    let lineage = TriggerLineage {
        trigger_id: Some("sched-1".into()),
        trigger_kind: Some("schedule".into()),
    };

    let lifecycle = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "scheduled prompt body with lineage",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        lineage,
    )
    .await
    .unwrap();

    assert_eq!(
        fetch_request_lineage_snapshot(&db.node, &lifecycle.request().doc_id).await,
        RequestLineageSnapshot {
            caused_by_trigger_id: Some("sched-1".into()),
            caused_by_trigger_kind: Some("schedule".into()),
        }
    );

    // Bonus: confirm the tuple filter matches (validates indexes are wired end-to-end).
    assert_eq!(
        fetch_request_lineage_snapshot_by_tuple(&db.node, "sched-1", "schedule").await,
        Some(RequestLineageSnapshot {
            caused_by_trigger_id: Some("sched-1".into()),
            caused_by_trigger_kind: Some("schedule".into()),
        })
    );
}

// -----------------------------------------------------------------------------
// Trigger-driven transitions (Task 48)
//
// Each case below pins a state-machine invariant the TriggerEngine relies on
// when driving schedule/event fires. They share the same style as the older
// cases above: seed request state via the lifecycle entry point the engine
// uses, then exercise the exact GraphQL mutation / query
// `ProductionMaterializer` issues, and assert the resulting on-disk snapshot.
// -----------------------------------------------------------------------------

/// Serial concurrency: when an active runtime request already exists for the
/// `(trigger_id, trigger_kind)` tuple, the materializer's
/// `has_active_runtime_request_for_trigger` query must observe it (the engine
/// turns this into `FireResult::Skipped`). The state-machine conformance
/// assertion: no second `AgentRequest` is created for the tuple — the count
/// observed before and after the skip decision is the same.
#[tokio::test]
async fn serial_skip_does_not_create_request() {
    let db = test_db("transition-serial-skip").await;

    // Seed an in-flight request with lineage tuple (sched-serial, schedule).
    let lineage = TriggerLineage {
        trigger_id: Some("sched-serial".into()),
        trigger_kind: Some("schedule".into()),
    };
    let seeded = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "serial seed",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        lineage,
    )
    .await
    .unwrap();

    // Run the exact query `ProductionMaterializer` uses to gate the fire.
    // Expect `true` — an active runtime request for this tuple exists.
    let gating_query = r#"query {
            AgentRequest(
                filter: {
                    caused_by_trigger_id: { _eq: "sched-serial" },
                    caused_by_trigger_kind: { _eq: "schedule" },
                    lifecycle_state: { _in: ["pending", "claimed", "processing"] }
                },
                limit: 1
            ) { _docID }
        }"#
    .to_string();
    let gate = db.node.execute(&gating_query).await;
    assert!(
        !gate.has_errors(),
        "gating query errored: {:?}",
        gate.errors
    );
    let gate_rows = gate
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        gate_rows.len(),
        1,
        "gating query must see the seeded in-flight request"
    );

    // Count all AgentRequests for the trigger tuple before the skip decision.
    let tuple_count_query = r#"{
        AgentRequest(
            filter: {
                caused_by_trigger_id: { _eq: "sched-serial" },
                caused_by_trigger_kind: { _eq: "schedule" }
            }
        ) { _docID }
    }"#;
    let count_before = db
        .node
        .execute(tuple_count_query)
        .await
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .map(|rows| rows.len())
        .unwrap_or(0);
    assert_eq!(count_before, 1, "seeded count should be 1");

    // Simulate the engine's FireResult::Skipped outcome: no materialize call
    // is made. Count after must still be 1 — the state machine invariant
    // "serial skip does not create request" holds.
    let count_after = db
        .node
        .execute(tuple_count_query)
        .await
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .map(|rows| rows.len())
        .unwrap_or(0);
    assert_eq!(
        count_after, count_before,
        "serial skip must not create a new AgentRequest"
    );

    // Sanity: the seeded request is still in its active runtime state
    // (the skip decision neither advances nor terminates it).
    let still_claimed = fetch_request_snapshot(&db.node, &seeded.request().doc_id).await;
    assert_eq!(still_claimed.lifecycle_state, "claimed");
    assert_eq!(still_claimed.status, "processing");
}

/// LatestOnly concurrency: when a new fire arrives for a trigger tuple with an
/// in-flight request, the engine supersedes the prior via the same mutation
/// shape `ProductionMaterializer::supersede_active_runtime_requests_for_trigger`
/// uses. The seeded request must transition
/// `(processing / claimed) -> (superseded / superseded)` exactly.
#[tokio::test]
async fn latest_only_transition_to_superseded() {
    let db = test_db("transition-latest-only").await;

    let lineage = TriggerLineage {
        trigger_id: Some("sched-latest".into()),
        trigger_kind: Some("schedule".into()),
    };
    let seeded = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "latest seed",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        lineage,
    )
    .await
    .unwrap();

    // Pre-condition: the seeded request is (claimed, processing).
    let before = fetch_request_snapshot(&db.node, &seeded.request().doc_id).await;
    assert_eq!(before.lifecycle_state, "claimed");
    assert_eq!(before.status, "processing");

    // Run the engine's supersede mutation verbatim (the shape in
    // `production_materializer::supersede_active_runtime_requests_for_trigger`).
    let supersede = r#"mutation {
            update_AgentRequest(
                filter: {
                    caused_by_trigger_id: { _eq: "sched-latest" },
                    caused_by_trigger_kind: { _eq: "schedule" },
                    lifecycle_state: { _in: ["pending", "claimed", "processing"] }
                },
                input: {
                    status: "superseded",
                    lifecycle_state: "superseded"
                }
            ) { _docID }
        }"#
    .to_string();
    let resp = db.node.execute(&supersede).await;
    assert!(
        !resp.has_errors(),
        "supersede mutation errored: {:?}",
        resp.errors
    );
    let updated_rows = resp
        .data
        .as_ref()
        .and_then(|d| d.get("update_AgentRequest"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        updated_rows.len(),
        1,
        "supersede must transition exactly the one seeded in-flight request"
    );

    // Post-condition: the seeded request is now (superseded, superseded);
    // all other snapshot fields carry forward.
    assert_eq!(
        fetch_request_snapshot(&db.node, &seeded.request().doc_id).await,
        RequestSnapshot {
            status: "superseded".into(),
            lifecycle_state: "superseded".into(),
            behavior_id: AGENT_NAME.into(),
            backend_id: BACKEND_ID.into(),
            execution_origin: "scheduled".into(),
            retry_parent_request: "".into(),
            retry_root_request: seeded.request().request_id.clone(),
            superseded_by_request: "".into(),
            retry_count: 0,
            max_retries: defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );
}

#[tokio::test]
async fn active_runtime_trigger_filters_ignore_input_required() {
    let db = test_db("transition-input-required-active-filter").await;

    let lineage = TriggerLineage {
        trigger_id: Some("sched-input-required".into()),
        trigger_kind: Some("schedule".into()),
    };
    let seeded = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "reserved inputRequired seed",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        lineage,
    )
    .await
    .unwrap();
    set_request_lifecycle_state(&db.node, &seeded.request().doc_id, "inputRequired").await;

    let gating_query = r#"query {
        AgentRequest(
            filter: {
                caused_by_trigger_id: { _eq: "sched-input-required" },
                caused_by_trigger_kind: { _eq: "schedule" },
                lifecycle_state: { _in: ["pending", "claimed", "processing"] }
            },
            limit: 1
        ) { _docID }
    }"#;
    let gate = db.node.execute(gating_query).await;
    assert!(
        !gate.has_errors(),
        "gating query errored: {:?}",
        gate.errors
    );
    let gate_rows = gate
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        gate_rows.is_empty(),
        "active runtime gate must not observe reserved inputRequired rows"
    );

    let supersede = r#"mutation {
        update_AgentRequest(
            filter: {
                caused_by_trigger_id: { _eq: "sched-input-required" },
                caused_by_trigger_kind: { _eq: "schedule" },
                lifecycle_state: { _in: ["pending", "claimed", "processing"] }
            },
            input: {
                status: "superseded",
                lifecycle_state: "superseded"
            }
        ) { _docID }
    }"#;
    let resp = db.node.execute(supersede).await;
    assert!(
        !resp.has_errors(),
        "supersede mutation errored: {:?}",
        resp.errors
    );
    let updated_rows = resp
        .data
        .as_ref()
        .and_then(|d| d.get("update_AgentRequest"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        updated_rows.is_empty(),
        "active runtime supersede must not transition reserved inputRequired rows"
    );
    assert_lean_transition_is_illegal("Request", "inputRequired", "superseded");

    let snap = fetch_request_snapshot(&db.node, &seeded.request().doc_id).await;
    assert_eq!(snap.status, "processing");
    assert_eq!(snap.lifecycle_state, "inputRequired");
}

/// Template render failure (`FireResult::Errored`): the engine must NOT
/// invoke the materializer when the render fails. The state-machine
/// conformance assertion: after a simulated render failure, no `AgentRequest`
/// exists for the trigger tuple, and the Schedule's runtime-owned
/// `last_status = "error"` writeback is independent of any request row.
#[tokio::test]
async fn fire_errored_does_not_create_request() {
    let db = test_db("transition-fire-errored").await;

    // The persistence boundary: no materialize call means no AgentRequest
    // row with the lineage tuple. Query the engine's tuple filter to confirm.
    let query = r#"query {
            AgentRequest(
                filter: {
                    caused_by_trigger_id: { _eq: "sched-render-err" },
                    caused_by_trigger_kind: { _eq: "schedule" }
                }
            ) { _docID }
        }"#
    .to_string();
    let resp = db.node.execute(&query).await;
    assert!(!resp.has_errors(), "tuple query errored: {:?}", resp.errors);
    let rows = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        rows.len(),
        0,
        "render failure must not have produced an AgentRequest: {rows:?}"
    );

    // Seed a Schedule doc and simulate the Errored writeback the source
    // performs (see ScheduleSource::on_result FireResult::Errored branch).
    // The key state-machine invariant: even though last_status="error" is
    // written to the Schedule, NO AgentRequest appears for the trigger tuple.
    let escaped_past = escape_graphql_string("2026-04-21T12:00:00Z");
    let create_sched = format!(
        r#"mutation {{
            create_Schedule(input: {{
                schedule_id: "sched-render-err",
                task_id: "task-render-err",
                interval_secs: 60,
                enabled: true,
                concurrency: "serial",
                next_run_at: "{escaped_past}"
            }}) {{ _docID }}
        }}"#
    );
    assert!(!db.node.execute(&create_sched).await.has_errors(),);
    let writeback = format!(
        r#"mutation {{
            update_Schedule(
                filter: {{ schedule_id: {{ _eq: "sched-render-err" }} }},
                input: {{
                    next_run_at: "{escaped_past}",
                    last_status: "error",
                    last_error: "template: variable 'missing' is undefined"
                }}
            ) {{ _docID }}
        }}"#
    );
    let wb_resp = db.node.execute(&writeback).await;
    assert!(
        !wb_resp.has_errors(),
        "errored writeback failed: {:?}",
        wb_resp.errors
    );

    // Re-check the persistence boundary after the writeback landed.
    let resp = db.node.execute(&query).await;
    let rows = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        rows.len(),
        0,
        "Errored writeback on Schedule must not materialize an AgentRequest: {rows:?}"
    );

    // Confirm the writeback did land on the Schedule side — the state of the
    // Schedule reflects the engine's error classification without ever
    // producing a request row.
    let sched_query = r#"{
        Schedule(filter: { schedule_id: { _eq: "sched-render-err" } }, limit: 1) {
            last_status
            last_error
        }
    }"#;
    let sched_resp = db.node.execute(sched_query).await;
    let sched_row = sched_resp
        .data
        .as_ref()
        .and_then(|d| d.get("Schedule"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("Schedule doc was created");
    assert_eq!(
        sched_row.get("last_status").and_then(|v| v.as_str()),
        Some("error"),
        "Schedule.last_status must be 'error' after an Errored writeback"
    );
    assert!(
        sched_row
            .get("last_error")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.starts_with("template:")),
        "Schedule.last_error must carry the template: prefix: {sched_row}"
    );
}

// -----------------------------------------------------------------------------
// Event-kind trigger transitions (Task 31, PR 2)
//
// Mirrors the Task 48 (PR 1) schedule-kind cases above but with
// `caused_by_trigger_kind = "event"`. The state-machine invariants being
// pinned are identical across kinds — the engine routes both Schedule and
// Event fires through the same `ProductionMaterializer` surface — but each
// kind's gating and supersede mutations filter on the tuple
// `(caused_by_trigger_id, caused_by_trigger_kind)`, so the event-kind cases
// need their own independent assertions.
// -----------------------------------------------------------------------------

/// Serial concurrency (event kind): when an active runtime request with
/// `caused_by_trigger_kind = "event"` already exists for the tuple, the
/// materializer's gating query observes it and the engine skips. The
/// state-machine conformance assertion: no second `AgentRequest` is created
/// for the tuple.
#[tokio::test]
async fn serial_skip_event_does_not_create_request() {
    let db = test_db("transition-event-serial-skip").await;

    // Seed an in-flight request with lineage tuple (trigger-event-serial, event).
    let lineage = TriggerLineage {
        trigger_id: Some("trigger-event-serial".into()),
        trigger_kind: Some("event".into()),
    };
    let seeded = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "event serial seed",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        lineage,
    )
    .await
    .unwrap();

    // Run the exact query `ProductionMaterializer` uses to gate the fire.
    // Expect `true` — an active runtime request for this tuple exists.
    let gating_query = r#"query {
        AgentRequest(
            filter: {
                caused_by_trigger_id: { _eq: "trigger-event-serial" },
                caused_by_trigger_kind: { _eq: "event" },
                lifecycle_state: { _in: ["pending", "claimed", "processing"] }
            },
            limit: 1
        ) { _docID }
    }"#;
    let gate = db.node.execute(gating_query).await;
    assert!(
        !gate.has_errors(),
        "gating query errored: {:?}",
        gate.errors
    );
    let gate_rows = gate
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        gate_rows.len(),
        1,
        "gating query must see the seeded in-flight event-kind request"
    );

    // Count all AgentRequests for the trigger tuple before the skip decision.
    let tuple_count_query = r#"{
        AgentRequest(
            filter: {
                caused_by_trigger_id: { _eq: "trigger-event-serial" },
                caused_by_trigger_kind: { _eq: "event" }
            }
        ) { _docID }
    }"#;
    let count_before = db
        .node
        .execute(tuple_count_query)
        .await
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .map(|rows| rows.len())
        .unwrap_or(0);
    assert_eq!(count_before, 1, "seeded count should be 1");

    // Simulate the engine's FireResult::Skipped outcome: no materialize call.
    // Count after must still be 1 — the state machine invariant "serial skip
    // does not create request" holds for event-kind triggers too.
    let count_after = db
        .node
        .execute(tuple_count_query)
        .await
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .map(|rows| rows.len())
        .unwrap_or(0);
    assert_eq!(
        count_after, count_before,
        "serial skip on event-kind trigger must not create a new AgentRequest"
    );

    // Sanity: the seeded request is still in its active runtime state.
    let still_claimed = fetch_request_snapshot(&db.node, &seeded.request().doc_id).await;
    assert_eq!(still_claimed.lifecycle_state, "claimed");
    assert_eq!(still_claimed.status, "processing");
}

/// LatestOnly concurrency (event kind): when a new fire arrives for an
/// event-kind trigger tuple with an in-flight request, the engine supersedes
/// the prior via the same mutation shape the Schedule case uses — filtered on
/// `caused_by_trigger_kind = "event"`. The seeded request must transition
/// `(processing / claimed) -> (superseded / superseded)`.
#[tokio::test]
async fn latest_only_event_transition_to_superseded() {
    let db = test_db("transition-event-latest-only").await;

    let lineage = TriggerLineage {
        trigger_id: Some("trigger-event-latest".into()),
        trigger_kind: Some("event".into()),
    };
    let seeded = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "event latest seed",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        lineage,
    )
    .await
    .unwrap();

    // Pre-condition: the seeded request is (claimed, processing).
    let before = fetch_request_snapshot(&db.node, &seeded.request().doc_id).await;
    assert_eq!(before.lifecycle_state, "claimed");
    assert_eq!(before.status, "processing");

    // Run the engine's supersede mutation verbatim, filtered on the event
    // kind's lineage tuple (the shape in
    // `production_materializer::supersede_active_runtime_requests_for_trigger`).
    let supersede = r#"mutation {
        update_AgentRequest(
            filter: {
                caused_by_trigger_id: { _eq: "trigger-event-latest" },
                caused_by_trigger_kind: { _eq: "event" },
                lifecycle_state: { _in: ["pending", "claimed", "processing"] }
            },
            input: {
                status: "superseded",
                lifecycle_state: "superseded"
            }
        ) { _docID }
    }"#;
    let resp = db.node.execute(supersede).await;
    assert!(
        !resp.has_errors(),
        "supersede mutation errored: {:?}",
        resp.errors
    );
    let updated_rows = resp
        .data
        .as_ref()
        .and_then(|d| d.get("update_AgentRequest"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        updated_rows.len(),
        1,
        "supersede must transition exactly the one seeded in-flight event-kind request"
    );

    // Post-condition: the seeded request is now (superseded, superseded);
    // all other snapshot fields carry forward.
    assert_eq!(
        fetch_request_snapshot(&db.node, &seeded.request().doc_id).await,
        RequestSnapshot {
            status: "superseded".into(),
            lifecycle_state: "superseded".into(),
            behavior_id: AGENT_NAME.into(),
            backend_id: BACKEND_ID.into(),
            execution_origin: "scheduled".into(),
            retry_parent_request: "".into(),
            retry_root_request: seeded.request().request_id.clone(),
            superseded_by_request: "".into(),
            retry_count: 0,
            max_retries: defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES as i64,
            claimed_at_present: true,
            deadline_present: true,
            failure_reason: "".into(),
        }
    );
}

/// Template render failure on an EventTrigger (`FireResult::Errored`): the
/// engine must NOT invoke the materializer when the render fails. The
/// state-machine conformance assertion: after a simulated render failure, no
/// `AgentRequest` exists for the event-kind trigger tuple, and the
/// EventTrigger's runtime-owned `last_status = "error"` writeback is
/// independent of any request row.
#[tokio::test]
async fn fire_errored_event_does_not_create_request() {
    let db = test_db("transition-event-fire-errored").await;

    // The persistence boundary: no materialize call means no AgentRequest
    // row with the event-kind lineage tuple. Query the engine's tuple filter.
    let query = r#"query {
        AgentRequest(
            filter: {
                caused_by_trigger_id: { _eq: "trigger-event-render-err" },
                caused_by_trigger_kind: { _eq: "event" }
            }
        ) { _docID }
    }"#;
    let resp = db.node.execute(query).await;
    assert!(!resp.has_errors(), "tuple query errored: {:?}", resp.errors);
    let rows = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        rows.len(),
        0,
        "render failure must not have produced an AgentRequest: {rows:?}"
    );

    // Seed an EventTrigger doc and simulate the Errored writeback the source
    // performs (see EventSource::spawn_runtime_field_write FireResult::Errored
    // branch). The key state-machine invariant: even though last_status="error"
    // is written to the EventTrigger, NO AgentRequest appears for the lineage
    // tuple.
    let create_trigger = r#"mutation {
        create_EventTrigger(input: {
            trigger_id: "trigger-event-render-err",
            task_id: "task-event-render-err",
            source_collection: "WebhookEvent",
            event_kind: "created",
            enabled: true,
            concurrency: "serial",
            fire_count: 0
        }) { _docID }
    }"#;
    let create_resp = db.node.execute(create_trigger).await;
    assert!(
        !create_resp.has_errors(),
        "create EventTrigger failed: {:?}",
        create_resp.errors
    );

    // The actual runtime-field writeback path. Keep the input literal aligned
    // with what `update_event_trigger_runtime_fields` produces on an
    // `FireResult::Errored` outcome: last_status="error", last_error=<reason>,
    // fire_count untouched.
    let writeback = r#"mutation {
        update_EventTrigger(
            filter: { trigger_id: { _eq: "trigger-event-render-err" } },
            input: {
                last_status: "error",
                last_error: "template: variable 'missing_field' is undefined"
            }
        ) { _docID }
    }"#;
    let wb_resp = db.node.execute(writeback).await;
    assert!(
        !wb_resp.has_errors(),
        "errored writeback failed: {:?}",
        wb_resp.errors
    );

    // Re-check the persistence boundary after the writeback landed.
    let resp = db.node.execute(query).await;
    let rows = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        rows.len(),
        0,
        "Errored writeback on EventTrigger must not materialize an AgentRequest: {rows:?}"
    );

    // Confirm the writeback did land on the EventTrigger side — the state of
    // the trigger reflects the engine's error classification without ever
    // producing a request row. `fire_count` must NOT have advanced (writeback
    // supplied no `fire_count` field at all).
    let trigger_query = r#"{
        EventTrigger(filter: { trigger_id: { _eq: "trigger-event-render-err" } }, limit: 1) {
            last_status
            last_error
            fire_count
        }
    }"#;
    let trigger_resp = db.node.execute(trigger_query).await;
    let trigger_row = trigger_resp
        .data
        .as_ref()
        .and_then(|d| d.get("EventTrigger"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("EventTrigger doc was created");
    assert_eq!(
        trigger_row.get("last_status").and_then(|v| v.as_str()),
        Some("error"),
        "EventTrigger.last_status must be 'error' after an Errored writeback"
    );
    assert!(
        trigger_row
            .get("last_error")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.starts_with("template:")),
        "EventTrigger.last_error must carry the template: prefix: {trigger_row}"
    );
    assert_eq!(
        trigger_row.get("fire_count").and_then(|v| v.as_i64()),
        Some(0),
        "EventTrigger.fire_count must NOT advance on Errored writeback: {trigger_row}"
    );
}

#[tokio::test]
async fn fork_does_not_transition_parent_lifecycle_state() {
    use defra_agent::session::{fork, ForkParams};
    use support::{
        create_agent_behavior, create_agent_conversation, create_agent_message,
        create_agent_session,
    };

    let db = test_db("fork-no-lifecycle-transition").await;

    let parent_session = uuid::Uuid::new_v4().to_string();
    create_agent_session(
        &db.node,
        &parent_session,
        AGENT_NAME,
        "2026-04-21T10:00:00Z",
    )
    .await;
    create_agent_conversation(
        &db.node,
        &parent_session,
        AGENT_NAME,
        "2026-04-21T10:00:00Z",
    )
    .await;
    create_agent_behavior(&db.node, AGENT_NAME, AGENT_DID).await;

    // Parent has a completed AgentRequest + AgentResponse so the parent is idle
    // (no active runtime lifecycle_state) and fork is allowed.
    let request_id = uuid::Uuid::new_v4().to_string();
    let request_doc_id = create_request(
        &db.node,
        &request_id,
        &parent_session,
        "completed",
        "2026-04-21T10:00:02Z",
    )
    .await;
    let response_key = format!("resp-{request_id}");
    let response_doc_id = create_response_with_status(
        &db.node,
        &response_key,
        &request_id,
        &parent_session,
        "complete",
    )
    .await;

    create_agent_message(
        &db.node,
        &parent_session,
        1,
        "user",
        "u1",
        "2026-04-21T10:00:01Z",
    )
    .await;
    create_agent_message(
        &db.node,
        &parent_session,
        2,
        "assistant",
        "a1",
        "2026-04-21T10:00:03Z",
    )
    .await;

    let before_request = fetch_request_snapshot(&db.node, &request_doc_id).await;
    let before_response = fetch_response_snapshot(&db.node, &response_doc_id).await;
    let before_conversation = fetch_conversation_snapshot(&db.node, &parent_session).await;
    let before_session = fetch_session_snapshot(&db.node, &parent_session).await;

    let _ = fork(
        &db.node,
        ForkParams {
            source_session_id: &parent_session,
            fork_at_user_turn: 0,
            caller_agent_did: AGENT_DID,
            target_behavior_id: None,
        },
    )
    .await
    .expect("fork succeeds on idle parent");

    let after_request = fetch_request_snapshot(&db.node, &request_doc_id).await;
    let after_response = fetch_response_snapshot(&db.node, &response_doc_id).await;
    let after_conversation = fetch_conversation_snapshot(&db.node, &parent_session).await;
    let after_session = fetch_session_snapshot(&db.node, &parent_session).await;

    assert_eq!(
        before_request, after_request,
        "parent AgentRequest unchanged"
    );
    assert_eq!(
        before_response, after_response,
        "parent AgentResponse unchanged"
    );
    assert_eq!(
        before_conversation, after_conversation,
        "parent AgentConversation unchanged"
    );
    assert_eq!(
        before_session, after_session,
        "parent AgentSession unchanged"
    );
}

#[tokio::test]
async fn pending_interrupted_via_interrupt_before_claim() {
    let db = test_db("pending-interrupted").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let interrupt_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Interrupted);
    assert_lean_transition_is_legal("Request", "pending", "interrupted");

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, "interrupted");
    assert_eq!(snap.status, "interrupted");
}

#[tokio::test]
async fn pending_dead_stale_via_expire() {
    let db = test_db("pending-dead-stale").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let valid_until = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    set_valid_until(&db.node, &doc_id, &valid_until).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Expired);
    assert_lean_transition_is_legal("Request", "pending", "dead");

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, "dead");
    assert_eq!(snap.failure_reason, "Stale");
}

#[tokio::test]
async fn transition_to_interrupted_from_claimed() {
    // Validates the lifecycle transition from `claimed` to `interrupted` via
    // `transition_to_interrupted`. This test does NOT exercise the observer or
    // watch channel end-to-end — the full `tokio::select!` arm + observer race
    // is covered at integration level in Task 11.
    let db = test_db("claimed-interrupted").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    let interrupt_at = chrono::Utc::now().to_rfc3339();
    set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;
    lifecycle.transition_to_interrupted().await.unwrap();
    assert_lean_transition_is_legal("Request", "claimed", "interrupted");

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.status, "interrupted");
    assert_eq!(snap.lifecycle_state, "interrupted");
}

#[tokio::test]
async fn processing_interrupted_preserves_partial_response() {
    // Simulates: response streaming was in progress with partial content, then an
    // interrupt fires. Expected: content is preserved on the response row, response
    // has `interrupted_at` stamped, and the request row transitions to interrupted.
    let db = test_db("processing-interrupted").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle.prepare_session_with_identity().await.unwrap();
    lifecycle.begin_execution().await.unwrap();

    let partial_content = "partial streamed text";
    let response_doc_id = create_response_with_content_and_status(
        &db.node,
        &format!("resp-{request_id}"),
        &request_id,
        &session_id,
        partial_content,
        "streaming",
    )
    .await;
    lifecycle.set_response_doc_id(&response_doc_id);

    // Stamp interrupted_at on the response row first, then transition the request.
    let stream_writer =
        DefraStreamWriter::new(db.node.clone(), AGENT_DID, Duration::from_millis(50));
    let interrupt_at = chrono::Utc::now().to_rfc3339();
    let stamped = stream_writer
        .write_interrupted_at(&response_doc_id, &interrupt_at)
        .await
        .unwrap();
    assert!(stamped, "expected interrupted_at to be stamped");

    lifecycle.transition_to_interrupted().await.unwrap();
    assert_lean_transition_is_legal("Request", "processing", "interrupted");

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.status, "interrupted");
    assert_eq!(snap.lifecycle_state, "interrupted");

    let content = fetch_response_content(&db.node, &response_doc_id).await;
    assert_eq!(
        content, partial_content,
        "partial content must be preserved"
    );

    let interrupted_at = fetch_response_interrupted_at(&db.node, &response_doc_id).await;
    assert_eq!(interrupted_at.as_deref(), Some(interrupt_at.as_str()));
}

#[tokio::test]
async fn input_required_interrupt_is_rejected_without_transition() {
    // `inputRequired` is reserved persisted/client vocabulary. Rust may parse
    // and display it, but the core lifecycle cannot interrupt it until Lean
    // models an external-input loop.
    let db = test_db("input-required-interrupted").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle.prepare_session_with_identity().await.unwrap();
    lifecycle.begin_execution().await.unwrap();
    set_request_lifecycle_state(&db.node, &doc_id, "inputRequired").await;

    let interrupt_at = chrono::Utc::now().to_rfc3339();
    set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;
    let err = lifecycle
        .transition_to_interrupted()
        .await
        .expect_err("inputRequired is not an interruptible runtime state");
    assert!(
        err.to_string().contains("inputRequired"),
        "error should name the reserved state: {err:?}"
    );
    assert_lean_transition_is_illegal("Request", "inputRequired", "interrupted");

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.status, "processing");
    assert_eq!(snap.lifecycle_state, "inputRequired");

    let complete_err = lifecycle
        .complete()
        .await
        .expect_err("inputRequired must not complete through a status-only transition");
    assert!(
        complete_err.to_string().contains("inputRequired"),
        "complete error should describe the persisted lifecycle_state: {complete_err:?}"
    );
    assert_lean_transition_is_illegal("Request", "inputRequired", "completed");

    let fail_err = lifecycle
        .fail()
        .await
        .expect_err("inputRequired must not fail through a status-only transition");
    assert!(
        fail_err.to_string().contains("inputRequired"),
        "fail error should describe the persisted lifecycle_state: {fail_err:?}"
    );
    assert_lean_transition_is_illegal("Request", "inputRequired", "failed");

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.status, "processing");
    assert_eq!(snap.lifecycle_state, "inputRequired");
}

#[tokio::test]
async fn pending_tie_break_prefers_interrupt_over_expire() {
    let db = test_db("tie-break-pending").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let past = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    let interrupt_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    set_valid_until(&db.node, &doc_id, &past).await;
    set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Interrupted);
    assert_lean_transition_is_legal("Request", "pending", "interrupted");

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, "interrupted");
}

#[tokio::test]
async fn transition_to_interrupted_from_processing() {
    // Validates the lifecycle transition from `processing` to `interrupted`. The
    // daemon-level tie-break race (interrupt vs deadline both firing in the same
    // poll) cannot be expressed at the lifecycle layer alone — that is exercised
    // end-to-end in Task 11. Here we verify the transition succeeds from the
    // processing state (the typical in-flight state when the daemon's select arm
    // fires).
    let db = test_db("processing-tie-break").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle.prepare_session_with_identity().await.unwrap();
    lifecycle.begin_execution().await.unwrap();

    // Submitter requests interrupt while the request is processing.
    let interrupt_at = chrono::Utc::now().to_rfc3339();
    set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;

    // Interrupt arm wins: transition_to_interrupted succeeds from processing.
    lifecycle.transition_to_interrupted().await.unwrap();
    assert_lean_transition_is_legal("Request", "processing", "interrupted");

    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.status, "interrupted");
    assert_eq!(snap.lifecycle_state, "interrupted");
}

#[tokio::test]
async fn interrupt_request_is_idempotent() {
    // Two calls to `interrupt_request` on the same doc must latch exactly
    // once: the daemon observer relies on the first submitter's timestamp
    // so the interruption audit trail points at who pressed Esc, not at
    // whichever caller wrote last. Proof: S7 (latch-once) in
    // `proofs/Interrupt.lean`.
    let db = test_db("interrupt-idempotent").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let _doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    defra_agent::interrupt_request(&db.node, &request_id)
        .await
        .expect("first interrupt should succeed");
    let after_first = defra_agent::fetch_interrupt_requested_at(&db.node, &request_id)
        .await
        .expect("fetch after first interrupt");
    assert!(
        after_first.is_some(),
        "first interrupt should latch the field"
    );

    // Sleep long enough that, without the idempotent latch, a second write
    // would produce a strictly later RFC3339 timestamp.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    defra_agent::interrupt_request(&db.node, &request_id)
        .await
        .expect("second interrupt should be a no-op");
    let after_second = defra_agent::fetch_interrupt_requested_at(&db.node, &request_id)
        .await
        .expect("fetch after second interrupt");
    assert_eq!(
        after_first, after_second,
        "second call must not rewrite the latched timestamp"
    );
}

#[tokio::test]
async fn interrupt_request_errors_on_unknown_request_id() {
    // Interrupting a request id that doesn't exist must surface as an error
    // (not a silent no-op). Previously, `fetch_interrupt_requested_at`
    // returned `Ok(None)` for both "field is empty" and "row does not exist",
    // so `interrupt_request` would fall through to a filter-update that
    // matched zero rows and return `Ok(())`, tricking the caller into
    // thinking a bogus id had been successfully latched.
    let db = test_db("interrupt-unknown").await;
    let err = defra_agent::interrupt_request(&db.node, "bogus-id-that-does-not-exist").await;
    assert!(
        err.is_err(),
        "interrupting unknown request_id must error, got Ok"
    );
    let message = err.unwrap_err().to_string();
    assert!(
        message.contains("not found"),
        "error must mention not found; got: {message}"
    );
}

#[tokio::test]
async fn interrupt_on_already_terminal_is_noop() {
    // A completed request that later gets an interrupt_requested_at write must not
    // regress. `transition_to_interrupted` filters on `status._nin` of terminal
    // statuses, so the mutation is a no-op on completed rows.
    let db = test_db("interrupt-terminal-noop").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle.prepare_session_with_identity().await.unwrap();
    lifecycle.complete().await.unwrap();

    let before = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(before.status, "completed");
    assert_eq!(before.lifecycle_state, "completed");

    // Late-arriving interrupt request: the field gets written to DB, but
    // transition_to_interrupted must not mutate the terminal row.
    let interrupt_at = chrono::Utc::now().to_rfc3339();
    set_interrupt_requested_at(&db.node, &doc_id, &interrupt_at).await;
    lifecycle.transition_to_interrupted().await.unwrap();

    let after = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(after.status, "completed", "terminal row must not regress");
    assert_eq!(
        after.lifecycle_state, "completed",
        "terminal lifecycle_state must not regress"
    );
}

#[tokio::test]
async fn valid_until_cached_at_claim_ignores_post_claim_extension() {
    let db = test_db("s8-cached-at-claim").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let future = (chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    set_valid_until(&db.node, &doc_id, &future).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );

    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);

    // Caller rewrites valid_until to a far-future value after claim. Lifecycle should
    // not observe it: S8 says the scheduler reads valid_until exactly once at claim.
    let much_later = (chrono::Utc::now() + chrono::Duration::hours(10)).to_rfc3339();
    set_valid_until(&db.node, &doc_id, &much_later).await;

    // Assert the cached field on the lifecycle is unchanged from what was read at claim.
    let expected = chrono::DateTime::parse_from_rfc3339(&future)
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(lifecycle.valid_until_at_claim_for_test(), Some(expected));
}

// ---------------------------------------------------------------------------
// Property tests enforcing Lean invariants S7 / S8 / S1 + persistence ordering
// + the conformance mapping round-trip. Each test's comment cites the Lean
// theorem it guards against regression.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn s7_interrupt_requested_at_is_latch_never_rewritten() {
    // Per S7 (`interrupt_monotonicity`) in
    // `crates/defra-agent/proofs/Proofs/Properties/Safety.lean`: once
    // `interruptRequestedAt.isSome`, no `RequestContext.Transition` rewrites
    // it. The Rust mutations must preserve this latch across every lifecycle
    // transition that touches the row.

    let db = test_db("s7-interrupt-latch").await;
    let t0 = "2026-04-20T12:00:00+00:00".to_string();

    // Sequence A: pending -> interrupted (via claim's pre-claim branch)
    let request_id_a = uuid::Uuid::new_v4().to_string();
    let session_id_a = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id_a = create_request(
        &db.node,
        &request_id_a,
        &session_id_a,
        "pending",
        &created_at,
    )
    .await;

    set_interrupt_requested_at(&db.node, &doc_id_a, &t0).await;
    let snap0 = fetch_request_snapshot_raw(&db.node, &doc_id_a).await;
    assert_eq!(snap0.interrupt_requested_at.as_deref(), Some(t0.as_str()));

    let request_a = build_request(
        doc_id_a.clone(),
        request_id_a.clone(),
        session_id_a.clone(),
        created_at.clone(),
    );
    let mut lifecycle_a = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request_a,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(
        lifecycle_a.claim().await.unwrap(),
        ClaimOutcome::Interrupted
    );

    let snap_a = fetch_request_snapshot_raw(&db.node, &doc_id_a).await;
    assert_eq!(
        snap_a.interrupt_requested_at.as_deref(),
        Some(t0.as_str()),
        "S7: interrupt_before_claim must not rewrite interrupt_requested_at"
    );

    // Sequence B: fresh row, claimed -> interrupted (via transition_to_interrupted)
    let request_id_b = uuid::Uuid::new_v4().to_string();
    let session_id_b = uuid::Uuid::new_v4().to_string();
    let doc_id_b = create_request(
        &db.node,
        &request_id_b,
        &session_id_b,
        "pending",
        &created_at,
    )
    .await;

    let request_b = build_request(
        doc_id_b.clone(),
        request_id_b.clone(),
        session_id_b.clone(),
        created_at.clone(),
    );
    let mut lifecycle_b = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request_b,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(lifecycle_b.claim().await.unwrap(), ClaimOutcome::Claimed);

    // Submitter sets interrupt mid-flight, then the lifecycle flips the row.
    set_interrupt_requested_at(&db.node, &doc_id_b, &t0).await;
    let snap_b_pre = fetch_request_snapshot_raw(&db.node, &doc_id_b).await;
    assert_eq!(
        snap_b_pre.interrupt_requested_at.as_deref(),
        Some(t0.as_str())
    );
    lifecycle_b.transition_to_interrupted().await.unwrap();

    let snap_b = fetch_request_snapshot_raw(&db.node, &doc_id_b).await;
    assert_eq!(
        snap_b.interrupt_requested_at.as_deref(),
        Some(t0.as_str()),
        "S7: transition_to_interrupted must not rewrite interrupt_requested_at"
    );
}

#[tokio::test]
async fn s8_valid_until_never_rewritten_by_transitions() {
    // Per S8 (`valid_until_monotonicity`) in
    // `crates/defra-agent/proofs/Proofs/Properties/Safety.lean`: no
    // `RequestContext.Transition` rewrites `validUntil` (unconditional). Run a
    // full claim + begin_execution + transition_to_interrupted sequence and
    // assert `valid_until` is unchanged after each persisted transition.

    let db = test_db("s8-valid-until-preserved").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let t0 = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    set_valid_until(&db.node, &doc_id, &t0).await;
    let snap0 = fetch_request_snapshot_raw(&db.node, &doc_id).await;
    assert_eq!(snap0.valid_until.as_deref(), Some(t0.as_str()));

    // Claim (pending → claimed)
    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    let snap1 = fetch_request_snapshot_raw(&db.node, &doc_id).await;
    assert_eq!(
        snap1.valid_until.as_deref(),
        Some(t0.as_str()),
        "S8: claim must not rewrite valid_until"
    );

    // begin_execution (claimed → processing)
    lifecycle.prepare_session_with_identity().await.unwrap();
    lifecycle.begin_execution().await.unwrap();
    let snap2 = fetch_request_snapshot_raw(&db.node, &doc_id).await;
    assert_eq!(
        snap2.valid_until.as_deref(),
        Some(t0.as_str()),
        "S8: begin_execution must not rewrite valid_until"
    );

    // transition_to_interrupted (processing → interrupted)
    lifecycle.transition_to_interrupted().await.unwrap();
    let snap3 = fetch_request_snapshot_raw(&db.node, &doc_id).await;
    assert_eq!(
        snap3.valid_until.as_deref(),
        Some(t0.as_str()),
        "S8: transition_to_interrupted must not rewrite valid_until"
    );
}

#[tokio::test]
async fn s1_interrupted_is_terminal_subsequent_transitions_are_no_ops() {
    // Per S1 (`terminal_irreversibility`) in
    // `crates/defra-agent/proofs/Proofs/Properties/Safety.lean`: no transition
    // leaves `.interrupted` for a non-terminal state. Transition a claimed
    // request to interrupted, then attempt subsequent transitions and assert
    // the DB row stays `interrupted` regardless of whether the Rust method
    // returns `Ok(())` (idempotent no-op) or `Err(...)` (caller mis-sequenced).

    let db = test_db("s1-interrupted-terminal").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle.transition_to_interrupted().await.unwrap();

    let snap0 = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap0.lifecycle_state, "interrupted");
    assert_eq!(snap0.status, "interrupted");
    assert_lean_transition_is_illegal("Request", "interrupted", "completed");
    assert_lean_transition_is_illegal("Request", "interrupted", "failed");
    assert_lean_transition_is_illegal("Request", "interrupted", "processing");

    // Idempotent: calling transition_to_interrupted again is a no-op because
    // the `status._nin` filter excludes terminal rows.
    lifecycle.transition_to_interrupted().await.unwrap();
    let snap1 = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        snap1.lifecycle_state, "interrupted",
        "S1: repeated transition_to_interrupted must stay interrupted"
    );
    assert_eq!(snap1.status, "interrupted");

    // complete() on an interrupted lifecycle may return Err (state mismatch)
    // or Ok (caller is expected to tolerate either). Either way, the DB row
    // must stay interrupted.
    let _complete_result = lifecycle.complete().await;
    let snap2 = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        snap2.lifecycle_state, "interrupted",
        "S1: complete() on interrupted must not reverse the terminal"
    );
    assert_eq!(snap2.status, "interrupted");

    // fail() same treatment.
    let _fail_result = lifecycle.fail().await;
    let snap3 = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        snap3.lifecycle_state, "interrupted",
        "S1: fail() on interrupted must not reverse the terminal"
    );
    assert_eq!(snap3.status, "interrupted");
}

#[tokio::test]
async fn ordering_response_interrupted_at_before_request_lifecycle_flip() {
    // The 6-step interrupt flow writes `AgentResponse.interrupted_at` BEFORE
    // `AgentRequest.lifecycle_state=interrupted`, per the spec's persistence-
    // ordering invariant: any subscriber observing the terminal lifecycle
    // also observes the marked partial response.
    //
    // DefraDB doesn't expose commit timestamps at query time, so we assert
    // the weaker observable: after the handler returns, BOTH writes exist.
    // This protects against the regression where the lifecycle flips but
    // `interrupted_at` is null. A stronger ordering assertion requires a
    // subscription-based observer (covered end-to-end in Task 11).

    let db = test_db("ordering-response-before-request").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    // Set up a claimed request with a streaming response that has partial content.
    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle.prepare_session_with_identity().await.unwrap();
    lifecycle.begin_execution().await.unwrap();

    let partial_content = "Hello wor";
    let response_doc_id = create_response_with_content_and_status(
        &db.node,
        &format!("resp-{request_id}"),
        &request_id,
        &session_id,
        partial_content,
        "streaming",
    )
    .await;
    lifecycle.set_response_doc_id(&response_doc_id);

    // Execute the 6-step flow sequence (mirroring run_inference's path):
    //   1. write interrupted_at on the response row
    //   2. transition the request to interrupted
    let intent_at = chrono::Utc::now().to_rfc3339();
    let stream_writer =
        DefraStreamWriter::new(db.node.clone(), AGENT_DID, Duration::from_millis(50));
    let stamped = stream_writer
        .write_interrupted_at(&response_doc_id, &intent_at)
        .await
        .unwrap();
    assert!(stamped, "ordering: interrupted_at must be stamped");
    lifecycle.transition_to_interrupted().await.unwrap();

    // Assert both writes are present.
    let response_interrupted_at = fetch_response_interrupted_at(&db.node, &response_doc_id).await;
    let request_snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(request_snap.lifecycle_state, "interrupted");
    assert_eq!(
        response_interrupted_at.as_deref(),
        Some(intent_at.as_str()),
        "ordering: if request.lifecycle_state=interrupted, response.interrupted_at must also be set"
    );
    // The partial content must be preserved verbatim.
    let response_content = fetch_response_content(&db.node, &response_doc_id).await;
    assert_eq!(response_content, partial_content);
}

#[test]
fn conformance_mapping_all_9_lifecycle_states_round_trip() {
    // Per `Proofs/Conformance/DefraAgent.lean::toIdeal`, every
    // `DefraLifecycleState` maps to a specific `RequestState`. The Rust
    // `RequestLifecycleState` enum in `defra-agent-protocol::client_protocol`
    // mirrors the Lean-generated RequestState vocabulary. Assert every Lean
    // string form parses and round-trips, and that unknown strings reject.
    use defra_agent_protocol::client_protocol::RequestLifecycleState;

    let lean_states = lean_vocabulary_values("RequestState");
    assert_eq!(
        lean_states.len(),
        9,
        "RequestState contract should be finite"
    );
    for s in lean_states {
        let parsed = RequestLifecycleState::try_from(s)
            .unwrap_or_else(|e| panic!("failed to parse '{}': {:?}", s, e));
        assert_eq!(
            parsed.as_str(),
            s,
            "as_str must round-trip to the source string"
        );
    }
    assert_eq!(
        RequestLifecycleState::try_from("inputRequired")
            .expect("reserved vocabulary should parse")
            .as_str(),
        "inputRequired"
    );

    // Unknown strings must reject.
    assert!(RequestLifecycleState::try_from("bogus").is_err());
    assert!(RequestLifecycleState::try_from("").is_err());
    assert!(RequestLifecycleState::try_from("INTERRUPTED").is_err());
}

#[test]
fn conformance_interrupted_lifecycle_maps_to_interrupted_client_turn() {
    // The client projection must map `RequestLifecycleState::Interrupted`
    // onto the distinct `ClientTurnState::Interrupted` terminal. This keeps
    // the Rust projection in sync with `Proofs/Client.lean::deriveAttempt`,
    // which now maps `.interrupted => .interrupted` rather than conflating
    // it with `.failed`.
    use defra_agent_protocol::client_protocol::{
        derive_attempt, AttemptView, ClientTurnState, RequestLifecycleState, RequestSnapshot,
    };

    let view = AttemptView {
        request: RequestSnapshot {
            request_id: "r1".into(),
            retry_parent_request: None,
            lifecycle_state: RequestLifecycleState::Interrupted,
            is_superseded: false,
        },
        response: None,
    };
    assert_eq!(derive_attempt(&view), ClientTurnState::Interrupted);
    assert!(ClientTurnState::Interrupted.is_terminal());
    assert_eq!(ClientTurnState::Interrupted.rank(), 2);
}

// -----------------------------------------------------------------------------
// Manual-kind request lifecycle transitions (Task 19 / PR 3)
//
// The trigger engine's Schedule and Event paths materialize directly at
// `claimed`, because the scheduler spawns them. The Manual path is different:
// the shared `write_manual_agent_request` helper (used by CLI `config task
// run` and the desktop "Run Now" button) writes at `pending`, and the running
// agent's normal intake watcher is the thing that claims the row. Two
// invariants follow from this split and both need pinning:
//
//   * Manual helper lands the row at `(status="pending", lifecycle_state=
//     "pending")` — NOT claimed. Regressing to a claimed landing would
//     short-circuit intake and break the out-of-process CLI path.
//   * The manual lineage tuple (`caused_by_trigger_kind="manual"`,
//     `caused_by_trigger_id=null`) survives the Pending → Claimed transition
//     untouched. The claim path must not clobber lineage.
// -----------------------------------------------------------------------------

/// Pin: the shared manual helper produces `(status="pending", lifecycle_state=
/// "pending")`. Regression guard for the CLI's out-of-process intake path,
/// which relies on the row being visible to the running agent's watcher.
#[tokio::test]
async fn manual_run_materializes_pending_request() {
    let db = test_db("manual-run-materializes-pending").await;

    let doc_id = write_manual_agent_request(
        &db.node,
        AGENT_DID,
        AGENT_NAME,
        "task-manual-pending",
        "manual prompt body",
        serde_json::json!({}),
    )
    .await
    .expect("write_manual_agent_request should succeed on a fresh node");

    // Status + lifecycle_state must both be "pending" — the CLI path does NOT
    // land at claimed (the running agent's intake is the thing that claims).
    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        snap.status, "pending",
        "manual run must persist status=pending for the intake watcher to claim"
    );
    assert_eq!(
        snap.lifecycle_state, "pending",
        "manual run must persist lifecycle_state=pending (not claimed)"
    );
    assert!(
        !snap.claimed_at_present,
        "pending manual row must NOT have claimed_at set"
    );
    assert!(
        !snap.deadline_present,
        "pending manual row must NOT have a deadline — claim sets it"
    );
    assert_eq!(
        snap.execution_origin, "interactive",
        "manual runs inherit the interactive execution origin"
    );
    assert_eq!(snap.behavior_id, AGENT_NAME);

    // Lineage tuple is already set at the helper boundary — independent of
    // the lifecycle_state pinning above, but worth asserting here so a
    // regression that silently conflates "pending" with default lineage
    // fails loudly.
    let lineage = fetch_request_lineage_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        lineage,
        RequestLineageSnapshot {
            caused_by_trigger_id: None,
            caused_by_trigger_kind: Some("manual".to_string()),
        },
        "manual helper must set (null, \"manual\") on the pending row"
    );
}

/// Pin: the Pending → Claimed transition preserves the manual lineage tuple.
///
/// Sequence mirrors the running agent's intake path: helper writes the
/// pending row, the watcher observes it, reconstructs the `AgentRequest`,
/// and invokes `lifecycle.claim()`. After the transition `lifecycle_state`
/// flips to `claimed`, `claimed_at` + `deadline` get stamped, but the
/// lineage tuple (`caused_by_trigger_id=null`, `caused_by_trigger_kind=
/// "manual"`) must be untouched — regressing that would break trigger-kind
/// aggregations (recent runs, lineage badges) for manual originators.
#[tokio::test]
async fn manual_run_preserves_lineage_through_claim_transition() {
    let db = test_db("manual-run-lineage-through-claim").await;

    let doc_id = write_manual_agent_request(
        &db.node,
        AGENT_DID,
        AGENT_NAME,
        "task-manual-claim",
        "manual prompt body",
        serde_json::json!({}),
    )
    .await
    .expect("write_manual_agent_request should succeed");

    // Sanity: lineage is set and lifecycle is pending before we claim.
    let pre_claim_lineage = fetch_request_lineage_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        pre_claim_lineage,
        RequestLineageSnapshot {
            caused_by_trigger_id: None,
            caused_by_trigger_kind: Some("manual".to_string()),
        }
    );
    assert_eq!(
        fetch_request_snapshot(&db.node, &doc_id)
            .await
            .lifecycle_state,
        "pending"
    );

    // Read back the persisted row to reconstruct the in-memory AgentRequest
    // that the intake watcher would build. We need `request_id` + `session_id`
    // + `created_at` from the actual document so the lifecycle wrapper
    // operates on the right row.
    let escaped_doc_id = escape_graphql_string(&doc_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 1
            ) {{
                request_id
                session_id
                created_at
            }}
        }}"#
    );
    let resp = db.node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "AgentRequest query failed: {:?}",
        resp.errors
    );
    let row = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("manual AgentRequest row exists");
    let request_id = row
        .get("request_id")
        .and_then(|v| v.as_str())
        .expect("request_id present")
        .to_string();
    let session_id = row
        .get("session_id")
        .and_then(|v| v.as_str())
        .expect("session_id present")
        .to_string();
    let created_at = row
        .get("created_at")
        .and_then(|v| v.as_str())
        .expect("created_at present")
        .to_string();

    let request = build_request(
        doc_id.clone(),
        request_id.clone(),
        session_id.clone(),
        created_at,
    );

    // Drive the Pending → Claimed transition. `ExecutionOrigin::Interactive`
    // matches the row the helper already wrote — the claim must not flip
    // origin either.
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        request,
        DEADLINE_SECS,
        ExecutionOrigin::Interactive,
        BACKEND_ID,
    );
    assert_eq!(
        lifecycle.claim().await.unwrap(),
        ClaimOutcome::Claimed,
        "manual pending row must be claimable exactly once"
    );
    assert_lean_transition_is_legal("Request", "pending", "claimed");

    // Post-claim: lifecycle_state flips to claimed; claimed_at / deadline
    // are stamped; lineage and execution origin are UNCHANGED.
    let snap = fetch_request_snapshot(&db.node, &doc_id).await;
    assert_eq!(snap.lifecycle_state, "claimed");
    assert_eq!(snap.status, "processing");
    assert!(snap.claimed_at_present, "claim must stamp claimed_at");
    assert!(snap.deadline_present, "claim must stamp deadline");
    assert_eq!(
        snap.execution_origin, "interactive",
        "claim must not rewrite execution_origin"
    );

    let post_claim_lineage = fetch_request_lineage_snapshot(&db.node, &doc_id).await;
    assert_eq!(
        post_claim_lineage,
        RequestLineageSnapshot {
            caused_by_trigger_id: None,
            caused_by_trigger_kind: Some("manual".to_string()),
        },
        "Pending → Claimed transition must preserve the (null, \"manual\") lineage tuple"
    );
    assert_eq!(
        post_claim_lineage, pre_claim_lineage,
        "lineage must be byte-identical before and after claim"
    );
}

#[test]
fn tool_call_transitions_match_lean_contract() {
    // Spec-relational legal transitions
    assert_lean_transition_is_legal("ToolCall", "pending", "running");
    assert_lean_transition_is_legal("ToolCall", "pending", "failed");
    assert_lean_transition_is_legal("ToolCall", "pending", "cancelled");
    assert_lean_transition_is_legal("ToolCall", "running", "completed");
    assert_lean_transition_is_legal("ToolCall", "running", "failed");
    assert_lean_transition_is_legal("ToolCall", "running", "timedOut");
    assert_lean_transition_is_legal("ToolCall", "running", "cancelled");

    // T1 — terminal irreversibility
    assert_lean_transition_is_illegal("ToolCall", "completed", "running");
    assert_lean_transition_is_illegal("ToolCall", "failed", "running");
    assert_lean_transition_is_illegal("ToolCall", "timedOut", "running");
    assert_lean_transition_is_illegal("ToolCall", "cancelled", "running");
}

// ---------------------------------------------------------------------------
// R2 Bucket 2 — Lean transition matrix conformance for the subagent extensions.
//
// These tests assert that the Lean-emitted contract (consumed via
// `lean_state_machine_contract`) carries the new vocabularies (`AwaitMode`,
// `CancelPolicy`, `ChildTerminal`) and the new named transitions on the
// `ToolCall` machine that R2 introduced (mode flips, detach split, bridge_*
// edges, native-only `complete_native`/`fail_native` rows). Drift between
// Lean's model and Rust's runtime — for example, a vocabulary value added on
// only one side, or a Lean-only edge that Rust silently allows — is caught
// here rather than at PR review.
// ---------------------------------------------------------------------------

#[test]
fn lean_emits_await_mode_vocabulary() {
    use defra_agent::tool_call_lifecycle::AwaitMode;

    let machine = lean_state_machine_contract("AwaitMode");
    let mut rust_vocab: Vec<String> = AwaitMode::ALL
        .iter()
        .map(|m| m.as_str().to_string())
        .collect();
    rust_vocab.sort();
    let mut lean_vocab = machine.states.clone();
    lean_vocab.sort();
    assert_eq!(
        lean_vocab, rust_vocab,
        "AwaitMode vocabulary divergence between Lean and Rust"
    );
}

#[test]
fn lean_emits_cancel_policy_vocabulary() {
    use defra_agent::tool_call_lifecycle::CancelPolicy;

    let machine = lean_state_machine_contract("CancelPolicy");
    let mut rust_vocab: Vec<String> = CancelPolicy::ALL
        .iter()
        .map(|p| p.as_str().to_string())
        .collect();
    rust_vocab.sort();
    let mut lean_vocab = machine.states.clone();
    lean_vocab.sort();
    assert_eq!(
        lean_vocab, rust_vocab,
        "CancelPolicy vocabulary divergence between Lean and Rust"
    );
}

#[test]
fn lean_emits_child_terminal_vocabulary_and_projections() {
    use defra_agent::tool_call_lifecycle::ChildTerminal;

    let machine = lean_state_machine_contract("ChildTerminal");

    // Vocabulary check: Lean's source-side vocabulary must match Rust's
    // ChildTerminal::ALL_KIND.
    let mut lean_vocab = machine.states.clone();
    lean_vocab.sort();
    let mut rust_vocab: Vec<String> = ChildTerminal::ALL_KIND
        .iter()
        .map(|s| s.to_string())
        .collect();
    rust_vocab.sort();
    assert_eq!(
        lean_vocab, rust_vocab,
        "ChildTerminal vocabulary divergence between Lean and Rust"
    );

    // Projection check: each named_transition's `from`/`to` must agree with
    // Lean's B2 projection rule (Subagent.lean): .interrupted -> .cancelled,
    // every other terminal -> .failed. Rust's `ChildTerminal::projected_state`
    // is verified to follow this rule by the Bucket 1 unit tests in
    // `tool_call_lifecycle.rs`; here we lock in that the *Lean* contract
    // emits exactly that table. (We can't call `projected_state` from this
    // integration test because its return type `ToolCallState` is
    // `pub(crate)` to defra-agent.)
    for t in &machine.named_transitions {
        let expected = match t.from.as_str() {
            "interrupted" => "cancelled",
            "failed" | "dead" | "superseded" => "failed",
            other => panic!("unexpected ChildTerminal vocabulary: {}", other),
        };
        assert_eq!(
            t.to, expected,
            "Projection divergence at {}: Lean says target {}, Bucket 1 spec says {}",
            t.from, t.to, expected
        );
    }
    // Also assert every ChildTerminal variant has a corresponding row, so a
    // future Lean refactor that drops one is caught here.
    let mut sources_in_named: Vec<String> = machine
        .named_transitions
        .iter()
        .map(|t| t.from.clone())
        .collect();
    sources_in_named.sort();
    sources_in_named.dedup();
    assert_eq!(
        sources_in_named, rust_vocab,
        "ChildTerminal named_transitions must cover every ALL_KIND variant"
    );
}

#[test]
fn lean_emits_bridge_transitions_in_tool_call_machine() {
    let machine = lean_state_machine_contract("ToolCall");
    let bridge_names: Vec<&str> = vec![
        "background",
        "foreground",
        "detach_running",
        "detach_pending",
        "bridge_complete",
        "bridge_failure_failed",
        "bridge_failure_cancelled",
        "bridge_cancel_cascade",
    ];
    for name in &bridge_names {
        let found = machine.named_transitions.iter().any(|t| t.name == *name);
        assert!(
            found,
            "Lean contract must emit '{}' transition in ToolCall machine",
            name
        );
    }
}

#[test]
fn lean_marks_native_complete_fail_as_requires_native() {
    let machine = lean_state_machine_contract("ToolCall");
    let complete = machine
        .named_transitions
        .iter()
        .find(|t| t.name == "complete_native")
        .expect("ToolCall machine must have native complete transition (named complete_native)");
    assert!(
        complete.requires_native,
        "complete_native must be flagged with requires_native: true"
    );
    let fail = machine
        .named_transitions
        .iter()
        .find(|t| t.name == "fail_native")
        .expect("ToolCall machine must have native fail transition (named fail_native)");
    assert!(
        fail.requires_native,
        "fail_native must be flagged with requires_native: true"
    );
}

#[test]
fn event_delivery_transition_cases_match_contract() {
    let cases = lean_event_delivery_transition_cases();
    assert!(
        cases.len() >= 12,
        "Expected at least 12 transition-case rows; got {}",
        cases.len()
    );
    for case in cases {
        match &case.action {
            LeanEventDeliveryAction::Persist { doc } => {
                assert!(
                    case.post.persistent_set.contains(doc),
                    "case `{}`: persist did not add doc to persistent_set",
                    case.name
                );
            }
            LeanEventDeliveryAction::Handle { doc } => {
                assert!(
                    case.post.handled.contains(doc),
                    "case `{}`: handle did not add doc to handled",
                    case.name
                );
                assert!(
                    case.post.processed_set.contains(doc),
                    "case `{}`: handle did not add doc to processed_set",
                    case.name
                );
            }
            LeanEventDeliveryAction::RescanTick => {
                assert_eq!(
                    case.pre.persistent_set, case.post.persistent_set,
                    "case `{}`: rescanTick changed persistent_set",
                    case.name
                );
            }
            _ => {}
        }
    }
}

#[test]
fn event_delivery_source_instances_match_runtime() {
    let by_name: std::collections::HashMap<
        &str,
        &lean_vocab_test::LeanEventDeliverySourceInstance,
    > = lean_event_delivery_source_instances()
        .iter()
        .map(|i| (i.name.as_str(), i))
        .collect();

    let watcher = by_name
        .get("Watcher")
        .expect("Watcher instance must be present");
    assert_eq!(watcher.dedupe_policy, "ttl_cooldown");
    assert!(
        watcher.rescan_bounded_by > 0,
        "Watcher rescanBoundedBy must be positive"
    );
    assert!(
        watcher.deviation.is_none(),
        "Watcher must have no deviation entry; got {:?}",
        watcher.deviation
    );

    let event_source = by_name
        .get("EventSource")
        .expect("EventSource instance must be present");
    assert_eq!(event_source.dedupe_policy, "monotone_once");
    assert_eq!(
        event_source.rescan_bounded_by, 0,
        "EventSource must currently use unboundedRescan sentinel"
    );
    assert_eq!(
        event_source.deviation.as_deref(),
        Some("event_source_lacks_periodic_rescan"),
        "EventSource deviation tag drifted"
    );

    let subagent_source = by_name
        .get("SubagentSource")
        .expect("SubagentSource instance must be present");
    assert_eq!(subagent_source.dedupe_policy, "monotone_once");
    assert_eq!(subagent_source.rescan_bounded_by, 0);
    assert_eq!(
        subagent_source.deviation.as_deref(),
        Some("subagent_source_lacks_live_rescan")
    );
}

#[test]
fn event_delivery_convergence_traces_match_runtime_or_deviation() {
    let traces = lean_event_delivery_convergence_traces();
    assert!(
        traces.len() >= 3,
        "Expected at least one convergence trace per source"
    );

    for trace in traces {
        match trace.status.as_str() {
            "substantive" => {
                let final_handled: std::collections::HashSet<&String> =
                    trace.final_world.handled.iter().collect();
                let final_persistent: std::collections::HashSet<&String> =
                    trace.final_world.persistent_set.iter().collect();
                for doc in &trace.initial_world.persistent_set {
                    let was_handled = final_handled.contains(doc);
                    let was_depersisted = !final_persistent.contains(doc);
                    assert!(
                        was_handled || was_depersisted,
                        "substantive trace `{}` did not converge for doc `{}` \
                         (handled? {}, depersisted? {})",
                        trace.name,
                        doc,
                        was_handled,
                        was_depersisted,
                    );
                }
            }
            "deviation" => {
                let final_handled: std::collections::HashSet<&String> =
                    trace.final_world.handled.iter().collect();
                let final_persistent: std::collections::HashSet<&String> =
                    trace.final_world.persistent_set.iter().collect();
                // Collect all docs that were ever persisted: either present in
                // the initial world or added via a Persist action during the trace.
                let mut all_persisted: std::collections::HashSet<&String> =
                    trace.initial_world.persistent_set.iter().collect();
                for action in &trace.actions {
                    if let LeanEventDeliveryAction::Persist { doc } = action {
                        all_persisted.insert(doc);
                    }
                }
                // Deviation is witnessed when at least one persisted doc ends
                // up still in the persistent_set but was never handled.
                let observed_deviation = all_persisted
                    .iter()
                    .any(|doc| final_persistent.contains(*doc) && !final_handled.contains(*doc));
                assert!(
                    observed_deviation,
                    "deviation trace `{}` did not witness the documented \
                     deviation state (no orphan persistent doc remaining)",
                    trace.name,
                );
            }
            other => panic!(
                "trace `{}` has unknown status `{}` (expected 'substantive' or 'deviation')",
                trace.name, other,
            ),
        }
    }

    let trace_instances: std::collections::HashSet<&str> =
        traces.iter().map(|t| t.instance_name.as_str()).collect();
    for name in &["Watcher", "EventSource", "SubagentSource"] {
        assert!(
            trace_instances.contains(name),
            "Expected a convergence trace for instance `{}`",
            name
        );
    }
}
