//! Sealed source reconstruction, refining CanonicalOutput/Reconstruction.lean.
//! Callers supply authorized observations and explicit denial evidence. Absence
//! never implies denial, and missing bytes never produce a shortened message.

mod extents;
mod message;
mod payload;
mod validation;

use super::{
    OutputSegment, OutputSource, OutputWriter, PayloadPresentation, PayloadRef, PresentationPart,
    PresentedPayload, ReconstructionError, SourceClose, StreamDeclaration, StreamPayload,
    TranscriptMessage,
};
use crate::message::Message;
use extents::Extents;
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

/// Refines the owned tool-output reference boundary. An authored background
/// receipt is not terminal tool output, even after its tool becomes terminal.
pub fn tool_output_reference_owned(
    records: &[ObservedSegment<'_>],
    reference: &PayloadRef,
    request_doc_id: &str,
    tool_call_doc_id: &str,
) -> bool {
    let Ok(closing) = closing_for_reference(records, &[], &[], reference) else {
        return false;
    };
    closing.segment.request_doc_id == request_doc_id
        && closing.segment.source
            == (OutputSource::ToolCall {
                tool_call_doc_id: tool_call_doc_id.to_owned(),
            })
        && closing.segment.writer
            == (OutputWriter::ToolExecution {
                tool_call_doc_id: tool_call_doc_id.to_owned(),
            })
        && reconstruct_extent_streams(records, &[], &[], reference).is_ok_and(|streams| {
            streams.iter().all(|stream| {
                matches!(
                    stream.declaration.payload,
                    StreamPayload::ToolOutput | StreamPayload::Media { .. }
                )
            })
        })
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

/// Reconstruct one complete canonical transcript header into its native message.
/// This is the sole protocol-level path for history, provider input, forks and
/// exports: it validates immutable provenance before exposing any bytes.
pub fn reconstruct_message(
    records: &[ObservedSegment<'_>],
    denied: &[String],
    dependency_denials: &[DependencyDenial],
    message: &TranscriptMessage,
) -> Result<Message, ReconstructionError> {
    message::reconstruct_message_with(&Extents::new(records, denied, dependency_denials), message)
}
