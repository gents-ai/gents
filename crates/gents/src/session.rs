use std::time::Duration;

use crate::llm::message::Message;
use anyhow::Result;
use defra_node::{EmbeddedNode, QueryResponse};
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

pub use crate::tool_call_lifecycle::query::load_tool_call_result;
pub(crate) use compaction_entries::save_compaction_entry_with_requester_did;
pub use compaction_entries::{load_compaction_entries, save_compaction_entry};
#[allow(unused_imports)]
pub(crate) use conversation::{
    conversation_needs_generated_title, load_recent_titles_for_agent,
    update_conversation_status_if_latest_with_identity, update_conversation_title_with_source,
    upsert_conversation_from_request_with_identity,
    upsert_conversation_from_request_with_identity_and_requester_did,
    upsert_conversation_from_request_with_identity_and_title, CONVERSATION_TITLE_SOURCE_FALLBACK,
    CONVERSATION_TITLE_SOURCE_GENERATED, CONVERSATION_TITLE_SOURCE_TASK,
};
pub use fork::{
    fork, fork_via_http, ForkError, ForkOutcome, ForkParams, GraphqlExecuteResponse,
    GraphqlExecutor, HttpGraphqlExecutor,
};
pub use history::load_history;
#[allow(unused_imports)]
pub(crate) use history::{
    append_message_once_with_key_and_requester_did, append_message_with_requester_did,
    mark_response_materialized, message_sequence_for_request_content, save_message,
    save_message_with_requester_did,
};
pub(crate) use query::load_session_behavior_id;
pub use retry::count_active_sessions;
pub(crate) use retry::execute_mutation_with_retry;
pub use sessions::{close_session, create_session};
#[allow(unused_imports)]
pub(crate) use sessions::{
    create_session_with_behavior_id, create_session_with_id, ensure_session,
    ensure_session_with_behavior_id, ensure_session_with_behavior_id_and_requester_did,
    max_sequence,
};

/// Render an immutable requester route key for a document create branch.
/// Ordinary local lineage leaves the field null by omitting it; remote child
/// lineage stamps the normalized coordinator DID exactly once.
pub(crate) fn requester_did_create_field(requester_did: Option<&str>) -> String {
    requester_did
        .map(str::trim)
        .filter(|did| !did.is_empty())
        .map(|did| format!(r#"requester_did: "{}","#, escape_graphql_string(did)))
        .unwrap_or_default()
}

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
