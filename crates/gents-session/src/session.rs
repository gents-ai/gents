use anyhow::Result;
use defra_node::Node as EmbeddedNode;
use gents_inference::llm::message::Message;
use serde::{Deserialize, Serialize};

use gents_store::graphql::escape_graphql_string;

mod archive;
pub mod caused;
pub mod mailbox;
pub mod message;
pub mod origin;
pub mod plugin_effect_rows;
pub mod tool_call_state;
pub mod tool_output_rows;
pub mod tool_presentation;
pub use archive::set_session_archived_on;
mod compaction_entries;
mod control;
mod fork;
mod history;
#[cfg(test)]
mod model_test_support;
mod observations;
mod output;
pub mod output_render;
mod preview;
pub mod provider_context_reduction;
pub(crate) use preview::{request_preview_in_txn, visible_reply_preview};
#[cfg(test)]
mod output_replay_tests;
mod query;
mod request_output;
mod rows;
mod sessions;
pub use sessions::{close_session_with_access, recover_agent_session_relationships_with_access};
pub mod title_text;
pub use title_text::display_session_title;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::import_history_observation_as;
#[cfg(test)]
pub use tests::{import_history_observation, publish_signed_history};

pub use crate::tool_call::query::result::{
    load_tool_call_arguments, load_tool_call_presentation, load_tool_call_result,
    CanonicalToolCallPresentation,
};
#[cfg(test)]
pub(crate) use compaction_entries::load_prompt_compaction_state;
pub use compaction_entries::load_prompt_compaction_state_with_access;
pub use compaction_entries::StaleCompactionGeneration;
pub use compaction_entries::{compaction_key, load_compaction_entries};
pub use compaction_entries::{save_exact_compaction_entry, NewExactSessionCompaction};
#[cfg(test)]
pub(crate) use control::stage_session_resume_in_txn;
pub use control::{
    resume_session_with_access, stop_session_with_access, SessionExecutionWitness,
    SessionStopReceipt,
};
pub use fork::message_tool_references;
pub use fork::{
    fork_edit_on, fork_on, is_user_turn, publish_fork_relationships, ForkBoundary, ForkError,
    ForkOutcome, ForkParams, ForkRelationshipIntent,
};
#[cfg(test)]
pub use history::load_history_with_access;
pub use history::{load_history, sequence_message_key};
pub use history::{load_sequenced_history_for_request, load_sequenced_history_projection_on};
pub use history::{resumed_request_has_published_input, retry_has_published_input};
pub use observations::load_latest_requests_in_txn;
pub(crate) use observations::load_session_current_hop;
pub use observations::load_session_current_hop_in_txn;
pub use observations::load_recent_titles_for_agent;
pub(crate) use observations::{
    advance_refused_request_observation_in_txn, advance_session_request_observation_in_txn,
    load_scoped_request_facts_in_txn,
};
pub use observations::{apply_title_in_txn, rename_session_on};
pub use observations::{
    derive_session_preview, refresh_session_request_observation_in_txn,
    session_needs_generated_title, update_session_title_with_source, SessionRequestRefresh,
    REFUSAL_PROJECTION_EVENT_TARGET,
};
pub use observations::{load_latest_request_in_txn, SessionRequestFact};
#[cfg(test)]
pub(crate) use output::count_capture_row_queries;
#[cfg(test)]
pub use output::count_request_output_scans;
#[cfg(test)]
pub(crate) use output::count_source_output_queries;
#[cfg(test)]
pub use output::reader::load_canonical_payload_from_access;
pub(crate) use output::reader::load_canonical_payload_in_txn;
pub(crate) use output::reader::load_canonical_payload_with_access;
pub use output::reader::load_source_rows_in_txn;
pub use output::reader::{load_canonical_message, load_canonical_message_from_node};
pub use output::replay_request::validate_canonical_replay_boundary;
pub use output::replay_tags::resolve_canonical_replay_tags_on;
pub use output::CanonicalOutputReadError;
pub use output::CanonicalReplayScope;
pub use output::{
    reader::load_canonical_message_in_txn, reader::load_request_headers_in_txn,
    reader::TxnCanonicalReader,
};
#[cfg(test)]
pub use query::load_session_agent_did;
pub use query::require_session;
pub use query::session_is_stopped;
pub use query::session_stopped_in_txn;
pub use query::{client_requester_scope, public_request_filter};
pub use query::{load_session_agent_selection, load_session_requester_scope};
pub use request_output::canonical_read_loading;
pub use request_output::{
    observe_request_output, CanonicalPresentation, CanonicalRequestOutput, CanonicalSelectedSource,
};
pub(crate) use sessions::{
    ensure_reserved_session_in_txn, finalize_agent_new_session_in_txn,
    finalize_plugin_job_session_in_txn, finalize_reserved_runtime_session_in_txn,
    reopen_session_in_txn, stage_agent_new_session_in_txn, stage_plugin_job_session_in_txn,
    touch_session_in_txn,
};
pub use sessions::{
    ensure_session_in_txn, grant_created_session_relationships, max_sequence_as,
    stage_created_session_relationships_in_txn,
};

#[cfg(test)]
pub use sessions::max_sequence;

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
pub struct PromptCompactionState {
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
pub struct SequencedMessage {
    /// Provider source derived from this header's exact reconstructed closes,
    /// never its native bytes or provider-generated message ID.
    pub provider_source: Option<gents_loop::claude_messages_body::ReplayTag>,
    /// The validated origin header for forks; never a provider-assigned ID.
    pub canonical_header_doc_id: Option<String>,
    /// Original native block positions, before any provider-view rewrite.
    pub block_indices: Vec<usize>,
    pub sequence: u32,
    pub message: Message,
}

#[cfg(test)]
mod user_title_tests;

pub use sessions::stage_session_relationships_in_txn;
