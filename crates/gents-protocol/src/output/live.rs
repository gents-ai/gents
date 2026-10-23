//! Tolerant dense-prefix reconstruction for live views, refining
//! `Proofs/StreamingResponse` (`reconstructPrefix`/`contiguousFlushes`,
//! `PrefixGrowth`, `Executable`) and `Proofs/CanonicalOutput/Reconstruction`
//! (`flushAt`, `consumeRuns`). Owns the live eligibility classifier as the
//! exact Rust projection of Lean's `StreamingResponse.project`; nothing
//! outside this module decides whether a source may be shown live.
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, FixedOffset};

use super::origin::{self, ObservedMessage, OriginError};
use super::reconstruction::{self, DependencyDenial, ObservedSegment, ReconstructedStream};
use super::{
    LiveStream, LiveStreamState, MessagePublication, MessageRole, OutputOutcome, OutputSource,
    OutputWriter, PayloadRef, ReconstructionError, SourceClose, StreamPayload, TerminalOutput,
    TranscriptMessage,
};
use crate::message::Message;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DensePrefixError {
    Loading,
    Conflicted { ordinal: u32 },
    Invalid { detail: String },
}

impl std::fmt::Display for DensePrefixError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Loading => f.write_str("no contiguous output prefix is visible yet"),
            Self::Conflicted { ordinal } => {
                write!(f, "conflicting output records at ordinal {ordinal}")
            }
            Self::Invalid { detail } => write!(f, "invalid output prefix: {detail}"),
        }
    }
}

impl std::error::Error for DensePrefixError {}

#[derive(Clone, Debug, PartialEq)]
pub struct DensePrefix {
    pub segments: u32,
    pub streams: Vec<ReconstructedStream>,
    pub highest_visible_ordinal: Option<u32>,
}

pub(crate) fn writer_matches_source(source: &OutputSource, writer: &OutputWriter) -> bool {
    match (source, writer) {
        (
            OutputSource::ProviderTurn { .. } | OutputSource::ToolEffectArguments { .. },
            OutputWriter::RequestExecution { .. },
        ) => true,
        (
            OutputSource::ToolCall {
                tool_call_doc_id: a,
            },
            OutputWriter::ToolExecution {
                tool_call_doc_id: b,
            },
        ) => a == b,
        (OutputSource::Authored { .. }, _) => true,
        _ => false,
    }
}

fn invalid(detail: impl std::fmt::Display) -> DensePrefixError {
    DensePrefixError::Invalid {
        detail: detail.to_string(),
    }
}

pub fn reconstruct_dense_prefix(
    records: &[ObservedSegment<'_>],
    request_doc_id: &str,
    source: &OutputSource,
    expected_writer: &OutputWriter,
    limit: Option<u32>,
) -> Result<DensePrefix, DensePrefixError> {
    if !writer_matches_source(source, expected_writer) {
        return Err(invalid("prefix writer does not bind its source"));
    }
    let scoped = records.iter().copied().filter(|record| {
        record.segment.request_doc_id == request_doc_id && record.segment.source == *source
    });
    let data: Vec<ObservedSegment<'_>> = match limit {
        None => scoped.collect(),
        // Select flushes inside the closed extent before any check, so records
        // beyond the committed extent stay inert for the bounded preview.
        Some(count) => scoped
            .filter(|record| {
                record
                    .segment
                    .ordinal
                    .is_some_and(|ordinal| ordinal < count)
            })
            .collect(),
    };
    if data.is_empty() {
        return Err(DensePrefixError::Loading);
    }
    if data
        .iter()
        .any(|record| record.segment.writer != *expected_writer)
    {
        return Err(invalid("prefix records disagree on writer"));
    }

    // Ordinal order implies timestamp order; equal ordinals must carry equal
    // timestamps. Runs before the coordinate-twin check, so timestamp-
    // disagreeing twins are `Invalid`, not `Conflicted`.
    let mut stamped: Vec<(u32, DateTime<FixedOffset>)> = Vec::new();
    for record in &data {
        if let Some(ordinal) = record.segment.ordinal {
            let time = DateTime::parse_from_rfc3339(&record.segment.created_at)
                .map_err(|_| invalid("prefix flush has malformed timestamp"))?;
            stamped.push((ordinal, time));
        }
    }
    stamped.sort_by_key(|(ordinal, _)| *ordinal);
    for pair in stamped.windows(2) {
        let (left_ordinal, left_time) = &pair[0];
        let (right_ordinal, right_time) = &pair[1];
        let nondecreasing = if left_ordinal == right_ordinal {
            left_time == right_time
        } else {
            left_time <= right_time
        };
        if !nondecreasing {
            return Err(invalid("prefix flush timestamps decrease"));
        }
    }

    let mut identities: BTreeMap<&str, ObservedSegment<'_>> = BTreeMap::new();
    let mut slots: BTreeMap<u32, Vec<ObservedSegment<'_>>> = BTreeMap::new();
    for record in &data {
        if let Some(existing) = identities.insert(record.doc_id, *record) {
            if existing != *record {
                return Err(invalid(format!(
                    "physical segment identity {} has conflicting facts",
                    record.doc_id
                )));
            }
        }
        if let Some(ordinal) = record.segment.ordinal {
            slots.entry(ordinal).or_default().push(*record);
        }
    }
    for (ordinal, slot) in &slots {
        let first = &slot[0];
        if slot.iter().any(|other| other != first) {
            return Err(DensePrefixError::Conflicted { ordinal: *ordinal });
        }
    }

    let mut streams: Vec<ReconstructedStream> = Vec::new();
    let mut positions: BTreeSet<(u32, u32, bool)> = BTreeSet::new();
    let mut consumed: u32 = 0;
    for (ordinal, slot) in &slots {
        if *ordinal != consumed {
            break;
        }
        let record = slot[0];
        if record.segment.runs.is_empty() {
            return Err(invalid("prefix flush has no runs"));
        }
        let mut offset = 0usize;
        for run in &record.segment.runs {
            if run.bytes == 0 && run.declaration.is_none() {
                return Err(invalid("zero-byte continuation is not a flush"));
            }
            let Some(end) = offset.checked_add(
                usize::try_from(run.bytes).map_err(|_| invalid("run length overflows"))?,
            ) else {
                return Err(invalid("run length overflows the payload"));
            };
            let Some(part) = record.segment.payload.get(offset..end) else {
                return Err(invalid("runs split UTF-8 or exceed the payload"));
            };
            if let Some(declaration) = &run.declaration {
                if run.stream as usize != streams.len()
                    || !positions.insert((
                        declaration.block_index,
                        declaration.part_index,
                        matches!(declaration.payload, StreamPayload::ReasoningSignature),
                    ))
                {
                    return Err(invalid(
                        "stream declarations are not dense or positions conflict",
                    ));
                }
                streams.push(ReconstructedStream {
                    declaration: declaration.clone(),
                    text: part.to_owned(),
                });
            } else {
                let Some(existing) = streams.get_mut(run.stream as usize) else {
                    return Err(invalid("continuation before declaration"));
                };
                existing.text.push_str(part);
            }
            offset = end;
        }
        if offset != record.segment.payload.len() {
            return Err(invalid("runs do not partition the payload"));
        }
        consumed += 1;
    }
    if consumed == 0 {
        return Err(DensePrefixError::Loading);
    }
    let highest_visible_ordinal = slots.keys().next_back().copied();
    Ok(DensePrefix {
        segments: consumed,
        streams,
        highest_visible_ordinal,
    })
}

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

pub struct LiveTarget<'a> {
    pub request_doc_id: &'a str,
    pub source: &'a OutputSource,
    pub writer: &'a OutputWriter,
    pub message_id: Option<&'a str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveTargetSelection {
    Absent,
    Selected {
        source: OutputSource,
        writer: OutputWriter,
        message_id: Option<String>,
    },
    Conflicted,
}

/// Rust projection of Lean `StreamingResponse.selectTarget`.
pub fn select_live_target(
    request_doc_id: &str,
    execution_generation: &str,
    records: &[ObservedSegment<'_>],
    messages: &[(&str, &TranscriptMessage)],
) -> LiveTargetSelection {
    use crate::rendered_request::CaptureScopeKind;

    let selected = records
        .iter()
        .filter_map(|observed| {
            let OutputSource::ProviderTurn {
                scope,
                turn_index,
                attempt,
            } = &observed.segment.source
            else {
                return None;
            };
            let OutputWriter::RequestExecution {
                execution_generation: writer_generation,
            } = &observed.segment.writer
            else {
                return None;
            };
            (observed.segment.request_doc_id == request_doc_id
                && writer_generation == execution_generation
                && scope.kind == CaptureScopeKind::Inference)
                .then_some((
                    (*scope, *turn_index, *attempt),
                    &observed.segment.source,
                    &observed.segment.writer,
                ))
        })
        .max_by_key(|(coordinate, _, _)| *coordinate);
    let Some((_, source, writer)) = selected else {
        return LiveTargetSelection::Absent;
    };

    let ids = messages
        .iter()
        .filter_map(|(doc_id, message)| {
            (message.request_doc_id.as_deref() == Some(request_doc_id)
                && message.payload_references().iter().any(|reference| {
                    records.iter().any(|observed| {
                        observed.doc_id == reference.close_doc_id
                            && observed.segment.request_doc_id == request_doc_id
                            && observed.segment.source == *source
                            && observed.segment.writer == *writer
                            && observed.segment.close.is_some()
                    })
                }))
            .then_some((*doc_id).to_owned())
        })
        .collect::<std::collections::BTreeSet<_>>();
    if ids.len() > 1 {
        return LiveTargetSelection::Conflicted;
    }
    LiveTargetSelection::Selected {
        source: source.clone(),
        writer: writer.clone(),
        message_id: ids.into_iter().next(),
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

/// Lean's `ownerLive` for the target writer.
fn owner_live(observation: &LiveObservation<'_>) -> bool {
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

fn source_denied(observation: &LiveObservation<'_>) -> bool {
    observation.records.iter().any(|record| {
        record.segment.request_doc_id == observation.request_doc_id
            && record.segment.source == *observation.target.source
            && observation
                .denied_segments
                .iter()
                .any(|denied| denied == record.doc_id)
    })
}

fn target_scoped(observation: &LiveObservation<'_>) -> bool {
    observation.target.request_doc_id == observation.request_doc_id
}

fn lookup_observed<'a>(
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
enum CloseObservation_ {
    Open,
    Closed { outcome: OutputOutcome },
    Retracted,
    Conflict,
}

fn observe_close<'a>(
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

fn stream_referenced(message: &TranscriptMessage, close_doc_id: &str, stream: u32) -> bool {
    message
        .payload_references()
        .iter()
        .any(|reference| reference.close_doc_id == close_doc_id && reference.stream == stream)
}

fn classify_message_error(error: &ReconstructionError) -> LiveView {
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

fn resolve_terminal_payload(
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

fn project_unheaded_closed(
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

fn project_published<'a>(observation: &'a LiveObservation<'a>) -> LiveView {
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
