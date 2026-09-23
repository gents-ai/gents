//! Strict desktop projection of immutable canonical transcript facts.
use gents::session::canonical_rows::{OutputSegmentRow, TranscriptMessageRow};
use gents_protocol::message::Message;
use gents_protocol::output::live::{
    project_live, LiveObservation, LiveTarget, LiveView, OwnerLiveness,
};
use gents_protocol::output::reconstruction::{
    reconstruct_message, DependencyDenial, ObservedSegment,
};
use gents_protocol::output::{OutputSource, OutputWriter, ReconstructionError, TerminalOutput};

#[derive(Debug, Clone, PartialEq)]
pub enum CanonicalMessageProjection {
    Ready(Message),
    Loading(ReconstructionError),
    Denied { doc_id: String },
    Invalid(ReconstructionError),
}

/// Project exactly one immutable header. The caller supplies only facts visible
/// in its authenticated agent/requester/fork dependency scope.
pub fn project_canonical_message(
    header: &TranscriptMessageRow,
    segments: &[OutputSegmentRow],
    denied: &[String],
    dependency_denials: &[DependencyDenial],
) -> CanonicalMessageProjection {
    let observed = segments
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    match reconstruct_message(&observed, denied, dependency_denials, &header.message) {
        Ok(message) => CanonicalMessageProjection::Ready(message),
        Err(ReconstructionError::AccessDenied { doc_id }) => {
            CanonicalMessageProjection::Denied { doc_id }
        }
        Err(error) if error.is_incomplete() => CanonicalMessageProjection::Loading(error),
        Err(error) => CanonicalMessageProjection::Invalid(error),
    }
}

/// Project one live canonical source using the protocol owner. Desktop only
/// adapts authenticated row envelopes into observations; eligibility,
/// contiguous-prefix reconstruction, closure, terminal selection and header
/// publication remain owned by `gents_protocol::output::live`.
#[allow(clippy::too_many_arguments)]
pub fn project_canonical_live(
    request_doc_id: &str,
    session_id: &str,
    target_request_doc_id: &str,
    target_source: &OutputSource,
    target_writer: &OutputWriter,
    target_message_id: Option<&str>,
    agent_did: &str,
    requester_did: Option<&str>,
    headers: &[TranscriptMessageRow],
    segments: &[OutputSegmentRow],
    denied_headers: &[String],
    denied_segments: &[String],
    dependency_denials: &[DependencyDenial],
    owner: OwnerLiveness<'_>,
    request_terminal: bool,
    terminal_selection: Option<TerminalOutput>,
) -> LiveView {
    let messages = headers
        .iter()
        .map(|row| (row.doc_id.as_str(), &row.message))
        .collect::<Vec<_>>();
    let records = segments
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    project_live(&LiveObservation {
        request_doc_id,
        session_id,
        target: LiveTarget {
            request_doc_id: target_request_doc_id,
            source: target_source,
            writer: target_writer,
            message_id: target_message_id,
        },
        messages: &messages,
        agent_did,
        requester_did,
        records: &records,
        denied_headers,
        denied_segments,
        dependency_denials,
        owner,
        request_terminal,
        terminal_selection,
    })
}
