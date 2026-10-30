//! Retries reuse prepared output facts; callers acknowledge only committed batches.

use anyhow::Result;
use gents_protocol::output::{OutputSegment, OutputSource, TranscriptMessage};

mod append;
mod close;
mod publish;
mod source;

pub(crate) use append::{append_in_txn, append_provider_segment};
#[cfg(test)]
pub(crate) use append::{append_in_txn_at, append_provider_segment_at};
pub(crate) use close::{close_provider_attempt, close_provider_attempt_at};
pub(crate) use publish::publish_provider_turn;
#[cfg(test)]
pub(crate) use publish::publish_provider_turn_at;
pub(super) use publish::publish_provider_turn_with_time;
pub(crate) use source::{
    retained_partial_blocks, validate_closed_source_extent, validate_source_physical_identities,
    validate_source_purpose,
};

pub(crate) struct ProviderPublicationPlan {
    pub(crate) final_flush: Option<OutputSegment>,
    pub(crate) message_key: String,
    pub(crate) encoded: std::sync::Arc<super::native_encoding::EncodedNativeMessage>,
    pub(crate) expected: std::sync::Arc<gents_protocol::message::Message>,
    pub(crate) tool_deadline_at: String,
    pub(crate) background_calls: Vec<String>,
    pub(crate) consumes_folded: Option<crate::lifecycle::queue::FoldedConsumption>,
}

pub(crate) fn session_message_call_ids(message: &gents_protocol::message::Message) -> Vec<String> {
    match message {
        gents_protocol::message::Message::Assistant { content, .. } => content
            .iter()
            .filter_map(|part| match part {
                gents_protocol::message::AssistantContent::ToolCall(call)
                    if crate::toolset::is_session_message_tool(&call.function.name) =>
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
pub(crate) struct PublishedProviderTurn {
    pub(crate) message_doc_id: String,
    #[cfg(test)]
    pub(crate) sequence: u32,
    pub(crate) accepted_tools: Vec<super::AcceptedToolCall>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderAttemptClose {
    Retracted,
    Partial,
    AuxiliaryComplete,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProviderCloseRejection {
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
pub(crate) enum ProviderReplayRejection {
    #[error("publication replay await mode changed")]
    AwaitModeChanged,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProviderAppendRejection {
    #[error("cannot append to a closed provider source")]
    ClosedSource,
    #[error("provider append lost its live processing lease")]
    LostLease,
}

pub(crate) type PartialHeaderFactory =
    std::sync::Arc<dyn Fn(&str) -> Result<TranscriptMessage> + Send + Sync>;

#[derive(Clone)]
pub(crate) struct ProviderPartialCandidate {
    pub(crate) closing: OutputSegment,
    pub(crate) header: Option<PartialHeaderFactory>,
}

pub(crate) fn partial_message_key(request_doc_id: &str, source: &OutputSource) -> Result<String> {
    Ok(format!(
        "request-recovery:{request_doc_id}:{}",
        serde_json::to_string(source)?
    ))
}
