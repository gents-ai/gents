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

/// The cross-record facts of an open source: what an appender needs to place
/// its next record, derived without the prior records' payload.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenSourceShape {
    pub segments: u32,
    pub stream_bytes: Vec<u64>,
    pub last_created_at: Option<String>,
}

/// One observed record's structural facts. `payload` is absent when the row
/// came from a payload-free projection. Every cross-record check below is then
/// decided from `runs`: that a record's run byte counts partition its own
/// payload is a property of that immutable record, established by the write
/// that created it, so only the payload-local checks are skipped.
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

/// Two observations state the same facts. Physical identity is excluded so one
/// `_docID` observed twice and an ordinal twin are compared the same way.
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

/// The same validation over a payload-free projection of the same records.
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

    fn tool_source() -> OutputSource {
        OutputSource::ToolCall {
            tool_call_doc_id: "tool".into(),
        }
    }

    fn tool_writer() -> OutputWriter {
        OutputWriter::ToolExecution {
            tool_call_doc_id: "tool".into(),
        }
    }

    fn segment(ordinal: Option<u32>, runs: Vec<SegmentRun>, payload: &str) -> OutputSegment {
        OutputSegment {
            node_did: "node".into(),
            requester_did: Some("requester".into()),
            session_id: "session".into(),
            request_doc_id: "request".into(),
            source: tool_source(),
            writer: tool_writer(),
            ordinal,
            runs,
            payload: payload.into(),
            close: None,
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn declared(bytes: u32, payload: StreamPayload) -> SegmentRun {
        SegmentRun {
            stream: 0,
            bytes,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload,
            }),
        }
    }

    fn continued(stream: u32, bytes: u32) -> SegmentRun {
        SegmentRun {
            stream,
            bytes,
            declaration: None,
        }
    }

    fn flush(ordinal: u32, text: &str) -> OutputSegment {
        let run = if ordinal == 0 {
            declared(text.len() as u32, StreamPayload::ToolOutput)
        } else {
            continued(0, text.len() as u32)
        };
        let mut segment = segment(Some(ordinal), vec![run], text);
        segment.created_at = format!("2026-01-01T00:00:{:02}Z", ordinal);
        segment
    }

    /// Each run's byte count partitions the record's own payload on UTF-8
    /// boundaries: the property the record's own write established, and the
    /// only one a payload-free projection cannot recheck.
    fn locally_well_formed(segment: &OutputSegment) -> bool {
        let mut offset = 0usize;
        for run in &segment.runs {
            let Some(end) = offset.checked_add(run.bytes as usize) else {
                return false;
            };
            if segment.payload.get(offset..end).is_none() {
                return false;
            }
            offset = end;
        }
        offset == segment.payload.len()
    }

    fn cases() -> Vec<(&'static str, Vec<(&'static str, OutputSegment)>)> {
        let mut cases: Vec<(&'static str, Vec<(&'static str, OutputSegment)>)> = vec![
            ("empty", vec![]),
            ("one flush", vec![("a", flush(0, "hello"))]),
            (
                "three flushes",
                vec![
                    ("a", flush(0, "one")),
                    ("b", flush(1, "two")),
                    ("c", flush(2, "three")),
                ],
            ),
            (
                "ordinal gap",
                vec![("a", flush(0, "one")), ("c", flush(2, "three"))],
            ),
            (
                "ordinal twin",
                vec![("a", flush(0, "one")), ("b", flush(0, "two"))],
            ),
            (
                "identical ordinal twin under two identities",
                vec![("a", flush(1, "two")), ("b", flush(1, "two"))],
            ),
            (
                "repeated identity",
                vec![("a", flush(0, "one")), ("a", flush(0, "one"))],
            ),
            (
                "repeated identity with conflicting facts",
                vec![("a", flush(0, "one")), ("a", flush(1, "two"))],
            ),
            (
                "decreasing timestamps",
                vec![
                    ("a", {
                        let mut first = flush(0, "one");
                        first.created_at = "2026-01-01T00:00:09Z".into();
                        first
                    }),
                    ("b", flush(1, "two")),
                ],
            ),
            (
                "malformed timestamp",
                vec![("a", {
                    let mut first = flush(0, "one");
                    first.created_at = "yesterday".into();
                    first
                })],
            ),
            (
                "foreign request",
                vec![("a", {
                    let mut first = flush(0, "one");
                    first.request_doc_id = "other".into();
                    first
                })],
            ),
            (
                "foreign writer",
                vec![("a", {
                    let mut first = flush(0, "one");
                    first.writer = OutputWriter::ToolExecution {
                        tool_call_doc_id: "other".into(),
                    };
                    first
                })],
            ),
            (
                "terminal-only closure",
                vec![
                    ("a", flush(0, "one")),
                    ("z", {
                        let mut close = segment(None, vec![], "");
                        close.close = Some(SourceClose::Closed {
                            outcome: OutputOutcome::Complete,
                            segments: 1,
                            stream_bytes: vec![3],
                        });
                        close
                    }),
                ],
            ),
            (
                "record with neither ordinal nor closure",
                vec![("a", segment(None, vec![], ""))],
            ),
            (
                "flush without runs",
                vec![("a", segment(Some(0), vec![], ""))],
            ),
            (
                "zero-byte continuation",
                vec![
                    ("a", flush(0, "one")),
                    ("b", segment(Some(1), vec![continued(0, 0)], "")),
                ],
            ),
            (
                "continuation before declaration",
                vec![("a", segment(Some(0), vec![continued(0, 3)], "one"))],
            ),
            (
                "second declaration after a first stream",
                vec![("a", {
                    segment(
                        Some(0),
                        vec![
                            declared(1, StreamPayload::Reasoning),
                            SegmentRun {
                                stream: 1,
                                bytes: 1,
                                declaration: Some(StreamDeclaration {
                                    block_index: 0,
                                    part_index: 1,
                                    payload: StreamPayload::Text,
                                }),
                            },
                        ],
                        "RT",
                    )
                })],
            ),
            (
                "conflicting declaration position",
                vec![("a", {
                    segment(
                        Some(0),
                        vec![
                            declared(1, StreamPayload::Text),
                            declared(1, StreamPayload::Text),
                        ],
                        "AB",
                    )
                })],
            ),
            (
                "reasoning stop then continuation",
                vec![
                    ("a", {
                        segment(
                            Some(0),
                            vec![
                                declared(1, StreamPayload::Reasoning),
                                SegmentRun {
                                    stream: 1,
                                    bytes: 0,
                                    declaration: Some(StreamDeclaration {
                                        block_index: 0,
                                        part_index: 0,
                                        payload: StreamPayload::ReasoningStop,
                                    }),
                                },
                            ],
                            "R",
                        )
                    }),
                    ("b", segment(Some(1), vec![continued(0, 1)], "x")),
                ],
            ),
            (
                "runs do not partition payload",
                vec![(
                    "a",
                    segment(Some(0), vec![declared(2, StreamPayload::ToolOutput)], "abc"),
                )],
            ),
            (
                "runs exceed payload",
                vec![(
                    "a",
                    segment(Some(0), vec![declared(9, StreamPayload::ToolOutput)], "abc"),
                )],
            ),
            (
                "runs split UTF-8",
                vec![("a", {
                    segment(
                        Some(0),
                        vec![declared(1, StreamPayload::ToolOutput), continued(0, 2)],
                        "é",
                    )
                })],
            ),
            (
                "terminal-only closure carrying payload",
                vec![("z", {
                    let mut close = segment(None, vec![], "bytes");
                    close.close = Some(SourceClose::Closed {
                        outcome: OutputOutcome::Complete,
                        segments: 0,
                        stream_bytes: vec![],
                    });
                    close
                })],
            ),
        ];
        let mut long = Vec::new();
        for ordinal in 0..8u32 {
            long.push((
                ["a", "b", "c", "d", "e", "f", "g", "h"][ordinal as usize],
                flush(ordinal, "chunk"),
            ));
        }
        cases.push(("eight flushes", long));
        cases
    }

    fn summary(
        result: &Result<OpenSourceExtent, ReconstructionError>,
    ) -> Result<(u32, Vec<u64>, Option<String>), String> {
        match result {
            Ok(extent) => Ok((
                extent.segments,
                extent.stream_bytes.clone(),
                extent.last_created_at.clone(),
            )),
            Err(error) => Err(error.to_string()),
        }
    }

    fn shape_summary(
        result: &Result<OpenSourceShape, ReconstructionError>,
    ) -> Result<(u32, Vec<u64>, Option<String>), String> {
        match result {
            Ok(extent) => Ok((
                extent.segments,
                extent.stream_bytes.clone(),
                extent.last_created_at.clone(),
            )),
            Err(error) => Err(error.to_string()),
        }
    }

    /// The payload-free inspector decides exactly what the full one decides,
    /// for every record list a source read can return: one row per physical
    /// identity, each already satisfying its own run/payload partition.
    #[test]
    fn shape_inspection_agrees_with_full_inspection() {
        for (name, records) in cases() {
            let full = records
                .iter()
                .map(|(doc_id, segment)| ObservedSegment { doc_id, segment })
                .collect::<Vec<_>>();
            let shapes = records
                .iter()
                .map(|(doc_id, segment)| ObservedShape::from_segment(doc_id, segment))
                .map(|observed| ObservedShape {
                    payload: None,
                    ..observed
                })
                .collect::<Vec<_>>();
            let expected = inspect_open_source(&full, "request", &tool_source(), &tool_writer());
            let actual =
                inspect_open_source_shape(&shapes, "request", &tool_source(), &tool_writer());
            // One physical identity observed twice can only differ in payload
            // in a hand-built list; a source read returns one row per identity.
            let payload_only_conflict =
                records.iter().enumerate().any(|(index, (doc_id, left))| {
                    records.iter().skip(index + 1).any(|(other, right)| {
                        other == doc_id
                            && left.payload != right.payload
                            && left.shape() == right.shape()
                    })
                });
            let readable = !payload_only_conflict
                && records
                    .iter()
                    .all(|(_, segment)| locally_well_formed(segment));
            if readable {
                assert_eq!(
                    summary(&expected),
                    shape_summary(&actual),
                    "{name}: payload-free inspection disagreed"
                );
            } else {
                assert!(
                    expected.is_err(),
                    "{name}: a record the projection cannot recheck must be rejected by the full inspector"
                );
            }
        }
    }

    /// Tool appends derive the next ordinal and byte offset from the shape of
    /// the prefix alone.
    #[test]
    fn shape_inspection_counts_segments_and_bytes() {
        let records = (0..5u32)
            .map(|ordinal| (format!("segment-{ordinal}"), flush(ordinal, "chunk")))
            .collect::<Vec<_>>();
        let shapes = records
            .iter()
            .map(|(doc_id, segment)| ObservedShape {
                payload: None,
                ..ObservedShape::from_segment(doc_id, segment)
            })
            .collect::<Vec<_>>();
        let extent =
            inspect_open_source_shape(&shapes, "request", &tool_source(), &tool_writer()).unwrap();
        assert_eq!(extent.segments, 5);
        assert_eq!(extent.stream_bytes, vec![25]);
    }
}
