use gents::llm::message::{Message, Text, UserContent};
use gents_desktop_core::client::{ClientStore, ClientStoreRows};
use gents_protocol::row::{
    AgentConversationRow, AgentMessageRow, AgentRequestRow, AgentResponseRow, AgentSessionRow,
    GoalRow,
};

use super::super::types::{
    ConversationSummary, RenderedTimelineItem, ScheduleView, TaskRecentRunsView, TaskView,
};
use super::build_session_snapshot_from_store;
use super::build_session_snapshot_from_store_for_agent;
use super::conversation_task_tag;
use super::recent_runs_for_task_views;
use super::request_backed_conversation_summaries;
use super::retain_latest_conversation_summaries;
use super::task_run_history;

fn user_message_json(text: &str) -> String {
    serde_json::to_string(&Message::User {
        content: vec![UserContent::Text(Text {
            text: text.to_string(),
        })],
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
        cron: None,
        timezone: None,
        missed_run_policy: None,
        enabled: Some(true),
        concurrency: None,
        next_run_at: None,
        last_attempt_at: None,
        last_status: None,
        last_error: None,
        fire_count: None,
    }
}

#[path = "tests/mcp_health.rs"]
mod mcp_health;
#[path = "tests/runtime.rs"]
mod runtime;
#[path = "tests/session_basic.rs"]
mod session_basic;
#[path = "tests/session_stale_rows.rs"]
mod session_stale_rows;
#[path = "tests/session_state.rs"]
mod session_state;
#[path = "tests/session_timeline.rs"]
mod session_timeline;
#[path = "tests/subagent_lineage.rs"]
mod subagent_lineage;
