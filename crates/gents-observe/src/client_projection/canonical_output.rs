use gents_protocol::message::Message;
use gents_protocol::output::live::{
    project_live, LiveObservation, LiveTarget, LiveView, OwnerLiveness,
};
use gents_protocol::output::reconstruction::{
    fork_copy_observations, reconstruct_message, DependencyDenial, ObservedSegment,
};
use gents_protocol::output::rows::{OutputSegmentRow, TranscriptMessageRow};
use gents_protocol::output::{OutputSource, OutputWriter, ReconstructionError, TerminalOutput};

mod live_text;
mod terminal_tool_output;

pub use live_text::{canonical_live_text, CanonicalLiveText};
pub use terminal_tool_output::terminal_tool_output;

#[derive(Debug, Clone, PartialEq)]
pub enum CanonicalMessageProjection {
    Ready(Message),
    Loading(ReconstructionError),
    Denied { doc_id: String },
    Invalid(ReconstructionError),
}

/// Whether `header` is a fork resolving from copies observed in its own
/// session; its projection then needs nothing from the fork origin.
pub fn resolves_from_fork_copies(
    header: &TranscriptMessageRow,
    segments: &[OutputSegmentRow],
) -> bool {
    let observed = segments
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    fork_copy_observations(&header.message, &observed).is_some()
}

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
    let observed = fork_copy_observations(&header.message, &observed).unwrap_or(observed);
    match reconstruct_message(&observed, denied, dependency_denials, &header.message) {
        Ok(message) => CanonicalMessageProjection::Ready(message),
        Err(ReconstructionError::AccessDenied { doc_id }) => {
            CanonicalMessageProjection::Denied { doc_id }
        }
        Err(error) if error.is_incomplete() => CanonicalMessageProjection::Loading(error),
        Err(error) => CanonicalMessageProjection::Invalid(error),
    }
}

pub struct LiveReadScope<'a> {
    pub request_doc_id: &'a str,
    pub session_id: &'a str,
    pub node_did: &'a str,
    pub requester_did: Option<&'a str>,
}

pub struct LiveStream<'a> {
    pub source: &'a OutputSource,
    pub writer: &'a OutputWriter,
    pub message_id: Option<&'a str>,
}

pub struct LiveEvidence<'a> {
    pub headers: &'a [TranscriptMessageRow],
    pub segments: &'a [OutputSegmentRow],
    pub denied_headers: &'a [String],
    pub denied_segments: &'a [String],
    pub dependency_denials: &'a [DependencyDenial],
}

pub fn project_canonical_live(
    scope: LiveReadScope<'_>,
    stream: LiveStream<'_>,
    evidence: LiveEvidence<'_>,
    owner: OwnerLiveness<'_>,
    request_terminal: bool,
    terminal_selection: Option<TerminalOutput>,
) -> LiveView {
    let messages = evidence
        .headers
        .iter()
        .map(|row| (row.doc_id.as_str(), &row.message))
        .collect::<Vec<_>>();
    let records = evidence
        .segments
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    project_live(&LiveObservation {
        request_doc_id: scope.request_doc_id,
        session_id: scope.session_id,
        target: LiveTarget {
            request_doc_id: scope.request_doc_id,
            source: stream.source,
            writer: stream.writer,
            message_id: stream.message_id,
        },
        messages: &messages,
        node_did: scope.node_did,
        requester_did: scope.requester_did,
        records: &records,
        denied_headers: evidence.denied_headers,
        denied_segments: evidence.denied_segments,
        dependency_denials: evidence.dependency_denials,
        owner,
        request_terminal,
        terminal_selection,
    })
}
