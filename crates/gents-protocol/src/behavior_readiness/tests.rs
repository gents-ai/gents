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
        format_version: BEHAVIOR_READINESS_FORMAT_VERSION,
        process_state: NodeReadinessProcessState::Ready,
        active_generation: 4,
        router_generation: 4,
        default_agent_id: "a".to_string(),
        agents,
    }
}

fn observed_at() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-08-28T00:00:10Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn projected_unknown(row: &NodeReadinessRow) -> Option<AgentReadinessUnknownReason> {
    project_behavior_readiness(
        Some(row),
        "did:test:agent",
        ["a", "b"],
        Some("a"),
        observed_at(),
    )
    .unknown_reason
}

#[test]
fn source_projection_is_sorted_and_unavailability_wins() {
    let snapshot = project_behavior_readiness_source(
        NodeReadinessProcessState::Ready,
        4,
        4,
        "a",
        [
            AgentReadinessSourceEntry {
                agent_id: "b".to_string(),
                dispatcher_present: true,
                unavailable_reason: Some(AgentReadinessUnavailableReason::BackendDisabled),
                startup_demoted: false,
            },
            AgentReadinessSourceEntry {
                agent_id: "a".to_string(),
                dispatcher_present: true,
                unavailable_reason: None,
                startup_demoted: false,
            },
        ],
    )
    .unwrap();
    assert_eq!(snapshot.agents[0].agent_id, "a");
    assert_eq!(
        snapshot.agents[1].state,
        AgentReadinessState::Unavailable
    );
}

#[test]
fn ready_snapshot_does_not_expire_without_semantic_changes() {
    let snapshot = readiness_snapshot(vec![AgentReadinessEntry {
        agent_id: "a".to_string(),
        state: AgentReadinessState::Ready,
        reason: None,
    }]);
    let row = readiness_row("did:test:agent", serde_json::to_string(&snapshot).unwrap());
    let stale_at = DateTime::parse_from_rfc3339("2027-08-28T00:00:46Z")
        .unwrap()
        .with_timezone(&Utc);
    let projection =
        project_behavior_readiness(Some(&row), "did:test:agent", ["a"], Some("a"), stale_at);
    assert_eq!(projection.unknown_reason, None);
    assert_eq!(
        projection.agents.get("a"),
        Some(&ProjectedAgentReadiness::Ready),
        "a lagged replica must keep last-known dispatcher readiness"
    );
    assert!(matches!(
        project_behavior_readiness_summary(Some(&row), "did:test:agent", stale_at),
        ProjectedNodeReadinessSummary::Observed(_)
    ));
}

#[test]
fn projection_accepts_only_canonical_bound_payloads_and_process_states() {
    let ready = AgentReadinessEntry {
        agent_id: "a".to_string(),
        state: AgentReadinessState::Ready,
        reason: None,
    };
    let unavailable = AgentReadinessEntry {
        agent_id: "b".to_string(),
        state: AgentReadinessState::Unavailable,
        reason: Some(AgentReadinessUnavailableReason::BackendDisabled),
    };
    let canonical = readiness_row(
        "did:test:agent",
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
            agent_id: " a".to_string(),
            ..ready.clone()
        }]),
        readiness_snapshot(vec![AgentReadinessEntry {
            reason: Some(AgentReadinessUnavailableReason::BackendDisabled),
            ..ready.clone()
        }]),
        readiness_snapshot(vec![AgentReadinessEntry {
            agent_id: "a".to_string(),
            state: AgentReadinessState::Unavailable,
            reason: None,
        }]),
    ];
    for snapshot in malformed_snapshots {
        let row = readiness_row("did:test:agent", serde_json::to_string(&snapshot).unwrap());
        assert_eq!(
            projected_unknown(&row),
            Some(AgentReadinessUnknownReason::ReadinessMalformed)
        );
    }

    let unknown_process = readiness_row(
        "did:test:agent",
        canonical.snapshot_json.replace("\"ready\"", "\"starting\""),
    );
    assert_eq!(
        projected_unknown(&unknown_process),
        Some(AgentReadinessUnknownReason::ReadinessMalformed)
    );
    assert_eq!(
        project_behavior_readiness(
            Some(&canonical),
            "did:test:agent",
            [" a"],
            Some("a"),
            observed_at(),
        )
        .unknown_reason,
        Some(AgentReadinessUnknownReason::ReadinessMalformed)
    );

    let mut whitespace_default = readiness_snapshot(vec![ready.clone()]);
    whitespace_default.default_agent_id = "a ".to_string();
    let row = readiness_row(
        "did:test:agent",
        serde_json::to_string(&whitespace_default).unwrap(),
    );
    assert_eq!(
        projected_unknown(&row),
        Some(AgentReadinessUnknownReason::ReadinessMalformed)
    );

    let mut invalid_default = readiness_snapshot(vec![ready]);
    invalid_default.default_agent_id = "missing".to_string();
    let row = readiness_row(
        "did:test:agent",
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
        "did:test:agent",
        r#"{"format_version":1,"process_state":"ready","active_generation":4,"router_generation":4,"default_behavior_id":"a","behaviors":[{"behavior_id":"a","state":"ready","reason":null,"extra":true}]}"#.to_string(),
    );
    assert_eq!(
        projected_unknown(&with_unknown_field),
        Some(AgentReadinessUnknownReason::ReadinessMalformed)
    );
    let with_unknown_top_level_field = readiness_row(
        "did:test:agent",
        r#"{"format_version":1,"process_state":"ready","active_generation":4,"router_generation":4,"default_behavior_id":"a","behaviors":[{"behavior_id":"a","state":"ready","reason":null}],"extra":true}"#.to_string(),
    );
    assert_eq!(
        project_behavior_readiness_summary(
            Some(&with_unknown_top_level_field),
            "did:test:agent",
            observed_at(),
        ),
        ProjectedNodeReadinessSummary::Unknown(
            AgentReadinessUnknownReason::ReadinessMalformed
        )
    );
    assert!(matches!(
        project_behavior_readiness_summary(Some(&canonical), "did:test:agent", observed_at()),
        ProjectedNodeReadinessSummary::Observed(AgentReadinessSummary {
            ready_count: 1,
            unavailable_agents: ref unavailable,
            ..
        }) if unavailable.len() == 1
    ));
}

#[test]
fn malformed_configured_id_preserves_all_canonical_ids_independent_of_order() {
    for configured in [vec!["bad ", "a", "b"], vec!["a", "bad ", "b"]] {
        let projection = project_behavior_readiness(
            None,
            "did:test:agent",
            configured,
            Some("default"),
            observed_at(),
        );
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
fn behavior_unavailable_rejections_are_exactly_routing_messages() {
    use AgentReadinessUnavailableReason as Reason;
    // Exhaustive: a new reason fails to compile here until `ALL` lists it.
    let ordinal = |reason: Reason| match reason {
        Reason::AgentDisabled => 0,
        Reason::RuntimeConfigurationInvalid => 1,
        Reason::BackendNotConfigured => 2,
        Reason::BackendDisabled => 3,
        Reason::BackendTemporarilyUnavailable => 4,
        Reason::CredentialsRequired => 5,
        Reason::InferenceProfileInvalid => 6,
        Reason::ToolConfigurationInvalid => 7,
        Reason::ToolSurfaceUnavailable => 8,
        Reason::ExecutorStartFailed => 9,
    };
    assert_eq!(Reason::ALL.map(ordinal), [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
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
