use super::{LiveObservation, LiveView};
use crate::message::Message;
use crate::output::origin::{self, ObservedMessage, OriginError};
use crate::output::reconstruction::{self, ObservedSegment};
use crate::output::{
    MessagePublication, MessageRole, OutputOutcome, OutputSource, OutputWriter,
    ReconstructionError, SourceClose, TerminalOutput, TranscriptMessage,
};

/// Lean's `ownerLive` for the target writer.
pub(super) fn owner_live(observation: &LiveObservation<'_>) -> bool {
    match (observation.target.source, observation.target.writer) {
        (
            OutputSource::ProviderTurn { .. },
            OutputWriter::RequestExecution {
                execution_generation,
            },
        ) => {
            observation.owner.current_request
                == Some((observation.request_doc_id, execution_generation.as_str()))
        }
        (
            OutputSource::ToolCall {
                tool_call_doc_id: source,
            },
            OutputWriter::ToolExecution {
                tool_call_doc_id: writer,
            },
        ) => source == writer && observation.owner.live_tools.contains(&source.as_str()),
        _ => false,
    }
}

pub(super) fn source_denied(observation: &LiveObservation<'_>) -> bool {
    observation.records.iter().any(|record| {
        record.segment.request_doc_id == observation.request_doc_id
            && record.segment.source == *observation.target.source
            && observation
                .denied_segments
                .iter()
                .any(|denied| denied == record.doc_id)
    })
}

pub(super) fn target_scoped(observation: &LiveObservation<'_>) -> bool {
    observation.target.request_doc_id == observation.request_doc_id
}

pub(super) fn lookup_observed<'a>(
    observation: &'a LiveObservation<'a>,
    doc_id: &str,
) -> Result<ObservedMessage<'a>, LiveView> {
    let facts: Vec<ObservedMessage<'_>> = observation
        .messages
        .iter()
        .map(|(id, message)| ObservedMessage {
            doc_id: id,
            message,
        })
        .collect();
    origin::lookup_message(
        &facts,
        observation.denied_headers,
        doc_id,
        observation.node_did,
        observation.requester_did,
    )
    .map_err(|error| match error {
        OriginError::Unavailable { .. } => LiveView::Loading,
        OriginError::Denied { .. } => LiveView::Denied,
        OriginError::Conflict { .. } => LiveView::Conflicted,
        OriginError::ScopeMismatch { .. }
        | OriginError::InvalidOrigin { .. }
        | OriginError::MetadataMismatch { .. } => LiveView::Invalid,
    })
}

/// Lean's `CloseObservation` over protocol types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CloseObservation_ {
    Open,
    Closed { outcome: OutputOutcome },
    Retracted,
    Conflict,
}

pub(super) fn observe_close<'a>(
    observation: &'a LiveObservation<'a>,
) -> Result<(Option<&'a ObservedSegment<'a>>, CloseObservation_), ()> {
    let mut closures: Vec<&ObservedSegment<'_>> = Vec::new();
    for record in observation.records {
        if record.segment.request_doc_id != observation.request_doc_id
            || record.segment.source != *observation.target.source
            || record.segment.close.is_none()
        {
            continue;
        }
        if !closures
            .iter()
            .any(|existing| existing.doc_id == record.doc_id && existing.segment == record.segment)
        {
            closures.push(record);
        }
    }
    match closures.as_slice() {
        [] => Ok((None, CloseObservation_::Open)),
        [only] => match &only.segment.close {
            Some(SourceClose::Closed { outcome, .. }) => {
                Ok((Some(only), CloseObservation_::Closed { outcome: *outcome }))
            }
            Some(SourceClose::Retracted) => Ok((Some(only), CloseObservation_::Retracted)),
            None => Ok((None, CloseObservation_::Conflict)),
        },
        _ => Err(()),
    }
}

pub(super) fn stream_referenced(
    message: &TranscriptMessage,
    close_doc_id: &str,
    stream: u32,
) -> bool {
    message
        .payload_references()
        .iter()
        .any(|reference| reference.close_doc_id == close_doc_id && reference.stream == stream)
}

pub(super) fn classify_message_error(error: &ReconstructionError) -> LiveView {
    match error {
        ReconstructionError::UnresolvedClose { .. }
        | ReconstructionError::MissingSegment { .. } => LiveView::Loading,
        ReconstructionError::AccessDenied { .. } => LiveView::Denied,
        ReconstructionError::ConflictingClosures { .. }
        | ReconstructionError::ConflictingSegments { .. }
        | ReconstructionError::ConflictingMessages { .. } => LiveView::Conflicted,
        ReconstructionError::InvalidReference { .. }
        | ReconstructionError::ExtentMismatch { .. }
        | ReconstructionError::InvalidWriter { .. }
        | ReconstructionError::UnresolvedTerminalOutput { .. }
        | ReconstructionError::InvalidStructure { .. }
        | ReconstructionError::InvalidPresentation { .. }
        | ReconstructionError::InvalidPayload { .. } => LiveView::Invalid,
    }
}

fn resolve_terminal<'a>(
    observation: &'a LiveObservation<'a>,
) -> Result<Option<&'a TranscriptMessage>, LiveView> {
    let Some(selection) = &observation.terminal_selection else {
        return Err(LiveView::Loading);
    };
    let message_doc_id = match selection {
        TerminalOutput::NoMessage => return Ok(None),
        TerminalOutput::Message { message_doc_id } => message_doc_id,
    };
    let message = lookup_observed(observation, message_doc_id)?.message;
    if message.request_doc_id.as_deref() != Some(observation.request_doc_id)
        || message.session_id != observation.session_id
    {
        return Err(LiveView::Invalid);
    }
    if message.role != MessageRole::Assistant
        || !matches!(
            message.publication,
            MessagePublication::RequestExecution { .. }
                | MessagePublication::RequestRecovery { .. }
        )
    {
        return Err(LiveView::Invalid);
    }
    Ok(Some(message))
}

pub(super) fn resolve_terminal_payload(
    observation: &LiveObservation<'_>,
) -> Result<Option<Message>, LiveView> {
    let Some(message) = resolve_terminal(observation)? else {
        return Ok(None);
    };
    if observation.dependency_denials.iter().any(|denial| {
        message
            .payload_references()
            .iter()
            .any(|r| r.close_doc_id == denial.root_close_id)
    }) {
        return Err(LiveView::Denied);
    }
    reconstruction::reconstruct_message(
        observation.records,
        observation.denied_segments,
        observation.dependency_denials,
        message,
    )
    .map(Some)
    .map_err(|error| classify_message_error(&error))
}
