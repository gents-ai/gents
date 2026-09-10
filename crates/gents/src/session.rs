use std::time::Duration;

use crate::llm::message::Message;
use anyhow::Result;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::escape_graphql_string;

mod compaction_entries;
mod fork;
mod history;
mod observations;
mod query;
mod retry;
mod rows;
mod sessions;
#[cfg(test)]
mod tests;

pub use crate::tool_call_lifecycle::query::load_tool_call_result;
pub(crate) use compaction_entries::load_prompt_compaction_state;
pub use compaction_entries::{compaction_key, load_compaction_entries};
#[cfg(test)]
pub(crate) use compaction_entries::{
    save_compaction_entry, save_compaction_entry_with_requester_did,
};
pub(crate) use compaction_entries::{save_exact_compaction_entry, NewExactSessionCompaction};
pub use fork::{fork, fork_via_http, is_user_turn, ForkError, ForkOutcome, ForkParams};
#[cfg(test)]
pub(crate) use history::load_history_through_sequence;
pub(crate) use history::load_sequenced_history_for_request;
#[allow(unused_imports)]
pub(crate) use history::{
    append_message_once_with_key_and_requester_did, append_message_with_requester_did,
    create_message_mutation, mark_response_materialized, message_sequence_for_request_content,
    save_message, save_message_with_requester_did,
};
pub use history::{load_history, sequence_message_key};
pub use observations::apply_title_in_txn;
pub(crate) use observations::{
    advance_session_request_observation_in_txn, derive_session_preview,
    load_recent_titles_for_agent, load_scoped_request_facts_in_txn,
    refresh_session_request_observation_in_txn, session_needs_generated_title,
    update_session_title_with_source,
};
pub use observations::{load_latest_request_in_txn, SessionRequestFact};
pub use query::{decode_session_row, session_scope_filter, AGENT_SESSION_FIELDS};
pub(crate) use query::{
    load_session_behavior_id, require_session, session_has_live_response,
    session_has_other_live_response,
};
pub use retry::count_active_sessions;
pub use rows::SessionOwnerRow;
pub use sessions::close_session;
pub use sessions::load_agent_session_row_in_txn;
#[cfg(test)]
pub(crate) use sessions::{
    create_session_with_behavior_id, create_session_with_id,
    ensure_session_with_behavior_id_and_requester_did,
};
pub(crate) use sessions::{ensure_session_in_txn, max_sequence, reopen_session_in_txn};

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

pub(crate) fn request_doc_id_create_field(request_doc_id: Option<&str>) -> String {
    request_doc_id
        .map(str::trim)
        .filter(|doc_id| !doc_id.is_empty())
        .map(|doc_id| format!(r#"request_doc_id: "{}","#, escape_graphql_string(doc_id)))
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
    /// Inclusive canonical `AgentMessage.sequence` cursor for the cumulative
    /// compacted provider prefix. Storage decoding remains nullable so missing
    /// cursors can be rejected explicitly at the validation boundary.
    pub compacted_through_sequence: Option<u32>,
    pub original_tokens: usize,
    pub compacted_tokens: usize,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PromptCompactionState {
    pub summaries: Vec<String>,
    pub total_messages_compacted: usize,
    pub compacted_through_sequence: Option<u32>,
    /// Canonical fingerprint of the complete ordered compaction generation
    /// used to build this prompt. Writers compare it transactionally before
    /// appending so two reductions of the same generation cannot both land.
    pub generation: String,
    /// False when a background transcript cutoff selected an older compatible
    /// compaction generation. Such a request may read that generation but must
    /// not append a new compaction to the live session chain.
    pub is_latest_generation: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SequencedMessage {
    pub sequence: u32,
    pub message: Message,
}

#[cfg(test)]
mod user_title_tests;
