use super::*;

fn readiness_row(node_did: &str, snapshot_json: String) -> NodeReadinessRow {
    NodeReadinessRow {
        node_did: node_did.to_string(),
        snapshot_json,
        updated_at: "2026-08-28T00:00:00Z".to_string(),
    }
}

fn readiness_snapshot(agents: Vec<AgentReadinessEntry>) -> NodeReadinessSnapshot {
    NodeReadinessSnapshot {
        format_version: NODE_READINESS_FORMAT_VERSION,
        process_state: NodeReadinessProcessState::Ready,
        active_generation: 4,
        router_generation: 4,
        default_agent_did: "a".to_string(),
        agents,
    }
}

fn projected_unknown(row: &NodeReadinessRow) -> Option<AgentReadinessUnknownReason> {
    project_node_readiness(Some(row), "did:test:node", ["a", "b"], Some("a")).unknown_reason
}

#[test]
fn source_projection_is_sorted_and_unavailability_wins() {
    let snapshot = project_node_readiness_source(
        NodeReadinessProcessState::Ready,
        4,
        4,
        "a",
        [
            AgentReadinessSourceEntry {
                agent_did: "b".to_string(),
                dispatcher_present: true,
                unavailable_reason: Some(AgentReadinessUnavailableReason::BackendDisabled),
                startup_demoted: false,
            },
            AgentReadinessSourceEntry {
                agent_did: "a".to_string(),
                dispatcher_present: true,
                unavailable_reason: None,
                startup_demoted: false,
            },
        ],
    )
    .unwrap();
    assert_eq!(snapshot.agents[0].agent_did, "a");
    assert_eq!(snapshot.agents[1].state, AgentReadinessState::Unavailable);
}

#[test]
fn ready_snapshot_does_not_expire_without_semantic_changes() {
    let snapshot = readiness_snapshot(vec![AgentReadinessEntry {
        agent_did: "a".to_string(),
        state: AgentReadinessState::Ready,
        reason: None,
    }]);
    let row = readiness_row("did:test:node", serde_json::to_string(&snapshot).unwrap());
    let projection = project_node_readiness(Some(&row), "did:test:node", ["a"], Some("a"));
    assert_eq!(projection.unknown_reason, None);
    assert_eq!(
        projection.agents.get("a"),
        Some(&ProjectedAgentReadiness::Ready),
    );
    assert!(matches!(
        project_node_readiness_summary(Some(&row), "did:test:node"),
        ProjectedNodeReadinessSummary::Observed(_)
    ));
}

#[test]
fn projection_accepts_only_canonical_bound_payloads_and_process_states() {
    let ready = AgentReadinessEntry {
        agent_did: "a".to_string(),
        state: AgentReadinessState::Ready,
        reason: None,
    };
    let unavailable = AgentReadinessEntry {
        agent_did: "b".to_string(),
        state: AgentReadinessState::Unavailable,
        reason: Some(AgentReadinessUnavailableReason::BackendDisabled),
    };
    let canonical = readiness_row(
        "did:test:node",
        serde_json::to_string(&readiness_snapshot(vec![
            ready.clone(),
            unavailable.clone(),
        ]))
        .unwrap(),
    );
    assert_eq!(projected_unknown(&canonical), None);

    let malformed_snapshots = [
        readiness_snapshot(vec![ready.clone(), ready.clone()]),
        readiness_snapshot(vec![unavailable.clone(), ready.clone()]),
        readiness_snapshot(vec![AgentReadinessEntry {
            agent_did: " a".to_string(),
            ..ready.clone()
        }]),
        readiness_snapshot(vec![AgentReadinessEntry {
            reason: Some(AgentReadinessUnavailableReason::BackendDisabled),
            ..ready.clone()
        }]),
        readiness_snapshot(vec![AgentReadinessEntry {
            agent_did: "a".to_string(),
            state: AgentReadinessState::Unavailable,
            reason: None,
        }]),
    ];
    for snapshot in malformed_snapshots {
        let row = readiness_row("did:test:node", serde_json::to_string(&snapshot).unwrap());
        assert_eq!(
            projected_unknown(&row),
            Some(AgentReadinessUnknownReason::ReadinessMalformed)
        );
    }

    let unknown_process = readiness_row(
        "did:test:node",
        canonical.snapshot_json.replace("\"ready\"", "\"starting\""),
    );
    assert_eq!(
        projected_unknown(&unknown_process),
        Some(AgentReadinessUnknownReason::ReadinessMalformed)
    );
    assert_eq!(
        project_node_readiness(Some(&canonical), "did:test:node", [" a"], Some("a")).unknown_reason,
        Some(AgentReadinessUnknownReason::ReadinessMalformed)
    );

    let mut whitespace_default = readiness_snapshot(vec![ready.clone()]);
    whitespace_default.default_agent_did = "a ".to_string();
    let row = readiness_row(
        "did:test:node",
        serde_json::to_string(&whitespace_default).unwrap(),
    );
    assert_eq!(
        projected_unknown(&row),
        Some(AgentReadinessUnknownReason::ReadinessMalformed)
    );

    let mut invalid_default = readiness_snapshot(vec![ready]);
    invalid_default.default_agent_did = "missing".to_string();
    let row = readiness_row(
        "did:test:node",
        serde_json::to_string(&invalid_default).unwrap(),
    );
    assert_eq!(
        projected_unknown(&row),
        Some(AgentReadinessUnknownReason::AgentNotAssigned)
    );

    let foreign = readiness_row("did:test:foreign", canonical.snapshot_json.clone());
    assert_eq!(
        projected_unknown(&foreign),
        Some(AgentReadinessUnknownReason::ReadinessMalformed)
    );

    let with_unknown_field = readiness_row(
        "did:test:node",
        r#"{"format_version":2,"process_state":"ready","active_generation":4,"router_generation":4,"default_agent_did":"a","agents":[{"agent_did":"a","state":"ready","reason":null,"extra":true}]}"#.to_string(),
    );
    assert_eq!(
        projected_unknown(&with_unknown_field),
        Some(AgentReadinessUnknownReason::ReadinessMalformed)
    );
    let with_unknown_top_level_field = readiness_row(
        "did:test:node",
        r#"{"format_version":2,"process_state":"ready","active_generation":4,"router_generation":4,"default_agent_did":"a","agents":[{"agent_did":"a","state":"ready","reason":null}],"extra":true}"#.to_string(),
    );
    assert_eq!(
        project_node_readiness_summary(Some(&with_unknown_top_level_field), "did:test:node"),
        ProjectedNodeReadinessSummary::Unknown(AgentReadinessUnknownReason::ReadinessMalformed)
    );
    assert!(matches!(
        project_node_readiness_summary(Some(&canonical), "did:test:node"),
        ProjectedNodeReadinessSummary::Observed(NodeReadinessSummary {
            ready_count: 1,
            unavailable_agents: ref unavailable,
            ..
        }) if unavailable.len() == 1
    ));
}

#[test]
fn malformed_configured_id_preserves_all_canonical_ids_independent_of_order() {
    for configured in [vec!["bad ", "a", "b"], vec!["a", "bad ", "b"]] {
        let projection = project_node_readiness(None, "did:test:node", configured, Some("default"));
        assert_eq!(
            projection.unknown_reason,
            Some(AgentReadinessUnknownReason::ReadinessMalformed)
        );
        assert_eq!(
            projection.agents.keys().cloned().collect::<Vec<_>>(),
            vec!["a".to_string(), "b".to_string(), "default".to_string()]
        );
    }
}

#[test]
fn agent_unavailable_rejections_are_exactly_routing_messages() {
    use AgentReadinessUnavailableReason as Reason;
    let ordinal = |reason: Reason| match reason {
        Reason::AgentDisabled => 0,
        Reason::RuntimeConfigurationInvalid => 1,
        Reason::BackendNotConfigured => 2,
        Reason::BackendDisabled => 3,
        Reason::BackendTemporarilyUnavailable => 4,
        Reason::CredentialsRequired => 5,
        Reason::SubscriptionNotEntitled => 6,
        Reason::InferenceProfileInvalid => 7,
        Reason::ToolConfigurationInvalid => 8,
        Reason::ToolSurfaceUnavailable => 9,
        Reason::ExecutorStartFailed => 10,
    };
    assert_eq!(Reason::ALL.map(ordinal), [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    for reason in Reason::ALL {
        assert!(is_behavior_unavailable_rejection(reason.public_message()));
    }
    assert!(is_behavior_unavailable_rejection(
        BEHAVIOR_NOT_ASSIGNED_MESSAGE
    ));
    for other in [
        "",
        "request must select a behavior",
        "request admission denied: invalid signature",
        "runtime configuration is invalid: missing backend",
        "Runtime configuration is invalid",
    ] {
        assert!(!is_behavior_unavailable_rejection(other), "{other:?}");
    }
}

#[test]
fn unsupported_version_is_classified_before_current_payload_shape() {
    let row = readiness_row(
        "did:test:node",
        r#"{"format_version":1,"process_state":"ready","active_generation":4,"router_generation":4,"default_behavior_id":"a","behaviors":[{"behavior_id":"a","state":"ready","reason":null}]}"#.to_string(),
    );
    assert_eq!(
        decode_node_readiness_snapshot(&row, "did:test:node"),
        Err(AgentReadinessUnknownReason::ReadinessVersionUnsupported),
    );
    let current = readiness_row(
        "did:test:node",
        format!(r#"{{"format_version":{NODE_READINESS_FORMAT_VERSION}}}"#),
    );
    assert_eq!(
        decode_node_readiness_snapshot(&current, "did:test:node"),
        Err(AgentReadinessUnknownReason::ReadinessMalformed),
    );
    assert_eq!(
        decode_node_readiness_snapshot(&row, "did:test:other"),
        Err(AgentReadinessUnknownReason::ReadinessMalformed),
    );
}

fn readiness_reason(code: &str) -> crate::row::AgentReadinessUnavailableReason {
    use crate::row::AgentReadinessUnavailableReason as Reason;
    match code {
        "agent_disabled" => Reason::AgentDisabled,
        "runtime_configuration_invalid" => Reason::RuntimeConfigurationInvalid,
        "backend_not_configured" => Reason::BackendNotConfigured,
        "backend_disabled" => Reason::BackendDisabled,
        "backend_temporarily_unavailable" => Reason::BackendTemporarilyUnavailable,
        "credentials_required" => Reason::CredentialsRequired,
        "subscription_not_entitled" => Reason::SubscriptionNotEntitled,
        "inference_profile_invalid" => Reason::InferenceProfileInvalid,
        "tool_configuration_invalid" => Reason::ToolConfigurationInvalid,
        "tool_surface_unavailable" => Reason::ToolSurfaceUnavailable,
        other => panic!("unknown Lean readiness reason {other}"),
    }
}

fn readiness_reason_code<T>(reason: T) -> String
where
    T: serde::Serialize,
{
    serde_json::to_value(reason)
        .expect("serialize readiness reason")
        .as_str()
        .expect("readiness reason serializes as string")
        .to_string()
}

#[test]
fn generated_agent_readiness_cases_drive_the_production_projector() {
    use crate::node_readiness::{
        project_node_readiness_source, project_node_readiness_with_storage,
        AgentReadinessSourceEntry, NodeReadinessProcessState, NodeReadinessRow,
        ProjectedAgentReadiness, RuntimeStorageObservation, NODE_READINESS_FORMAT_VERSION,
    };

    let cases = gents_lean_vocab::lean_client_agent_readiness_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let runtime_reason = readiness_reason(&case.runtime_unavailable_reason);
        let process_state = serde_json::from_value::<NodeReadinessProcessState>(
            serde_json::Value::String(case.process_state.clone()),
        )
        .unwrap_or_else(|error| panic!("{} invalid generated process state: {error}", case.name));
        let selected_assigned = case.runnable || case.unavailable || case.startup_demoted;
        let default_agent_did = if selected_assigned { "20" } else { "10" };
        let sources = vec![AgentReadinessSourceEntry {
            agent_did: default_agent_did.to_string(),
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
        let mut snapshot = project_node_readiness_source(
            process_state,
            case.active_generation,
            case.router_generation,
            default_agent_did,
            sources,
        )
        .unwrap_or_else(|error| panic!("{} invalid generated source: {error}", case.name));
        if case.observation_kind == "unsupported_version" {
            snapshot.format_version = NODE_READINESS_FORMAT_VERSION + 1;
        }
        let row = NodeReadinessRow {
            node_did: "did:test:lean-readiness".to_string(),
            snapshot_json: if case.observation_kind == "malformed" {
                "not-json".to_string()
            } else {
                serde_json::to_string(&snapshot).unwrap()
            },
            updated_at: "2026-08-28T00:00:00Z".to_string(),
        };
        let storage = RuntimeStorageObservation::from_unavailable(case.storage_unavailable);
        let projection = project_node_readiness_with_storage(
            case.observation_present.then_some(&row),
            "did:test:lean-readiness",
            ["20"],
            Some("20"),
            storage,
        );
        let projected = projection
            .agents
            .get("20")
            .unwrap_or_else(|| panic!("{} missing selected agent", case.name));
        for timestamp in ["1970-01-01T00:00:00Z", "2999-01-01T00:00:00Z"] {
            let mut aged_row = row.clone();
            aged_row.updated_at = timestamp.to_string();
            let aged = project_node_readiness_with_storage(
                case.observation_present.then_some(&aged_row),
                "did:test:lean-readiness",
                ["20"],
                Some("20"),
                storage,
            );
            assert_eq!(aged.agents, projection.agents, "{}: {timestamp}", case.name);
            assert_eq!(
                aged.unknown_reason, projection.unknown_reason,
                "{}: {timestamp}",
                case.name
            );
        }
        let (state, reason) = match projected {
            ProjectedAgentReadiness::Ready => ("ready", None),
            ProjectedAgentReadiness::Unavailable(reason) => {
                ("unavailable", Some(readiness_reason_code(*reason)))
            }
            ProjectedAgentReadiness::Unknown(reason) => {
                ("unknown", Some(readiness_reason_code(*reason)))
            }
        };
        assert_eq!(state, case.expected_state, "{} state", case.name);
        assert_eq!(reason, case.expected_reason, "{} reason", case.name);
        if case.observation_kind == "observed" && !case.storage_unavailable {
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
