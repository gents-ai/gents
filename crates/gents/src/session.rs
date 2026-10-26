use crate::config_client::{ConfigClientError, TransactionFailure};
use crate::graphql::GraphqlResponseError;
use crate::llm::message::Message;
use defra_node::EmbeddedNode;
use serde::{Deserialize, Serialize};

use crate::graphql::escape_graphql_string;

type Result<T, E = SessionError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Config(#[from] ConfigClientError),
    #[error(transparent)]
    Graphql(#[from] GraphqlResponseError),
    #[error("session_id must be non-empty")]
    SessionIdBlank,
    #[error("node_did must be non-empty")]
    NodeDidBlank,
    #[error("agent_id must be non-empty")]
    AgentIdBlank,
    #[error("requester_did must be non-empty when present")]
    RequesterDidBlank,
    #[error("invalid session created_at")]
    InvalidCreatedAt(#[source] chrono::ParseError),
    #[error("invalid session closed_at")]
    InvalidClosedAt(#[source] chrono::ParseError),
    #[error("AgentSession query omitted rows")]
    AgentSessionRowsOmitted,
    #[error("session create found duplicate AgentSession rows for session_id={session_id}")]
    CreateDuplicateRows { session_id: String },
    #[error("duplicate AgentSession rows for session_id={session_id}")]
    DuplicateRowsForSession { session_id: String },
    #[error("duplicate AgentSession rows for one label")]
    DuplicateAgentSessionRows,
    #[error("AgentSession scope mismatch: existing session_id={existing} requested={requested}")]
    SessionIdMismatch { existing: String, requested: String },
    #[error("AgentSession scope mismatch: existing node_did={existing} requested={requested}")]
    NodeDidMismatch { existing: String, requested: String },
    #[error("AgentSession requester scope mismatch: existing={existing} requested={requested}")]
    RequesterMismatch { existing: String, requested: String },
    #[error(
        "AgentSession requester scope mismatch: existing requester scope {existing} does not match absent scope"
    )]
    RequesterExistingOnly { existing: String },
    #[error(
        "AgentSession requester scope mismatch: existing absent scope does not match requester {requested}"
    )]
    RequesterRequestedOnly { requested: String },
    #[error(
        "AgentSession requester scope mismatch: existing absent scope does not match a requester"
    )]
    RequesterAbsentMismatch,
    #[error("AgentSession agent mismatch: existing={existing} requested={requested}")]
    AgentMismatch { existing: String, requested: String },
    #[error("session create omitted data")]
    CreateOmittedData,
    #[error("AgentSession scope mismatch raced with a concurrent create")]
    CreateRaced,
    #[error("session create matched no document")]
    CreateNoDocument,
    #[error("reopening session: no AgentSession for session_id={session_id}")]
    ReopenMissing { session_id: String },
    #[error("closing session: no AgentSession for session_id={session_id}")]
    CloseMissing { session_id: String },
    #[error("session patch matched no document")]
    PatchNoDocument,
    #[error("message sequence query omitted rows")]
    SequenceRowsOmitted,
    #[error("message sequence is invalid")]
    SequenceInvalid,
    #[error("message sequence exceeds u32")]
    SequenceOverflow(#[source] std::num::TryFromIntError),
    #[error("loading session for completion: no AgentSession for session_id={session_id}")]
    AgentSessionMissing { session_id: String },
    #[error("session stop-latch query omitted rows")]
    StopLatchRowsOmitted,
    #[error("session stop-latch scope is ambiguous")]
    StopLatchAmbiguous,
    #[error("session stop-latch query omitted stopped_at")]
    StopLatchOmitted,
    #[error("session stopped_at is malformed")]
    StopLatchMalformed,
    #[error("session {field} is invalid")]
    InvalidTimestamp {
        field: &'static str,
        #[source]
        source: chrono::ParseError,
    },
    #[error("session {name} must be nonblank")]
    BlankField { name: &'static str },
    #[error("session requester_did must be nonblank when present")]
    BlankRequester,
    #[error("session preview exceeds the 240-character bound")]
    PreviewTooLong,
    #[error("session title text must be nonblank")]
    BlankTitle,
    #[error("AgentSession row omitted _docID")]
    MissingDocId,
    #[error("AgentSession row is not an object")]
    RowNotAnObject,
    #[error("decoding canonical AgentSession")]
    DecodeRow(#[source] serde_json::Error),
    #[error("compaction sequence overflow")]
    CompactionSequenceOverflow(#[source] std::num::TryFromIntError),
    #[error("ambiguous compaction chain for session {session_id}: expected {expected_key}, found {found_key} at sequence {sequence}")]
    AmbiguousCompactionChain {
        session_id: String,
        expected_key: String,
        found_key: String,
        sequence: u32,
    },
    #[error(
        "compaction entry for session {session_id} at sequence {sequence} has no canonical cursor"
    )]
    CompactionMissingCursor { session_id: String, sequence: u32 },
    #[error("compaction cursor regression for session {session_id} at sequence {sequence}")]
    CompactionCursorRegression { session_id: String, sequence: u32 },
    #[error(transparent)]
    FilePathJson(serde_json::Error),
    #[error("unsupported compaction file-list version")]
    UnsupportedFilePathVersion,
    #[error("request facts query omitted AgentRequest rows")]
    RequestFactsOmitted,
    #[error("decoding request fact rows")]
    RequestFactsNotArray,
    #[error("request row omitted {field}")]
    RequestRowOmitted { field: &'static str },
    #[error("invalid request created_at")]
    InvalidRequestCreatedAt(#[source] chrono::ParseError),
    #[error("decoding request lifecycle_state")]
    DecodeLifecycleState(#[source] serde_json::Error),
    #[error("request-head selection found duplicate logical request roots")]
    DuplicateRequestRoot,
    #[error("incoming request does not match the session owner/session/requester/agent scope")]
    IncomingScopeMismatch,
    #[error(
        "session observation selection found duplicate request_id roots for session_id={session_id}"
    )]
    DuplicateRequestIdRoots { session_id: String },
    #[error("incoming request row is missing from the scoped request facts")]
    IncomingMissing,
    #[error("advancing observation: session disappeared")]
    ObservationSessionDisappeared,
    #[error("session observation refresh found duplicate authoritative request rows")]
    RefreshDuplicateRows,
    #[error("invalid title update timestamp")]
    InvalidTitleTimestamp(#[source] chrono::ParseError),
    #[error("updating title: no AgentSession for session_id={session_id}")]
    TitleSessionMissing { session_id: String },
    #[error("loading prompt compaction state for session_id={session_id}: {errors}")]
    LoadingPromptCompactionStateSessionId { session_id: String, errors: String },
    #[error("loading compaction entries for session_id={session_id}: {errors}")]
    LoadingCompactionEntriesSessionId { session_id: String, errors: String },
    #[error("stale compaction generation for session {session_id}")]
    StaleCompactionGenerationSession { session_id: String },
    #[error("compaction cursor regression for session {session_id}")]
    CompactionCursorRegressionSession { session_id: String },
    #[error("CompactionEntry messages_compacted total overflowed usize")]
    CompactionEntryMessagesCompactedTotalOverflowedUsize,
    #[error("compaction original_tokens must be a non-negative usize")]
    CompactionOriginalTokensMustNonNegative(#[source] std::num::TryFromIntError),
    #[error("compaction compacted_tokens must be a non-negative usize")]
    CompactionCompactedTokensMustNonNegative(#[source] std::num::TryFromIntError),
    #[error(transparent)]
    History(#[from] crate::session::HistoryError),
    #[error(transparent)]
    ProviderContextReduction(
        #[from] crate::provider_context_reduction::ProviderContextReductionError,
    ),
    #[error(transparent)]
    Compaction(#[from] gents_loop::compaction::CompactionError),
    #[error(transparent)]
    RenderedRequest(#[from] crate::rendered_request::RenderedRequestError),
    #[error(transparent)]
    Streaming(Box<crate::streaming::StreamingError>),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    LoopRenderedRequest(#[from] gents_loop::rendered_request::RenderedRequestError),
    #[error(transparent)]
    Reduction(#[from] gents_loop::compaction::ReductionError),
}

impl From<crate::streaming::StreamingError> for SessionError {
    fn from(error: crate::streaming::StreamingError) -> Self {
        Self::Streaming(Box::new(error))
    }
}

impl TransactionFailure for SessionError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Config(error) => Some(error),
            Self::Graphql(error) => TransactionFailure::config_client_error(error),
            Self::History(error) => error.config_client_error(),
            Self::RenderedRequest(error) => error.config_client_error(),
            _ => None,
        }
    }
}

pub mod canonical_rows;
mod compaction_entries;
mod control;
mod fork;
mod history;
mod observations;
mod output;
#[cfg(test)]
mod output_replay_tests;
mod query;
mod request_output;
mod rows;
mod sessions;
pub(crate) mod title_text;
pub use title_text::display_session_title;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::import_history_observation;

pub use crate::tool_call_lifecycle::query::{
    load_tool_call_arguments, load_tool_call_presentation, load_tool_call_result,
    CanonicalToolCallPresentation,
};
pub(crate) use compaction_entries::load_prompt_compaction_state;
pub use compaction_entries::{compaction_key, load_compaction_entries};
#[cfg(test)]
pub(crate) use compaction_entries::{
    save_compaction_entry, save_compaction_entry_with_requester_did,
};
pub(crate) use compaction_entries::{save_exact_compaction_entry, NewExactSessionCompaction};
pub(crate) use control::SessionControlError;
pub use control::{resume_session, stop_session, SessionStopReceipt};
pub use fork::{
    fork, fork_on, fork_via_http, is_user_turn, ForkBoundary, ForkError, ForkOutcome, ForkParams,
    SessionForkError,
};
#[cfg(test)]
pub(crate) use history::load_history_through_sequence;
pub use history::{load_history, sequence_message_key, HistoryError};
pub(crate) use history::{
    load_sequenced_history_for_request, load_sequenced_history_projection,
    retry_has_published_input,
};
pub use observations::apply_title_in_txn;
pub(crate) use observations::load_latest_requests_in_txn;
pub(crate) use observations::{
    advance_refused_request_observation_in_txn, advance_session_request_observation_in_txn,
    derive_session_preview, load_recent_titles_for_agent, load_scoped_request_facts_in_txn,
    refresh_session_request_observation_in_txn, session_needs_generated_title,
    update_session_title_with_source, REFUSAL_PROJECTION_EVENT_TARGET,
};
pub use observations::{load_latest_request_in_txn, SessionRequestFact};
pub(crate) use observations::{load_session_current_hop, load_session_current_hop_in_txn};
#[cfg(test)]
pub(crate) use output::count_capture_row_queries;
#[cfg(test)]
pub(crate) use output::count_request_output_scans;
#[cfg(test)]
pub(crate) use output::count_source_output_queries;
#[cfg(test)]
pub(crate) use output::load_canonical_payload_from_node;
pub(crate) use output::load_canonical_payload_in_txn;
pub(crate) use output::load_canonical_payload_with_access;
pub(crate) use output::load_source_rows_in_txn;
pub(crate) use output::{
    find_reconstruction_error, load_canonical_message_in_txn, load_request_headers_in_txn,
    replay_resolution_error, resolve_canonical_replay_tags, validate_canonical_replay_boundary,
    CanonicalReplayScope, TxnCanonicalReader,
};
pub use output::{
    load_canonical_message, load_canonical_message_from_node, CanonicalOutputReadError,
    SessionOutputError,
};
pub(crate) use query::session_is_stopped;
pub(crate) use query::session_stopped_in_txn;
pub use query::{
    decode_session_row, public_request_filter, session_scope_filter, AGENT_SESSION_FIELDS,
};
pub(crate) use query::{load_session_agent_id, load_session_requester_scope, require_session};
pub use request_output::{
    observe_request_output, CanonicalPresentation, CanonicalRequestOutput, CanonicalSelectedSource,
    RequestOutputError,
};
pub use rows::SessionOwnerRow;
pub use sessions::close_session;
pub(crate) use sessions::load_agent_session_owner_in_txn;
pub use sessions::load_agent_session_row_in_txn;
#[cfg(test)]
pub(crate) use sessions::{
    create_session_with_agent_id, create_session_with_id,
    ensure_session_with_agent_id_and_requester_did,
};
pub(crate) use sessions::{
    ensure_session_in_txn, max_sequence, reopen_session_in_txn, touch_session_in_txn,
};

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
