use super::*;

pub(super) fn lean_executable_contracts_cover_initial_domains() {
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
    assert_eq!(lean_recovery_sweep_cases().len(), 55);
    assert_eq!(lean_transcript_cases().len(), 11);
    assert_eq!(lean_composed_invariant_witnesses().len(), 4);
}

#[tokio::test]
async fn agent_tool_call_carries_only_generic_background_fields() {
    let db = crate::support::test_db("agent-tool-call-background-fields").await;
    let response = db
        .node
        .execute(
            r#"{
                __type(name: "AgentToolCall") {
                    fields { name }
                }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "introspection errors: {:?}",
        response.errors
    );
    let names: RapidHashSet<String> = response
        .data
        .as_ref()
        .and_then(|d| d.get("__type"))
        .and_then(|t| t.get("fields"))
        .and_then(|fs| fs.as_array())
        .map(|fs| {
            fs.iter()
                .filter_map(|f| f.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    for field in [
        "await_mode",
        "cancel_cause",
        "stuck_since",
        "spawned_by_tool_call_doc_id",
        "completion_notification_delivered_at",
    ] {
        assert!(names.contains(field), "AgentToolCall missing field {field}");
    }
    for field in [
        "child_request_id",
        "spawn_target_did",
        "spawn_agent_did",
        "delegated_input",
        "delegated_workspace",
        "cancel_policy",
        "cancel_cascade_intent_at",
        "cancel_pending_remote_ack",
        "unclaimed_deadline_at",
    ] {
        assert!(
            !names.contains(field),
            "AgentToolCall retains field {field}"
        );
    }
}

#[test]
fn lean_boundary_metadata_is_typed_and_reviewable() {
    let snapshot = lean_contract_snapshot();
    let expected_boundary_ids = [
        "boundary.request.dead-preclaim-only",
        "boundary.request.recovery-sweep-reachable",
        "boundary.tool-call.permanent-without-retry-evidence",
        "boundary.mcp.call-tool-dispatch-retry-evidence",
        "boundary.inference-slots.running-row-derived",
        "boundary.fleet-slot-accounting.derived-view",
        "boundary.command-policy.host-execution-assumptions",
        "boundary.compaction.unique-call-ids-checked",
        "boundary.trigger.dispatch-source-delivery",
        "boundary.persistence.abstract-lifecycle",
        "boundary.storage.hook-failure-policy",
        "boundary.storage.observation-daemon-visible",
        "boundary.storage.minimum-visibility-path",
        "boundary.backend-health.admission-freshness",
        "boundary.session-recovery.client-retry-surface",
        "boundary.coverage-ledger.review-discipline",
        "boundary.event-delivery.fair-substrate",
        "boundary.prompt-assembly.provider-input-sanitization",
        "boundary.model.nat-typed-ids-time",
        "boundary.p2p-backpressure.obligation-model",
        "boundary.rendered-capture.assembled-request-artifact",
        "boundary.rendered-capture.key-encoding-injectivity",
        "boundary.plugin-job.guest-determinism",
        "boundary.plugin-job.single-step-driver",
        "boundary.plugin-data.transaction",
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
                .is_none_or(|text| !text.trim().is_empty()),
            "boundary accepted_failure_mode must be omitted or non-empty: {:?}",
            boundary
        );
        assert!(
            boundary
                .accepted_follow_up
                .as_deref()
                .is_none_or(|text| !text.trim().is_empty()),
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

const REQUIRE_FEATURE_TAG_FOR_ALL_ROWS: bool = true;

#[test]
fn lean_feature_matrix_covers_every_declared_required_surface() {
    let snapshot = lean_contract_snapshot();
    let valid_features: BTreeSet<&str> = snapshot
        .feature_surface_requirements
        .iter()
        .map(|req| req.feature.as_str())
        .collect();

    for entry in &snapshot.coverage_ledger {
        if REQUIRE_FEATURE_TAG_FOR_ALL_ROWS {
            assert!(
                !entry.feature.is_empty(),
                "coverage ledger row is untagged: {:?}",
                entry
            );
        }
        if !entry.feature.is_empty() {
            assert!(
                valid_features.contains(entry.feature.as_str()),
                "coverage ledger row tags unknown feature: {:?}",
                entry
            );
            if entry.category != "follow_up_hook" {
                assert!(
                    !entry.surfaces.is_empty(),
                    "coverage ledger row carries feature {:?} but no surfaces; \
                     each tagged non-follow-up hook row must declare at least one surface: {:?}",
                    entry.feature,
                    entry
                );
            }
        }
    }

    for req in &snapshot.feature_surface_requirements {
        for surface in &req.required {
            let covered = snapshot.coverage_ledger.iter().any(|entry| {
                entry.feature == req.feature
                    && entry.surfaces.iter().any(|candidate| candidate == surface)
            });
            assert!(
                covered,
                "feature {:?} declares required surface {:?} but no \
                 ledger row tags this (feature, surface). Either add a \
                 ledger row, or move this surface to `deferred` with a \
                 follow-up note.",
                req.feature, surface
            );
        }
    }

    for req in &snapshot.feature_surface_requirements {
        for surface in &req.required {
            let cell = snapshot
                .feature_matrix
                .get(&req.feature)
                .and_then(|surfaces| surfaces.get(surface.as_str()));
            let strength = cell
                .map(|cell| cell.coverage_strength.as_str())
                .unwrap_or("missing");
            assert!(
                strength != "missing",
                "feature_matrix[{}][{:?}] is `missing` but required",
                req.feature,
                surface
            );
        }
    }
}
use gents_lean_vocab::lean_composed_invariant_witnesses;
