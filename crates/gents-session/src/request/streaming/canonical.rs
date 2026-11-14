//! Retries reuse prepared output facts; callers acknowledge only committed batches.

use anyhow::Result;
use gents_protocol::output::{OutputSegment, OutputSource, TranscriptMessage};

mod append;
mod close;
mod publish;
mod source;

#[cfg(any(test, feature = "test-fixtures"))]
pub use append::append_provider_segment_at_with_access;
pub use append::append_provider_segment_with_signer;
#[cfg(any(test, feature = "test-fixtures"))]
pub use close::close_provider_attempt_at_with_access;
pub use close::close_provider_attempt_with_signer;
pub(crate) use close::request_headers_in_txn;
#[cfg(any(test, feature = "test-fixtures"))]
pub use publish::publish_provider_turn_at_with_access;
pub use publish::publish_provider_turn_with_signer;
pub(super) use publish::publish_provider_turn_with_time;
pub(crate) use source::{
    retained_partial_blocks, validate_closed_source_extent, validate_source_physical_identities,
    validate_source_purpose,
};

pub struct ProviderPublicationPlan {
    pub final_flush: Option<OutputSegment>,
    pub message_key: String,
    pub encoded: std::sync::Arc<super::native_encoding::EncodedNativeMessage>,
    pub expected: std::sync::Arc<gents_protocol::message::Message>,
    pub tool_deadline_at: String,
    pub background_calls: Vec<String>,
    pub consumes_folded: Option<crate::request::lifecycle::queue::FoldedConsumption>,
}

pub(crate) fn session_message_call_ids(message: &gents_protocol::message::Message) -> Vec<String> {
    match message {
        gents_protocol::message::Message::Assistant { content, .. } => content
            .iter()
            .filter_map(|part| match part {
                gents_protocol::message::AssistantContent::ToolCall(call)
                    if gents_protocol::tool_names::is_session_message_tool(&call.function.name) =>
                {
                    Some(call.id.clone())
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

#[derive(Clone, Debug)]
pub struct PublishedProviderTurn {
    pub message_doc_id: String,
    #[cfg(any(test, feature = "test-fixtures"))]
    pub sequence: u32,
    pub accepted_tools: Vec<super::AcceptedToolCall>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProviderAttemptClose {
    Retracted,
    Partial,
    AuxiliaryComplete,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderCloseRejection {
    #[error("provider close requires its exact request writer")]
    WriterMismatch,
    #[error("provider close candidate disagrees with committed extent")]
    InvalidExtent,
    #[error("provider close lost its live lease")]
    LostLease,
    #[error("provider closure timestamp precedes committed source data")]
    RegressedTimestamp,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderReplayRejection {
    #[error("publication replay await mode changed")]
    AwaitModeChanged,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderAppendRejection {
    #[error("cannot append to a closed provider source")]
    ClosedSource,
    #[error("provider append lost its live processing lease")]
    LostLease,
}

pub type PartialHeaderFactory =
    std::sync::Arc<dyn Fn(&str) -> Result<TranscriptMessage> + Send + Sync>;

#[derive(Clone)]
pub struct ProviderPartialCandidate {
    pub closing: OutputSegment,
    pub header: Option<PartialHeaderFactory>,
}

pub fn partial_message_key(request_doc_id: &str, source: &OutputSource) -> Result<String> {
    Ok(format!(
        "request-recovery:{request_doc_id}:{}",
        serde_json::to_string(source)?
    ))
}
