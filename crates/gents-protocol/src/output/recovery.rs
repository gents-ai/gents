//! Pure planning for conservative recovery of one unclosed provider source.
//!
//! The caller owns authorization, expiry, generation fencing, physical IDs and
//! the atomic database write. This module only validates the exact committed
//! flush extent and constructs the Partial closure plus the narrowed text-only
//! header blocks that may be committed together by that owner.

use std::collections::BTreeMap;

use super::reconstruction::ObservedSegment;
use super::{
    MessageBlock, OutputOutcome, OutputSource, OutputWriter, PayloadPresentation, PayloadRef,
    PresentedPayload, SourceClose, StreamPayload,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryPlanError {
    BlankRequest,
    MissingSource,
    ExistingClosure,
    InvalidWriter,
    IdentityConflict { doc_id: String },
    MissingOrdinal { ordinal: u32 },
    ConflictingOrdinal { ordinal: u32 },
    MalformedExtent,
    BlankCloseIdentity,
}

impl std::fmt::Display for RecoveryPlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BlankRequest => f.write_str("recovery request identity is blank"),
            Self::MissingSource => f.write_str("no committed source records are available"),
            Self::ExistingClosure => f.write_str("source already has a closing record"),
            Self::InvalidWriter => {
                f.write_str("source or writer is not the expected request-owned provider source")
            }
            Self::IdentityConflict { doc_id } => {
                write!(
                    f,
                    "physical segment identity {doc_id} has conflicting facts"
                )
            }
            Self::MissingOrdinal { ordinal } => {
                write!(f, "source extent is missing ordinal {ordinal}")
            }
            Self::ConflictingOrdinal { ordinal } => {
                write!(
                    f,
                    "source extent has conflicting facts at ordinal {ordinal}"
                )
            }
            Self::MalformedExtent => f.write_str(
                "source extent has malformed runs, declarations, timestamps or byte accounting",
            ),
            Self::BlankCloseIdentity => f.write_str("closing record identity is blank"),
        }
    }
}

impl std::error::Error for RecoveryPlanError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryPrefixPlan {
    pub close: SourceClose,
    /// Text survivors in original native `(block_index, part_index)` order.
    /// Stream IDs remain those of the source; omitted blocks are not compacted.
    pub retained_streams: Vec<u32>,
}

impl RecoveryPrefixPlan {
    /// Bind planned stream IDs to the exact physical closing-record identity
    /// returned or reserved by the persistence owner after planning.
    pub fn retained_blocks(
        &self,
        close_doc_id: &str,
    ) -> Result<Vec<MessageBlock>, RecoveryPlanError> {
        if close_doc_id.trim().is_empty() {
            return Err(RecoveryPlanError::BlankCloseIdentity);
        }
        Ok(self
            .retained_streams
            .iter()
            .copied()
            .map(|stream| MessageBlock::Text {
                text: PresentedPayload {
                    output: PayloadRef {
                        close_doc_id: close_doc_id.to_owned(),
                        stream,
                    },
                    presentation: PayloadPresentation::Full,
                },
            })
            .collect())
    }
}

/// Validate and plan recovery of the complete locally observed source extent.
///
/// The persistence owner first uses `close`, then binds the returned physical
/// identity with [`RecoveryPrefixPlan::retained_blocks`].
pub fn plan_recovery_prefix(
    records: &[ObservedSegment<'_>],
    request_doc_id: &str,
    source: &OutputSource,
    expected_writer: &OutputWriter,
) -> Result<RecoveryPrefixPlan, RecoveryPlanError> {
    if request_doc_id.trim().is_empty() {
        return Err(RecoveryPlanError::BlankRequest);
    }

    let matching = records
        .iter()
        .copied()
        .filter(|record| {
            record.segment.request_doc_id == request_doc_id && &record.segment.source == source
        })
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return Err(RecoveryPlanError::MissingSource);
    }
    if matching.iter().any(|record| record.segment.close.is_some()) {
        return Err(RecoveryPlanError::ExistingClosure);
    }

    if !matches!(source, OutputSource::ProviderTurn { .. })
        || !matches!(expected_writer, OutputWriter::RequestExecution { .. })
    {
        return Err(RecoveryPlanError::InvalidWriter);
    }
    if matching
        .iter()
        .any(|record| &record.segment.writer != expected_writer)
    {
        return Err(RecoveryPlanError::InvalidWriter);
    }

    let mut identities = BTreeMap::<&str, ObservedSegment<'_>>::new();
    let mut slots = BTreeMap::<u32, Vec<ObservedSegment<'_>>>::new();
    for record in matching {
        if identities
            .insert(record.doc_id, record)
            .is_some_and(|existing| existing != record)
        {
            return Err(RecoveryPlanError::IdentityConflict {
                doc_id: record.doc_id.to_owned(),
            });
        }
        let Some(ordinal) = record.segment.ordinal else {
            return Err(RecoveryPlanError::MalformedExtent);
        };
        slots.entry(ordinal).or_default().push(record);
    }
    let count = u32::try_from(slots.len()).map_err(|_| RecoveryPlanError::MalformedExtent)?;
    if slots.keys().copied().ne(0..count) {
        let missing = (0..=slots.keys().next_back().copied().unwrap_or(0))
            .find(|ordinal| !slots.contains_key(ordinal))
            .unwrap_or(count);
        return Err(RecoveryPlanError::MissingOrdinal { ordinal: missing });
    }

    for ordinal in 0..count {
        let candidates = slots.get(&ordinal).expect("dense extent checked");
        let first = candidates[0];
        if candidates.iter().any(|candidate| *candidate != first) {
            return Err(RecoveryPlanError::ConflictingOrdinal { ordinal });
        }
    }
    // Producer writes and recovery share run/declaration/time validation. Only
    // recovery's narrower source eligibility and text selection differ.
    let extent =
        super::extent::inspect_open_source(records, request_doc_id, source, expected_writer)
            .map_err(|_| RecoveryPlanError::MalformedExtent)?;
    let stream_bytes = extent.stream_bytes;
    let mut text = extent
        .streams
        .iter()
        .enumerate()
        .filter(|(_, stream)| {
            matches!(stream.declaration.payload, StreamPayload::Text)
                && stream.declaration.part_index == 0
        })
        .map(|(index, stream)| (&stream.declaration, index as u32))
        .collect::<Vec<_>>();
    text.sort_by_key(|(declaration, _)| (declaration.block_index, declaration.part_index));
    let retained_streams = text.into_iter().map(|(_, stream)| stream).collect();

    Ok(RecoveryPrefixPlan {
        close: SourceClose::Closed {
            outcome: OutputOutcome::Partial,
            segments: count,
            stream_bytes,
        },
        retained_streams,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{MediaKind, OutputSegment, SegmentRun, StreamDeclaration};
    use crate::rendered_request::{CaptureScope, CaptureScopeKind};

    fn source() -> OutputSource {
        OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: 1,
            },
            turn_index: 0,
            attempt: 0,
        }
    }

    fn writer() -> OutputWriter {
        OutputWriter::RequestExecution {
            execution_generation: "generation".into(),
        }
    }

    fn declaration(block: u32, part: u32, payload: StreamPayload) -> StreamDeclaration {
        StreamDeclaration {
            block_index: block,
            part_index: part,
            payload,
        }
    }

    fn segment(
        doc_id: &str,
        ordinal: u32,
        payload: &str,
        runs: Vec<SegmentRun>,
    ) -> (String, OutputSegment) {
        (
            doc_id.into(),
            OutputSegment {
                agent_did: "did:test:agent".into(),
                requester_did: None,
                session_id: "session".into(),
                request_doc_id: "request".into(),
                source: source(),
                writer: OutputWriter::RequestExecution {
                    execution_generation: "generation".into(),
                },
                ordinal: Some(ordinal),
                runs,
                payload: payload.into(),
                close: None,
                created_at: format!("2026-09-21T00:00:0{ordinal}Z"),
            },
        )
    }

    fn observed(rows: &[(String, OutputSegment)]) -> Vec<ObservedSegment<'_>> {
        rows.iter()
            .map(|(doc_id, segment)| ObservedSegment { doc_id, segment })
            .collect()
    }

    #[test]
    fn plans_exact_dense_extent_and_orders_text_by_native_position() {
        let rows = vec![
            segment(
                "zero",
                0,
                "laterfirst",
                vec![
                    SegmentRun {
                        stream: 0,
                        bytes: 5,
                        declaration: Some(declaration(2, 0, StreamPayload::Text)),
                    },
                    SegmentRun {
                        stream: 1,
                        bytes: 5,
                        declaration: Some(declaration(0, 0, StreamPayload::Text)),
                    },
                ],
            ),
            segment(
                "one",
                1,
                "!",
                vec![SegmentRun {
                    stream: 1,
                    bytes: 1,
                    declaration: None,
                }],
            ),
        ];
        let plan = plan_recovery_prefix(&observed(&rows), "request", &source(), &writer())
            .expect("dense recovery plan");
        assert_eq!(
            plan.close,
            SourceClose::Closed {
                outcome: OutputOutcome::Partial,
                segments: 2,
                stream_bytes: vec![5, 6],
            }
        );
        assert_eq!(plan.retained_streams, vec![1, 0]);
        let streams = plan
            .retained_blocks("close")
            .expect("bind returned closing identity")
            .iter()
            .map(|block| match block {
                MessageBlock::Text { text } => text.output.stream,
                _ => unreachable!("recovery retains text only"),
            })
            .collect::<Vec<_>>();
        assert_eq!(streams, vec![1, 0]);
    }

    #[test]
    fn omits_incomplete_nontext_without_decoding_it() {
        let rows = vec![segment(
            "zero",
            0,
            "answer{not-jsonnot-base64",
            vec![
                SegmentRun {
                    stream: 0,
                    bytes: 6,
                    declaration: Some(declaration(0, 0, StreamPayload::Text)),
                },
                SegmentRun {
                    stream: 1,
                    bytes: 9,
                    declaration: Some(declaration(1, 0, StreamPayload::Reasoning)),
                },
                SegmentRun {
                    stream: 2,
                    bytes: 10,
                    declaration: Some(declaration(
                        2,
                        0,
                        StreamPayload::Media {
                            media_kind: MediaKind::Image,
                        },
                    )),
                },
            ],
        )];
        let plan = plan_recovery_prefix(&observed(&rows), "request", &source(), &writer())
            .expect("nontext payloads are accounted, not decoded");
        assert_eq!(plan.retained_streams.len(), 1);
        assert_eq!(
            plan.close,
            SourceClose::Closed {
                outcome: OutputOutcome::Partial,
                segments: 1,
                stream_bytes: vec![6, 9, 10],
            }
        );
    }

    #[test]
    fn rejects_twins_and_gaps() {
        let first = segment(
            "first",
            0,
            "a",
            vec![SegmentRun {
                stream: 0,
                bytes: 1,
                declaration: Some(declaration(0, 0, StreamPayload::Text)),
            }],
        );
        let twin = segment(
            "twin",
            0,
            "b",
            vec![SegmentRun {
                stream: 0,
                bytes: 1,
                declaration: Some(declaration(0, 0, StreamPayload::Text)),
            }],
        );
        assert!(matches!(
            plan_recovery_prefix(
                &observed(&[first.clone(), twin]),
                "request",
                &source(),
                &writer()
            ),
            Err(RecoveryPlanError::ConflictingOrdinal { ordinal: 0 })
        ));
        let gap = segment(
            "gap",
            2,
            "b",
            vec![SegmentRun {
                stream: 0,
                bytes: 1,
                declaration: None,
            }],
        );
        assert!(matches!(
            plan_recovery_prefix(&observed(&[first, gap]), "request", &source(), &writer()),
            Err(RecoveryPlanError::MissingOrdinal { ordinal: 1 })
        ));
    }

    #[test]
    fn rejects_one_physical_identity_claiming_two_ordinals() {
        let zero = segment(
            "same-doc",
            0,
            "a",
            vec![SegmentRun {
                stream: 0,
                bytes: 1,
                declaration: Some(declaration(0, 0, StreamPayload::Text)),
            }],
        );
        let one = segment(
            "same-doc",
            1,
            "b",
            vec![SegmentRun {
                stream: 0,
                bytes: 1,
                declaration: None,
            }],
        );
        assert!(matches!(
            plan_recovery_prefix(&observed(&[zero, one]), "request", &source(), &writer()),
            Err(RecoveryPlanError::IdentityConflict { .. })
        ));
    }
}
