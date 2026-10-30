use super::observe::{
    classify_message_error, resolve_terminal_payload, source_denied, stream_referenced,
    target_scoped,
};
use super::{loading_or_settling, LiveObservation, LiveView};
use crate::output::reconstruction::{self, ObservedSegment, ReconstructedStream};
use crate::output::{
    LiveStream, LiveStreamState, MessagePublication, OutputOutcome, OutputSource, PayloadRef,
    ReconstructionError, SourceClose, TranscriptMessage,
};

#[derive(Clone, Debug, PartialEq)]
pub enum ReferencedHeaderResolution<'a> {
    Resolved { headers: Vec<&'a TranscriptMessage> },
    None,
    Failed(LiveView),
}

fn resolved_referencing_headers<'a>(
    observation: &'a LiveObservation<'a>,
    closing: &ObservedSegment<'_>,
) -> ReferencedHeaderResolution<'a> {
    let mut candidates: Vec<(&'a str, &'a TranscriptMessage)> = Vec::new();
    for (doc_id, message) in observation.messages {
        let references_close = message
            .payload_references()
            .iter()
            .any(|r| r.close_doc_id == closing.doc_id);
        if !references_close {
            continue;
        }
        let request_member = match &message.publication {
            MessagePublication::ToolDelivery { .. } => true,
            _ => message.request_doc_id.as_deref() == Some(observation.request_doc_id),
        };
        if !request_member || message.session_id != observation.session_id {
            continue;
        }
        if !candidates
            .iter()
            .any(|(existing_id, existing)| *existing_id == *doc_id && **existing == **message)
        {
            candidates.push((doc_id, message));
        }
    }
    if candidates.is_empty() {
        return ReferencedHeaderResolution::None;
    }
    let mut headers = Vec::new();
    for (doc_id, candidate) in candidates {
        let mut matching: Vec<&'a TranscriptMessage> = observation
            .messages
            .iter()
            .filter_map(|(id, message)| (*id == doc_id).then_some(*message))
            .collect();
        matching.dedup();
        if matching.len() != 1 || matching[0] != candidate {
            return ReferencedHeaderResolution::Failed(LiveView::Conflicted);
        }
        if observation.dependency_denials.iter().any(|denial| {
            candidate
                .payload_references()
                .iter()
                .any(|r| r.close_doc_id == denial.root_close_id)
        }) {
            return ReferencedHeaderResolution::Failed(LiveView::Denied);
        }
        match reconstruction::reconstruct_message(
            observation.records,
            observation.denied_segments,
            observation.dependency_denials,
            candidate,
        ) {
            Ok(_) => headers.push(candidate),
            Err(error) => {
                return ReferencedHeaderResolution::Failed(classify_message_error(&error));
            }
        }
    }
    ReferencedHeaderResolution::Resolved { headers }
}

fn retained_streams(
    observation: &LiveObservation<'_>,
    closing: &ObservedSegment<'_>,
    headers: &[&TranscriptMessage],
    streams: Vec<ReconstructedStream>,
) -> Vec<LiveStream> {
    LiveView::visible_streams(
        streams
            .into_iter()
            .enumerate()
            .filter(|(stream_index, _)| {
                !headers
                    .iter()
                    .any(|header| stream_referenced(header, closing.doc_id, *stream_index as u32))
            })
            .map(|(stream_index, reconstructed)| LiveStream {
                source: observation.target.source.clone(),
                stream: stream_index as u32,
                declaration: reconstructed.declaration,
                state: LiveStreamState::RetainedPartial,
                text: reconstructed.text,
            })
            .collect(),
    )
}

pub(super) fn project_unheaded_closed(
    observation: &LiveObservation<'_>,
    closing: &ObservedSegment<'_>,
    outcome: OutputOutcome,
) -> LiveView {
    if !target_scoped(observation) {
        return LiveView::Invalid;
    }
    if source_denied(observation)
        || observation
            .dependency_denials
            .iter()
            .any(|denial| denial.root_close_id == closing.doc_id)
    {
        return LiveView::Denied;
    }
    if observation.request_terminal {
        let resolved_terminal = match resolve_terminal_payload(observation) {
            Ok(resolved) => resolved,
            Err(view) => return view,
        };
        let _ = resolved_terminal;
    }
    if !observation.request_terminal {
        return loading_or_settling(observation, Some(closing));
    }
    if matches!(observation.target.source, OutputSource::ProviderTurn { .. })
        && outcome == OutputOutcome::Partial
    {
        let headers = match resolved_referencing_headers(observation, closing) {
            ReferencedHeaderResolution::Failed(view) => return view,
            ReferencedHeaderResolution::Resolved { headers } => headers,
            ReferencedHeaderResolution::None => Vec::new(),
        };
        match reconstruct_extent_streams(observation, *closing) {
            Err(ReconstructionError::MissingSegment { .. }) => {
                loading_or_settling(observation, Some(closing))
            }
            Err(
                ReconstructionError::ConflictingSegments { .. }
                | ReconstructionError::ConflictingClosures { .. },
            ) => LiveView::Conflicted,
            Err(_) => LiveView::Invalid,
            Ok(streams) => LiveView::RetainedPartial {
                streams: retained_streams(observation, closing, &headers, streams),
            },
        }
    } else {
        loading_or_settling(observation, Some(closing))
    }
}

fn reconstruct_extent_streams(
    observation: &LiveObservation<'_>,
    closing: ObservedSegment<'_>,
) -> Result<Vec<ReconstructedStream>, ReconstructionError> {
    let Some(SourceClose::Closed { stream_bytes, .. }) = &closing.segment.close else {
        return Err(ReconstructionError::InvalidStructure {
            detail: "live retained extent is not closed".to_owned(),
        });
    };
    if stream_bytes.is_empty() {
        return Ok(Vec::new());
    }
    reconstruction::reconstruct_extent_streams(
        observation.records,
        observation.denied_segments,
        observation.dependency_denials,
        &PayloadRef {
            close_doc_id: closing.doc_id.to_owned(),
            stream: 0,
        },
    )
}
