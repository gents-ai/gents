use super::*;

#[test]
fn generated_client_shell_cases_cover_shell_projection_contracts() {
    let ephemeral = lean_client_shell_case("new_conversation_is_ephemeral");
    assert_eq!(ephemeral.post_selection_session, None);
    assert_eq!(ephemeral.post_workflow_kind.as_str(), "idle");

    let submitted = lean_client_shell_case("submitted_request_selects_session");
    assert_eq!(submitted.input.as_str(), "mutation.submitted");
    assert_eq!(submitted.post_selection_session, Some(1));
    assert_eq!(submitted.post_workflow_kind.as_str(), "awaiting");

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

    let replay = lean_runtime_reconcile_case("replayed_request_is_not_accepted_twice");
    assert!(!replay.legal);
    assert_eq!(replay.action.as_str(), "acceptRequest");

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

    let finish = lean_runtime_reconcile_case("finish_request_releases_generation");
    assert!(finish.legal);
    assert_eq!(finish.action.as_str(), "finishRequest");
    assert_eq!(finish.pre_in_flight_count, finish.post_in_flight_count + 1);
    assert_eq!(finish.tracked_request_id, 500);
    assert_eq!(finish.pre_active_generation, finish.post_active_generation);

    let apply_failed = lean_runtime_reconcile_case("apply_failed_clears_pending");
    assert!(apply_failed.legal);
    assert_eq!(apply_failed.action.as_str(), "applyFailed");
    assert_eq!(apply_failed.pre_phase.as_str(), "applying");
    assert_eq!(apply_failed.post_phase.as_str(), "idle");
    assert_eq!(
        apply_failed.pre_active_generation,
        apply_failed.post_active_generation
    );

    let missing_dependency =
        lean_runtime_reconcile_case("missing_dependency_snapshot_is_not_resolved");
    assert!(!missing_dependency.legal);
    assert_eq!(missing_dependency.action.as_str(), "resolveVisible");

    let covered = [
        "publish_changed_snapshot",
        "router_observe_published_generation",
        "accept_request_after_router_observe",
        "replayed_request_is_not_accepted_twice",
        "retire_unobserved_generation",
        "finish_request_releases_generation",
        "apply_failed_clears_pending",
        "missing_dependency_snapshot_is_not_resolved",
    ]
    .into_iter()
    .collect::<std::collections::BTreeSet<_>>();
    let emitted = lean_runtime_reconcile_cases()
        .iter()
        .map(|case| case.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        emitted, covered,
        "runtime-reconcile case set drifted from this consumer's coverage"
    );
}

fn readiness_reason(code: &str) -> gents_protocol::row::BehaviorReadinessUnavailableReason {
    use gents_protocol::row::BehaviorReadinessUnavailableReason as Reason;
    match code {
        "behavior_disabled" => Reason::BehaviorDisabled,
        "runtime_configuration_invalid" => Reason::RuntimeConfigurationInvalid,
        "backend_not_configured" => Reason::BackendNotConfigured,
        "backend_disabled" => Reason::BackendDisabled,
        "backend_temporarily_unavailable" => Reason::BackendTemporarilyUnavailable,
        "credentials_required" => Reason::CredentialsRequired,
        "inference_profile_invalid" => Reason::InferenceProfileInvalid,
        "tool_configuration_invalid" => Reason::ToolConfigurationInvalid,
        "tool_surface_unavailable" => Reason::ToolSurfaceUnavailable,
        other => panic!("unknown Lean readiness reason {other}"),
    }
}

fn readiness_reason_code<T: serde::Serialize>(reason: T) -> String {
    serde_json::to_value(reason)
        .expect("serialize readiness reason")
        .as_str()
        .expect("readiness reason serializes as string")
        .to_string()
}

#[test]
fn generated_behavior_readiness_cases_drive_the_production_projector() {
    use gents_protocol::row::{
        project_behavior_readiness, project_behavior_readiness_source, AgentBehaviorReadinessRow,
        BehaviorReadinessProcessState, BehaviorReadinessSourceEntry, ProjectedBehaviorReadiness,
        BEHAVIOR_READINESS_FORMAT_VERSION,
    };

    let cases = lean_client_behavior_readiness_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let runtime_reason = readiness_reason(&case.runtime_unavailable_reason);
        let process_state = serde_json::from_value::<BehaviorReadinessProcessState>(
            serde_json::Value::String(case.process_state.clone()),
        )
        .unwrap_or_else(|error| panic!("{} invalid generated process state: {error}", case.name));
        let selected_assigned = case.runnable || case.unavailable || case.startup_demoted;
        let default_behavior_id = if selected_assigned { "20" } else { "10" };
        let mut sources = vec![BehaviorReadinessSourceEntry {
            behavior_id: default_behavior_id.to_string(),
            dispatcher_present: if selected_assigned {
                case.runnable
            } else {
                true
            },
            unavailable_reason: if selected_assigned && case.unavailable {
                Some(runtime_reason)
            } else {
                None
            },
            startup_demoted: selected_assigned && case.startup_demoted,
        }];
        if selected_assigned && default_behavior_id != "20" {
            sources.push(BehaviorReadinessSourceEntry {
                behavior_id: "20".to_string(),
                dispatcher_present: case.runnable,
                unavailable_reason: case.unavailable.then_some(runtime_reason),
                startup_demoted: case.startup_demoted,
            });
        }
        let mut snapshot = project_behavior_readiness_source(
            process_state,
            case.active_generation,
            case.router_generation,
            default_behavior_id,
            sources,
        )
        .unwrap_or_else(|error| panic!("{} invalid generated source: {error}", case.name));
        if case.observation_kind == "unsupported_version" {
            snapshot.format_version = BEHAVIOR_READINESS_FORMAT_VERSION + 1;
        }
        let row = AgentBehaviorReadinessRow {
            agent_did: "did:test:lean-readiness".to_string(),
            snapshot_json: if case.observation_kind == "malformed" {
                "not-json".to_string()
            } else {
                serde_json::to_string(&snapshot).unwrap()
            },
            updated_at: "2026-08-28T00:00:00Z".to_string(),
        };
        let projection = project_behavior_readiness(
            case.observation_present.then_some(&row),
            "did:test:lean-readiness",
            ["20"],
            Some("20"),
            chrono::DateTime::parse_from_rfc3339("2026-08-28T00:00:10Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let projected = projection
            .behaviors
            .get("20")
            .unwrap_or_else(|| panic!("{} missing selected behavior", case.name));
        let (state, reason) = match projected {
            ProjectedBehaviorReadiness::Ready => ("ready", None),
            ProjectedBehaviorReadiness::Unavailable(reason) => {
                ("unavailable", Some(readiness_reason_code(*reason)))
            }
            ProjectedBehaviorReadiness::Unknown(reason) => {
                ("unknown", Some(readiness_reason_code(*reason)))
            }
        };
        assert_eq!(state, case.expected_state, "{} state", case.name);
        assert_eq!(reason, case.expected_reason, "{} reason", case.name);
        if case.observation_kind == "observed" {
            assert_eq!(
                state == "ready",
                case.expected_runtime_admissible,
                "{} canonical observation/runtime admission parity",
                case.name
            );
        } else {
            assert_eq!(
                state, "unknown",
                "{} transport failure closes client",
                case.name
            );
        }
    }
}
