//! Sealed source reconstruction, refining CanonicalOutput/Reconstruction.lean.
//! Callers supply authorized observations and explicit denial evidence. Absence
//! never implies denial, and missing bytes never produce a shortened message.

use super::{
    MediaData, MediaKind, MediaType, MessageBlock, MessagePublication, MessageRole, OutputSegment,
    OutputSource, OutputWriter, PayloadPresentation, PayloadRef, PresentationPart,
    PresentedPayload, ReasoningPart, ReconstructionError, SourceClose, StreamDeclaration,
    StreamPayload, ToolResultPart, TranscriptMessage,
};
use crate::message::{
    AssistantContent, Audio, Document, DocumentSourceKind, Image, Message, Reasoning,
    ReasoningContent, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent, UserContent,
    Video,
};
use base64::Engine;
use rapidhash::{HashMapExt, RapidHashMap};
use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObservedSegment<'a> {
    pub doc_id: &'a str,
    pub segment: &'a OutputSegment,
}

/// Dependency membership AND denial established by the hydration/ACP owner.
/// A bare denied ID is insufficient to link an absent dependency to a closure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependencyDenial {
    pub root_close_id: String,
    pub denied_doc_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconstructedStream {
    pub declaration: StreamDeclaration,
    /// Exact UTF-8 payload; use `as_bytes()` for byte-oriented consumers.
    pub text: String,
}

/// Require one exact fact. Duplicate arrival is harmless; distinct physical
/// identities or contents conflict. No winner is selected from twins.
fn unique<'a>(
    mut records: impl Iterator<Item = ObservedSegment<'a>>,
    missing: ReconstructionError,
    conflict: ReconstructionError,
) -> Result<ObservedSegment<'a>, ReconstructionError> {
    let first = records.next().ok_or(missing)?;
    if records.all(|record| record == first) {
        Ok(first)
    } else {
        Err(conflict)
    }
}

fn writer_matches_source(source: &OutputSource, writer: &OutputWriter) -> bool {
    match (source, writer) {
        (OutputSource::ProviderTurn { .. }, OutputWriter::RequestExecution { .. }) => true,
        (OutputSource::ToolEffectArguments { .. }, OutputWriter::RequestExecution { .. }) => true,
        (
            OutputSource::ToolCall {
                tool_call_doc_id: source,
            },
            OutputWriter::ToolExecution {
                tool_call_doc_id: writer,
            },
        ) => source == writer,
        (OutputSource::Authored { .. }, _) => true,
        _ => false,
    }
}

fn invalid_extent(reference: &PayloadRef, bytes: u64) -> ReconstructionError {
    ReconstructionError::ExtentMismatch {
        reference: reference.clone(),
        bytes,
    }
}

/// Reconstruct a header's selected text without retaining a second payload
/// copy. Ranges are byte offsets in the sealed UTF-8 stream; literals are the
/// only small runtime-owned bytes allowed between selected output ranges.
pub fn reconstruct_presented_payload(
    records: &[ObservedSegment<'_>],
    denied: &[String],
    dependency_denials: &[DependencyDenial],
    payload: &PresentedPayload,
) -> Result<String, ReconstructionError> {
    let stream = reconstruct_stream(records, denied, dependency_denials, &payload.output)?;
    present_stream(&stream.text, payload)
}

fn present_stream(text: &str, payload: &PresentedPayload) -> Result<String, ReconstructionError> {
    match &payload.presentation {
        PayloadPresentation::Full => Ok(text.to_owned()),
        PayloadPresentation::Composed { parts } => {
            let mut rendered = String::new();
            for part in parts {
                match part {
                    PresentationPart::Literal { text } => rendered.push_str(text),
                    PresentationPart::OutputRange {
                        start_byte,
                        end_byte,
                    } => {
                        let start = usize::try_from(*start_byte).map_err(|_| {
                            ReconstructionError::InvalidPresentation {
                                reference: payload.output.clone(),
                            }
                        })?;
                        let end = usize::try_from(*end_byte).map_err(|_| {
                            ReconstructionError::InvalidPresentation {
                                reference: payload.output.clone(),
                            }
                        })?;
                        let selected = text.get(start..end).ok_or_else(|| {
                            ReconstructionError::InvalidPresentation {
                                reference: payload.output.clone(),
                            }
                        })?;
                        rendered.push_str(selected);
                    }
                }
            }
            Ok(rendered)
        }
    }
}

fn closing_for_reference<'a>(
    records: &[ObservedSegment<'a>],
    denied: &[String],
    dependency_denials: &[DependencyDenial],
    reference: &PayloadRef,
) -> Result<ObservedSegment<'a>, ReconstructionError> {
    // Keep authorization behavior identical to the stream primitive.
    if let Some(doc_id) = dependency_denials
        .iter()
        .filter(|d| d.root_close_id == reference.close_doc_id)
        .map(|d| &d.denied_doc_id)
        .min()
    {
        return Err(ReconstructionError::AccessDenied {
            doc_id: doc_id.clone(),
        });
    }
    if denied.contains(&reference.close_doc_id) {
        return Err(ReconstructionError::AccessDenied {
            doc_id: reference.close_doc_id.clone(),
        });
    }
    let closing = unique(
        records
            .iter()
            .copied()
            .filter(|record| record.doc_id == reference.close_doc_id),
        ReconstructionError::UnresolvedClose {
            close_doc_id: reference.close_doc_id.clone(),
        },
        ReconstructionError::InvalidStructure {
            detail: format!(
                "conflicting physical segment identity: {}",
                reference.close_doc_id
            ),
        },
    )?;
    if !matches!(closing.segment.close, Some(SourceClose::Closed { .. })) {
        return Err(ReconstructionError::InvalidReference {
            reference: reference.clone(),
        });
    }
    Ok(closing)
}

fn reference_allowed_by_publication(
    message: &TranscriptMessage,
    closing: ObservedSegment<'_>,
) -> bool {
    if closing.segment.source.is_auxiliary_audit() {
        return false;
    }
    match &message.publication {
        MessagePublication::RequestExecution {
            execution_generation,
        } => {
            message.request_doc_id.as_deref() == Some(&closing.segment.request_doc_id)
                && matches!(
                    (&closing.segment.source, &closing.segment.writer),
                    (OutputSource::ProviderTurn { .. }, OutputWriter::RequestExecution { execution_generation: writer })
                        | (OutputSource::Authored { .. }, OutputWriter::RequestExecution { execution_generation: writer })
                        if writer == execution_generation
                )
        }
        MessagePublication::RequestRecovery { .. } => {
            message.request_doc_id.as_deref() == Some(&closing.segment.request_doc_id)
                && matches!(
                    (&closing.segment.source, &closing.segment.writer),
                    (
                        OutputSource::ProviderTurn { .. },
                        OutputWriter::RequestExecution { .. }
                    )
                )
        }
        MessagePublication::ToolDelivery { tool_call_doc_id } => {
            match (&closing.segment.source, &closing.segment.writer) {
                (
                    OutputSource::ToolCall {
                        tool_call_doc_id: source,
                    },
                    OutputWriter::ToolExecution {
                        tool_call_doc_id: writer,
                    },
                ) => source == tool_call_doc_id && writer == tool_call_doc_id,
                (
                    OutputSource::Authored { .. },
                    OutputWriter::ToolExecution {
                        tool_call_doc_id: writer,
                    },
                ) => {
                    writer == tool_call_doc_id
                        && message.request_doc_id.as_deref()
                            == Some(&closing.segment.request_doc_id)
                }
                _ => false,
            }
        }
        MessagePublication::Fork { .. } => true,
    }
}

fn validate_reference_source(
    extents: &Extents<'_, '_>,
    message: &TranscriptMessage,
    reference: &PayloadRef,
) -> Result<(), ReconstructionError> {
    let closing = extents.closing(reference)?;
    if reference_allowed_by_publication(message, closing) {
        Ok(())
    } else {
        Err(ReconstructionError::InvalidStructure {
            detail: "payload source is not admitted by message publication".to_owned(),
        })
    }
}

struct SealedExtent {
    streams: Vec<ReconstructedStream>,
    signatures: RapidHashMap<(u32, u32), Vec<usize>>,
}

impl SealedExtent {
    fn new(streams: Vec<ReconstructedStream>) -> Self {
        let mut signatures: RapidHashMap<(u32, u32), Vec<usize>> = RapidHashMap::new();
        for (index, stream) in streams.iter().enumerate() {
            if matches!(
                stream.declaration.payload,
                StreamPayload::ReasoningSignature
            ) {
                signatures
                    .entry((
                        stream.declaration.block_index,
                        stream.declaration.part_index,
                    ))
                    .or_default()
                    .push(index);
            }
        }
        Self {
            streams,
            signatures,
        }
    }
}

struct ObservationIndex<'a> {
    by_doc: RapidHashMap<&'a str, Vec<ObservedSegment<'a>>>,
    by_source: RapidHashMap<(&'a str, &'a OutputSource), Vec<ObservedSegment<'a>>>,
}

impl<'a> ObservationIndex<'a> {
    fn new(records: &[ObservedSegment<'a>]) -> Self {
        let mut by_doc: RapidHashMap<&'a str, Vec<ObservedSegment<'a>>> = RapidHashMap::new();
        let mut by_source: RapidHashMap<(&'a str, &'a OutputSource), Vec<_>> = RapidHashMap::new();
        for record in records {
            by_doc.entry(record.doc_id).or_default().push(*record);
            by_source
                .entry((
                    record.segment.request_doc_id.as_str(),
                    &record.segment.source,
                ))
                .or_default()
                .push(*record);
        }
        Self { by_doc, by_source }
    }
}

struct Extents<'s, 'a> {
    records: &'s [ObservedSegment<'a>],
    denied: &'s [String],
    dependency_denials: &'s [DependencyDenial],
    #[cfg(test)]
    reuse: bool,
    index: OnceCell<ObservationIndex<'a>>,
    closings: RefCell<RapidHashMap<&'a str, ObservedSegment<'a>>>,
    extents: RefCell<RapidHashMap<&'a str, Rc<SealedExtent>>>,
    #[cfg(test)]
    closing_scans: std::cell::Cell<usize>,
    #[cfg(test)]
    extent_builds: std::cell::Cell<usize>,
}

impl<'s, 'a> Extents<'s, 'a> {
    fn new(
        records: &'s [ObservedSegment<'a>],
        denied: &'s [String],
        dependency_denials: &'s [DependencyDenial],
    ) -> Self {
        Self {
            records,
            denied,
            dependency_denials,
            #[cfg(test)]
            reuse: true,
            index: OnceCell::new(),
            closings: RefCell::default(),
            extents: RefCell::default(),
            #[cfg(test)]
            closing_scans: Default::default(),
            #[cfg(test)]
            extent_builds: Default::default(),
        }
    }

    fn reuses(&self) -> bool {
        #[cfg(test)]
        return self.reuse;
        #[cfg(not(test))]
        true
    }

    fn index(&self) -> &ObservationIndex<'a> {
        self.index
            .get_or_init(|| ObservationIndex::new(self.records))
    }

    fn closing(&self, reference: &PayloadRef) -> Result<ObservedSegment<'a>, ReconstructionError> {
        if self.reuses() {
            if let Some(closing) = self.closings.borrow().get(reference.close_doc_id.as_str()) {
                return Ok(*closing);
            }
        }
        #[cfg(test)]
        self.closing_scans.set(self.closing_scans.get() + 1);
        let candidates = if self.reuses() {
            self.index()
                .by_doc
                .get(reference.close_doc_id.as_str())
                .map_or(&[][..], Vec::as_slice)
        } else {
            self.records
        };
        let closing =
            closing_for_reference(candidates, self.denied, self.dependency_denials, reference)?;
        if self.reuses() {
            self.closings.borrow_mut().insert(closing.doc_id, closing);
        }
        Ok(closing)
    }

    /// Equal to `reconstruct_extent_streams(.., reference)`, with `reference.stream`
    /// guaranteed in bounds on success.
    fn extent(&self, reference: &PayloadRef) -> Result<Rc<SealedExtent>, ReconstructionError> {
        let reused = if self.reuses() {
            self.extents
                .borrow()
                .get(reference.close_doc_id.as_str())
                .cloned()
        } else {
            None
        };
        let extent = match reused {
            Some(extent) => extent,
            None => {
                let closing = self.closing(reference)?;
                #[cfg(test)]
                self.extent_builds.set(self.extent_builds.get() + 1);
                let same_source = if self.reuses() {
                    self.index()
                        .by_source
                        .get(&(
                            closing.segment.request_doc_id.as_str(),
                            &closing.segment.source,
                        ))
                        .map_or(&[][..], Vec::as_slice)
                } else {
                    self.records
                };
                let extent = Rc::new(SealedExtent::new(extent_streams_from_closing(
                    same_source,
                    self.denied,
                    closing,
                    reference,
                )?));
                if self.reuses() {
                    self.extents
                        .borrow_mut()
                        .insert(closing.doc_id, Rc::clone(&extent));
                }
                extent
            }
        };
        if (reference.stream as usize) < extent.streams.len() {
            Ok(extent)
        } else {
            Err(ReconstructionError::InvalidReference {
                reference: reference.clone(),
            })
        }
    }

    /// Equal to `reconstruct_stream(.., reference)`.
    fn stream(&self, reference: &PayloadRef) -> Result<ReconstructedStream, ReconstructionError> {
        Ok(self.extent(reference)?.streams[reference.stream as usize].clone())
    }
}

pub(super) fn reconstruct_extent_streams(
    records: &[ObservedSegment<'_>],
    denied: &[String],
    dependency_denials: &[DependencyDenial],
    reference: &PayloadRef,
) -> Result<Vec<ReconstructedStream>, ReconstructionError> {
    let closing = closing_for_reference(records, denied, dependency_denials, reference)?;
    extent_streams_from_closing(records, denied, closing, reference)
}

fn extent_streams_from_closing(
    records: &[ObservedSegment<'_>],
    denied: &[String],
    closing: ObservedSegment<'_>,
    reference: &PayloadRef,
) -> Result<Vec<ReconstructedStream>, ReconstructionError> {
    let Some(SourceClose::Closed {
        segments: count,
        stream_bytes,
        ..
    }) = &closing.segment.close
    else {
        unreachable!("closing_for_reference only returns closed records")
    };
    let Some(expected_bytes) = stream_bytes.get(reference.stream as usize) else {
        return Err(ReconstructionError::InvalidReference {
            reference: reference.clone(),
        });
    };
    let same_source = |r: &ObservedSegment<'_>| {
        r.segment.request_doc_id == closing.segment.request_doc_id
            && r.segment.source == closing.segment.source
    };
    let closure_conflict = ReconstructionError::ConflictingClosures {
        request_doc_id: closing.segment.request_doc_id.clone(),
        source: closing.segment.source.clone(),
    };
    let only = unique(
        records
            .iter()
            .copied()
            .filter(same_source)
            .filter(|r| r.segment.close.is_some()),
        closure_conflict.clone(),
        closure_conflict.clone(),
    )?;
    if only != closing {
        return Err(closure_conflict);
    }

    // Index once, excluding out-of-extent raw facts BEFORE twin checks. Never
    // allocate from an untrusted declared count or scan all records per ordinal.
    let mut slots: BTreeMap<u32, Vec<ObservedSegment<'_>>> = BTreeMap::new();
    let mut denied_in_extent = BTreeSet::new();
    for record in records.iter().copied().filter(same_source) {
        if let Some(ordinal) = record.segment.ordinal.filter(|n| n < count) {
            if denied.iter().any(|id| id == record.doc_id) {
                denied_in_extent.insert(record.doc_id);
            }
            slots.entry(ordinal).or_default().push(record);
        }
    }
    if let Some(doc_id) = denied_in_extent.first() {
        return Err(ReconstructionError::AccessDenied {
            doc_id: (*doc_id).to_owned(),
        });
    }
    let invalid_writer = || ReconstructionError::InvalidWriter {
        request_doc_id: closing.segment.request_doc_id.clone(),
        source: closing.segment.source.clone(),
    };
    let malformed = || ReconstructionError::ExtentMismatch {
        reference: reference.clone(),
        bytes: *expected_bytes,
    };
    if !writer_matches_source(&closing.segment.source, &closing.segment.writer) {
        return Err(invalid_writer());
    }
    if let Some(ordinal) = closing.segment.ordinal {
        if u64::from(ordinal) + 1 != u64::from(*count) {
            return Err(malformed());
        }
    } else if !closing.segment.runs.is_empty() || !closing.segment.payload.is_empty() {
        // Native shape check: Lean's Option Flush cannot represent this record.
        return Err(malformed());
    }
    let mut streams: Vec<ReconstructedStream> = Vec::new();
    let mut positions = BTreeSet::new();
    for ordinal in 0..*count {
        let missing = ReconstructionError::MissingSegment {
            close_doc_id: reference.close_doc_id.clone(),
            ordinal,
        };
        let slot = slots.get(&ordinal).ok_or_else(|| missing.clone())?;
        let record = unique(
            slot.iter().copied(),
            missing,
            ReconstructionError::ConflictingSegments {
                close_doc_id: reference.close_doc_id.clone(),
                ordinal,
            },
        )?;
        if record.segment.writer != closing.segment.writer {
            return Err(invalid_writer());
        }
        if record.segment.runs.is_empty() {
            return Err(malformed());
        }
        let mut offset = 0usize;
        for run in &record.segment.runs {
            if run.bytes == 0 && run.declaration.is_none() {
                return Err(malformed());
            }
            let end = offset
                .checked_add(usize::try_from(run.bytes).map_err(|_| malformed())?)
                .ok_or_else(malformed)?;
            // str::get rejects both out-of-bounds runs and split UTF-8 scalars.
            let part = record
                .segment
                .payload
                .get(offset..end)
                .ok_or_else(malformed)?;
            if let Some(declaration) = &run.declaration {
                if run.stream as usize != streams.len()
                    || !positions.insert((
                        declaration.block_index,
                        declaration.part_index,
                        matches!(declaration.payload, StreamPayload::ReasoningSignature),
                    ))
                {
                    return Err(malformed());
                }
                streams.push(ReconstructedStream {
                    declaration: declaration.clone(),
                    text: part.to_owned(),
                });
            } else {
                streams
                    .get_mut(run.stream as usize)
                    .ok_or_else(malformed)?
                    .text
                    .push_str(part);
            }
            offset = end;
        }
        if offset != record.segment.payload.len() {
            return Err(malformed());
        }
    }
    if streams.len() != stream_bytes.len()
        || streams
            .iter()
            .zip(stream_bytes)
            .any(|(s, n)| s.text.len() as u64 != *n)
    {
        return Err(malformed());
    }
    Ok(streams)
}

pub fn reconstruct_stream(
    records: &[ObservedSegment<'_>],
    denied: &[String],
    dependency_denials: &[DependencyDenial],
    reference: &PayloadRef,
) -> Result<ReconstructedStream, ReconstructionError> {
    reconstruct_extent_streams(records, denied, dependency_denials, reference)?
        .get(reference.stream as usize)
        .cloned()
        .ok_or_else(|| ReconstructionError::InvalidReference {
            reference: reference.clone(),
        })
}

fn expect_payload(
    stream: &ReconstructedStream,
    reference: &PayloadRef,
    allowed: &[fn(&StreamPayload) -> bool],
    presentation_is_full: bool,
    json: bool,
) -> Result<(), ReconstructionError> {
    if !allowed
        .iter()
        .any(|allowed| allowed(&stream.declaration.payload))
        || (!presentation_is_full
            && !matches!(
                stream.declaration.payload,
                StreamPayload::Text | StreamPayload::ToolOutput
            ))
    {
        return Err(invalid_extent(reference, stream.text.len() as u64));
    }
    if json && serde_json::from_str::<serde_json::Value>(&stream.text).is_err() {
        return Err(ReconstructionError::InvalidPayload {
            reference: reference.clone(),
        });
    }
    Ok(())
}

fn is_text(payload: &StreamPayload) -> bool {
    matches!(payload, StreamPayload::Text)
}
fn is_tool_output(payload: &StreamPayload) -> bool {
    matches!(payload, StreamPayload::ToolOutput)
}
fn is_reasoning(payload: &StreamPayload) -> bool {
    matches!(payload, StreamPayload::Reasoning)
}
fn is_reasoning_encrypted(payload: &StreamPayload) -> bool {
    matches!(payload, StreamPayload::ReasoningEncrypted)
}
fn is_reasoning_redacted(payload: &StreamPayload) -> bool {
    matches!(payload, StreamPayload::ReasoningRedacted)
}
fn is_reasoning_summary(payload: &StreamPayload) -> bool {
    matches!(payload, StreamPayload::ReasoningSummary)
}
fn is_arguments(payload: &StreamPayload) -> bool {
    matches!(payload, StreamPayload::ToolArguments { .. })
}

fn resolve_full(
    extents: &Extents<'_, '_>,
    reference: &PayloadRef,
    allowed: &[fn(&StreamPayload) -> bool],
    json: bool,
) -> Result<String, ReconstructionError> {
    let stream = extents.stream(reference)?;
    expect_payload(&stream, reference, allowed, true, json)?;
    Ok(stream.text)
}

fn resolve_presented(
    extents: &Extents<'_, '_>,
    payload: &PresentedPayload,
    allowed: &[fn(&StreamPayload) -> bool],
) -> Result<String, ReconstructionError> {
    let stream = extents.stream(&payload.output)?;
    expect_payload(
        &stream,
        &payload.output,
        allowed,
        matches!(payload.presentation, PayloadPresentation::Full),
        false,
    )?;
    present_stream(&stream.text, payload)
}

fn media_data(
    extents: &Extents<'_, '_>,
    media: &super::MediaBlock,
) -> Result<DocumentSourceKind, ReconstructionError> {
    let media_stream = |reference: &PayloadRef| {
        let stream = extents.stream(reference)?;
        match &stream.declaration.payload {
            StreamPayload::Media { media_kind } if media_kind == &media.kind => Ok(stream.text),
            _ => Err(invalid_extent(reference, stream.text.len() as u64)),
        }
    };
    match &media.data {
        MediaData::Url { url } => Ok(DocumentSourceKind::Url(url.clone())),
        MediaData::Unknown => Ok(DocumentSourceKind::Unknown),
        MediaData::Base64 { data } => Ok(DocumentSourceKind::Base64(media_stream(data)?)),
        MediaData::String { data } => Ok(DocumentSourceKind::String(media_stream(data)?)),
        MediaData::Raw { data } => {
            let encoded = media_stream(data)?;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| ReconstructionError::InvalidPayload {
                    reference: data.clone(),
                })?;
            Ok(DocumentSourceKind::Raw(decoded))
        }
    }
}

fn validate_media_shape(media: &super::MediaBlock) -> bool {
    matches!(
        (&media.kind, &media.media_type, &media.detail),
        (MediaKind::Image, Some(MediaType::Image(_)) | None, _)
            | (MediaKind::Audio, Some(MediaType::Audio(_)) | None, None)
            | (MediaKind::Video, Some(MediaType::Video(_)) | None, None)
            | (
                MediaKind::Document,
                Some(MediaType::Document(_)) | None,
                None
            )
    )
}

fn reconstruct_media(
    extents: &Extents<'_, '_>,
    media: &super::MediaBlock,
) -> Result<(MediaKind, DocumentSourceKind), ReconstructionError> {
    if !validate_media_shape(media) {
        return Err(ReconstructionError::InvalidStructure {
            detail: "media kind, media type, and detail are not a native combination".to_owned(),
        });
    }
    Ok((media.kind, media_data(extents, media)?))
}

fn validate_native_shape(message: &TranscriptMessage) -> Result<(), ReconstructionError> {
    let allowed = |block: &MessageBlock| match block {
        MessageBlock::Text { .. } => true,
        MessageBlock::Reasoning { .. } | MessageBlock::ToolCall { .. } => {
            message.role == MessageRole::Assistant
        }
        MessageBlock::ToolResult { parts, .. } => {
            message.role == MessageRole::User
                && parts.iter().all(|part| match part {
                    ToolResultPart::Text { .. } => true,
                    ToolResultPart::Media(media) => media.kind == MediaKind::Image,
                })
        }
        MessageBlock::Media(media) => {
            message.role == MessageRole::User
                || (message.role == MessageRole::Assistant && media.kind == MediaKind::Image)
        }
    };
    if !message.blocks.iter().all(allowed)
        || matches!(message.role, MessageRole::System)
            && (message.native_id.is_some()
                || !matches!(message.blocks.as_slice(), [MessageBlock::Text { .. }]))
        || matches!(message.role, MessageRole::User) && message.native_id.is_some()
    {
        return Err(ReconstructionError::InvalidStructure {
            detail: "illegal native message role or block shape".to_owned(),
        });
    }
    match &message.publication {
        MessagePublication::RequestExecution { .. } => {
            if message.request_doc_id.is_none() {
                return Err(ReconstructionError::InvalidStructure { detail: "request publication lacks request membership".to_owned() });
            }
        }
        MessagePublication::RequestRecovery { .. } => {
            if message.request_doc_id.is_none()
                || message.role != MessageRole::Assistant
                || message.outcome != super::OutputOutcome::Partial
                || message.native_id.is_some()
                || !message.blocks.iter().all(|block| matches!(block, MessageBlock::Text { text } if matches!(text.presentation, PayloadPresentation::Full)))
            {
                return Err(ReconstructionError::InvalidStructure { detail: "invalid recovery header".to_owned() });
            }
        }
        MessagePublication::ToolDelivery { tool_call_doc_id } => {
            if message.request_doc_id.is_none()
                || message.role != MessageRole::User
                || message.blocks.iter().any(|block| match block {
                    MessageBlock::Text { .. } => false,
                    MessageBlock::ToolResult { tool_call_doc_id: id, .. } => id != tool_call_doc_id,
                    _ => true,
                })
            {
                return Err(ReconstructionError::InvalidStructure { detail: "invalid tool delivery header".to_owned() });
            }
        }
        MessagePublication::Fork { origin_message_doc_id } => {
            if message.request_doc_id.is_some() || origin_message_doc_id.is_empty() {
                return Err(ReconstructionError::InvalidStructure { detail: "invalid fork provenance".to_owned() });
            }
        }
    }
    Ok(())
}

fn validate_reasoning_signature(
    extents: &Extents<'_, '_>,
    body_reference: &PayloadRef,
    signature: Option<&str>,
) -> Result<(), ReconstructionError> {
    let extent = extents.extent(body_reference)?;
    let body = &extent.streams[body_reference.stream as usize];
    let mut signatures = extent
        .signatures
        .get(&(body.declaration.block_index, body.declaration.part_index))
        .into_iter()
        .flatten()
        .map(|index| &extent.streams[*index]);
    let Some(stream) = signatures.next() else {
        if signature.is_none()
            || matches!(
                extents.closing(body_reference)?.segment.source,
                OutputSource::Authored { .. }
            )
        {
            return Ok(());
        }
        return Err(ReconstructionError::InvalidStructure {
            detail: "reasoning signature has no retained stream".to_owned(),
        });
    };
    if signatures.next().is_some() || signature != Some(stream.text.as_str()) {
        return Err(ReconstructionError::InvalidStructure {
            detail: "reasoning signature disagrees with its retained stream".to_owned(),
        });
    }
    Ok(())
}

/// Reconstruct one complete canonical transcript header into its native message.
/// This is the sole protocol-level path for history, provider input, forks and
/// exports: it validates immutable provenance before exposing any bytes.
pub fn reconstruct_message(
    records: &[ObservedSegment<'_>],
    denied: &[String],
    dependency_denials: &[DependencyDenial],
    message: &TranscriptMessage,
) -> Result<Message, ReconstructionError> {
    reconstruct_message_with(&Extents::new(records, denied, dependency_denials), message)
}

fn reconstruct_message_with(
    extents: &Extents<'_, '_>,
    message: &TranscriptMessage,
) -> Result<Message, ReconstructionError> {
    validate_native_shape(message)?;

    let validate_reference =
        |reference: &PayloadRef| validate_reference_source(extents, message, reference);
    // Closures/source provenance are validated before native decoding.
    let mut provenance_failure = None;
    for reference in message.payload_references() {
        if provenance_failure.is_none() {
            provenance_failure = validate_reference(reference).err();
        }
    }
    if let Some(error) = provenance_failure {
        return Err(error);
    }

    let mut provider_positions: Vec<(OutputSource, u32, u32)> = Vec::new();
    let mut check_position = |reference: &PayloadRef,
                              block: u32,
                              part: u32,
                              exact: bool|
     -> Result<(), ReconstructionError> {
        let closing = extents.closing(reference)?;
        if let OutputSource::ProviderTurn { .. } = &closing.segment.source {
            let extent = extents.extent(reference)?;
            let stream = &extent.streams[reference.stream as usize];
            if exact
                && (stream.declaration.block_index != block
                    || stream.declaration.part_index != part)
            {
                return Err(invalid_extent(reference, stream.text.len() as u64));
            }
            if matches!(
                message.publication,
                MessagePublication::RequestRecovery { .. }
            ) && (!matches!(stream.declaration.payload, StreamPayload::Text)
                || stream.declaration.part_index != 0)
            {
                return Err(invalid_extent(reference, stream.text.len() as u64));
            }
            provider_positions.push((
                closing.segment.source.clone(),
                stream.declaration.block_index,
                stream.declaration.part_index,
            ));
        }
        Ok(())
    };
    for (block_index, block) in message.blocks.iter().enumerate() {
        let block_index =
            u32::try_from(block_index).map_err(|_| ReconstructionError::InvalidStructure {
                detail: "too many message blocks".to_owned(),
            })?;
        match block {
            MessageBlock::Text { text } => check_position(
                &text.output,
                block_index,
                0,
                matches!(
                    message.publication,
                    MessagePublication::RequestExecution { .. }
                ),
            )?,
            MessageBlock::Reasoning { parts, .. } => {
                for (part_index, part) in parts.iter().enumerate() {
                    let reference = match part {
                        ReasoningPart::Text { text, .. } | ReasoningPart::Summary { text } => text,
                        ReasoningPart::Encrypted { data } | ReasoningPart::Redacted { data } => {
                            data
                        }
                    };
                    check_position(
                        reference,
                        block_index,
                        u32::try_from(part_index).map_err(|_| {
                            ReconstructionError::InvalidStructure {
                                detail: "too many message parts".to_owned(),
                            }
                        })?,
                        matches!(
                            message.publication,
                            MessagePublication::RequestExecution { .. }
                        ),
                    )?;
                }
            }
            MessageBlock::ToolCall { arguments, .. } => check_position(
                arguments,
                block_index,
                0,
                matches!(
                    message.publication,
                    MessagePublication::RequestExecution { .. }
                ),
            )?,
            MessageBlock::ToolResult { parts, .. } => {
                for (part_index, part) in parts.iter().enumerate() {
                    let part_index = u32::try_from(part_index).map_err(|_| {
                        ReconstructionError::InvalidStructure {
                            detail: "too many message parts".to_owned(),
                        }
                    })?;
                    match part {
                        ToolResultPart::Text { text } => check_position(
                            &text.output,
                            block_index,
                            part_index,
                            matches!(
                                message.publication,
                                MessagePublication::RequestExecution { .. }
                            ),
                        )?,
                        ToolResultPart::Media(media) => {
                            if let MediaData::Base64 { data }
                            | MediaData::Raw { data }
                            | MediaData::String { data } = &media.data
                            {
                                check_position(
                                    data,
                                    block_index,
                                    part_index,
                                    matches!(
                                        message.publication,
                                        MessagePublication::RequestExecution { .. }
                                    ),
                                )?;
                            }
                        }
                    }
                }
            }
            MessageBlock::Media(media) => {
                if let MediaData::Base64 { data }
                | MediaData::Raw { data }
                | MediaData::String { data } = &media.data
                {
                    check_position(
                        data,
                        block_index,
                        0,
                        matches!(
                            message.publication,
                            MessagePublication::RequestExecution { .. }
                        ),
                    )?;
                }
            }
        }
    }
    for pair in provider_positions.windows(2) {
        if pair[0].0 == pair[1].0 && (pair[0].1, pair[0].2) >= (pair[1].1, pair[1].2) {
            return Err(ReconstructionError::InvalidStructure {
                detail: "provider payload declarations are out of native order".to_owned(),
            });
        }
    }

    let assistant_content =
        |block: &MessageBlock| -> Result<AssistantContent, ReconstructionError> {
            match block {
                MessageBlock::Text { text } => Ok(AssistantContent::Text(Text {
                    text: resolve_presented(extents, text, &[is_text, is_tool_output])?,
                })),
                MessageBlock::Reasoning { id, parts } => {
                    Ok(AssistantContent::Reasoning(Reasoning {
                        id: id.clone(),
                        content: parts
                            .iter()
                            .map(|part| match part {
                                ReasoningPart::Text { text, signature } => {
                                    validate_reasoning_signature(
                                        extents,
                                        text,
                                        signature.as_deref(),
                                    )?;
                                    Ok(ReasoningContent::Text {
                                        text: resolve_full(extents, text, &[is_reasoning], false)?,
                                        signature: signature.clone(),
                                    })
                                }
                                ReasoningPart::Encrypted { data } => {
                                    Ok(ReasoningContent::Encrypted(resolve_full(
                                        extents,
                                        data,
                                        &[is_reasoning_encrypted],
                                        false,
                                    )?))
                                }
                                ReasoningPart::Redacted { data } => {
                                    Ok(ReasoningContent::Redacted {
                                        data: resolve_full(
                                            extents,
                                            data,
                                            &[is_reasoning_redacted],
                                            false,
                                        )?,
                                    })
                                }
                                ReasoningPart::Summary { text } => Ok(ReasoningContent::Summary(
                                    resolve_full(extents, text, &[is_reasoning_summary], false)?,
                                )),
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                    }))
                }
                MessageBlock::ToolCall {
                    id,
                    call_id,
                    name,
                    arguments,
                    signature,
                    additional_params,
                    ..
                } => {
                    let stream = extents.stream(arguments)?;
                    if !matches!(&stream.declaration.payload, StreamPayload::ToolArguments { id: declared_id, call_id: declared_call_id, name: declared_name } if declared_id == id && declared_call_id == call_id && declared_name == name)
                    {
                        return Err(invalid_extent(arguments, stream.text.len() as u64));
                    }
                    let argument_reference = arguments.clone();
                    expect_payload(&stream, arguments, &[is_arguments], true, true)?;
                    let arguments = stream.text;
                    let arguments = serde_json::from_str(&arguments).map_err(|_| {
                        ReconstructionError::InvalidPayload {
                            reference: argument_reference,
                        }
                    })?;
                    Ok(AssistantContent::ToolCall(ToolCall {
                        id: id.clone(),
                        call_id: call_id.clone(),
                        function: ToolFunction {
                            name: name.clone(),
                            arguments,
                        },
                        signature: signature.clone(),
                        additional_params: additional_params.clone(),
                    }))
                }
                MessageBlock::Media(media) if media.kind == MediaKind::Image => {
                    let (_, data) = reconstruct_media(extents, media)?;
                    Ok(AssistantContent::Image(Image {
                        data,
                        media_type: match &media.media_type {
                            Some(MediaType::Image(value)) => Some(value.clone()),
                            _ => None,
                        },
                        detail: media.detail.clone(),
                        additional_params: media.additional_params.clone(),
                    }))
                }
                _ => Err(ReconstructionError::InvalidStructure {
                    detail: "non-assistant block in assistant message".to_owned(),
                }),
            }
        };
    match message.role {
        MessageRole::System => match &message.blocks[0] {
            MessageBlock::Text { text } => Ok(Message::System {
                content: resolve_presented(extents, text, &[is_text, is_tool_output])?,
            }),
            _ => unreachable!(),
        },
        MessageRole::Assistant => Ok(Message::Assistant {
            id: message.native_id.clone(),
            content: message
                .blocks
                .iter()
                .map(assistant_content)
                .collect::<Result<Vec<_>, _>>()?,
        }),
        MessageRole::User => {
            let mut content = Vec::new();
            for block in &message.blocks {
                match block {
                    MessageBlock::Text { text } => content.push(UserContent::Text(Text {
                        text: resolve_presented(extents, text, &[is_text, is_tool_output])?,
                    })),
                    MessageBlock::ToolResult {
                        id, call_id, parts, ..
                    } => content.push(UserContent::ToolResult(ToolResult {
                        id: id.clone(),
                        call_id: call_id.clone(),
                        content: parts
                            .iter()
                            .map(|part| match part {
                                ToolResultPart::Text { text } => {
                                    Ok(ToolResultContent::Text(Text {
                                        text: resolve_presented(extents, text, &[is_tool_output])?,
                                    }))
                                }
                                ToolResultPart::Media(media) => {
                                    let (_, data) = reconstruct_media(extents, media)?;
                                    Ok(ToolResultContent::Image(Image {
                                        data,
                                        media_type: match &media.media_type {
                                            Some(MediaType::Image(value)) => Some(value.clone()),
                                            _ => None,
                                        },
                                        detail: media.detail.clone(),
                                        additional_params: media.additional_params.clone(),
                                    }))
                                }
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                    })),
                    MessageBlock::Media(media) => {
                        let (kind, data) = reconstruct_media(extents, media)?;
                        match (kind, &media.media_type) {
                            (MediaKind::Image, Some(MediaType::Image(media_type))) => {
                                content.push(UserContent::Image(Image {
                                    data,
                                    media_type: Some(media_type.clone()),
                                    detail: media.detail.clone(),
                                    additional_params: media.additional_params.clone(),
                                }))
                            }
                            (MediaKind::Image, None) => content.push(UserContent::Image(Image {
                                data,
                                media_type: None,
                                detail: media.detail.clone(),
                                additional_params: media.additional_params.clone(),
                            })),
                            (MediaKind::Audio, Some(MediaType::Audio(media_type))) => {
                                content.push(UserContent::Audio(Audio {
                                    data,
                                    media_type: Some(media_type.clone()),
                                    additional_params: media.additional_params.clone(),
                                }))
                            }
                            (MediaKind::Audio, None) => content.push(UserContent::Audio(Audio {
                                data,
                                media_type: None,
                                additional_params: media.additional_params.clone(),
                            })),
                            (MediaKind::Video, Some(MediaType::Video(media_type))) => {
                                content.push(UserContent::Video(Video {
                                    data,
                                    media_type: Some(media_type.clone()),
                                    additional_params: media.additional_params.clone(),
                                }))
                            }
                            (MediaKind::Video, None) => content.push(UserContent::Video(Video {
                                data,
                                media_type: None,
                                additional_params: media.additional_params.clone(),
                            })),
                            (MediaKind::Document, Some(MediaType::Document(media_type))) => content
                                .push(UserContent::Document(Document {
                                    data,
                                    media_type: Some(media_type.clone()),
                                    additional_params: media.additional_params.clone(),
                                })),
                            (MediaKind::Document, None) => {
                                content.push(UserContent::Document(Document {
                                    data,
                                    media_type: None,
                                    additional_params: media.additional_params.clone(),
                                }))
                            }
                            _ => {
                                return Err(ReconstructionError::InvalidStructure {
                                    detail: "media type does not match native content".to_owned(),
                                })
                            }
                        }
                    }
                    _ => {
                        return Err(ReconstructionError::InvalidStructure {
                            detail: "non-user block in user message".to_owned(),
                        })
                    }
                }
            }
            Ok(Message::User { content })
        }
    }
}
