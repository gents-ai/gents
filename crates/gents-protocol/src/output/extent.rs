//! Exact open-source extent validation used before a recovery/producer close:
//! gaps, twins and malformed runs fail rather than yielding a shorter prefix.
//! Closure authority and uniqueness remain with the caller.
use super::reconstruction::{ObservedSegment, ReconstructedStream};
use super::{OutputSource, OutputWriter, ReconstructionError, StreamPayload};
use chrono::DateTime;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq)]
pub struct OpenSourceExtent {
    pub segments: u32,
    pub streams: Vec<ReconstructedStream>,
    pub stream_bytes: Vec<u64>,
    pub last_created_at: Option<String>,
}

fn writer_matches(source: &OutputSource, writer: &OutputWriter) -> bool {
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
fn bad(detail: impl Into<String>) -> ReconstructionError {
    ReconstructionError::InvalidStructure {
        detail: detail.into(),
    }
}

/// Records must be the authorized, exact `(request_doc_id, source)` scope.
/// A final flush may carry a close; terminal-only records are ignored only
/// after their native empty-data shape is checked.
pub fn inspect_open_source(
    records: &[ObservedSegment<'_>],
    request_doc_id: &str,
    source: &OutputSource,
    expected_writer: &OutputWriter,
) -> Result<OpenSourceExtent, ReconstructionError> {
    if !writer_matches(source, expected_writer) {
        return Err(bad("open source writer does not bind its source"));
    }
    let scoped: Vec<_> = records
        .iter()
        .copied()
        .filter(|r| r.segment.request_doc_id == request_doc_id && r.segment.source == *source)
        .collect();
    if scoped.is_empty() {
        return Ok(OpenSourceExtent {
            segments: 0,
            streams: vec![],
            stream_bytes: vec![],
            last_created_at: None,
        });
    }
    let writer = expected_writer;
    let mut slots: BTreeMap<u32, Vec<ObservedSegment<'_>>> = BTreeMap::new();
    let mut physical = BTreeMap::new();
    for r in scoped {
        if let Some(previous) = physical.insert(r.doc_id, r.segment) {
            if previous != r.segment {
                return Err(bad("one physical segment identity has conflicting facts"));
            }
        }
        if r.segment.writer != *writer {
            return Err(bad("open source records disagree on writer"));
        };
        let Some(n) = r.segment.ordinal else {
            // A terminal-only closure has no data coordinate and is irrelevant
            // to prefix accounting; closure uniqueness is the caller's owner.
            if r.segment.close.is_some()
                && r.segment.runs.is_empty()
                && r.segment.payload.is_empty()
            {
                continue;
            }
            return Err(bad("record has neither data ordinal nor closure"));
        };
        if r.segment.runs.is_empty() {
            return Err(bad("open flush has no runs"));
        };
        slots.entry(n).or_default().push(r);
    }
    let mut streams: Vec<ReconstructedStream> = vec![];
    let mut positions = BTreeSet::new();
    let mut last_time = None;
    for (expected, (ordinal, slot)) in slots.iter().enumerate() {
        let expected = u32::try_from(expected).map_err(|_| bad("ordinal exceeds u32"))?;
        if *ordinal != expected {
            return Err(bad("open extent has ordinal gap"));
        }
        let r = slot[0];
        if slot
            .iter()
            .any(|x| x.doc_id != r.doc_id || x.segment != r.segment)
        {
            return Err(bad("open extent has conflicting flush twin"));
        };
        let time = DateTime::parse_from_rfc3339(&r.segment.created_at)
            .map_err(|_| bad("open flush has malformed timestamp"))?;
        if last_time
            .as_ref()
            .is_some_and(|prev: &DateTime<chrono::FixedOffset>| time < *prev)
        {
            return Err(bad("open flush timestamps decrease"));
        };
        last_time = Some(time);
        let mut offset = 0usize;
        for run in &r.segment.runs {
            if run.bytes == 0 && run.declaration.is_none() {
                return Err(bad("zero-byte continuation"));
            };
            let end = offset
                .checked_add(run.bytes as usize)
                .ok_or_else(|| bad("run overflow"))?;
            let part = r
                .segment
                .payload
                .get(offset..end)
                .ok_or_else(|| bad("runs split UTF-8 or exceed payload"))?;
            if let Some(decl) = &run.declaration {
                if run.stream as usize != streams.len()
                    || !positions.insert((
                        decl.block_index,
                        decl.part_index,
                        matches!(decl.payload, StreamPayload::ReasoningSignature),
                    ))
                {
                    return Err(bad(
                        "stream declarations are not dense or positions conflict",
                    ));
                };
                streams.push(ReconstructedStream {
                    declaration: decl.clone(),
                    text: part.to_owned(),
                })
            } else {
                streams
                    .get_mut(run.stream as usize)
                    .ok_or_else(|| bad("continuation before declaration"))?
                    .text
                    .push_str(part)
            };
            offset = end
        }
        if offset != r.segment.payload.len() {
            return Err(bad("runs do not partition payload"));
        }
    }
    let bytes = streams.iter().map(|s| s.text.len() as u64).collect();
    Ok(OpenSourceExtent {
        segments: slots.len() as u32,
        streams,
        stream_bytes: bytes,
        last_created_at: last_time.map(|t| t.to_rfc3339()),
    })
}
