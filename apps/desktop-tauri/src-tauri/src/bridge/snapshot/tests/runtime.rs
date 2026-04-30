use super::*;

#[test]
fn conversation_summaries_keep_newest_row_per_session() {
    let mut conversations = vec![
        conversation_summary("session-1", "req-3", "2026-04-21T12:03:00Z"),
        conversation_summary("session-2", "req-a", "2026-04-21T12:02:00Z"),
        conversation_summary("session-1", "req-2", "2026-04-21T12:01:00Z"),
    ];

    retain_latest_conversation_summaries(&mut conversations);

    assert_eq!(conversations.len(), 2);
    assert_eq!(conversations[0].session_id, "session-1");
    assert_eq!(conversations[0].latest_request_id.as_deref(), Some("req-3"));
    assert_eq!(conversations[1].session_id, "session-2");
}

#[test]
fn conversation_task_tag_uses_latest_schedule_lineage() {
    let store = ClientStore::from_rows(ClientStoreRows {
        requests: vec![
            AgentRequestRow {
                request_id: "req-old".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("old".to_string()),
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: Some("scheduled".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: Some("sched-old".to_string()),
                caused_by_trigger_kind: Some("schedule".to_string()),
                interrupt_requested_at: None,
                valid_until: None,
            },
            AgentRequestRow {
                request_id: "req-new".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("new".to_string()),
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: Some("scheduled".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:02:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: Some("sched-new".to_string()),
                caused_by_trigger_kind: Some("schedule".to_string()),
                interrupt_requested_at: None,
                valid_until: None,
            },
        ],
        ..ClientStoreRows::default()
    });
    let tasks = vec![
        task_view("task-old", "Old task"),
        task_view("task-new", "Freshness check"),
    ];
    let schedules = vec![
        schedule_view("sched-old", "task-old"),
        schedule_view("sched-new", "task-new"),
    ];

    let tag = conversation_task_tag(
        &store,
        "did:defra:amy",
        true,
        "session-1",
        &tasks,
        &schedules,
        &[],
    )
    .expect("task tag");

    assert_eq!(tag.task_id, "task-new");
    assert_eq!(tag.task_name.as_deref(), Some("Freshness check"));
    assert_eq!(tag.trigger_id.as_deref(), Some("sched-new"));
    assert_eq!(tag.trigger_kind.as_deref(), Some("schedule"));
}

#[test]
fn task_run_history_is_agent_scoped_when_trigger_ids_match() {
    let store = ClientStore::from_rows(ClientStoreRows {
        requests: vec![
            AgentRequestRow {
                request_id: "req-mini-1".to_string(),
                agent_did: Some("did:defra:mini-1".to_string()),
                behavior_id: Some("default".to_string()),
                session_id: Some("session-mini-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("run task".to_string()),
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: Some("scheduled".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: Some("shared-schedule".to_string()),
                caused_by_trigger_kind: Some("schedule".to_string()),
                interrupt_requested_at: None,
                valid_until: None,
            },
            AgentRequestRow {
                request_id: "req-mini-2".to_string(),
                agent_did: Some("did:defra:mini-2".to_string()),
                behavior_id: Some("default".to_string()),
                session_id: Some("session-mini-2".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("run task".to_string()),
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: Some("scheduled".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:01:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: Some("shared-schedule".to_string()),
                caused_by_trigger_kind: Some("schedule".to_string()),
                interrupt_requested_at: None,
                valid_until: None,
            },
        ],
        ..ClientStoreRows::default()
    });
    let schedules = vec![schedule_view("shared-schedule", "task-1")];

    let runs = task_run_history(&store, "did:defra:mini-1", true, "task-1", &schedules, &[]);

    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].request_id, "req-mini-1");
}
