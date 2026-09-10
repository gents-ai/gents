use gents::llm::message::{Message, Text, UserContent};
use gents_desktop_core::client::{ClientStore, ClientStoreRows};
use gents_protocol::row::{
    AgentMessageRow, AgentRequestRow, AgentResponseRow, CompactionEntryRow, GoalRow,
};
use gents_protocol::session::{
    AgentSession, SessionObservation, SessionRequestObservation, SessionTitle, SessionTitleSource,
};

use super::super::types::RenderedTimelineItem;
use super::apply_session_timeline_page;
use super::apply_session_timeline_page_with_query;
use super::attach_last_request_context;
use super::build_session_live_delta_from_store;
use super::build_session_snapshot_from_store;
use super::build_session_snapshot_from_store_for_agent;
use super::recent_runs_for_task_views;
use super::session_summaries;
use super::task_run_history;

fn user_message_json(text: &str) -> String {
    serde_json::to_string(&Message::User {
        content: vec![UserContent::Text(Text {
            text: text.to_string(),
        })],
    })
    .expect("serialize user message")
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
#[path = "tests/sync.rs"]
mod sync;
