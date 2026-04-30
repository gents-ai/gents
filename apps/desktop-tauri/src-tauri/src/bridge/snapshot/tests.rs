use defra_agent_desktop_core::client::{ClientStore, ClientStoreRows};
use defra_agent_protocol::row::{
    AgentConversationRow, AgentMessageRow, AgentRequestRow, AgentResponseRow, AgentSessionRow,
};
use rig::completion::message::{Message, Text, UserContent};
use rig::one_or_many::OneOrMany;

use super::super::types::{
    ConversationSummary, RenderedTimelineItem, ScheduleView, TaskRecentRunsView, TaskView,
};
use super::build_session_snapshot_from_store;
use super::build_session_snapshot_from_store_for_agent;
use super::conversation_task_tag;
use super::retain_latest_conversation_summaries;
use super::task_run_history;

fn user_message_json(text: &str) -> String {
    serde_json::to_string(&Message::User {
        content: OneOrMany::one(UserContent::Text(Text {
            text: text.to_string(),
        })),
    })
    .expect("serialize user message")
}

fn conversation_summary(
    session_id: &str,
    latest_request_id: &str,
    updated_at: &str,
) -> ConversationSummary {
    ConversationSummary {
        session_id: session_id.to_string(),
        title: None,
        preview_text: None,
        status: None,
        behavior_id: None,
        latest_request_id: Some(latest_request_id.to_string()),
        task_id: None,
        task_name: None,
        trigger_id: None,
        trigger_kind: None,
        created_at: Some("2026-04-21T12:00:00Z".to_string()),
        updated_at: Some(updated_at.to_string()),
        turn_state: None,
        message_count: 0,
        tool_call_count: 0,
    }
}

fn task_view(task_id: &str, name: &str) -> TaskView {
    TaskView {
        task_id: task_id.to_string(),
        name: Some(name.to_string()),
        description: None,
        behavior_id: None,
        prompt_template: None,
        enabled: Some(true),
        output_schema_ref: None,
        recent_runs: TaskRecentRunsView {
            total_fires: 0,
            last_attempt_at: None,
            last_status: None,
            last_error: None,
            schedule_count: 0,
            event_trigger_count: 0,
        },
        run_history: Vec::new(),
    }
}

fn schedule_view(schedule_id: &str, task_id: &str) -> ScheduleView {
    ScheduleView {
        schedule_id: schedule_id.to_string(),
        task_id: Some(task_id.to_string()),
        interval_secs: Some(60),
        enabled: Some(true),
        concurrency: None,
        next_run_at: None,
        last_attempt_at: None,
        last_status: None,
        last_error: None,
        fire_count: None,
    }
}

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

#[test]
fn session_snapshot_is_agent_scoped_when_session_ids_match() {
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![
            AgentConversationRow {
                session_id: "shared-session".to_string(),
                agent_name: Some("Mini 1".to_string()),
                agent_did: Some("did:defra:mini-1".to_string()),
                behavior_id: Some("default".to_string()),
                title: Some("mini-1 run".to_string()),
                title_source: Some("manual".to_string()),
                preview_text: None,
                status: Some("active".to_string()),
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                updated_at: Some("2026-04-21T12:00:00Z".to_string()),
                latest_request_id: None,
            },
            AgentConversationRow {
                session_id: "shared-session".to_string(),
                agent_name: Some("Mini 2".to_string()),
                agent_did: Some("did:defra:mini-2".to_string()),
                behavior_id: Some("default".to_string()),
                title: Some("mini-2 run".to_string()),
                title_source: Some("manual".to_string()),
                preview_text: None,
                status: Some("active".to_string()),
                created_at: Some("2026-04-21T12:01:00Z".to_string()),
                updated_at: Some("2026-04-21T12:01:00Z".to_string()),
                latest_request_id: None,
            },
        ],
        messages: vec![
            AgentMessageRow {
                message_key: "msg-mini-1".to_string(),
                session_id: Some("shared-session".to_string()),
                sequence: Some(1),
                role: Some("user".to_string()),
                content: Some(user_message_json("mini 1 only")),
                timestamp: Some("2026-04-21T12:00:01Z".to_string()),
            },
            AgentMessageRow {
                message_key: "msg-mini-2".to_string(),
                session_id: Some("shared-session".to_string()),
                sequence: Some(2),
                role: Some("user".to_string()),
                content: Some(user_message_json("mini 2 only")),
                timestamp: Some("2026-04-21T12:01:01Z".to_string()),
            },
        ],
        message_source_agent_dids: vec![
            Some("did:defra:mini-1".to_string()),
            Some("did:defra:mini-2".to_string()),
        ],
        ..ClientStoreRows::default()
    });

    let snapshot = build_session_snapshot_from_store_for_agent(
        &store,
        Some("did:defra:mini-1"),
        "shared-session",
        None,
    )
    .expect("session snapshot");

    assert_eq!(snapshot.agent_did.as_deref(), Some("did:defra:mini-1"));
    assert_eq!(snapshot.title.as_deref(), Some("mini-1 run"));
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(snapshot.messages[0].message_key, "msg-mini-1");
}

#[test]
fn session_snapshot_exposes_pending_turn_when_latest_request_is_not_materialized() {
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            title: Some("architecture-review".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("follow up question".to_string()),
            status: Some("active".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:02:00Z".to_string()),
            latest_request_id: Some("req-2".to_string()),
        }],
        requests: vec![
            AgentRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("first question".to_string()),
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            },
            AgentRequestRow {
                request_id: "req-2".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("follow up question".to_string()),
                status: Some("processing".to_string()),
                lifecycle_state: Some("processing".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:01:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            },
        ],
        messages: vec![AgentMessageRow {
            message_key: "msg-1".to_string(),
            session_id: Some("session-1".to_string()),
            sequence: Some(1),
            role: Some("user".to_string()),
            content: Some(user_message_json("first question")),
            timestamp: Some("2026-04-21T12:00:00Z".to_string()),
        }],
        ..ClientStoreRows::default()
    });

    let snapshot =
        build_session_snapshot_from_store(&store, "session-1", None).expect("session snapshot");
    let pending = snapshot.pending_turn.expect("pending turn");
    assert_eq!(pending.request_id, "req-2");
    assert_eq!(pending.content, "follow up question");
    assert_eq!(pending.lifecycle_state.as_deref(), Some("processing"));
}

#[test]
fn session_snapshot_hides_pending_turn_once_user_message_is_materialized() {
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            title: Some("architecture-review".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("follow up question".to_string()),
            status: Some("active".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:02:00Z".to_string()),
            latest_request_id: Some("req-2".to_string()),
        }],
        requests: vec![AgentRequestRow {
            request_id: "req-2".to_string(),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            retry_parent_request: None,
            retry_root_request: None,
            superseded_by_request: None,
            content: Some("follow up question".to_string()),
            status: Some("processing".to_string()),
            lifecycle_state: Some("processing".to_string()),
            backend_id: None,
            execution_origin: Some("interactive".to_string()),
            failure_reason: None,
            created_at: Some("2026-04-21T12:01:00Z".to_string()),
            claimed_at: None,
            deadline: None,
            retry_count: Some(0),
            max_retries: Some(3),
            caused_by_trigger_id: None,
            caused_by_trigger_kind: None,
            interrupt_requested_at: None,
            valid_until: None,
        }],
        messages: vec![AgentMessageRow {
            message_key: "msg-2".to_string(),
            session_id: Some("session-1".to_string()),
            sequence: Some(2),
            role: Some("user".to_string()),
            content: Some(user_message_json("follow up question")),
            timestamp: Some("2026-04-21T12:01:01Z".to_string()),
        }],
        ..ClientStoreRows::default()
    });

    let snapshot =
        build_session_snapshot_from_store(&store, "session-1", None).expect("session snapshot");
    assert!(snapshot.pending_turn.is_none());
}

#[test]
fn session_snapshot_keeps_pending_turn_for_repeated_prompt_until_second_user_message_materializes()
{
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            title: Some("conversation".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("same prompt".to_string()),
            status: Some("active".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:02:00Z".to_string()),
            latest_request_id: Some("req-2".to_string()),
        }],
        requests: vec![
            AgentRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("same prompt".to_string()),
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            },
            AgentRequestRow {
                request_id: "req-2".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("same prompt".to_string()),
                status: Some("processing".to_string()),
                lifecycle_state: Some("processing".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:01:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            },
        ],
        messages: vec![AgentMessageRow {
            message_key: "msg-1".to_string(),
            session_id: Some("session-1".to_string()),
            sequence: Some(1),
            role: Some("user".to_string()),
            content: Some(user_message_json("same prompt")),
            timestamp: Some("2026-04-21T12:00:00Z".to_string()),
        }],
        ..ClientStoreRows::default()
    });

    let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-2"))
        .expect("session snapshot");
    assert_eq!(
        snapshot
            .pending_turn
            .as_ref()
            .map(|turn| turn.request_id.as_str()),
        Some("req-2")
    );
}

#[test]
fn session_snapshot_orders_pending_turn_before_orphan_tool_groups_and_live_overlay() {
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            title: Some("conversation".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("turn two".to_string()),
            status: Some("active".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:02:00Z".to_string()),
            latest_request_id: Some("req-2".to_string()),
        }],
        requests: vec![
            AgentRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("turn one".to_string()),
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            },
            AgentRequestRow {
                request_id: "req-2".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("turn two".to_string()),
                status: Some("processing".to_string()),
                lifecycle_state: Some("processing".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:01:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            },
        ],
        messages: vec![AgentMessageRow {
            message_key: "msg-1".to_string(),
            session_id: Some("session-1".to_string()),
            sequence: Some(1),
            role: Some("user".to_string()),
            content: Some(user_message_json("turn one")),
            timestamp: Some("2026-04-21T12:00:00Z".to_string()),
        }],
        responses: vec![AgentResponseRow {
            response_key: "resp-2".to_string(),
            request_id: Some("req-2".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            content: Some("streaming reply".to_string()),
            reasoning: None,
            status: Some("streaming".to_string()),
            error_message: None,
            token_count: Some(12),
            progress_seq: Some(1),
            materialized_message_sequence: None,
            materialized_at: None,
            created_at: Some("2026-04-21T12:01:01Z".to_string()),
            completed_at: None,
            interrupted_at: None,
        }],
        tool_calls: vec![defra_agent_protocol::row::AgentToolCallRow {
            tool_call_key: "tool-1".to_string(),
            session_id: Some("session-1".to_string()),
            message_sequence: None,
            tool_name: Some("glob".to_string()),
            tool_call_id: Some("call-1".to_string()),
            args: Some("{\"pattern\":\"**/*.rs\"}".to_string()),
            result: None,
            status: Some("running".to_string()),
            started_at: Some("2026-04-21T12:01:02Z".to_string()),
            completed_at: None,
        }],
        ..ClientStoreRows::default()
    });

    let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-2"))
        .expect("session snapshot");
    let kinds = snapshot
        .timeline_items
        .iter()
        .map(|item| match item {
            RenderedTimelineItem::UserMessage { .. } => "user",
            RenderedTimelineItem::AssistantMessage { .. } => "assistant",
            RenderedTimelineItem::ToolGroup { .. } => "tools",
            RenderedTimelineItem::PendingUserTurn { .. } => "pending",
            RenderedTimelineItem::LiveAssistant { .. } => "live",
        })
        .collect::<Vec<_>>();
    assert_eq!(kinds, vec!["user", "pending", "tools", "live"]);
}

#[test]
fn session_snapshot_keeps_full_live_overlay_when_only_prior_turn_shares_prefix() {
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            title: Some("conversation".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("turn two".to_string()),
            status: Some("active".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:02:00Z".to_string()),
            latest_request_id: Some("req-2".to_string()),
        }],
        requests: vec![
            AgentRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("turn one".to_string()),
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            },
            AgentRequestRow {
                request_id: "req-2".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("turn two".to_string()),
                status: Some("processing".to_string()),
                lifecycle_state: Some("processing".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:01:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            },
        ],
        messages: vec![
            AgentMessageRow {
                message_key: "msg-1".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(1),
                role: Some("user".to_string()),
                content: Some(user_message_json("turn one")),
                timestamp: Some("2026-04-21T12:00:00Z".to_string()),
            },
            AgentMessageRow {
                message_key: "msg-2".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(2),
                role: Some("assistant".to_string()),
                content: Some(
                    serde_json::to_string(&Message::assistant("I'll investigate"))
                        .expect("serialize assistant"),
                ),
                timestamp: Some("2026-04-21T12:00:01Z".to_string()),
            },
            AgentMessageRow {
                message_key: "msg-3".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(3),
                role: Some("user".to_string()),
                content: Some(user_message_json("turn two")),
                timestamp: Some("2026-04-21T12:01:00Z".to_string()),
            },
        ],
        responses: vec![AgentResponseRow {
            response_key: "resp-2".to_string(),
            request_id: Some("req-2".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            content: Some("I'll investigate further into p2p".to_string()),
            reasoning: None,
            status: Some("streaming".to_string()),
            error_message: None,
            token_count: Some(12),
            progress_seq: Some(1),
            materialized_message_sequence: None,
            materialized_at: None,
            created_at: Some("2026-04-21T12:01:01Z".to_string()),
            completed_at: None,
            interrupted_at: None,
        }],
        ..ClientStoreRows::default()
    });

    let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-2"))
        .expect("session snapshot");
    let live_content = snapshot.timeline_items.iter().find_map(|item| match item {
        RenderedTimelineItem::LiveAssistant { content, .. } => content.as_deref(),
        _ => None,
    });
    assert_eq!(live_content, Some("I'll investigate further into p2p"));
}

#[test]
fn session_snapshot_renders_structured_tool_payloads_in_timeline() {
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            title: Some("conversation".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("turn one".to_string()),
            status: Some("active".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:02:00Z".to_string()),
            latest_request_id: Some("req-1".to_string()),
        }],
        requests: vec![AgentRequestRow {
            request_id: "req-1".to_string(),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            retry_parent_request: None,
            retry_root_request: None,
            superseded_by_request: None,
            content: Some("turn one".to_string()),
            status: Some("processing".to_string()),
            lifecycle_state: Some("processing".to_string()),
            backend_id: None,
            execution_origin: Some("interactive".to_string()),
            failure_reason: None,
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            claimed_at: None,
            deadline: None,
            retry_count: Some(0),
            max_retries: Some(3),
            caused_by_trigger_id: None,
            caused_by_trigger_kind: None,
            interrupt_requested_at: None,
            valid_until: None,
        }],
        messages: vec![AgentMessageRow {
            message_key: "msg-1".to_string(),
            session_id: Some("session-1".to_string()),
            sequence: Some(1),
            role: Some("user".to_string()),
            content: Some(user_message_json("turn one")),
            timestamp: Some("2026-04-21T12:00:00Z".to_string()),
        }],
        tool_calls: vec![defra_agent_protocol::row::AgentToolCallRow {
            tool_call_key: "tool-1".to_string(),
            session_id: Some("session-1".to_string()),
            message_sequence: Some(2),
            tool_name: Some("glob".to_string()),
            tool_call_id: Some("call-1".to_string()),
            args: Some("{\"pattern\":\"**/*.rs\",\"recursive\":true}".to_string()),
            result: Some("{\"matches\":12}".to_string()),
            status: Some("completed".to_string()),
            started_at: Some("2026-04-21T12:00:01Z".to_string()),
            completed_at: Some("2026-04-21T12:00:02Z".to_string()),
        }],
        ..ClientStoreRows::default()
    });

    let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-1"))
        .expect("session snapshot");
    let tools = snapshot
        .timeline_items
        .iter()
        .find_map(|item| match item {
            RenderedTimelineItem::ToolGroup { tools, .. } => Some(tools),
            _ => None,
        })
        .expect("tool group");

    assert_eq!(tools.len(), 1);
    let tool = &tools[0];
    assert_eq!(tool.tool_name, "glob");
    assert_eq!(tool.status_kind, "success");
    assert_eq!(
        tool.args.as_ref().map(|value| value
            .fields
            .iter()
            .map(|field| field.key.as_str())
            .collect::<Vec<_>>()),
        Some(vec!["pattern", "recursive"])
    );
    assert_eq!(
        tool.result
            .as_ref()
            .and_then(|value| value.fields.iter().find(|field| field.key == "matches"))
            .map(|field| field.value.as_str()),
        Some("12")
    );
}

#[test]
fn session_snapshot_can_be_built_without_conversation_row_when_session_is_observed() {
    let store = ClientStore::from_rows(ClientStoreRows {
        sessions: vec![AgentSessionRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            started: Some("2026-04-21T12:00:00Z".to_string()),
            ended: None,
            status: Some("active".to_string()),
        }],
        requests: vec![AgentRequestRow {
            request_id: "req-1".to_string(),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            retry_parent_request: None,
            retry_root_request: None,
            superseded_by_request: None,
            content: Some("follow up question".to_string()),
            status: Some("completed".to_string()),
            lifecycle_state: Some("completed".to_string()),
            backend_id: None,
            execution_origin: Some("interactive".to_string()),
            failure_reason: None,
            created_at: Some("2026-04-21T12:01:00Z".to_string()),
            claimed_at: None,
            deadline: None,
            retry_count: Some(0),
            max_retries: Some(3),
            caused_by_trigger_id: None,
            caused_by_trigger_kind: None,
            interrupt_requested_at: None,
            valid_until: None,
        }],
        responses: vec![AgentResponseRow {
            response_key: "resp-1".to_string(),
            request_id: Some("req-1".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            content: Some("done".to_string()),
            reasoning: None,
            status: Some("complete".to_string()),
            error_message: None,
            token_count: Some(12),
            progress_seq: Some(1),
            materialized_message_sequence: Some(2),
            materialized_at: Some("2026-04-21T12:01:05Z".to_string()),
            created_at: Some("2026-04-21T12:01:01Z".to_string()),
            completed_at: Some("2026-04-21T12:01:05Z".to_string()),
            interrupted_at: None,
        }],
        ..ClientStoreRows::default()
    });

    let snapshot =
        build_session_snapshot_from_store(&store, "session-1", None).expect("session snapshot");
    assert_eq!(snapshot.session_id, "session-1");
    assert_eq!(snapshot.agent_did.as_deref(), Some("did:defra:amy"));
    assert_eq!(snapshot.behavior_id.as_deref(), Some("amy-default"));
    assert_eq!(snapshot.status.as_deref(), Some("active"));
    assert_eq!(snapshot.turn_state.as_deref(), Some("completed"));
    assert_eq!(snapshot.latest_request_id.as_deref(), Some("req-1"));
}

#[test]
fn session_snapshot_prefers_tracked_request_over_stale_conversation_latest_request() {
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            title: Some("conversation".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("turn two".to_string()),
            status: Some("active".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:02:00Z".to_string()),
            latest_request_id: Some("req-1".to_string()),
        }],
        requests: vec![
            AgentRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("turn one".to_string()),
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            },
            AgentRequestRow {
                request_id: "req-2".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("turn two".to_string()),
                status: Some("processing".to_string()),
                lifecycle_state: Some("processing".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:01:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            },
        ],
        responses: vec![AgentResponseRow {
            response_key: "resp-2".to_string(),
            request_id: Some("req-2".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            content: Some("streaming reply".to_string()),
            reasoning: None,
            status: Some("streaming".to_string()),
            error_message: None,
            token_count: Some(12),
            progress_seq: Some(1),
            materialized_message_sequence: None,
            materialized_at: None,
            created_at: Some("2026-04-21T12:01:01Z".to_string()),
            completed_at: None,
            interrupted_at: None,
        }],
        messages: vec![AgentMessageRow {
            message_key: "msg-1".to_string(),
            session_id: Some("session-1".to_string()),
            sequence: Some(1),
            role: Some("user".to_string()),
            content: Some(user_message_json("turn one")),
            timestamp: Some("2026-04-21T12:00:00Z".to_string()),
        }],
        ..ClientStoreRows::default()
    });

    let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-2"))
        .expect("session snapshot");

    assert_eq!(snapshot.latest_request_id.as_deref(), Some("req-2"));
    assert_eq!(snapshot.turn_state.as_deref(), Some("streaming"));
    assert_eq!(
        snapshot
            .pending_turn
            .as_ref()
            .map(|turn| turn.request_id.as_str()),
        Some("req-2")
    );
    assert_eq!(
        snapshot
            .active_response_overlay
            .as_ref()
            .and_then(|response| response.content.as_deref()),
        Some("streaming reply")
    );
}

#[test]
fn session_snapshot_stays_renderable_across_single_turn_observation_updates() {
    let submitted = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            title: Some("conversation".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("turn one".to_string()),
            status: Some("active".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:00:01Z".to_string()),
            latest_request_id: None,
        }],
        requests: vec![AgentRequestRow {
            request_id: "req-1".to_string(),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            retry_parent_request: None,
            retry_root_request: None,
            superseded_by_request: None,
            content: Some("turn one".to_string()),
            status: Some("pending".to_string()),
            lifecycle_state: Some("pending".to_string()),
            backend_id: None,
            execution_origin: Some("interactive".to_string()),
            failure_reason: None,
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            claimed_at: None,
            deadline: None,
            retry_count: Some(0),
            max_retries: Some(3),
            caused_by_trigger_id: None,
            caused_by_trigger_kind: None,
            interrupt_requested_at: None,
            valid_until: None,
        }],
        ..ClientStoreRows::default()
    });
    let submitted_snapshot =
        build_session_snapshot_from_store(&submitted, "session-1", Some("req-1"))
            .expect("submitted snapshot");
    assert_eq!(
        submitted_snapshot.latest_request_id.as_deref(),
        Some("req-1")
    );
    assert_eq!(
        submitted_snapshot.turn_state.as_deref(),
        Some("waitingForClaim")
    );
    assert_eq!(
        submitted_snapshot
            .pending_turn
            .as_ref()
            .map(|turn| turn.request_id.as_str()),
        Some("req-1")
    );

    let streaming = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            title: Some("conversation".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("turn one".to_string()),
            status: Some("active".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:00:02Z".to_string()),
            latest_request_id: None,
        }],
        requests: vec![AgentRequestRow {
            request_id: "req-1".to_string(),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            retry_parent_request: None,
            retry_root_request: None,
            superseded_by_request: None,
            content: Some("turn one".to_string()),
            status: Some("processing".to_string()),
            lifecycle_state: Some("processing".to_string()),
            backend_id: None,
            execution_origin: Some("interactive".to_string()),
            failure_reason: None,
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            claimed_at: None,
            deadline: None,
            retry_count: Some(0),
            max_retries: Some(3),
            caused_by_trigger_id: None,
            caused_by_trigger_kind: None,
            interrupt_requested_at: None,
            valid_until: None,
        }],
        responses: vec![AgentResponseRow {
            response_key: "resp-1".to_string(),
            request_id: Some("req-1".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            content: Some("streaming reply".to_string()),
            reasoning: None,
            status: Some("streaming".to_string()),
            error_message: None,
            token_count: Some(12),
            progress_seq: Some(1),
            materialized_message_sequence: None,
            materialized_at: None,
            created_at: Some("2026-04-21T12:00:01Z".to_string()),
            completed_at: None,
            interrupted_at: None,
        }],
        ..ClientStoreRows::default()
    });
    let streaming_snapshot =
        build_session_snapshot_from_store(&streaming, "session-1", Some("req-1"))
            .expect("streaming snapshot");
    assert_eq!(streaming_snapshot.turn_state.as_deref(), Some("streaming"));
    assert_eq!(
        streaming_snapshot
            .active_response_overlay
            .as_ref()
            .and_then(|response| response.content.as_deref()),
        Some("streaming reply")
    );

    let completed = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            title: Some("conversation".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("final answer".to_string()),
            status: Some("completed".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:00:05Z".to_string()),
            latest_request_id: Some("req-1".to_string()),
        }],
        requests: vec![AgentRequestRow {
            request_id: "req-1".to_string(),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            retry_parent_request: None,
            retry_root_request: None,
            superseded_by_request: None,
            content: Some("turn one".to_string()),
            status: Some("completed".to_string()),
            lifecycle_state: Some("completed".to_string()),
            backend_id: None,
            execution_origin: Some("interactive".to_string()),
            failure_reason: None,
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            claimed_at: None,
            deadline: None,
            retry_count: Some(0),
            max_retries: Some(3),
            caused_by_trigger_id: None,
            caused_by_trigger_kind: None,
            interrupt_requested_at: None,
            valid_until: None,
        }],
        responses: vec![AgentResponseRow {
            response_key: "resp-1".to_string(),
            request_id: Some("req-1".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            content: Some("final answer".to_string()),
            reasoning: None,
            status: Some("complete".to_string()),
            error_message: None,
            token_count: Some(34),
            progress_seq: Some(2),
            materialized_message_sequence: Some(2),
            materialized_at: Some("2026-04-21T12:00:05Z".to_string()),
            created_at: Some("2026-04-21T12:00:01Z".to_string()),
            completed_at: Some("2026-04-21T12:00:05Z".to_string()),
            interrupted_at: None,
        }],
        messages: vec![
            AgentMessageRow {
                message_key: "msg-1".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(1),
                role: Some("user".to_string()),
                content: Some(user_message_json("turn one")),
                timestamp: Some("2026-04-21T12:00:00Z".to_string()),
            },
            AgentMessageRow {
                message_key: "msg-2".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(2),
                role: Some("assistant".to_string()),
                content: Some(
                    "{\"role\":\"assistant\",\"content\":[{\"text\":\"final answer\"}]}"
                        .to_string(),
                ),
                timestamp: Some("2026-04-21T12:00:05Z".to_string()),
            },
        ],
        ..ClientStoreRows::default()
    });
    let completed_snapshot =
        build_session_snapshot_from_store(&completed, "session-1", Some("req-1"))
            .expect("completed snapshot");
    assert_eq!(completed_snapshot.turn_state.as_deref(), Some("completed"));
    assert!(completed_snapshot.active_response_overlay.is_none());
    assert!(completed_snapshot.pending_turn.is_none());
}

#[test]
fn session_snapshot_hides_live_overlay_once_turn_is_terminal_even_if_response_is_stale() {
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            title: Some("conversation".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("turn one".to_string()),
            status: Some("completed".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:02:00Z".to_string()),
            latest_request_id: Some("req-1".to_string()),
        }],
        requests: vec![AgentRequestRow {
            request_id: "req-1".to_string(),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            retry_parent_request: None,
            retry_root_request: None,
            superseded_by_request: None,
            content: Some("turn one".to_string()),
            status: Some("completed".to_string()),
            lifecycle_state: Some("completed".to_string()),
            backend_id: None,
            execution_origin: Some("interactive".to_string()),
            failure_reason: None,
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            claimed_at: None,
            deadline: None,
            retry_count: Some(0),
            max_retries: Some(3),
            caused_by_trigger_id: None,
            caused_by_trigger_kind: None,
            interrupt_requested_at: None,
            valid_until: None,
        }],
        messages: vec![
            AgentMessageRow {
                message_key: "msg-1".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(1),
                role: Some("user".to_string()),
                content: Some(user_message_json("turn one")),
                timestamp: Some("2026-04-21T12:00:00Z".to_string()),
            },
            AgentMessageRow {
                message_key: "msg-2".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(2),
                role: Some("assistant".to_string()),
                content: Some(
                    serde_json::to_string(&Message::assistant("final answer"))
                        .expect("serialize assistant"),
                ),
                timestamp: Some("2026-04-21T12:00:01Z".to_string()),
            },
        ],
        responses: vec![AgentResponseRow {
            response_key: "resp-1".to_string(),
            request_id: Some("req-1".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            session_id: Some("session-1".to_string()),
            content: Some("final answer".to_string()),
            reasoning: None,
            status: Some("streaming".to_string()),
            error_message: None,
            token_count: Some(12),
            progress_seq: Some(1),
            materialized_message_sequence: None,
            materialized_at: None,
            created_at: Some("2026-04-21T12:00:01Z".to_string()),
            completed_at: None,
            interrupted_at: None,
        }],
        ..ClientStoreRows::default()
    });

    let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-1"))
        .expect("session snapshot");
    assert_eq!(snapshot.turn_state.as_deref(), Some("completed"));
    assert!(snapshot.active_response_overlay.is_none());
    assert!(!snapshot
        .timeline_items
        .iter()
        .any(|item| matches!(item, RenderedTimelineItem::LiveAssistant { .. })));
}

#[test]
fn session_snapshot_stays_renderable_across_three_turns_with_stale_conversation_rows() {
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: Some("Amy".to_string()),
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: Some("amy-default".to_string()),
            title: Some("conversation".to_string()),
            title_source: Some("generated".to_string()),
            preview_text: Some("turn three".to_string()),
            status: Some("active".to_string()),
            created_at: Some("2026-04-21T12:00:00Z".to_string()),
            updated_at: Some("2026-04-21T12:03:00Z".to_string()),
            latest_request_id: Some("req-2".to_string()),
        }],
        requests: vec![
            AgentRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("turn one".to_string()),
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:00:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            },
            AgentRequestRow {
                request_id: "req-2".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("turn two".to_string()),
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:01:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            },
            AgentRequestRow {
                request_id: "req-3".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: Some("turn three".to_string()),
                status: Some("processing".to_string()),
                lifecycle_state: Some("processing".to_string()),
                backend_id: None,
                execution_origin: Some("interactive".to_string()),
                failure_reason: None,
                created_at: Some("2026-04-21T12:02:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: Some(0),
                max_retries: Some(3),
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
                interrupt_requested_at: None,
                valid_until: None,
            },
        ],
        responses: vec![
            AgentResponseRow {
                response_key: "resp-1".to_string(),
                request_id: Some("req-1".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                content: Some("answer one".to_string()),
                reasoning: None,
                status: Some("complete".to_string()),
                error_message: None,
                token_count: Some(10),
                progress_seq: Some(1),
                materialized_message_sequence: Some(2),
                materialized_at: Some("2026-04-21T12:00:05Z".to_string()),
                created_at: Some("2026-04-21T12:00:01Z".to_string()),
                completed_at: Some("2026-04-21T12:00:05Z".to_string()),
            interrupted_at: None,
            },
            AgentResponseRow {
                response_key: "resp-2".to_string(),
                request_id: Some("req-2".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                content: Some("answer two".to_string()),
                reasoning: None,
                status: Some("complete".to_string()),
                error_message: None,
                token_count: Some(10),
                progress_seq: Some(1),
                materialized_message_sequence: Some(4),
                materialized_at: Some("2026-04-21T12:01:05Z".to_string()),
                created_at: Some("2026-04-21T12:01:01Z".to_string()),
                completed_at: Some("2026-04-21T12:01:05Z".to_string()),
            interrupted_at: None,
            },
            AgentResponseRow {
                response_key: "resp-3".to_string(),
                request_id: Some("req-3".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                session_id: Some("session-1".to_string()),
                content: Some("answer three in progress".to_string()),
                reasoning: None,
                status: Some("streaming".to_string()),
                error_message: None,
                token_count: Some(10),
                progress_seq: Some(1),
                materialized_message_sequence: None,
                materialized_at: None,
                created_at: Some("2026-04-21T12:02:01Z".to_string()),
                completed_at: None,
            interrupted_at: None,
            },
        ],
        messages: vec![
            AgentMessageRow {
                message_key: "msg-1".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(1),
                role: Some("user".to_string()),
                content: Some(user_message_json("turn one")),
                timestamp: Some("2026-04-21T12:00:00Z".to_string()),
            },
            AgentMessageRow {
                message_key: "msg-2".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(2),
                role: Some("assistant".to_string()),
                content: Some("{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"answer one\"}]}".to_string()),
                timestamp: Some("2026-04-21T12:00:05Z".to_string()),
            },
            AgentMessageRow {
                message_key: "msg-3".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(3),
                role: Some("user".to_string()),
                content: Some(user_message_json("turn two")),
                timestamp: Some("2026-04-21T12:01:00Z".to_string()),
            },
            AgentMessageRow {
                message_key: "msg-4".to_string(),
                session_id: Some("session-1".to_string()),
                sequence: Some(4),
                role: Some("assistant".to_string()),
                content: Some("{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"answer two\"}]}".to_string()),
                timestamp: Some("2026-04-21T12:01:05Z".to_string()),
            },
        ],
        ..ClientStoreRows::default()
    });

    let snapshot = build_session_snapshot_from_store(&store, "session-1", Some("req-3"))
        .expect("session snapshot");

    assert_eq!(snapshot.latest_request_id.as_deref(), Some("req-3"));
    assert_eq!(snapshot.turn_state.as_deref(), Some("streaming"));
    assert_eq!(snapshot.messages.len(), 4);
    assert_eq!(
        snapshot
            .pending_turn
            .as_ref()
            .map(|turn| turn.request_id.as_str()),
        Some("req-3")
    );
    assert_eq!(
        snapshot
            .active_response_overlay
            .as_ref()
            .and_then(|response| response.content.as_deref()),
        Some("answer three in progress")
    );
}
