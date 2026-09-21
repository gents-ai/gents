//! Sealed source reconstruction, refining CanonicalOutput/Reconstruction.lean.
//! Callers supply authorized observations and explicit denial evidence. Absence
//! never implies denial, and missing bytes never produce a shortened message.

use super::{
    OutputSegment, OutputSource, OutputWriter, PayloadRef, ReconstructionError, SourceClose,
    StreamDeclaration,
};
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

pub fn reconstruct_stream(
    records: &[ObservedSegment<'_>],
    denied: &[String],
    dependency_denials: &[DependencyDenial],
    reference: &PayloadRef,
) -> Result<ReconstructedStream, ReconstructionError> {
    // Stable diagnostics even when several denied dependencies are reordered.
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
            .filter(|r| r.doc_id == reference.close_doc_id),
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
    let invalid_reference = || ReconstructionError::InvalidReference {
        reference: reference.clone(),
    };
    let Some(SourceClose::Closed {
        segments: count,
        stream_bytes,
        ..
    }) = &closing.segment.close
    else {
        return Err(invalid_reference());
    };
    let Some(expected_bytes) = stream_bytes.get(reference.stream as usize) else {
        return Err(invalid_reference());
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
}
