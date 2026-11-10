//! Pure planning for conservative recovery of one unclosed provider source.
//! The caller owns authorization, expiry, generation fencing, physical IDs and
//! the atomic database write.

use std::collections::{BTreeMap, BTreeSet};

use super::reconstruction::ObservedSegment;
use super::{
    MessageBlock, OutputOutcome, OutputSource, OutputWriter, PayloadPresentation, PayloadRef,
    PresentedPayload, ReasoningPart, SourceClose, StreamPayload,
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
    /// Surviving payloads in original native `(block_index, part_index)` order.
    pub retained_streams: Vec<u32>,
    retained_blocks: Vec<RetainedBlock>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RetainedBlock {
    Text(u32),
    Reasoning {
        stream: u32,
        signature: Option<String>,
    },
    Redacted(u32),
}

impl RecoveryPrefixPlan {
    pub fn retained_blocks(
        &self,
        close_doc_id: &str,
    ) -> Result<Vec<MessageBlock>, RecoveryPlanError> {
        if close_doc_id.trim().is_empty() {
            return Err(RecoveryPlanError::BlankCloseIdentity);
        }
        let reference = |stream| PayloadRef {
            close_doc_id: close_doc_id.to_owned(),
            stream,
        };
        Ok(self
            .retained_blocks
            .iter()
            .map(|block| match block {
                RetainedBlock::Text(stream) => MessageBlock::Text {
                    text: PresentedPayload {
                        output: reference(*stream),
                        presentation: PayloadPresentation::Full,
                    },
                },
                RetainedBlock::Reasoning { stream, signature } => MessageBlock::Reasoning {
                    id: None,
                    parts: vec![ReasoningPart::Text {
                        text: reference(*stream),
                        signature: signature.clone(),
                    }],
                },
                RetainedBlock::Redacted(stream) => MessageBlock::Reasoning {
                    id: None,
                    parts: vec![ReasoningPart::Redacted {
                        data: reference(*stream),
                    }],
                },
            })
            .collect())
    }
}

/// Validate and plan recovery of the complete locally observed source extent.
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
    let extent =
        super::extent::inspect_open_source(records, request_doc_id, source, expected_writer)
            .map_err(|_| RecoveryPlanError::MalformedExtent)?;
    let stream_bytes = extent.stream_bytes;
    let (retained_streams, retained_blocks) =
        select_recovery_blocks(&extent.streams, !source.is_auxiliary_audit());

    Ok(RecoveryPrefixPlan {
        close: SourceClose::Closed {
            outcome: OutputOutcome::Partial,
            segments: count,
            stream_bytes,
        },
        retained_streams,
        retained_blocks,
    })
}

fn select_recovery_blocks(
    streams: &[super::reconstruction::ReconstructedStream],
    include: bool,
) -> (Vec<u32>, Vec<RetainedBlock>) {
    let stopped: BTreeSet<_> = streams
        .iter()
        .filter_map(|stream| {
            matches!(stream.declaration.payload, StreamPayload::ReasoningStop)
                .then_some(stream.declaration.block_index)
        })
        .collect();
    let mut text = streams
        .iter()
        .enumerate()
        .filter(|(_, stream)| {
            let declaration = &stream.declaration;
            declaration.part_index == 0
                && (matches!(declaration.payload, StreamPayload::Text)
                    || (matches!(
                        declaration.payload,
                        StreamPayload::Reasoning | StreamPayload::ReasoningRedacted
                    ) && stopped.contains(&declaration.block_index)))
        })
        .map(|(index, stream)| (&stream.declaration, index as u32))
        .collect::<Vec<_>>();
    text.sort_by_key(|(declaration, _)| (declaration.block_index, declaration.part_index));
    let retained_streams = if !include {
        Vec::new()
    } else {
        text.into_iter().map(|(_, stream)| stream).collect()
    };

    let signatures: BTreeMap<_, _> = streams
        .iter()
        .filter_map(|item| {
            matches!(item.declaration.payload, StreamPayload::ReasoningSignature).then_some((
                (item.declaration.block_index, item.declaration.part_index),
                &item.text,
            ))
        })
        .collect();
    let retained_blocks = retained_streams
        .iter()
        .map(|&stream| {
            let item = &streams[stream as usize];
            match item.declaration.payload {
                StreamPayload::Text => RetainedBlock::Text(stream),
                StreamPayload::Reasoning => RetainedBlock::Reasoning {
                    stream,
                    signature: signatures
                        .get(&(item.declaration.block_index, item.declaration.part_index))
                        .map(|signature| (*signature).clone()),
                },
                StreamPayload::ReasoningRedacted => RetainedBlock::Redacted(stream),
                _ => unreachable!("only recoverable payload kinds are selected"),
            }
        })
        .collect();

    (retained_streams, retained_blocks)
}

/// Project blocks only after the caller has validated the complete source extent.
pub fn retained_blocks_from_validated_extent(
    extent: &super::extent::OpenSourceExtent,
    close_doc_id: &str,
) -> Result<Vec<MessageBlock>, RecoveryPlanError> {
    let (retained_streams, retained_blocks) = select_recovery_blocks(&extent.streams, true);
    RecoveryPrefixPlan {
        close: SourceClose::Closed {
            outcome: OutputOutcome::Partial,
            segments: extent.segments,
            stream_bytes: extent.stream_bytes.clone(),
        },
        retained_streams,
        retained_blocks,
    }
    .retained_blocks(close_doc_id)
}

/// Preserve physical positions when recovery omits unfinished native blocks.
pub fn retained_block_indices_from_validated_extent(
    extent: &super::extent::OpenSourceExtent,
) -> Vec<u32> {
    let (streams, _) = select_recovery_blocks(&extent.streams, true);
    streams
        .iter()
        .map(|stream| extent.streams[*stream as usize].declaration.block_index)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{OutputSegment, SegmentRun, StreamDeclaration};

    fn source() -> OutputSource {
        OutputSource::ProviderTurn {
            scope: "inference.1".parse().unwrap(),
            turn_index: 0,
            attempt: 0,
        }
    }

    fn segment(runs: Vec<SegmentRun>, payload: &str) -> OutputSegment {
        OutputSegment {
            node_did: "node".into(),
            requester_did: Some("requester".into()),
            session_id: "session".into(),
            request_doc_id: "request".into(),
            source: source(),
            writer: OutputWriter::RequestExecution {
                execution_generation: "generation".into(),
            },
            ordinal: Some(0),
            runs,
            payload: payload.into(),
            close: None,
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn declaration(stream: u32, bytes: u32, payload: StreamPayload) -> SegmentRun {
        SegmentRun {
            stream,
            bytes,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload,
            }),
        }
    }

    #[test]
    fn stopped_reasoning_retains_body_and_exact_signature() {
        let record = segment(
            vec![
                declaration(0, 1, StreamPayload::Reasoning),
                declaration(1, 1, StreamPayload::ReasoningSignature),
                declaration(2, 0, StreamPayload::ReasoningStop),
            ],
            "AS",
        );
        let writer = record.writer.clone();
        let plan = plan_recovery_prefix(
            &[ObservedSegment {
                doc_id: "segment",
                segment: &record,
            }],
            "request",
            &source(),
            &writer,
        )
        .unwrap();
        assert_eq!(plan.retained_streams, [0]);
        let extent = super::super::extent::inspect_open_source(
            &[ObservedSegment {
                doc_id: "segment",
                segment: &record,
            }],
            "request",
            &source(),
            &writer,
        )
        .unwrap();
        assert_eq!(
            retained_blocks_from_validated_extent(&extent, "close").unwrap(),
            plan.retained_blocks("close").unwrap()
        );
        assert_eq!(
            plan.retained_blocks("close").unwrap(),
            vec![MessageBlock::Reasoning {
                id: None,
                parts: vec![ReasoningPart::Text {
                    text: PayloadRef {
                        close_doc_id: "close".into(),
                        stream: 0
                    },
                    signature: Some("S".into()),
                }],
            }]
        );
        assert_eq!(
            plan.close,
            SourceClose::Closed {
                outcome: OutputOutcome::Partial,
                segments: 1,
                stream_bytes: vec![1, 1, 0],
            }
        );
    }

    #[test]
    fn recovery_positions_preserve_gaps_from_unfinished_blocks() {
        let mut unfinished = declaration(0, 1, StreamPayload::Reasoning);
        unfinished.declaration.as_mut().unwrap().block_index = 1;
        let mut body = declaration(1, 1, StreamPayload::Reasoning);
        body.declaration.as_mut().unwrap().block_index = 3;
        let mut signature = declaration(2, 1, StreamPayload::ReasoningSignature);
        signature.declaration.as_mut().unwrap().block_index = 3;
        let mut stop = declaration(3, 0, StreamPayload::ReasoningStop);
        stop.declaration.as_mut().unwrap().block_index = 3;
        let mut text = declaration(4, 1, StreamPayload::Text);
        text.declaration.as_mut().unwrap().block_index = 5;
        let record = segment(vec![unfinished, body, signature, stop, text], "ABSC");
        let extent = super::super::extent::inspect_open_source(
            &[ObservedSegment {
                doc_id: "segment",
                segment: &record,
            }],
            "request",
            &source(),
            &record.writer,
        )
        .unwrap();
        assert_eq!(
            retained_block_indices_from_validated_extent(&extent),
            [3, 5]
        );
        assert_eq!(
            retained_blocks_from_validated_extent(&extent, "close")
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn signature_without_stop_is_not_recovered() {
        let record = segment(
            vec![
                declaration(0, 1, StreamPayload::Reasoning),
                declaration(1, 1, StreamPayload::ReasoningSignature),
            ],
            "AS",
        );
        let plan = plan_recovery_prefix(
            &[ObservedSegment {
                doc_id: "segment",
                segment: &record,
            }],
            "request",
            &source(),
            &record.writer,
        )
        .unwrap();
        assert!(plan.retained_blocks("close").unwrap().is_empty());
    }

    #[test]
    fn stop_without_body_or_later_signature_bytes_is_invalid() {
        let missing = segment(vec![declaration(0, 0, StreamPayload::ReasoningStop)], "");
        assert_eq!(
            plan_recovery_prefix(
                &[ObservedSegment {
                    doc_id: "segment",
                    segment: &missing
                }],
                "request",
                &source(),
                &missing.writer
            ),
            Err(RecoveryPlanError::MalformedExtent)
        );
        let late = segment(
            vec![
                declaration(0, 1, StreamPayload::Reasoning),
                declaration(1, 1, StreamPayload::ReasoningSignature),
                declaration(2, 0, StreamPayload::ReasoningStop),
                SegmentRun {
                    stream: 1,
                    bytes: 1,
                    declaration: None,
                },
            ],
            "AST",
        );
        assert_eq!(
            plan_recovery_prefix(
                &[ObservedSegment {
                    doc_id: "segment",
                    segment: &late
                }],
                "request",
                &source(),
                &late.writer
            ),
            Err(RecoveryPlanError::MalformedExtent)
        );
    }
}
