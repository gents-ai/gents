use super::super::*;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::session::{AgentSession, SessionObservation, SessionRequestObservation};

fn goal_row(goal_id: &str, created_at: &str, status: &str) -> GoalRow {
    serde_json::from_value(serde_json::json!({
        "goal_id": goal_id,
        "session_id": "session-1",
        "agent_did": "did:agent:1",
        "status": status,
        "created_at": created_at
    }))
    .expect("goal row")
}

fn schedule_row(
    schedule_id: &str,
    _task_id: &str,
    _fire_count: Option<i64>,
    _last_attempt_at: Option<&str>,
    _last_status: Option<&str>,
    _last_error: Option<&str>,
) -> Schedule {
    Schedule {
        schedule_id: schedule_id.to_string(),
        agent_did: "did:agent:1".to_string(),
        display_name: None,
        cadence: gents::document_config::ScheduleCadence::Interval { interval_secs: 300 },
        created_at: None,
        updated_at: None,
        tags: Vec::new(),
    }
}

fn trigger(
    trigger_id: &str,
    task_id: &str,
    source: gents::document_config::TriggerSource,
) -> Trigger {
    Trigger {
        agent_did: "did:agent:1".to_string(),
        trigger_id: trigger_id.to_string(),
        display_name: None,
        description: None,
        task_id: task_id.to_string(),
        source,
        enabled: true,
        concurrency: None,
        created_at: None,
        updated_at: None,
        tags: Vec::new(),
    }
}

fn trigger_observation(
    trigger_id: &str,
    fire_count: Option<i64>,
    last_attempt_at: Option<&str>,
    last_status: Option<&str>,
    last_error: Option<&str>,
) -> TriggerObservation {
    TriggerObservation {
        trigger_id: trigger_id.to_string(),
        last_attempt_at: last_attempt_at.map(str::to_string),
        last_fired_source_doc_id: None,
        last_status: last_status.map(str::to_string),
        last_error: last_error.map(str::to_string),
        fire_count,
    }
}

fn task_row(task_id: &str, behavior_id: &str) -> Task {
    Task {
        task_id: task_id.to_string(),
        agent_did: "did:agent:1".to_string(),
        display_name: None,
        description: None,
        behavior_id: behavior_id.to_string(),
        prompt_template: "run".to_string(),
        goal_objective_template: None,
        goal_token_budget: None,
        hooks: Vec::new(),
        enabled: true,
        output_schema_ref: None,
        created_at: None,
        updated_at: None,
        tags: Vec::new(),
    }
}

fn request_row(
    request_id: &str,
    created_at: &str,
    lifecycle_state: &str,
    execution_origin: &str,
) -> AgentRequestRow {
    serde_json::from_value(serde_json::json!({
        "_docID": request_id,
        "request_id": request_id,
        "agent_did": "did:agent:1",
        "behavior_id": "default",
        "session_id": "session-1",
        "content": "turn",
        "lifecycle_state": lifecycle_state,
        "execution_origin": execution_origin,
        "created_at": created_at
    }))
    .expect("request row")
}

#[test]
fn session_pointer_requires_the_observed_physical_request() {
    use gents_protocol::client_protocol::ClientTurnState;

    // A matching logical label is insufficient. Include a positive control so
    // always returning an unknown turn cannot satisfy the missing-row cases.
    for (doc_id, expected_turn) in [
        (None, None),
        (Some("different-physical-request"), None),
        (
            Some("physical-not-replicated-yet"),
            Some(ClientTurnState::Completed),
        ),
    ] {
        let mut requests = vec![request_row(
            "old-terminal",
            "2026-07-01T00:00:00Z",
            "completed",
            "interactive",
        )];
        if let Some(doc_id) = doc_id {
            let mut row = request_row(
                "not-replicated-yet",
                "2026-07-02T00:00:00Z",
                "completed",
                "interactive",
            );
            row.doc_id = Some(doc_id.into());
            requests.push(row);
        }
        let store = ClientStore::from_rows(ClientStoreRows {
            sessions: vec![AgentSession {
                session_id: "session-1".to_string(),
                agent_did: "did:agent:1".to_string(),
                requester_did: None,
                behavior_id: "default".to_string(),
                created_at: "2026-04-21T12:00:00Z".into(),
                closed_at: None,
                title: None,
                tags: Vec::new(),
                provenance: None,
                observation: Some(SessionObservation {
                    last_activity_at: "2026-07-02T00:00:00Z".into(),
                    preview: None,
                    latest_request: Some(SessionRequestObservation {
                        request_doc_id: "physical-not-replicated-yet".to_string(),
                        request_id: "not-replicated-yet".to_string(),
                        lifecycle_state: RequestLifecycleState::Processing,
                    }),
                }),
            }],
            requests,
            ..ClientStoreRows::default()
        });

        let expected_request_id = expected_turn.map(|_| "not-replicated-yet");
        assert_eq!(
            store.latest_request_id_for_session("session-1").as_deref(),
            expected_request_id
        );
        assert_eq!(
            store
                .latest_request_id_for_session_for_agent("session-1", "did:agent:1")
                .as_deref(),
            expected_request_id
        );
        assert_eq!(
            store.derive_turn_for_agent("session-1", "did:agent:1"),
            expected_turn,
            "only the exact observed document may supply turn state: {doc_id:?}"
        );
    }
}

#[test]
fn recent_runs_aggregates_across_canonical_triggers() {
    let mut store = ClientStore::default();
    store.triggers.push(trigger(
        "s1",
        "task-1",
        gents::document_config::TriggerSource::Schedule {
            schedule_id: "schedule-1".to_string(),
        },
    ));
    store.triggers.push(trigger(
        "t1",
        "task-1",
        gents::document_config::TriggerSource::Event {
            event_source_id: "source-1".to_string(),
        },
    ));
    store.trigger_observations.push(trigger_observation(
        "s1",
        Some(3),
        Some("2026-04-22T10:00:00Z"),
        Some("fired"),
        None,
    ));
    store.trigger_observations.push(trigger_observation(
        "t1",
        Some(5),
        Some("2026-04-22T11:00:00Z"),
        Some("skipped"),
        Some("in-flight"),
    ));

    let runs = store.recent_runs_for_task("task-1");
    assert_eq!(runs.total_fires, 8);
    assert_eq!(
        runs.last_attempt_at.as_deref(),
        Some("2026-04-22T11:00:00Z")
    );
    assert_eq!(runs.last_status.as_deref(), Some("skipped"));
    assert_eq!(runs.last_error.as_deref(), Some("in-flight"));
    assert_eq!(runs.schedule_count, 1);
    assert_eq!(runs.event_count, 1);
}

#[test]
fn recent_runs_empty_when_no_triggers() {
    let store = ClientStore::default();
    let runs = store.recent_runs_for_task("task-missing");
    assert_eq!(runs, TaskRecentRuns::default());
}

#[test]
fn source_agent_dids_round_trip_with_rows() {
    let store = ClientStore::from_rows(ClientStoreRows {
        tasks: vec![task_row("task-1", "default")],
        schedules: vec![schedule_row("schedule-1", "task-1", None, None, None, None)],
        triggers: vec![trigger(
            "trigger-1",
            "task-1",
            gents::document_config::TriggerSource::Event {
                event_source_id: "source-1".to_string(),
            },
        )],
        task_source_agent_dids: vec![Some("did:test:mini-1".to_string())],
        schedule_source_agent_dids: vec![Some("did:test:mini-1".to_string())],
        trigger_source_agent_dids: vec![Some("did:test:mini-1".to_string())],
        ..ClientStoreRows::default()
    });

    let restored = ClientStore::from_rows(store.to_rows());

    assert_eq!(
        restored.task_source_agent_dids,
        vec![Some("did:test:mini-1".to_string())]
    );
    assert_eq!(
        restored.schedule_source_agent_dids,
        vec![Some("did:test:mini-1".to_string())]
    );
    assert_eq!(
        restored.trigger_source_agent_dids,
        vec![Some("did:test:mini-1".to_string())]
    );
}

#[test]
fn goal_merge_preserves_the_earliest_canonical_twin() {
    let canonical_created_at = "2026-07-16T00:00:00Z";
    let store = ClientStore::from_rows(ClientStoreRows {
        goals: vec![
            goal_row("later-twin", "2026-07-16T00:00:01Z", "complete"),
            goal_row("canonical", canonical_created_at, "active"),
        ],
        ..ClientStoreRows::default()
    });
    assert_eq!(store.goals.len(), 1);
    assert_eq!(store.goals[0].goal_id, "canonical");

    let later_twin = ClientStore::from_rows(ClientStoreRows {
        goals: vec![goal_row(
            "arriving-twin",
            "2026-07-16T00:00:02Z",
            "complete",
        )],
        ..ClientStoreRows::default()
    });
    let store = store.merge_snapshot(later_twin);
    assert_eq!(store.goals.len(), 1);
    assert_eq!(store.goals[0].status.as_deref(), Some("active"));

    let canonical_update = ClientStore::from_rows(ClientStoreRows {
        goals: vec![goal_row("canonical", canonical_created_at, "complete")],
        ..ClientStoreRows::default()
    });
    let store = store.merge_snapshot(canonical_update);
    assert_eq!(store.goals.len(), 1);
    assert_eq!(store.goals[0].status.as_deref(), Some("complete"));
}
