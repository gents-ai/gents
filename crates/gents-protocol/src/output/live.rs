//! Tolerant dense-prefix reconstruction for live views, refining
//! `Proofs/StreamingResponse` (`reconstructPrefix`/`contiguousFlushes`,
//! `PrefixGrowth`, `Executable`) and `Proofs/CanonicalOutput/Reconstruction`
//! (`flushAt`, `consumeRuns`). Owns the live eligibility classifier as the
//! exact Rust projection of Lean's `StreamingResponse.project`; nothing
//! outside this module decides whether a source may be shown live.
mod closed;
mod dense_prefix;
mod observe;
mod published;
mod target;

use closed::project_unheaded_closed;
pub use closed::ReferencedHeaderResolution;
pub use dense_prefix::{reconstruct_dense_prefix, DensePrefix, DensePrefixError};
use observe::{observe_close, owner_live, source_denied, target_scoped, CloseObservation_};
use published::project_published;
pub use target::{select_live_target, LiveTarget, LiveTargetSelection};

use super::reconstruction::{DependencyDenial, ObservedSegment};
use super::{
    LiveStream, LiveStreamState, OutputOutcome, SourceClose, StreamPayload, TerminalOutput,
    TranscriptMessage,
};
use crate::message::Message;

#[derive(Clone, Debug, PartialEq)]
pub enum LiveView {
    Absent,
    Live {
        streams: Vec<LiveStream>,
    },
    Settling {
        streams: Vec<LiveStream>,
    },
    Loading,
    Denied,
    Conflicted,
    Invalid,
    Retracted,
    RetainedPartial {
        streams: Vec<LiveStream>,
    },
    Published {
        message: Box<TranscriptMessage>,
        native: Box<Message>,
    },
}

impl LiveView {
    /// Lean's `visibleStreams`: hidden reasoning fields never render,
    /// including in diagnostics and retained partials.
    fn visible_streams(streams: Vec<LiveStream>) -> Vec<LiveStream> {
        streams
            .into_iter()
            .filter(|stream| {
                !matches!(
                    stream.declaration.payload,
                    StreamPayload::ReasoningEncrypted
                        | StreamPayload::ReasoningRedacted
                        | StreamPayload::ReasoningSignature
                )
            })
            .collect()
    }
}

pub struct LiveObservation<'a> {
    pub request_doc_id: &'a str,
    pub session_id: &'a str,
    pub target: LiveTarget<'a>,
    pub messages: &'a [(&'a str, &'a TranscriptMessage)],
    pub node_did: &'a str,
    pub requester_did: Option<&'a str>,
    /// Visible segments, replicating in any order.
    pub records: &'a [ObservedSegment<'a>],
    /// Explicit header denial evidence; never inferred from absence.
    pub denied_headers: &'a [String],
    /// Explicit segment/closure denial evidence.
    pub denied_segments: &'a [String],
    pub dependency_denials: &'a [DependencyDenial],
    pub owner: OwnerLiveness<'a>,
    pub request_terminal: bool,
    pub terminal_selection: Option<TerminalOutput>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnerLiveness<'a> {
    pub current_request: Option<(&'a str, &'a str)>,
    pub live_tools: Vec<&'a str>,
}

/// Not write or renewal authorization; lifecycle recovery owns lease expiry.
pub fn observed_request_execution_owner(
    request: &crate::row::AgentRequestRow,
) -> Option<(&str, &str)> {
    let request_doc_id = request.doc_id.as_deref()?.trim();
    let generation = request.execution_generation.as_deref()?.trim();
    let deadline = request.execution_lease_expires_at.as_deref()?.trim();
    (matches!(
        request.lifecycle_state?,
        crate::request_lifecycle::RequestLifecycleState::Claimed
            | crate::request_lifecycle::RequestLifecycleState::Processing
    ) && !request_doc_id.is_empty()
        && !generation.is_empty()
        && request
            .execution_lease_secs
            .is_some_and(|seconds| seconds > 0)
        && !deadline.is_empty())
    .then_some((request_doc_id, generation))
}

pub fn project_live(observation: &LiveObservation<'_>) -> LiveView {
    if observation.target.source.is_auxiliary_audit() {
        return LiveView::Absent;
    }
    if observation.target.message_id.is_some() {
        return project_published(observation);
    }
    if !target_scoped(observation) {
        return LiveView::Invalid;
    }
    let (closing, classification) = match observe_close(observation) {
        Ok(result) => result,
        Err(()) => return LiveView::Conflicted,
    };
    match classification {
        CloseObservation_::Open => {
            if source_denied(observation) {
                return LiveView::Denied;
            }
            if owner_live(observation) {
                match reconstruct_open(observation) {
                    Ok(streams) => LiveView::Live { streams },
                    Err(DensePrefixError::Loading) => LiveView::Loading,
                    Err(DensePrefixError::Conflicted { .. }) => LiveView::Conflicted,
                    Err(DensePrefixError::Invalid { .. }) => LiveView::Invalid,
                }
            } else {
                LiveView::Absent
            }
        }
        CloseObservation_::Retracted => LiveView::Retracted,
        CloseObservation_::Conflict => LiveView::Conflicted,
        CloseObservation_::Closed { outcome } => {
            let closing = closing.expect("closed classification carries its closing record");
            project_unheaded_closed(observation, closing, outcome)
        }
    }
}

fn reconstruct_open(
    observation: &LiveObservation<'_>,
) -> Result<Vec<LiveStream>, DensePrefixError> {
    reconstruct_prefix(observation, None)
}

fn reconstruct_prefix(
    observation: &LiveObservation<'_>,
    limit: Option<u32>,
) -> Result<Vec<LiveStream>, DensePrefixError> {
    let prefix = reconstruct_dense_prefix(
        observation.records,
        observation.request_doc_id,
        observation.target.source,
        observation.target.writer,
        limit,
    )?;
    Ok(LiveView::visible_streams(
        prefix
            .streams
            .into_iter()
            .enumerate()
            .map(|(stream_index, reconstructed)| LiveStream {
                source: observation.target.source.clone(),
                stream: stream_index as u32,
                declaration: reconstructed.declaration,
                state: LiveStreamState::Unclosed,
                text: reconstructed.text,
            })
            .collect(),
    ))
}

fn reconstruct_before_close(
    observation: &LiveObservation<'_>,
    closing: &ObservedSegment<'_>,
) -> Result<Vec<LiveStream>, DensePrefixError> {
    match &closing.segment.close {
        Some(SourceClose::Closed { segments, .. }) => {
            reconstruct_prefix(observation, Some(*segments))
        }
        _ => Err(DensePrefixError::Invalid {
            detail: "bounded prefix without a closed extent".to_owned(),
        }),
    }
}

fn loading_or_settling(
    observation: &LiveObservation<'_>,
    closing: Option<&ObservedSegment<'_>>,
) -> LiveView {
    let reconstructed = match closing {
        Some(closing) => reconstruct_before_close(observation, closing),
        None => reconstruct_open(observation),
    };
    let outcome = closing.and_then(|closing| match &closing.segment.close {
        Some(SourceClose::Closed { outcome, .. }) => Some(*outcome),
        _ => None,
    });
    match reconstructed {
        Ok(mut streams) => {
            for stream in &mut streams {
                if stream.state == LiveStreamState::Unclosed {
                    stream.state = LiveStreamState::PendingPublication {
                        outcome: outcome.unwrap_or(OutputOutcome::Complete),
                    };
                }
            }
            LiveView::Settling { streams }
        }
        Err(DensePrefixError::Loading) => LiveView::Loading,
        Err(DensePrefixError::Conflicted { .. }) => LiveView::Conflicted,
        Err(DensePrefixError::Invalid { .. }) => LiveView::Invalid,
    }
}
