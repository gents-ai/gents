//! Exact open-source extent validation used before a recovery/producer close:
//! gaps, twins and malformed runs fail rather than yielding a shorter prefix.
//! Closure authority and uniqueness remain with the caller.
use super::reconstruction::{ObservedSegment, ReconstructedStream, StreamDeclarations};
use super::{
    OutputSegment, OutputSegmentShape, OutputSource, OutputWriter, ReconstructionError, SegmentRun,
    SourceClose, StreamDeclaration,
};
use chrono::DateTime;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub struct OpenSourceExtent {
    pub segments: u32,
    pub streams: Vec<ReconstructedStream>,
    pub stream_bytes: Vec<u64>,
    pub last_created_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OpenSourceShape {
    pub segments: u32,
    pub stream_bytes: Vec<u64>,
    pub last_created_at: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObservedShape<'a> {
    pub doc_id: &'a str,
    pub node_did: &'a str,
    pub requester_did: Option<&'a str>,
    pub session_id: &'a str,
    pub request_doc_id: &'a str,
    pub source: &'a OutputSource,
    pub writer: &'a OutputWriter,
    pub ordinal: Option<u32>,
    pub runs: &'a [SegmentRun],
    pub close: Option<&'a SourceClose>,
    pub created_at: &'a str,
    pub payload: Option<&'a str>,
}

impl<'a> ObservedShape<'a> {
    pub fn from_segment(doc_id: &'a str, segment: &'a OutputSegment) -> Self {
        Self {
            doc_id,
            node_did: &segment.node_did,
            requester_did: segment.requester_did.as_deref(),
            session_id: &segment.session_id,
            request_doc_id: &segment.request_doc_id,
            source: &segment.source,
            writer: &segment.writer,
            ordinal: segment.ordinal,
            runs: &segment.runs,
            close: segment.close.as_ref(),
            created_at: &segment.created_at,
            payload: Some(&segment.payload),
        }
    }

    pub fn from_shape(doc_id: &'a str, shape: &'a OutputSegmentShape) -> Self {
        Self {
            doc_id,
            node_did: &shape.node_did,
            requester_did: shape.requester_did.as_deref(),
            session_id: &shape.session_id,
            request_doc_id: &shape.request_doc_id,
            source: &shape.source,
            writer: &shape.writer,
            ordinal: shape.ordinal,
            runs: &shape.runs,
            close: shape.close.as_ref(),
            created_at: &shape.created_at,
            payload: None,
        }
    }
}

fn facts_agree(left: &ObservedShape<'_>, right: &ObservedShape<'_>) -> bool {
    ObservedShape {
        doc_id: "",
        ..*left
    } == ObservedShape {
        doc_id: "",
        ..*right
    }
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

struct StreamAccumulator {
    declaration: StreamDeclaration,
    bytes: u64,
    text: Option<String>,
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
    let observed = records
        .iter()
        .map(|record| ObservedShape::from_segment(record.doc_id, record.segment))
        .collect::<Vec<_>>();
    let (shape, streams) = inspect(&observed, request_doc_id, source, expected_writer)?;
    let streams = streams.ok_or_else(|| bad("payload-free records cannot reconstruct text"))?;
    Ok(OpenSourceExtent {
        segments: shape.segments,
        streams,
        stream_bytes: shape.stream_bytes,
        last_created_at: shape.last_created_at,
    })
}

pub fn inspect_open_source_shape(
    records: &[ObservedShape<'_>],
    request_doc_id: &str,
    source: &OutputSource,
    expected_writer: &OutputWriter,
) -> Result<OpenSourceShape, ReconstructionError> {
    Ok(inspect(records, request_doc_id, source, expected_writer)?.0)
}

fn inspect(
    records: &[ObservedShape<'_>],
    request_doc_id: &str,
    source: &OutputSource,
    expected_writer: &OutputWriter,
) -> Result<(OpenSourceShape, Option<Vec<ReconstructedStream>>), ReconstructionError> {
    if !writer_matches(source, expected_writer) {
        return Err(bad("open source writer does not bind its source"));
    }
    let scoped: Vec<_> = records
        .iter()
        .copied()
        .filter(|r| r.request_doc_id == request_doc_id && *r.source == *source)
        .collect();
    if scoped.is_empty() {
        return Ok((
            OpenSourceShape {
                segments: 0,
                stream_bytes: vec![],
                last_created_at: None,
            },
            Some(vec![]),
        ));
    }
    let reconstructs_text = scoped.iter().all(|r| r.payload.is_some());
    let writer = expected_writer;
    let mut slots: BTreeMap<u32, Vec<ObservedShape<'_>>> = BTreeMap::new();
    let mut physical: BTreeMap<&str, ObservedShape<'_>> = BTreeMap::new();
    for r in scoped {
        if let Some(previous) = physical.insert(r.doc_id, r) {
            if !facts_agree(&previous, &r) {
                return Err(bad("one physical segment identity has conflicting facts"));
            }
        }
        if *r.writer != *writer {
            return Err(bad("open source records disagree on writer"));
        };
        let Some(n) = r.ordinal else {
            // A terminal-only closure has no data coordinate and is irrelevant
            // to prefix accounting; closure uniqueness is the caller's owner.
            if r.close.is_some()
                && r.runs.is_empty()
                && r.payload.is_none_or(|payload| payload.is_empty())
            {
                continue;
            }
            return Err(bad("record has neither data ordinal nor closure"));
        };
        if r.runs.is_empty() {
            return Err(bad("open flush has no runs"));
        };
        slots.entry(n).or_default().push(r);
    }
    let mut streams: Vec<StreamAccumulator> = vec![];
    let mut declarations = StreamDeclarations::default();
    let mut last_time = None;
    for (expected, (ordinal, slot)) in slots.iter().enumerate() {
        let expected = u32::try_from(expected).map_err(|_| bad("ordinal exceeds u32"))?;
        if *ordinal != expected {
            return Err(bad("open extent has ordinal gap"));
        }
        let r = slot[0];
        if slot
            .iter()
            .any(|x| x.doc_id != r.doc_id || !facts_agree(x, &r))
        {
            return Err(bad("open extent has conflicting flush twin"));
        };
        let time = DateTime::parse_from_rfc3339(r.created_at)
            .map_err(|_| bad("open flush has malformed timestamp"))?;
        if last_time
            .as_ref()
            .is_some_and(|prev: &DateTime<chrono::FixedOffset>| time < *prev)
        {
            return Err(bad("open flush timestamps decrease"));
        };
        last_time = Some(time);
        let mut offset = 0usize;
        for run in r.runs {
            if run.bytes == 0 && run.declaration.is_none() {
                return Err(bad("zero-byte continuation"));
            };
            let end = offset
                .checked_add(run.bytes as usize)
                .ok_or_else(|| bad("run overflow"))?;
            let part = match r.payload {
                Some(payload) => Some(
                    payload
                        .get(offset..end)
                        .ok_or_else(|| bad("runs split UTF-8 or exceed payload"))?,
                ),
                None => None,
            };
            if let Some(decl) = &run.declaration {
                if run.stream as usize != streams.len() || !declarations.admit(decl, run.bytes) {
                    return Err(bad(
                        "stream declarations are not dense or positions conflict",
                    ));
                };
                streams.push(StreamAccumulator {
                    declaration: decl.clone(),
                    bytes: u64::from(run.bytes),
                    text: reconstructs_text.then(|| part.unwrap_or_default().to_owned()),
                })
            } else {
                let stream = streams
                    .get_mut(run.stream as usize)
                    .ok_or_else(|| bad("continuation before declaration"))?;
                if !declarations.allows_continuation(&stream.declaration) {
                    return Err(bad("continuation after reasoning stop"));
                }
                stream.bytes += u64::from(run.bytes);
                if let (Some(text), Some(part)) = (stream.text.as_mut(), part) {
                    text.push_str(part)
                }
            };
            offset = end
        }
        if let Some(payload) = r.payload {
            if offset != payload.len() {
                return Err(bad("runs do not partition payload"));
            }
        }
    }
    let stream_bytes = streams.iter().map(|s| s.bytes).collect();
    let reconstructed = reconstructs_text.then(|| {
        streams
            .into_iter()
            .map(|s| ReconstructedStream {
                declaration: s.declaration,
                text: s.text.unwrap_or_default(),
            })
            .collect()
    });
    Ok((
        OpenSourceShape {
            segments: slots.len() as u32,
            stream_bytes,
            last_created_at: last_time.map(|t| t.to_rfc3339()),
        },
        reconstructed,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{OutputOutcome, StreamPayload};

    fn segment(
        input: &gents_lean_vocab::families::open_output_shape_inspect::RecordInput<String>,
    ) -> OutputSegment {
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

    fn summary(
        segments: u32,
        stream_bytes: Vec<u64>,
        last_created_at: Option<String>,
    ) -> gents_lean_vocab::families::open_output_shape_inspect::OpenShape {
        gents_lean_vocab::families::open_output_shape_inspect::OpenShape {
            segments: segments.into(),
            stream_bytes,
            last_created_at: last_created_at.map(|value| {
                DateTime::parse_from_rfc3339(&value)
                    .unwrap()
                    .timestamp()
                    .try_into()
                    .unwrap()
            }),
        }
    }

    #[test]
    fn generated_open_source_shapes_match_model() {
        for case in gents_lean_vocab::families::open_output_shape_inspect::cases::<String>() {
            let records: Vec<_> = case
                .input
                .iter()
                .map(|input| (input.id.to_string(), segment(input)))
                .collect();
            let full: Vec<_> = records
                .iter()
                .map(|(id, segment)| ObservedSegment {
                    doc_id: id,
                    segment,
                })
                .collect();
            let shapes: Vec<_> = records
                .iter()
                .map(|(id, segment)| ObservedShape {
                    payload: None,
                    ..ObservedShape::from_segment(id, segment)
                })
                .collect();
            let source = OutputSource::ToolCall {
                tool_call_doc_id: "tool-1".into(),
            };
            let writer = OutputWriter::ToolExecution {
                tool_call_doc_id: "tool-1".into(),
            };
            let full = inspect_open_source(&full, "request-1", &source, &writer)
                .ok()
                .map(|result| {
                    summary(result.segments, result.stream_bytes, result.last_created_at)
                });
            let shape = inspect_open_source_shape(&shapes, "request-1", &source, &writer)
                .ok()
                .map(|result| {
                    summary(result.segments, result.stream_bytes, result.last_created_at)
                });
            case.check(
                gents_lean_vocab::families::open_output_shape_inspect::Output { full, shape },
            );
        }
    }

    #[test]
    fn malformed_timestamp_is_rejected_by_full_and_shape_inspection() {
        let input: gents_lean_vocab::families::open_output_shape_inspect::RecordInput<String> =
            serde_json::from_value(serde_json::json!({
                "id": 1, "request": 1, "source": 1, "writer": 1, "ordinal": 0,
                "runs": [{
                    "stream": 0, "bytes": 3,
                    "declaration": { "block": 0, "part": 0, "kind": "tool_output" },
                }],
                "payload": "one", "close": false, "created_at": 0,
            }))
            .unwrap();
        let mut record = segment(&input);
        record.created_at = "yesterday".into();
        assert!(inspect_open_source(
            &[ObservedSegment {
                doc_id: "a",
                segment: &record
            }],
            "request-1",
            &record.source,
            &record.writer
        )
        .is_err());
        assert!(inspect_open_source_shape(
            &[ObservedShape {
                payload: None,
                ..ObservedShape::from_segment("a", &record)
            }],
            "request-1",
            &record.source,
            &record.writer
        )
        .is_err());
    }
}
