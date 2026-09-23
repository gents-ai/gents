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
use std::collections::{BTreeMap, BTreeSet};

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
    records: &[ObservedSegment<'_>],
    denied: &[String],
    dependency_denials: &[DependencyDenial],
    message: &TranscriptMessage,
    reference: &PayloadRef,
) -> Result<(), ReconstructionError> {
    let closing = closing_for_reference(records, denied, dependency_denials, reference)?;
    if reference_allowed_by_publication(message, closing) {
        Ok(())
    } else {
        Err(ReconstructionError::InvalidStructure {
            detail: "payload source is not admitted by message publication".to_owned(),
        })
    }
}

pub fn reconstruct_stream(
    records: &[ObservedSegment<'_>],
    denied: &[String],
    dependency_denials: &[DependencyDenial],
    reference: &PayloadRef,
) -> Result<ReconstructedStream, ReconstructionError> {
    let closing = closing_for_reference(records, denied, dependency_denials, reference)?;
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
                    || !positions.insert((declaration.block_index, declaration.part_index))
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
    Ok(streams.swap_remove(reference.stream as usize))
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
fn is_reasoning_opaque(payload: &StreamPayload) -> bool {
    matches!(payload, StreamPayload::ReasoningOpaque)
}
fn is_reasoning_summary(payload: &StreamPayload) -> bool {
    matches!(payload, StreamPayload::ReasoningSummary)
}
fn is_arguments(payload: &StreamPayload) -> bool {
    matches!(payload, StreamPayload::ToolArguments { .. })
}

fn resolve_full(
    records: &[ObservedSegment<'_>],
    denied: &[String],
    dependency_denials: &[DependencyDenial],
    reference: &PayloadRef,
    allowed: &[fn(&StreamPayload) -> bool],
    json: bool,
) -> Result<String, ReconstructionError> {
    let stream = reconstruct_stream(records, denied, dependency_denials, reference)?;
    expect_payload(&stream, reference, allowed, true, json)?;
    Ok(stream.text)
}

fn resolve_presented(
    records: &[ObservedSegment<'_>],
    denied: &[String],
    dependency_denials: &[DependencyDenial],
    payload: &PresentedPayload,
    allowed: &[fn(&StreamPayload) -> bool],
) -> Result<String, ReconstructionError> {
    let stream = reconstruct_stream(records, denied, dependency_denials, &payload.output)?;
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
    records: &[ObservedSegment<'_>],
    denied: &[String],
    dependency_denials: &[DependencyDenial],
    media: &super::MediaBlock,
) -> Result<DocumentSourceKind, ReconstructionError> {
    let media_stream = |reference: &PayloadRef| {
        let stream = reconstruct_stream(records, denied, dependency_denials, reference)?;
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
    match (&media.kind, &media.media_type, &media.detail) {
        (MediaKind::Image, Some(MediaType::Image(_)) | None, _) => true,
        (MediaKind::Audio, Some(MediaType::Audio(_)) | None, None) => true,
        (MediaKind::Video, Some(MediaType::Video(_)) | None, None) => true,
        (MediaKind::Document, Some(MediaType::Document(_)) | None, None) => true,
        _ => false,
    }
}

fn reconstruct_media(
    records: &[ObservedSegment<'_>],
    denied: &[String],
    dependency_denials: &[DependencyDenial],
    media: &super::MediaBlock,
) -> Result<(MediaKind, DocumentSourceKind), ReconstructionError> {
    if !validate_media_shape(media) {
        return Err(ReconstructionError::InvalidStructure {
            detail: "media kind, media type, and detail are not a native combination".to_owned(),
        });
    }
    Ok((
        media.kind,
        media_data(records, denied, dependency_denials, media)?,
    ))
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

/// Reconstruct one complete canonical transcript header into its native message.
/// This is the sole protocol-level path for history, provider input, forks and
/// exports: it validates immutable provenance before exposing any bytes.
pub fn reconstruct_message(
    records: &[ObservedSegment<'_>],
    denied: &[String],
    dependency_denials: &[DependencyDenial],
    message: &TranscriptMessage,
) -> Result<Message, ReconstructionError> {
    validate_native_shape(message)?;

    let validate_reference = |reference: &PayloadRef| {
        validate_reference_source(records, denied, dependency_denials, message, reference)
    };
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
        let closing = closing_for_reference(records, denied, dependency_denials, reference)?;
        if let OutputSource::ProviderTurn { .. } = &closing.segment.source {
            let stream = reconstruct_stream(records, denied, dependency_denials, reference)?;
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
                    text: resolve_presented(
                        records,
                        denied,
                        dependency_denials,
                        text,
                        &[is_text, is_tool_output],
                    )?,
                })),
                MessageBlock::Reasoning { id, parts } => {
                    Ok(AssistantContent::Reasoning(Reasoning {
                        id: id.clone(),
                        content: parts
                            .iter()
                            .map(|part| match part {
                                ReasoningPart::Text { text, signature } => {
                                    Ok(ReasoningContent::Text {
                                        text: resolve_full(
                                            records,
                                            denied,
                                            dependency_denials,
                                            text,
                                            &[is_reasoning],
                                            false,
                                        )?,
                                        signature: signature.clone(),
                                    })
                                }
                                ReasoningPart::Encrypted { data } => {
                                    Ok(ReasoningContent::Encrypted(resolve_full(
                                        records,
                                        denied,
                                        dependency_denials,
                                        data,
                                        &[is_reasoning_opaque],
                                        false,
                                    )?))
                                }
                                ReasoningPart::Redacted { data } => {
                                    Ok(ReasoningContent::Redacted {
                                        data: resolve_full(
                                            records,
                                            denied,
                                            dependency_denials,
                                            data,
                                            &[is_reasoning_opaque],
                                            false,
                                        )?,
                                    })
                                }
                                ReasoningPart::Summary { text } => {
                                    Ok(ReasoningContent::Summary(resolve_full(
                                        records,
                                        denied,
                                        dependency_denials,
                                        text,
                                        &[is_reasoning_summary],
                                        false,
                                    )?))
                                }
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
                    let stream =
                        reconstruct_stream(records, denied, dependency_denials, arguments)?;
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
                    let (_, data) = reconstruct_media(records, denied, dependency_denials, media)?;
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
                content: resolve_presented(
                    records,
                    denied,
                    dependency_denials,
                    text,
                    &[is_text, is_tool_output],
                )?,
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
                        text: resolve_presented(
                            records,
                            denied,
                            dependency_denials,
                            text,
                            &[is_text, is_tool_output],
                        )?,
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
                                        text: resolve_presented(
                                            records,
                                            denied,
                                            dependency_denials,
                                            text,
                                            &[is_tool_output],
                                        )?,
                                    }))
                                }
                                ToolResultPart::Media(media) => {
                                    let (_, data) = reconstruct_media(
                                        records,
                                        denied,
                                        dependency_denials,
                                        media,
                                    )?;
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
                        let (kind, data) =
                            reconstruct_media(records, denied, dependency_denials, media)?;
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{OutputOutcome, SegmentRun, StreamPayload};
    use crate::rendered_request::{CaptureScope, CaptureScopeKind};

    fn provider_source() -> OutputSource {
        OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: 1,
            },
            turn_index: 0,
            attempt: 0,
        }
    }

    fn request_writer() -> OutputWriter {
        OutputWriter::RequestExecution {
            execution_generation: "gen-1".to_string(),
        }
    }

    fn text_declaration(block_index: u32) -> StreamDeclaration {
        StreamDeclaration {
            block_index,
            part_index: 0,
            payload: StreamPayload::Text,
        }
    }

    fn run(stream: u32, bytes: u32, declaration: Option<StreamDeclaration>) -> SegmentRun {
        SegmentRun {
            stream,
            bytes,
            declaration,
        }
    }

    fn closed(segments: u32, stream_bytes: Vec<u64>) -> Option<SourceClose> {
        Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments,
            stream_bytes,
        })
    }

    fn segment(
        doc_id: &str,
        ordinal: Option<u32>,
        runs: Vec<SegmentRun>,
        payload: &str,
        close: Option<SourceClose>,
    ) -> (String, OutputSegment) {
        (
            doc_id.to_string(),
            OutputSegment {
                agent_did: "did:key:z6MkAgent".to_string(),
                requester_did: None,
                session_id: "session-1".to_string(),
                request_doc_id: "request-1".to_string(),
                source: provider_source(),
                writer: request_writer(),
                ordinal,
                runs,
                payload: payload.to_string(),
                close,
                created_at: "2025-01-01T00:00:00Z".to_string(),
            },
        )
    }

    fn observations(records: &[(String, OutputSegment)]) -> Vec<ObservedSegment<'_>> {
        records
            .iter()
            .map(|(doc_id, segment)| ObservedSegment { doc_id, segment })
            .collect()
    }

    fn reconstruct(
        records: &[(String, OutputSegment)],
        reference: &PayloadRef,
    ) -> Result<ReconstructedStream, ReconstructionError> {
        reconstruct_stream(&observations(records), &[], &[], reference)
    }

    fn reconstruct_denied(
        records: &[(String, OutputSegment)],
        denied: &[String],
        dependency_denials: &[DependencyDenial],
        reference: &PayloadRef,
    ) -> Result<ReconstructedStream, ReconstructionError> {
        reconstruct_stream(
            &observations(records),
            denied,
            dependency_denials,
            reference,
        )
    }

    fn reference(close_doc_id: &str, stream: u32) -> PayloadRef {
        PayloadRef {
            close_doc_id: close_doc_id.to_string(),
            stream,
        }
    }

    /// Two-flush single-stream source: "he" + "llo" sealed as 5 bytes.
    fn hello_source() -> Vec<(String, OutputSegment)> {
        vec![
            segment(
                "flush-0",
                Some(0),
                vec![run(0, 2, Some(text_declaration(0)))],
                "he",
                None,
            ),
            segment(
                "close-1",
                Some(1),
                vec![run(0, 3, None)],
                "llo",
                closed(2, vec![5]),
            ),
        ]
    }

    #[test]
    fn reconstructs_sealed_stream_with_declaration_and_text() {
        let records = hello_source();
        let stream = reconstruct(&records, &reference("close-1", 0)).expect("reconstructs");
        assert_eq!(stream.declaration, text_declaration(0));
        assert_eq!(stream.text.as_bytes(), b"hello".to_vec());
        assert_eq!(stream.text, "hello");
    }

    #[test]
    fn conflicting_physical_close_identity_is_arrival_order_independent() {
        let mut records = hello_source();
        let mut conflicting = records[1].clone();
        conflicting.1.request_doc_id = "different-request".into();
        records.push(conflicting);
        let forward = reconstruct(&records, &reference("close-1", 0));
        records.reverse();
        assert_eq!(forward, reconstruct(&records, &reference("close-1", 0)));
        assert!(matches!(
            forward,
            Err(ReconstructionError::InvalidStructure { .. })
        ));
    }

    #[test]
    fn terminal_only_closure_rejects_unindexed_payload() {
        let records = vec![
            segment(
                "flush-0",
                Some(0),
                vec![run(0, 5, Some(text_declaration(0)))],
                "hello",
                None,
            ),
            segment("close-1", None, Vec::new(), "stray", closed(1, vec![5])),
        ];
        assert!(matches!(
            reconstruct(&records, &reference("close-1", 0)),
            Err(ReconstructionError::ExtentMismatch { .. })
        ));
    }

    #[test]
    fn exact_duplicate_delivery_is_idempotent() {
        let mut records = hello_source();
        // Same physical ID, same content: a replay, not a twin.
        records.push(records[0].clone());
        let stream = reconstruct(&records, &reference("close-1", 0)).expect("reconstructs");
        assert_eq!(stream.text, "hello");
    }

    #[test]
    fn interleaved_exact_close_replays_are_idempotent_but_physical_twins_conflict() {
        let source = hello_source();
        let mut records = vec![
            source[1].clone(),
            source[0].clone(),
            source[1].clone(),
            source[0].clone(),
        ];
        let expected = reconstruct(&source, &reference("close-1", 0));
        assert!(expected.is_ok());
        for _ in 0..records.len() {
            assert_eq!(reconstruct(&records, &reference("close-1", 0)), expected);
            records.rotate_left(1);
        }
        let mut twin = source[1].clone();
        twin.0 = "distinct-physical-close".into();
        records.push(twin);
        for _ in 0..records.len() {
            assert!(matches!(
                reconstruct(&records, &reference("close-1", 0)),
                Err(ReconstructionError::ConflictingClosures { .. })
            ));
            records.rotate_left(1);
        }
    }

    #[test]
    fn order_independent_across_shuffled_records() {
        let mut records = hello_source();
        records.reverse();
        let stream = reconstruct(&records, &reference("close-1", 0)).expect("reconstructs");
        assert_eq!(stream.text, "hello");
    }

    #[test]
    fn missing_closing_record_is_unresolved_not_shortened() {
        let records = hello_source();
        let error =
            reconstruct(&records, &reference("absent", 0)).expect_err("absent close is unresolved");
        assert_eq!(
            error,
            ReconstructionError::UnresolvedClose {
                close_doc_id: "absent".to_string()
            }
        );
    }

    #[test]
    fn missing_in_extent_flush_is_incomplete_not_shortened() {
        let mut records = hello_source();
        records.remove(0); // ordinal 0 is gone; extent is 0..2
        let error = reconstruct(&records, &reference("close-1", 0))
            .expect_err("missing ordinal is incomplete");
        assert_eq!(
            error,
            ReconstructionError::MissingSegment {
                close_doc_id: "close-1".to_string(),
                ordinal: 0,
            }
        );
    }

    #[test]
    fn twin_flush_at_same_ordinal_is_a_conflict() {
        let mut records = hello_source();
        // Same coordinate, different content and identity: a twin.
        let (twin_id, mut twin) = segment(
            "flush-0-twin",
            Some(0),
            vec![run(0, 2, Some(text_declaration(0)))],
            "HE",
            None,
        );
        twin.created_at = "2025-01-01T00:00:01Z".to_string();
        records.push((twin_id, twin));
        let error =
            reconstruct(&records, &reference("close-1", 0)).expect_err("twin flush is a conflict");
        assert_eq!(
            error,
            ReconstructionError::ConflictingSegments {
                close_doc_id: "close-1".to_string(),
                ordinal: 0,
            }
        );
    }

    #[test]
    fn twin_closures_are_a_conflict() {
        let mut records = hello_source();
        // A second distinct closure record for the same source coordinate.
        let (twin_id, twin) = segment("close-2", None, Vec::new(), "", closed(2, vec![5]));
        records.push((twin_id, twin));
        let error = reconstruct(&records, &reference("close-1", 0))
            .expect_err("twin closures are a conflict");
        assert_eq!(
            error,
            ReconstructionError::ConflictingClosures {
                request_doc_id: "request-1".to_string(),
                source: provider_source(),
            }
        );
    }

    #[test]
    fn reference_to_plain_flush_is_invalid() {
        let records = hello_source();
        let error = reconstruct(&records, &reference("flush-0", 0))
            .expect_err("plain flush is not a closure");
        assert_eq!(
            error,
            ReconstructionError::InvalidReference {
                reference: reference("flush-0", 0),
            }
        );
    }

    #[test]
    fn out_of_range_stream_is_invalid_reference() {
        let records = hello_source();
        let error =
            reconstruct(&records, &reference("close-1", 1)).expect_err("stream 1 is not sealed");
        assert_eq!(
            error,
            ReconstructionError::InvalidReference {
                reference: reference("close-1", 1),
            }
        );
    }

    #[test]
    fn late_out_of_extent_flush_is_inert() {
        let mut records = hello_source();
        // Recovery closed ordinals 0..2; a superseded writer's late flush at
        // ordinal 2 lands beyond the extent and must not collide or corrupt.
        let (late_id, late) = segment("late-2", Some(2), vec![run(0, 9, None)], "garbage!!", None);
        records.push((late_id, late));
        let stream = reconstruct(&records, &reference("close-1", 0)).expect("reconstructs");
        assert_eq!(stream.text, "hello");
    }

    #[test]
    fn terminal_only_closure_closes_flushed_extent() {
        // Everything was already flushed; closure arrives without new bytes.
        let records = vec![
            segment(
                "flush-0",
                Some(0),
                vec![run(0, 5, Some(text_declaration(0)))],
                "hello",
                None,
            ),
            segment("close-1", None, Vec::new(), "", closed(1, vec![5])),
        ];
        let stream = reconstruct(&records, &reference("close-1", 0)).expect("reconstructs");
        assert_eq!(stream.text, "hello");
    }

    #[test]
    fn explicitly_declared_empty_stream_is_preserved() {
        // A zero-stream source has segments = 0 and no stream_bytes; stream 0
        // is not sealed in it.
        let records = vec![segment("close-0", None, Vec::new(), "", closed(0, vec![]))];
        let error = reconstruct(&records, &reference("close-0", 0))
            .expect_err("no stream is sealed in a zero-stream source");
        assert_eq!(
            error,
            ReconstructionError::InvalidReference {
                reference: reference("close-0", 0),
            }
        );

        let records = vec![
            segment(
                "flush-0",
                Some(0),
                vec![run(0, 0, Some(text_declaration(0)))],
                "",
                None,
            ),
            segment("close-1", None, Vec::new(), "", closed(1, vec![0])),
        ];
        let stream = reconstruct(&records, &reference("close-1", 0)).expect("reconstructs");
        assert_eq!(stream.declaration, text_declaration(0));
        assert!(stream.text.as_bytes().is_empty());
        assert_eq!(stream.text, "");
    }

    #[test]
    fn zero_byte_continuation_is_malformed() {
        let records = vec![
            segment(
                "flush-0",
                Some(0),
                vec![run(0, 2, Some(text_declaration(0)))],
                "he",
                None,
            ),
            segment(
                "close-1",
                Some(1),
                vec![run(0, 0, None)],
                "",
                closed(2, vec![2]),
            ),
        ];
        let error = reconstruct(&records, &reference("close-1", 0))
            .expect_err("empty continuation is not progress");
        assert_eq!(
            error,
            ReconstructionError::ExtentMismatch {
                reference: reference("close-1", 0),
                bytes: 2,
            }
        );
    }

    #[test]
    fn runs_must_partition_payload_exactly() {
        // Runs cover fewer bytes than the payload carries.
        let records = vec![segment(
            "close-0",
            Some(0),
            vec![run(0, 2, Some(text_declaration(0)))],
            "hello",
            closed(1, vec![5]),
        )];
        let error = reconstruct(&records, &reference("close-0", 0))
            .expect_err("runs must cover the payload");
        assert_eq!(
            error,
            ReconstructionError::ExtentMismatch {
                reference: reference("close-0", 0),
                bytes: 5,
            }
        );
    }

    #[test]
    fn oversized_run_is_malformed() {
        let records = vec![segment(
            "close-0",
            Some(0),
            vec![run(0, 9, Some(text_declaration(0)))],
            "hi",
            closed(1, vec![9]),
        )];
        let error = reconstruct(&records, &reference("close-0", 0))
            .expect_err("run cannot exceed the payload");
        assert_eq!(
            error,
            ReconstructionError::ExtentMismatch {
                reference: reference("close-0", 0),
                bytes: 9,
            }
        );
    }

    #[test]
    fn run_split_inside_utf8_sequence_is_malformed() {
        // "é" is two bytes; the run boundary splits the sequence.
        let records = vec![segment(
            "close-0",
            Some(0),
            vec![run(0, 1, Some(text_declaration(0)))],
            "é",
            closed(1, vec![2]),
        )];
        let error = reconstruct(&records, &reference("close-0", 0))
            .expect_err("run must end on a UTF-8 boundary");
        assert_eq!(
            error,
            ReconstructionError::ExtentMismatch {
                reference: reference("close-0", 0),
                bytes: 2,
            }
        );
    }

    #[test]
    fn multibyte_text_preserves_unicode_byte_lengths() {
        // "héllo" is 6 bytes (é is 2); byte accounting is UTF-8 bytes.
        let records = vec![segment(
            "close-0",
            Some(0),
            vec![run(0, 6, Some(text_declaration(0)))],
            "héllo",
            closed(1, vec![6]),
        )];
        let stream = reconstruct(&records, &reference("close-0", 0)).expect("reconstructs");
        assert_eq!(stream.text.as_bytes().len(), 6);
        assert_eq!(stream.text, "héllo");
    }

    #[test]
    fn assembled_stream_must_match_sealed_byte_length() {
        // The flush assembles 5 bytes but the closure sealed 6.
        let records = vec![segment(
            "close-0",
            Some(0),
            vec![run(0, 5, Some(text_declaration(0)))],
            "hello",
            closed(1, vec![6]),
        )];
        let error = reconstruct(&records, &reference("close-0", 0))
            .expect_err("assembled bytes disagree with the sealed extent");
        assert_eq!(
            error,
            ReconstructionError::ExtentMismatch {
                reference: reference("close-0", 0),
                bytes: 6,
            }
        );
    }

    #[test]
    fn streams_open_densely_from_zero() {
        // First run opens stream 1 while no stream exists yet.
        let records = vec![segment(
            "close-0",
            Some(0),
            vec![run(1, 2, Some(text_declaration(0)))],
            "he",
            closed(1, vec![0, 2]),
        )];
        let error = reconstruct(&records, &reference("close-0", 1))
            .expect_err("streams open densely from zero");
        assert_eq!(
            error,
            ReconstructionError::ExtentMismatch {
                reference: reference("close-0", 1),
                bytes: 2,
            }
        );
    }

    #[test]
    fn duplicate_native_declaration_position_is_malformed() {
        // Two streams both declare (block 0, part 0).
        let records = vec![segment(
            "close-0",
            Some(0),
            vec![
                run(0, 1, Some(text_declaration(0))),
                run(1, 1, Some(text_declaration(0))),
            ],
            "ab",
            closed(1, vec![1, 1]),
        )];
        let error = reconstruct(&records, &reference("close-0", 0))
            .expect_err("a native position is declared exactly once");
        assert_eq!(
            error,
            ReconstructionError::ExtentMismatch {
                reference: reference("close-0", 0),
                bytes: 1,
            }
        );
    }

    #[test]
    fn multi_stream_flush_advances_each_stream() {
        let records = vec![
            segment(
                "flush-0",
                Some(0),
                vec![
                    run(0, 2, Some(text_declaration(0))),
                    run(1, 2, Some(text_declaration(1))),
                ],
                "heab",
                None,
            ),
            segment(
                "close-1",
                Some(1),
                vec![run(0, 3, None), run(1, 1, None)],
                "lloc",
                closed(2, vec![5, 3]),
            ),
        ];
        let first = reconstruct(&records, &reference("close-1", 0)).expect("stream 0");
        assert_eq!(first.text, "hello");
        assert_eq!(first.declaration, text_declaration(0));
        let second = reconstruct(&records, &reference("close-1", 1)).expect("stream 1");
        assert_eq!(second.text, "abc");
        assert_eq!(second.declaration, text_declaration(1));
    }

    #[test]
    fn in_extent_flush_by_a_different_writer_is_invalid() {
        // The only record at ordinal 0 names a different writer than the
        // closing record: the extent disagrees about its producer.
        let (foreign_id, mut foreign) = segment(
            "flush-0",
            Some(0),
            vec![run(0, 2, Some(text_declaration(0)))],
            "he",
            None,
        );
        foreign.writer = OutputWriter::RequestExecution {
            execution_generation: "gen-2".to_string(),
        };
        let (_, closing) = segment(
            "close-1",
            Some(1),
            vec![run(0, 3, None)],
            "llo",
            closed(2, vec![5]),
        );
        let records = vec![(foreign_id, foreign), ("close-1".to_string(), closing)];
        let error = reconstruct(&records, &reference("close-1", 0))
            .expect_err("in-extent writer must match the closing record");
        assert_eq!(
            error,
            ReconstructionError::InvalidWriter {
                request_doc_id: "request-1".to_string(),
                source: provider_source(),
            }
        );
    }

    #[test]
    fn tool_source_requires_the_exact_tool_writer() {
        let tool_call = "tool-call-1".to_string();
        let source = OutputSource::ToolCall {
            tool_call_doc_id: tool_call.clone(),
        };
        let writer = OutputWriter::ToolExecution {
            tool_call_doc_id: tool_call.clone(),
        };
        let mut base = segment(
            "close-0",
            Some(0),
            vec![run(0, 5, Some(text_declaration(0)))],
            "hello",
            closed(1, vec![5]),
        )
        .1;
        base.source = source.clone();
        base.writer = writer.clone();
        let records = vec![("close-0".to_string(), base)];
        let stream = reconstruct(&records, &reference("close-0", 0)).expect("reconstructs");
        assert_eq!(stream.text, "hello");

        // A request writer cannot own tool output.
        let mut mismatched = segment(
            "close-0",
            Some(0),
            vec![run(0, 5, Some(text_declaration(0)))],
            "hello",
            closed(1, vec![5]),
        )
        .1;
        mismatched.source = source;
        let records = vec![("close-0".to_string(), mismatched)];
        let error = reconstruct(&records, &reference("close-0", 0))
            .expect_err("tool output requires the exact tool writer");
        assert_eq!(
            error,
            ReconstructionError::InvalidWriter {
                request_doc_id: "request-1".to_string(),
                source: OutputSource::ToolCall {
                    tool_call_doc_id: "tool-call-1".to_string(),
                },
            }
        );
    }

    #[test]
    fn final_flush_ordinal_must_be_the_last_of_the_extent() {
        // The closing record carries ordinal 0 but seals two flushes.
        let records = vec![
            segment(
                "flush-0",
                Some(0),
                vec![run(0, 2, Some(text_declaration(0)))],
                "he",
                None,
            ),
            segment(
                "close-1",
                Some(0),
                vec![run(0, 3, None)],
                "llo",
                closed(2, vec![5]),
            ),
        ];
        let error = reconstruct(&records, &reference("close-1", 0))
            .expect_err("final flush must carry ordinal segments - 1");
        assert_eq!(
            error,
            ReconstructionError::ExtentMismatch {
                reference: reference("close-1", 0),
                bytes: 5,
            }
        );
    }

    #[test]
    fn known_denial_of_the_closing_record_is_access_denied() {
        let records = hello_source();
        let denied = vec!["close-1".to_string()];
        let error = reconstruct_denied(&records, &denied, &[], &reference("close-1", 0))
            .expect_err("known denial is an access error");
        assert_eq!(
            error,
            ReconstructionError::AccessDenied {
                doc_id: "close-1".to_string(),
            }
        );
    }

    #[test]
    fn known_denial_of_an_in_extent_segment_is_access_denied() {
        let records = hello_source();
        let denied = vec!["flush-0".to_string()];
        let error = reconstruct_denied(&records, &denied, &[], &reference("close-1", 0))
            .expect_err("denied dependency is an access error");
        assert_eq!(
            error,
            ReconstructionError::AccessDenied {
                doc_id: "flush-0".to_string(),
            }
        );
    }

    #[test]
    fn owner_verified_dependency_denial_propagates_over_absence() {
        let records = hello_source();
        let dependency_denials = vec![DependencyDenial {
            root_close_id: "close-1".to_string(),
            denied_doc_id: "remote-segment".to_string(),
        }];
        let error =
            reconstruct_denied(&records, &[], &dependency_denials, &reference("close-1", 0))
                .expect_err("owner-verified denial is an access error even when the row is absent");
        assert_eq!(
            error,
            ReconstructionError::AccessDenied {
                doc_id: "remote-segment".to_string(),
            }
        );
    }

    #[test]
    fn absence_alone_is_never_denial() {
        let records = hello_source();
        // A bare denied ID with no owner-verified dependency relationship does
        // not deny this root; the reference still resolves.
        let denied = vec!["unrelated-doc".to_string()];
        let stream = reconstruct_denied(&records, &denied, &[], &reference("close-1", 0))
            .expect("unrelated denial does not affect this root");
        assert_eq!(stream.text, "hello");
    }

    #[test]
    fn retracted_source_is_not_referenceable() {
        let records = vec![segment(
            "close-0",
            Some(0),
            vec![run(0, 2, Some(text_declaration(0)))],
            "he",
            Some(SourceClose::Retracted),
        )];
        let error = reconstruct(&records, &reference("close-0", 0))
            .expect_err("retracted output is never referenced");
        assert_eq!(
            error,
            ReconstructionError::InvalidReference {
                reference: reference("close-0", 0),
            }
        );
    }

    #[test]
    fn foreign_source_records_are_not_selected() {
        let mut records = hello_source();
        // A twin at a different source coordinate sharing the ordinal must not
        // collide with this source's extent.
        let (foreign_id, mut foreign) = segment(
            "foreign-0",
            Some(0),
            vec![run(0, 2, Some(text_declaration(0)))],
            "ZZ",
            None,
        );
        foreign.source = OutputSource::Authored {
            key: "authored-1".to_string(),
        };
        records.push((foreign_id, foreign));
        let stream = reconstruct(&records, &reference("close-1", 0)).expect("reconstructs");
        assert_eq!(stream.text, "hello");
    }

    fn provider_message(blocks: Vec<MessageBlock>) -> TranscriptMessage {
        TranscriptMessage {
            message_key: "message-1".to_owned(),
            session_id: "session-1".to_owned(),
            agent_did: "did:key:z6MkAgent".to_owned(),
            requester_did: None,
            request_doc_id: Some("request-1".to_owned()),
            publication: MessagePublication::RequestExecution {
                execution_generation: "gen-1".to_owned(),
            },
            outcome: OutputOutcome::Complete,
            sequence: 0,
            role: MessageRole::Assistant,
            native_id: Some("native-1".to_owned()),
            blocks,
            created_at: "2025-01-01T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn reconstructs_native_assistant_message_and_composed_payload() {
        let records = hello_source();
        let payload = PresentedPayload {
            output: reference("close-1", 0),
            presentation: PayloadPresentation::Composed {
                parts: vec![
                    PresentationPart::OutputRange {
                        start_byte: 0,
                        end_byte: 2,
                    },
                    PresentationPart::Literal {
                        text: "! ".to_owned(),
                    },
                    PresentationPart::OutputRange {
                        start_byte: 2,
                        end_byte: 5,
                    },
                ],
            },
        };
        let message = reconstruct_message(
            &observations(&records),
            &[],
            &[],
            &provider_message(vec![MessageBlock::Text { text: payload }]),
        )
        .expect("strictly reconstructs");
        assert_eq!(
            message,
            Message::Assistant {
                id: Some("native-1".to_owned()),
                content: vec![AssistantContent::Text(Text {
                    text: "he! llo".to_owned(),
                })],
            }
        );
    }

    #[test]
    fn rejects_provider_payload_at_a_rewritten_native_position() {
        let records = hello_source();
        let message = provider_message(vec![
            MessageBlock::Text {
                text: PresentedPayload {
                    output: reference("close-1", 0),
                    presentation: PayloadPresentation::Full,
                },
            },
            MessageBlock::Text {
                text: PresentedPayload {
                    output: reference("close-1", 0),
                    presentation: PayloadPresentation::Full,
                },
            },
        ]);
        assert!(matches!(
            reconstruct_message(&observations(&records), &[], &[], &message),
            Err(ReconstructionError::ExtentMismatch { .. })
        ));
    }

    #[test]
    fn rejects_invalid_presented_range_without_shortening_output() {
        let records = hello_source();
        let payload = PresentedPayload {
            output: reference("close-1", 0),
            presentation: PayloadPresentation::Composed {
                parts: vec![PresentationPart::OutputRange {
                    start_byte: 1,
                    end_byte: 99,
                }],
            },
        };
        assert!(matches!(
            reconstruct_presented_payload(&observations(&records), &[], &[], &payload),
            Err(ReconstructionError::InvalidPresentation { .. })
        ));
    }

    #[test]
    fn native_tool_call_requires_exact_declared_identity_and_valid_json() {
        let records = vec![segment(
            "close-args",
            Some(0),
            vec![run(
                0,
                2,
                Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::ToolArguments {
                        id: "call-1".to_owned(),
                        call_id: None,
                        name: "echo".to_owned(),
                    },
                }),
            )],
            "{}",
            closed(1, vec![2]),
        )];
        let header = provider_message(vec![MessageBlock::ToolCall {
            tool_call_doc_id: "tool-1".to_owned(),
            id: "call-1".to_owned(),
            call_id: None,
            name: "echo".to_owned(),
            arguments: reference("close-args", 0),
            signature: None,
            additional_params: None,
        }]);
        assert!(matches!(
            reconstruct_message(&observations(&records), &[], &[], &header),
            Ok(Message::Assistant { content, .. }) if matches!(&content[..], [AssistantContent::ToolCall(_)])
        ));
        let mut wrong = header;
        wrong.blocks = vec![MessageBlock::ToolCall {
            tool_call_doc_id: "tool-1".to_owned(),
            id: "wrong".to_owned(),
            call_id: None,
            name: "echo".to_owned(),
            arguments: reference("close-args", 0),
            signature: None,
            additional_params: None,
        }];
        assert!(matches!(
            reconstruct_message(&observations(&records), &[], &[], &wrong),
            Err(ReconstructionError::ExtentMismatch { .. })
        ));
    }

    #[test]
    fn recovery_can_omit_unparseable_argument_stream_but_not_reference_it() {
        let records = vec![segment(
            "close-mixed",
            Some(0),
            vec![
                run(0, 1, Some(text_declaration(0))),
                run(
                    1,
                    1,
                    Some(StreamDeclaration {
                        block_index: 1,
                        part_index: 0,
                        payload: StreamPayload::ToolArguments {
                            id: "call".to_owned(),
                            call_id: None,
                            name: "echo".to_owned(),
                        },
                    }),
                ),
            ],
            "x{",
            Some(SourceClose::Closed {
                outcome: OutputOutcome::Partial,
                segments: 1,
                stream_bytes: vec![1, 1],
            }),
        )];
        let recovered = TranscriptMessage {
            publication: MessagePublication::RequestRecovery {
                execution_generation: "recovery".to_owned(),
            },
            outcome: OutputOutcome::Partial,
            native_id: None,
            blocks: vec![MessageBlock::Text {
                text: PresentedPayload {
                    output: reference("close-mixed", 0),
                    presentation: PayloadPresentation::Full,
                },
            }],
            ..provider_message(vec![])
        };
        assert_eq!(
            reconstruct_message(&observations(&records), &[], &[], &recovered),
            Ok(Message::assistant("x"))
        );
    }

    #[test]
    fn fork_has_no_request_membership_and_preserves_dependency_authorization() {
        let records = hello_source();
        let fork = TranscriptMessage {
            request_doc_id: None,
            publication: MessagePublication::Fork {
                origin_message_doc_id: "origin-1".to_owned(),
            },
            blocks: vec![MessageBlock::Text {
                text: PresentedPayload {
                    output: reference("close-1", 0),
                    presentation: PayloadPresentation::Full,
                },
            }],
            ..provider_message(vec![])
        };
        assert!(reconstruct_message(&observations(&records), &[], &[], &fork).is_ok());
        assert!(matches!(
            reconstruct_message(&observations(&records), &["close-1".to_owned()], &[], &fork),
            Err(ReconstructionError::AccessDenied { .. })
        ));
        let invalid = TranscriptMessage {
            request_doc_id: Some("request-1".to_owned()),
            ..fork
        };
        assert!(matches!(
            reconstruct_message(&observations(&records), &[], &[], &invalid),
            Err(ReconstructionError::InvalidStructure { .. })
        ));
    }

    #[test]
    fn media_payload_kind_must_match_native_media_block() {
        let records = vec![segment(
            "close-media",
            Some(0),
            vec![run(
                0,
                4,
                Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::Media {
                        media_kind: MediaKind::Audio,
                    },
                }),
            )],
            "aGk=",
            closed(1, vec![4]),
        )];
        let media = super::super::MediaBlock {
            kind: MediaKind::Image,
            data: MediaData::Raw {
                data: reference("close-media", 0),
            },
            media_type: None,
            detail: None,
            additional_params: None,
        };
        let header = provider_message(vec![MessageBlock::Media(media)]);
        assert!(matches!(
            reconstruct_message(&observations(&records), &[], &[], &header),
            Err(ReconstructionError::ExtentMismatch { .. })
        ));
    }

    #[test]
    fn system_and_tool_delivery_role_shapes_are_not_interchangeable() {
        let records = hello_source();
        let mut system = provider_message(vec![MessageBlock::Text {
            text: PresentedPayload {
                output: reference("close-1", 0),
                presentation: PayloadPresentation::Full,
            },
        }]);
        system.role = MessageRole::System;
        assert!(matches!(
            reconstruct_message(&observations(&records), &[], &[], &system),
            Err(ReconstructionError::InvalidStructure { .. })
        ));

        let mut delivery = provider_message(vec![MessageBlock::Text {
            text: PresentedPayload {
                output: reference("close-1", 0),
                presentation: PayloadPresentation::Full,
            },
        }]);
        delivery.role = MessageRole::User;
        delivery.native_id = None;
        delivery.publication = MessagePublication::ToolDelivery {
            tool_call_doc_id: "tool-1".to_owned(),
        };
        // Provider bytes cannot be smuggled into a tool-owned delivery.
        assert!(matches!(
            reconstruct_message(&observations(&records), &[], &[], &delivery),
            Err(ReconstructionError::InvalidStructure { .. })
        ));
    }
}
