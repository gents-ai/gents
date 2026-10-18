//! Pure planning for conservative recovery of one unclosed provider source.
//! The caller owns authorization, expiry, generation fencing, physical IDs and
//! the atomic database write.

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
    pub retained_streams: Vec<u32>,
}

impl RecoveryPrefixPlan {
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
    let retained_streams = if source.is_auxiliary_audit() {
        Vec::new()
    } else {
        text.into_iter().map(|(_, stream)| stream).collect()
    };

    Ok(RecoveryPrefixPlan {
        close: SourceClose::Closed {
            outcome: OutputOutcome::Partial,
            segments: count,
            stream_bytes,
        },
        retained_streams,
    })
}
