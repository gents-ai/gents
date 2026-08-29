use super::*;

fn readiness_row(agent_did: &str, snapshot_json: String) -> AgentBehaviorReadinessRow {
    AgentBehaviorReadinessRow {
        agent_did: agent_did.to_string(),
        snapshot_json,
        updated_at: "2026-08-28T00:00:00Z".to_string(),
    }
}

fn readiness_snapshot(behaviors: Vec<BehaviorReadinessEntry>) -> BehaviorReadinessSnapshot {
    BehaviorReadinessSnapshot {
        format_version: BEHAVIOR_READINESS_FORMAT_VERSION,
        process_state: BehaviorReadinessProcessState::Ready,
        active_generation: 4,
        router_generation: 4,
        default_behavior_id: "a".to_string(),
        behaviors,
    }
}

fn projected_unknown(row: &AgentBehaviorReadinessRow) -> Option<BehaviorReadinessUnknownReason> {
    project_behavior_readiness(Some(row), "did:test:agent", ["a", "b"], Some("a")).unknown_reason
}

#[test]
fn source_projection_is_sorted_and_unavailability_wins() {
    let snapshot = project_behavior_readiness_source(
        BehaviorReadinessProcessState::Ready,
        4,
        4,
        "a",
        [
            BehaviorReadinessSourceEntry {
                behavior_id: "b".to_string(),
                dispatcher_present: true,
                unavailable_reason: Some(BehaviorReadinessUnavailableReason::BackendDisabled),
                startup_demoted: false,
            },
            BehaviorReadinessSourceEntry {
                behavior_id: "a".to_string(),
                dispatcher_present: true,
                unavailable_reason: None,
                startup_demoted: false,
            },
        ],
    )
    .unwrap();
    assert_eq!(snapshot.behaviors[0].behavior_id, "a");
    assert_eq!(
        snapshot.behaviors[1].state,
        BehaviorReadinessState::Unavailable
    );
}

#[test]
fn projection_accepts_only_canonical_bound_payloads_and_process_states() {
    let ready = BehaviorReadinessEntry {
        behavior_id: "a".to_string(),
        state: BehaviorReadinessState::Ready,
        reason: None,
    };
    let unavailable = BehaviorReadinessEntry {
        behavior_id: "b".to_string(),
        state: BehaviorReadinessState::Unavailable,
        reason: Some(BehaviorReadinessUnavailableReason::BackendDisabled),
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
        readiness_snapshot(vec![BehaviorReadinessEntry {
            behavior_id: " a".to_string(),
            ..ready.clone()
        }]),
        readiness_snapshot(vec![BehaviorReadinessEntry {
            reason: Some(BehaviorReadinessUnavailableReason::BackendDisabled),
            ..ready.clone()
        }]),
        readiness_snapshot(vec![BehaviorReadinessEntry {
            behavior_id: "a".to_string(),
            state: BehaviorReadinessState::Unavailable,
            reason: None,
        }]),
    ];
    for snapshot in malformed_snapshots {
        let row = readiness_row("did:test:agent", serde_json::to_string(&snapshot).unwrap());
        assert_eq!(
            projected_unknown(&row),
            Some(BehaviorReadinessUnknownReason::ReadinessMalformed)
        );
    }

    let unknown_process = readiness_row(
        "did:test:agent",
        canonical.snapshot_json.replace("\"ready\"", "\"starting\""),
    );
    assert_eq!(
        projected_unknown(&unknown_process),
        Some(BehaviorReadinessUnknownReason::ReadinessMalformed)
    );
    assert_eq!(
        project_behavior_readiness(Some(&canonical), "did:test:agent", [" a"], Some("a"))
            .unknown_reason,
        Some(BehaviorReadinessUnknownReason::ReadinessMalformed)
    );

    let mut whitespace_default = readiness_snapshot(vec![ready.clone()]);
    whitespace_default.default_behavior_id = "a ".to_string();
    let row = readiness_row(
        "did:test:agent",
        serde_json::to_string(&whitespace_default).unwrap(),
    );
    assert_eq!(
        projected_unknown(&row),
        Some(BehaviorReadinessUnknownReason::ReadinessMalformed)
    );

    let mut invalid_default = readiness_snapshot(vec![ready]);
    invalid_default.default_behavior_id = "missing".to_string();
    let row = readiness_row(
        "did:test:agent",
        serde_json::to_string(&invalid_default).unwrap(),
    );
    assert_eq!(
        projected_unknown(&row),
        Some(BehaviorReadinessUnknownReason::BehaviorNotAssigned)
    );

    let foreign = readiness_row("did:test:foreign", canonical.snapshot_json.clone());
    assert_eq!(
        projected_unknown(&foreign),
        Some(BehaviorReadinessUnknownReason::ReadinessMalformed)
    );

    let with_unknown_field = readiness_row(
        "did:test:agent",
        r#"{"format_version":1,"process_state":"ready","active_generation":4,"router_generation":4,"default_behavior_id":"a","behaviors":[{"behavior_id":"a","state":"ready","reason":null,"extra":true}]}"#.to_string(),
    );
    assert_eq!(
        projected_unknown(&with_unknown_field),
        Some(BehaviorReadinessUnknownReason::ReadinessMalformed)
    );
    let with_unknown_top_level_field = readiness_row(
        "did:test:agent",
        r#"{"format_version":1,"process_state":"ready","active_generation":4,"router_generation":4,"default_behavior_id":"a","behaviors":[{"behavior_id":"a","state":"ready","reason":null}],"extra":true}"#.to_string(),
    );
    assert_eq!(
        project_behavior_readiness_summary(Some(&with_unknown_top_level_field), "did:test:agent"),
        ProjectedBehaviorReadinessSummary::Unknown(
            BehaviorReadinessUnknownReason::ReadinessMalformed
        )
    );
    assert!(matches!(
        project_behavior_readiness_summary(Some(&canonical), "did:test:agent"),
        ProjectedBehaviorReadinessSummary::Observed(BehaviorReadinessSummary {
            ready_count: 1,
            unavailable_behaviors: ref unavailable,
            ..
        }) if unavailable.len() == 1
    ));
}

#[test]
fn malformed_configured_id_preserves_all_canonical_ids_independent_of_order() {
    for configured in [vec!["bad ", "a", "b"], vec!["a", "bad ", "b"]] {
        let projection =
            project_behavior_readiness(None, "did:test:agent", configured, Some("default"));
        assert_eq!(
            projection.unknown_reason,
            Some(BehaviorReadinessUnknownReason::ReadinessMalformed)
        );
        assert_eq!(
            projection.behaviors.keys().cloned().collect::<Vec<_>>(),
            vec!["a".to_string(), "b".to_string(), "default".to_string()]
        );
    }
}
