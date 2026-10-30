use super::observe::{classify_message_error, lookup_observed, source_denied};
use super::{loading_or_settling, LiveObservation, LiveView};
use crate::output::origin::{self, ObservedMessage};
use crate::output::reconstruction::{self, ObservedSegment};
use crate::output::{MessagePublication, PayloadRef, SourceClose, TranscriptMessage};

fn referenced_target_close<'a>(
    observation: &LiveObservation<'a>,
    message: &TranscriptMessage,
) -> Option<&'a ObservedSegment<'a>> {
    message.payload_references().iter().find_map(|reference| {
        resolve_close_reference(observation, reference)
            .ok()
            .filter(|closing| {
                closing.segment.source == *observation.target.source
                    && closing.segment.writer == *observation.target.writer
            })
    })
}

/// Resolve one close reference (Lean `resolveClose`).
fn resolve_close_reference<'a>(
    observation: &LiveObservation<'a>,
    reference: &PayloadRef,
) -> Result<&'a ObservedSegment<'a>, ()> {
    let mut matching = observation
        .records
        .iter()
        .filter(|r| r.doc_id == reference.close_doc_id);
    let record = matching.next().ok_or(())?;
    if matching.any(|other| other != record) {
        return Err(());
    }
    if observation
        .denied_segments
        .iter()
        .any(|denied| denied == record.doc_id)
    {
        return Err(());
    }
    match &record.segment.close {
        Some(SourceClose::Closed { stream_bytes, .. }) => {
            if (reference.stream as usize) >= stream_bytes.len() {
                return Err(());
            }
            let mut closures: Vec<&ObservedSegment<'_>> = Vec::new();
            for candidate in observation.records {
                if candidate.segment.request_doc_id == record.segment.request_doc_id
                    && candidate.segment.source == record.segment.source
                    && candidate.segment.close.is_some()
                    && !closures.iter().any(|existing| {
                        existing.doc_id == candidate.doc_id && existing.segment == candidate.segment
                    })
                {
                    closures.push(candidate);
                }
            }
            if closures.len() != 1 || *closures[0] != *record {
                return Err(());
            }
            Ok(record)
        }
        _ => Err(()),
    }
}

fn settling_for_referenced_target(
    observation: &LiveObservation<'_>,
    message: &TranscriptMessage,
) -> LiveView {
    if source_denied(observation) {
        return LiveView::Denied;
    }
    match referenced_target_close(observation, message) {
        Some(closing) => loading_or_settling(observation, Some(closing)),
        None => LiveView::Loading,
    }
}

pub(super) fn project_published<'a>(observation: &'a LiveObservation<'a>) -> LiveView {
    let Some(target_doc_id) = observation.target.message_id else {
        return LiveView::Loading;
    };
    let message = match lookup_observed(observation, target_doc_id) {
        Ok(observed) => observed.message,
        Err(view) => return view,
    };
    if message.session_id != observation.session_id {
        return LiveView::Invalid;
    }
    match &message.publication {
        MessagePublication::Fork {
            origin_message_doc_id,
        } => {
            if message.request_doc_id.is_some() {
                return LiveView::Invalid;
            }
            let fork_observed = match lookup_observed(observation, target_doc_id) {
                Ok(observed) => observed,
                Err(view) => return view,
            };
            if let Err(view) = project_fork(observation, fork_observed, origin_message_doc_id) {
                return view;
            }
        }
        _ => {
            if message.request_doc_id.as_deref() != Some(observation.request_doc_id) {
                return LiveView::Invalid;
            }
        }
    }
    if observation.dependency_denials.iter().any(|denial| {
        message
            .payload_references()
            .iter()
            .any(|r| r.close_doc_id == denial.root_close_id)
    }) {
        return LiveView::Denied;
    }
    match reconstruction::reconstruct_message(
        observation.records,
        observation.denied_segments,
        observation.dependency_denials,
        message,
    ) {
        Ok(native) => LiveView::Published {
            message: Box::new(message.clone()),
            native: Box::new(native),
        },
        Err(error) => match &error {
            ReconstructionError::UnresolvedClose { .. } => LiveView::Loading,
            ReconstructionError::MissingSegment { .. } => {
                settling_for_referenced_target(observation, message)
            }
            _ => classify_message_error(&error),
        },
    }
}

fn project_fork(
    observation: &LiveObservation<'_>,
    fork: ObservedMessage<'_>,
    origin_message_doc_id: &str,
) -> Result<(), LiveView> {
    let origin = lookup_observed(observation, origin_message_doc_id)?;
    origin::validate_fork_metadata(fork, origin).map_err(|_| LiveView::Invalid)
}
