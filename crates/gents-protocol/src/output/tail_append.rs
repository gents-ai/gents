//! Bounded tail planning for tool-output appends, refining
//! `CanonicalOutput/TailAppend.lean`.

use chrono::{DateTime, FixedOffset};
use std::collections::BTreeMap;

use super::extent::{facts_agree, writer_matches, ObservedShape};
use super::{OutputSource, OutputWriter, SegmentRun, StreamDeclaration, StreamPayload};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TailAppendKind {
    Fresh,
    Replay,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TailAppendPlan {
    pub kind: TailAppendKind,
    pub doc_id: Option<String>,
    pub ordinal: u32,
    pub start_byte: u64,
    pub end_byte: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TailAppendError {
    #[error("empty tool output has no physical append receipt")]
    EmptyPayload,
    #[error("open source writer does not bind its source")]
    InvalidWriter,
    #[error("cannot append raw output after tool source closure")]
    ClosedSource,
    #[error("canonical tool output append has ambiguous retry identity")]
    AmbiguousRetry,
    #[error("canonical tool output retry conflicts with the recorded segment")]
    ConflictingRetry,
    #[error("open tool output tail is malformed")]
    MalformedTail,
    #[error("open extent has conflicting flush twin")]
    ConflictingTwin,
    #[error("open flush timestamps decrease")]
    TimestampRegressed,
}

fn tool_output_declaration() -> StreamDeclaration {
    StreamDeclaration {
        block_index: 0,
        part_index: 0,
        payload: StreamPayload::ToolOutput,
    }
}

fn valid_tool_continuation_runs(runs: &[SegmentRun]) -> Option<u64> {
    let mut total = 0u64;
    for run in runs {
        if run.stream != 0 || run.declaration.is_some() || run.bytes == 0 {
            return None;
        }
        total = total.checked_add(u64::from(run.bytes))?;
    }
    Some(total)
}

fn valid_single_tool_flush(ordinal: u32, runs: &[SegmentRun]) -> Option<u64> {
    let (first, rest) = runs.split_first()?;
    if ordinal == 0 {
        if first.stream != 0 || first.declaration.as_ref() != Some(&tool_output_declaration()) {
            return None;
        }
        let tail = valid_tool_continuation_runs(rest)?;
        u64::from(first.bytes).checked_add(tail)
    } else {
        valid_tool_continuation_runs(runs)
    }
}

fn matches_expected_tool_run(ordinal: u32, payload_bytes: u64, runs: &[SegmentRun]) -> bool {
    let Ok(expected_bytes) = u32::try_from(payload_bytes) else {
        return false;
    };
    let [run] = runs else {
        return false;
    };
    let expected_decl = (ordinal == 0).then(tool_output_declaration);
    run.stream == 0 && run.bytes == expected_bytes && run.declaration == expected_decl
}

fn erase_payload<'a>(shape: ObservedShape<'a>) -> ObservedShape<'a> {
    ObservedShape {
        payload: None,
        ..shape
    }
}

fn parse_timestamp(value: &str) -> Result<DateTime<FixedOffset>, TailAppendError> {
    DateTime::parse_from_rfc3339(value).map_err(|_| TailAppendError::MalformedTail)
}

fn dedup_by_doc_id<'a>(rows: Vec<ObservedShape<'a>>) -> Vec<ObservedShape<'a>> {
    let mut unique = Vec::with_capacity(rows.len());
    for row in rows {
        if !unique
            .iter()
            .any(|seen: &ObservedShape<'a>| seen.doc_id == row.doc_id)
        {
            unique.push(row);
        }
    }
    unique
}

fn at_ordinal<'a>(rows: &[ObservedShape<'a>], ordinal: u32) -> Vec<ObservedShape<'a>> {
    rows.iter()
        .copied()
        .filter(|row| row.ordinal == Some(ordinal))
        .collect()
}

fn single_stream_bytes_after(shape: &ObservedShape<'_>) -> Option<u64> {
    match shape.stream_bytes_after {
        Some([bytes]) => Some(*bytes),
        _ => None,
    }
}

fn validate_clean_tail(
    data: &[ObservedShape<'_>],
) -> Result<(u32, u64, DateTime<FixedOffset>), TailAppendError> {
    let max_ord = data
        .iter()
        .filter_map(|row| row.ordinal)
        .max()
        .ok_or(TailAppendError::MalformedTail)?;
    let top = at_ordinal(data, max_ord);
    let latest = match top.as_slice() {
        [only] => *only,
        [] => return Err(TailAppendError::MalformedTail),
        _ => return Err(TailAppendError::ConflictingTwin),
    };
    let tail_bytes = single_stream_bytes_after(&latest).ok_or(TailAppendError::MalformedTail)?;
    let latest_bytes =
        valid_single_tool_flush(max_ord, latest.runs).ok_or(TailAppendError::MalformedTail)?;
    let latest_time = parse_timestamp(latest.created_at)?;
    if max_ord == 0 {
        if latest_bytes != tail_bytes {
            return Err(TailAppendError::MalformedTail);
        }
        return Ok((0, tail_bytes, latest_time));
    }
    let prev_slot = at_ordinal(data, max_ord - 1);
    let prev = match prev_slot.as_slice() {
        [only] => *only,
        [] => return Err(TailAppendError::MalformedTail),
        _ => return Err(TailAppendError::ConflictingTwin),
    };
    let prev_bytes = single_stream_bytes_after(&prev).ok_or(TailAppendError::MalformedTail)?;
    if valid_single_tool_flush(max_ord - 1, prev.runs).is_none() {
        return Err(TailAppendError::MalformedTail);
    }
    let prev_time = parse_timestamp(prev.created_at)?;
    if latest_time < prev_time {
        return Err(TailAppendError::TimestampRegressed);
    }
    if prev_bytes.checked_add(latest_bytes) != Some(tail_bytes) {
        return Err(TailAppendError::MalformedTail);
    }
    Ok((max_ord, tail_bytes, latest_time))
}

fn plan_replay(
    existing: ObservedShape<'_>,
    data: &[ObservedShape<'_>],
    payload_bytes: u64,
) -> Result<TailAppendPlan, TailAppendError> {
    let existing_ord = existing.ordinal.ok_or(TailAppendError::MalformedTail)?;
    let end_byte = single_stream_bytes_after(&existing).ok_or(TailAppendError::MalformedTail)?;
    if at_ordinal(data, existing_ord)
        .iter()
        .any(|row| row.doc_id != existing.doc_id)
    {
        return Err(TailAppendError::ConflictingTwin);
    }
    if !matches_expected_tool_run(existing_ord, payload_bytes, existing.runs) {
        return Err(TailAppendError::ConflictingRetry);
    }
    let start_byte = end_byte
        .checked_sub(payload_bytes)
        .ok_or(TailAppendError::MalformedTail)?;
    let existing_time = parse_timestamp(existing.created_at)?;
    let prior: Vec<_> = data
        .iter()
        .copied()
        .filter(|row| row.doc_id != existing.doc_id)
        .collect();
    if existing_ord == 0 {
        if start_byte != 0 {
            return Err(TailAppendError::MalformedTail);
        }
        if !prior.is_empty() {
            let (_, _, latest_time) = validate_clean_tail(data)?;
            if latest_time < existing_time {
                return Err(TailAppendError::TimestampRegressed);
            }
        }
        return Ok(TailAppendPlan {
            kind: TailAppendKind::Replay,
            doc_id: Some(existing.doc_id.to_owned()),
            ordinal: 0,
            start_byte: 0,
            end_byte,
        });
    }
    let max_ord = data
        .iter()
        .filter_map(|row| row.ordinal)
        .max()
        .unwrap_or(0)
        .max(existing_ord);
    if existing_ord == max_ord {
        let prev_slot = at_ordinal(&prior, existing_ord - 1);
        let prev = match prev_slot.as_slice() {
            [only] => *only,
            [] => return Err(TailAppendError::MalformedTail),
            _ => return Err(TailAppendError::ConflictingTwin),
        };
        let prev_bytes = single_stream_bytes_after(&prev).ok_or(TailAppendError::MalformedTail)?;
        if valid_single_tool_flush(existing_ord - 1, prev.runs).is_none() {
            return Err(TailAppendError::MalformedTail);
        }
        let prev_time = parse_timestamp(prev.created_at)?;
        if existing_time < prev_time {
            return Err(TailAppendError::TimestampRegressed);
        }
        if prev_bytes != start_byte {
            return Err(TailAppendError::MalformedTail);
        }
        return Ok(TailAppendPlan {
            kind: TailAppendKind::Replay,
            doc_id: Some(existing.doc_id.to_owned()),
            ordinal: existing_ord,
            start_byte,
            end_byte,
        });
    }
    if existing_ord < max_ord {
        let (_, _, latest_time) = validate_clean_tail(data)?;
        if latest_time < existing_time {
            return Err(TailAppendError::TimestampRegressed);
        }
        return Ok(TailAppendPlan {
            kind: TailAppendKind::Replay,
            doc_id: Some(existing.doc_id.to_owned()),
            ordinal: existing_ord,
            start_byte,
            end_byte,
        });
    }
    Err(TailAppendError::MalformedTail)
}

#[derive(Clone, Copy, Debug)]
pub struct TailAppendObservation<'a> {
    pub tail: &'a [ObservedShape<'a>],
    pub closure_witness: &'a [ObservedShape<'a>],
    pub same_attempt: &'a [ObservedShape<'a>],
}

/// Plan the next tool-output append from a bounded tail window (up to 3
/// highest-ordinal rows), bounded closure witness, and bounded retry-identity
/// lookup, without scanning the full source prefix.
pub fn plan_tool_append_from_tail(
    observation: TailAppendObservation<'_>,
    request_doc_id: &str,
    source: &OutputSource,
    expected_writer: &OutputWriter,
    created_at: &str,
    payload_bytes: u64,
) -> Result<TailAppendPlan, TailAppendError> {
    let TailAppendObservation {
        tail,
        closure_witness,
        same_attempt,
    } = observation;
    if payload_bytes == 0 {
        return Err(TailAppendError::EmptyPayload);
    }
    if !writer_matches(source, expected_writer) {
        return Err(TailAppendError::InvalidWriter);
    }
    let in_scope =
        |row: &ObservedShape<'_>| row.request_doc_id == request_doc_id && *row.source == *source;
    let tail_scoped: Vec<_> = tail
        .iter()
        .copied()
        .filter(in_scope)
        .map(erase_payload)
        .collect();
    let has_closure = closure_witness.iter().any(in_scope)
        || tail_scoped.iter().any(|row| row.close.is_some())
        || same_attempt
            .iter()
            .copied()
            .filter(in_scope)
            .any(|row| row.close.is_some());
    if has_closure {
        return Err(TailAppendError::ClosedSource);
    }
    let retry_scoped: Vec<_> = same_attempt
        .iter()
        .copied()
        .filter(in_scope)
        .map(erase_payload)
        .collect();
    let mut physical: BTreeMap<&str, ObservedShape<'_>> = BTreeMap::new();
    for row in tail_scoped.iter().chain(&retry_scoped) {
        if *row.writer != *expected_writer {
            return Err(TailAppendError::InvalidWriter);
        }
        if let Some(previous) = physical.insert(row.doc_id, *row) {
            if !facts_agree(&previous, row) {
                return Err(TailAppendError::ConflictingTwin);
            }
        }
        if row.ordinal.is_none() {
            return Err(TailAppendError::MalformedTail);
        }
    }
    let attempt_time = parse_timestamp(created_at)?;
    let mut retry_candidates = retry_scoped;
    for row in &tail_scoped {
        if parse_timestamp(row.created_at)? == attempt_time {
            retry_candidates.push(*row);
        }
    }
    let retry_candidates = dedup_by_doc_id(retry_candidates);
    let data = dedup_by_doc_id(tail_scoped);
    match retry_candidates.as_slice() {
        [] => {
            if data.is_empty() {
                return Ok(TailAppendPlan {
                    kind: TailAppendKind::Fresh,
                    doc_id: None,
                    ordinal: 0,
                    start_byte: 0,
                    end_byte: payload_bytes,
                });
            }
            let (max_ord, tail_bytes, latest_time) = validate_clean_tail(&data)?;
            if attempt_time < latest_time {
                return Err(TailAppendError::TimestampRegressed);
            }
            let ordinal = max_ord
                .checked_add(1)
                .ok_or(TailAppendError::MalformedTail)?;
            let end_byte = tail_bytes
                .checked_add(payload_bytes)
                .ok_or(TailAppendError::MalformedTail)?;
            Ok(TailAppendPlan {
                kind: TailAppendKind::Fresh,
                doc_id: None,
                ordinal,
                start_byte: tail_bytes,
                end_byte,
            })
        }
        [existing] => plan_replay(*existing, &data, payload_bytes),
        _ => Err(TailAppendError::AmbiguousRetry),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{OutputOutcome, OutputSegment, SourceClose};
    use gents_lean_vocab::families::open_output_shape_plan_tool_append as family;

    fn segment(input: &family::RecordInput<String>) -> OutputSegment {
        OutputSegment {
            node_did: "node".into(),
            requester_did: Some("requester".into()),
            session_id: "session".into(),
            request_doc_id: format!("request-{}", input.request),
            source: OutputSource::ToolCall {
                tool_call_doc_id: format!("tool-{}", input.source),
            },
            writer: OutputWriter::ToolExecution {
                tool_call_doc_id: format!("tool-{}", input.writer),
            },
            ordinal: input.ordinal.map(|value| value.try_into().unwrap()),
            runs: input
                .runs
                .iter()
                .map(|run| SegmentRun {
                    stream: run.stream.try_into().unwrap(),
                    bytes: run.bytes.try_into().unwrap(),
                    declaration: run
                        .declaration
                        .as_ref()
                        .map(|declaration| StreamDeclaration {
                            block_index: declaration.block.try_into().unwrap(),
                            part_index: declaration.part.try_into().unwrap(),
                            payload: match declaration.kind.as_str() {
                                "text" => StreamPayload::Text,
                                "reasoning" => StreamPayload::Reasoning,
                                "reasoning_stop" => StreamPayload::ReasoningStop,
                                "tool_output" => StreamPayload::ToolOutput,
                                other => panic!("unmapped declaration {other}"),
                            },
                        }),
                })
                .collect(),
            stream_bytes_after: input.stream_bytes_after.clone(),
            payload: input.payload.clone(),
            close: input.close.then_some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 0,
                stream_bytes: Vec::new(),
            }),
            created_at: chrono::DateTime::from_timestamp(input.created_at.try_into().unwrap(), 0)
                .unwrap()
                .to_rfc3339(),
        }
    }

    fn map_output(
        result: Result<TailAppendPlan, TailAppendError>,
    ) -> family::TailAppendOutput<String, String> {
        match result {
            Ok(plan) => family::TailAppendOutput {
                plan: Some(family::TailAppendPlan {
                    kind: match plan.kind {
                        TailAppendKind::Fresh => "fresh".to_owned(),
                        TailAppendKind::Replay => "replay".to_owned(),
                    },
                    doc_id: plan.doc_id.map(|id| id.parse().unwrap()),
                    ordinal: u64::from(plan.ordinal),
                    start_byte: plan.start_byte,
                    end_byte: plan.end_byte,
                }),
                error: None,
            },
            Err(error) => family::TailAppendOutput {
                plan: None,
                error: Some(
                    match error {
                        TailAppendError::EmptyPayload => "empty_payload",
                        TailAppendError::InvalidWriter => "invalid_writer",
                        TailAppendError::ClosedSource => "closed_source",
                        TailAppendError::AmbiguousRetry => "ambiguous_retry",
                        TailAppendError::ConflictingRetry => "conflicting_retry",
                        TailAppendError::MalformedTail => "malformed_tail",
                        TailAppendError::ConflictingTwin => "conflicting_twin",
                        TailAppendError::TimestampRegressed => "timestamp_regressed",
                    }
                    .to_owned(),
                ),
            },
        }
    }

    #[test]
    fn generated_plan_tool_append_matches_model() {
        for case in family::cases::<String, String, String>() {
            let tail_records: Vec<_> = case
                .input
                .tail
                .iter()
                .map(|item| (item.id.to_string(), segment(item)))
                .collect();
            let closure_records: Vec<_> = case
                .input
                .closures
                .iter()
                .map(|item| (item.id.to_string(), segment(item)))
                .collect();
            let same_records: Vec<_> = case
                .input
                .same_attempt
                .iter()
                .map(|item| (item.id.to_string(), segment(item)))
                .collect();
            fn to_shapes<'a>(records: &'a [(String, OutputSegment)]) -> Vec<ObservedShape<'a>> {
                records
                    .iter()
                    .map(|(id, seg)| ObservedShape {
                        payload: None,
                        ..ObservedShape::from_segment(id, seg)
                    })
                    .collect()
            }
            let source = OutputSource::ToolCall {
                tool_call_doc_id: "tool-1".into(),
            };
            let writer = OutputWriter::ToolExecution {
                tool_call_doc_id: format!("tool-{}", case.input.writer),
            };
            let created_at =
                chrono::DateTime::from_timestamp(case.input.created_at.try_into().unwrap(), 0)
                    .unwrap()
                    .to_rfc3339();
            let tail_shapes = to_shapes(&tail_records);
            let closure_shapes = to_shapes(&closure_records);
            let same_shapes = to_shapes(&same_records);
            let result = plan_tool_append_from_tail(
                TailAppendObservation {
                    tail: &tail_shapes,
                    closure_witness: &closure_shapes,
                    same_attempt: &same_shapes,
                },
                "request-1",
                &source,
                &writer,
                &created_at,
                case.input.payload_bytes,
            );
            case.check(map_output(result));
        }
    }
}
