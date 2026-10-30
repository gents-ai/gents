use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, FixedOffset};

use crate::output::reconstruction::{ObservedSegment, ReconstructedStream};
use crate::output::{OutputSource, OutputWriter, StreamPayload};

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
