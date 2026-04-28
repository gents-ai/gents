use std::time::Duration;

use anyhow::Result;
use defra_node::{EmbeddedNode, QueryResponse};
use rig::completion::message::Message;
use serde::{Deserialize, Serialize};

use crate::graphql::{escape_graphql_string, response_has_documents};

mod compaction_entries;
mod conversation;
mod fork;
mod history;
mod query;
mod retry;
mod rows;
mod sessions;
#[cfg(test)]
mod tests;
mod tool_calls;

pub use compaction_entries::{load_compaction_entries, save_compaction_entry};
#[allow(unused_imports)]
pub(crate) use conversation::{
    conversation_needs_generated_title, load_recent_titles_for_agent,
    update_conversation_status_if_latest_with_identity, update_conversation_status_with_identity,
    update_conversation_title_with_source, upsert_conversation_from_request_with_identity,
    CONVERSATION_TITLE_SOURCE_GENERATED, CONVERSATION_TITLE_SOURCE_PLACEHOLDER,
};
pub use fork::{fork, ForkError, ForkOutcome, ForkParams};
pub use history::load_history;
pub(crate) use history::{mark_response_materialized, save_message};
pub(crate) use query::load_session_behavior_id;
pub use retry::count_active_sessions;
pub(crate) use retry::execute_mutation_with_retry;
pub use sessions::{close_session, create_session};
#[allow(unused_imports)]
pub(crate) use sessions::{
    create_session_with_behavior_id, create_session_with_id, ensure_session,
    ensure_session_with_behavior_id, max_sequence,
};
pub async fn load_tool_call_result(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> Result<String> {
    tool_calls::load_tool_call_result(node, session_id, tool_call_id).await
}
pub(crate) use tool_calls::{complete_tool_call, save_tool_call};

const MAX_MUTATION_RETRIES: u32 = 3;
const INITIAL_RETRY_BACKOFF_MS: u64 = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionEntry {
    pub session_id: String,
    pub sequence: u32,
    pub summary: String,
    pub files_read: Vec<String>,
    pub files_modified: Vec<String>,
    pub messages_compacted: u32,
    pub original_tokens: usize,
    pub compacted_tokens: usize,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConversationUpdateOutcome {
    Updated,
    AlreadyApplied,
    SkippedStaleRequest,
}
