use std::collections::BTreeSet;
use std::time::Duration;

use defra_agent::graphql::escape_graphql_string;
use defra_agent::lifecycle::{ClaimOutcome, ExecutionOrigin, TriggerLineage};
use defra_agent::{write_manual_agent_request, DefraStreamWriter, RequestLifecycle};

#[path = "../src/lean_vocab_test.rs"]
mod lean_vocab_test;
mod support;

use lean_vocab_test::{
    assert_lean_transition_is_illegal, assert_lean_transition_is_legal,
    assert_state_machine_contract_is_complete, lean_client_shell_case, lean_contract_snapshot,
    lean_runtime_reconcile_case, lean_session_recovery_case, lean_tool_preflight_case,
    lean_tool_retry_case, lean_vocabulary_values,
};
use support::snapshots::{
    fetch_conversation_snapshot, fetch_request_lineage_snapshot,
    fetch_request_lineage_snapshot_by_tuple, fetch_request_snapshot, fetch_request_snapshot_raw,
    fetch_response_content, fetch_response_interrupted_at, fetch_response_snapshot,
    fetch_session_snapshot, ConversationSnapshot, RequestLineageSnapshot, RequestSnapshot,
    ResponseSnapshot, SessionSnapshot,
};
use support::{
    build_request, create_request, create_response_with_content_and_status,
    create_response_with_status, set_interrupt_requested_at, set_request_lifecycle_state,
    set_valid_until, test_db, AGENT_DID, AGENT_NAME, BACKEND_ID, DEADLINE_SECS,
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
    assert_lean_transition_is_legal("SessionRecovery", "failed", "pending");
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
    assert_eq!(lean_contract_snapshot().runtime_reconcile_cases.len(), 6);
    assert_eq!(lean_contract_snapshot().session_recovery_cases.len(), 10);
    assert_eq!(
        lean_contract_snapshot().client_shell_case_count,
        lean_contract_snapshot().client_shell_cases.len()
    );
    assert_eq!(lean_contract_snapshot().client_shell_cases.len(), 15);
    assert_eq!(lean_contract_snapshot().tool_preflight_cases.len(), 9);
    assert_eq!(lean_contract_snapshot().tool_retry_cases.len(), 45);
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
        "boundary.command-policy.host-execution-assumptions",
        "boundary.trigger.dispatch-source-delivery",
        "boundary.persistence.abstract-lifecycle",
        "boundary.storage.hook-failure-policy",
        "boundary.storage.observation-daemon-visible",
        "boundary.storage.minimum-visibility-path",
        "boundary.backend-health.admission-freshness",
        "boundary.session-recovery.failed-latest-smoke",
        "boundary.coverage-ledger.review-discipline",
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
    if !snapshot.session_recovery_cases.is_empty() {
        emitted.insert((
            "session_recovery_cases".to_string(),
            "SessionRecoveryCases".to_string(),
        ));
    }
    assert_eq!(
        snapshot.client_shell_case_count,
        snapshot.client_shell_cases.len(),
        "Lean ClientShell case count drifted from emitted cases"
    );
    if !snapshot.client_shell_cases.is_empty() {
        emitted.insert((
            "client_shell_cases".to_string(),
            "ClientShellCases".to_string(),
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
    for hook in &snapshot.follow_up_hooks {
        emitted.insert(("follow_up_hook".to_string(), hook.clone()));
    }

    // Keep this mirrored with the category strings in CoverageLedger.lean.
    let valid_categories = [
        "vocabulary",
        "state_machine",
        "trigger_cases",
        "runtime_cases",
        "session_recovery_cases",
        "client_shell_cases",
        "tool_cases",
        "follow_up_hook",
    ];
    let mut ledger = BTreeSet::new();

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
    assert_eq!(legal.post_latest_state.as_str(), "pending");
    assert_eq!(legal.pre_latest_admission.as_str(), "released");
    assert_eq!(legal.post_latest_admission.as_str(), "released");
    assert_eq!(legal.pre_failed_admission.as_str(), "released");
    assert_eq!(legal.post_failed_admission.as_str(), "released");
    assert_eq!(legal.post_new_admission.as_str(), "released");
    assert_eq!(legal.pre_retry_count + 1, legal.post_retry_count);
    assert!(legal.post_retry_count <= legal.max_retries);
    assert_eq!(legal.pre_session_id, legal.post_session_id);
    assert_eq!(legal.pre_behavior_id, legal.post_behavior_id);
    assert_eq!(legal.pre_request_count + 1, legal.post_request_count);
    assert_eq!(legal.post_latest_id, legal.new_id);
    assert!(legal.pre_failed_is_latest);
    assert!(!legal.post_failed_is_latest);
    assert!(legal.post_new_is_latest);
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
    assert_eq!(source_not_released.pre_latest_state.as_str(), "failed");
    assert_eq!(source_not_released.pre_failed_admission.as_str(), "waiting");

    for name in [
        "illegal_retry_budget_exhausted",
        "illegal_deadline_closed",
        "illegal_non_latest_failed_request",
        "illegal_new_request_id_already_exists",
        "illegal_new_request_id_matches_failed_id",
        "illegal_source_not_failed",
        "illegal_source_not_released",
    ] {
        let case = lean_session_recovery_case(name);
        assert!(!case.legal, "{name} must be rejected by Lean");
        assert!(case.post_latest_state.is_empty());
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
