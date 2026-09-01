use super::super::*;

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
    task_id: &str,
    fire_count: Option<i64>,
    last_attempt_at: Option<&str>,
    last_status: Option<&str>,
    last_error: Option<&str>,
) -> ScheduleRow {
    ScheduleRow {
        schedule_id: schedule_id.to_string(),
        task_id: Some(task_id.to_string()),
        interval_secs: None,
        cron: None,
        timezone: None,
        missed_run_policy: None,
        enabled: None,
        concurrency: None,
        next_run_at: None,
        last_attempt_at: last_attempt_at.map(str::to_string),
        last_status: last_status.map(str::to_string),
        last_error: last_error.map(str::to_string),
        fire_count,
        created_at: None,
        updated_at: None,
    }
}

fn event_trigger_row(
    trigger_id: &str,
    task_id: &str,
    fire_count: Option<i64>,
    last_attempt_at: Option<&str>,
    last_status: Option<&str>,
    last_error: Option<&str>,
) -> EventTriggerRow {
    EventTriggerRow {
        trigger_id: trigger_id.to_string(),
        task_id: Some(task_id.to_string()),
        source_collection: None,
        event_kind: None,
        filter: None,
        enabled: None,
        concurrency: None,
        correlation_field: None,
        fire_mode: None,
        expected_count: None,
        expected_count_field: None,
        group_timeout_secs: None,
        group_min_count: None,
        workspace_authority: None,
        created_at: None,
        updated_at: None,
        last_attempt_at: last_attempt_at.map(str::to_string),
        last_fired_source_doc_id: None,
        last_status: last_status.map(str::to_string),
        last_error: last_error.map(str::to_string),
        fire_count,
    }
}

fn task_row(task_id: &str, behavior_id: &str) -> TaskRow {
    TaskRow {
        task_id: task_id.to_string(),
        name: None,
        description: None,
        behavior_id: Some(behavior_id.to_string()),
        prompt_template: None,
        enabled: Some(true),
        output_schema_ref: None,
        created_at: None,
        updated_at: None,
    }
}

fn request_row(
    request_id: &str,
    created_at: &str,
    lifecycle_state: &str,
    execution_origin: &str,
    metadata: Option<String>,
) -> AgentRequestRow {
    serde_json::from_value(serde_json::json!({
        "request_id": request_id,
        "agent_did": "did:agent:1",
        "behavior_id": "default",
        "session_id": "session-1",
        "content": "turn",
        "status": lifecycle_state,
        "lifecycle_state": lifecycle_state,
        "execution_origin": execution_origin,
        "metadata": metadata,
        "created_at": created_at
    }))
    .expect("request row")
}

#[test]
fn unknown_conversation_pointer_preserves_partial_observation() {
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: None,
            agent_did: Some("did:agent:1".to_string()),
            requester_did: None,
            behavior_id: Some("default".to_string()),
            title: None,
            title_source: None,
            preview_text: None,
            status: Some("active".to_string()),
            created_at: None,
            updated_at: None,
            latest_request_id: Some("not-replicated-yet".to_string()),
        }],
        requests: vec![request_row(
            "old-terminal",
            "2026-07-01T00:00:00Z",
            "completed",
            "interactive",
            None,
        )],
        ..ClientStoreRows::default()
    });

    assert_eq!(
        store.latest_request_id_for_session("session-1").as_deref(),
        Some("not-replicated-yet")
    );
    assert_eq!(
        store
            .latest_request_id_for_session_for_agent("session-1", "did:agent:1")
            .as_deref(),
        Some("not-replicated-yet")
    );
    assert_eq!(
        store.derive_turn_for_agent("session-1", "did:agent:1"),
        None,
        "an unknown latest pointer must not regress to an older terminal request"
    );
}

#[test]
fn recent_runs_aggregates_across_schedules_and_event_triggers() {
    let mut store = ClientStore::default();
    store.schedules.push(schedule_row(
        "s1",
        "task-1",
        Some(3),
        Some("2026-04-22T10:00:00Z"),
        Some("fired"),
        None,
    ));
    store.event_triggers.push(event_trigger_row(
        "t1",
        "task-1",
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
    assert_eq!(runs.event_trigger_count, 1);
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
        event_triggers: vec![event_trigger_row(
            "trigger-1",
            "task-1",
            None,
            None,
            None,
            None,
        )],
        task_source_agent_dids: vec![Some("did:test:mini-1".to_string())],
        schedule_source_agent_dids: vec![Some("did:test:mini-1".to_string())],
        event_trigger_source_agent_dids: vec![Some("did:test:mini-1".to_string())],
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
        restored.event_trigger_source_agent_dids,
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
