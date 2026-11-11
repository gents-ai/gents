use super::*;
use gents_protocol::output::{
    OutputOutcome, OutputSegment, SegmentRun, SourceClose, StreamDeclaration,
};
use serde_json::json;

fn fixture() -> (AgentToolCallRow, Vec<OutputSegmentRow>) {
    let tool = serde_json::from_value(json!({
        "_docID":"child", "tool_call_key":"key", "node_did":"node", "requester_did":"reader",
        "session_id":"session", "request_doc_id":"request", "lifecycle_state":"completed",
        "spawned_by_tool_call_doc_id":"parent", "started_at":"2026-10-03T00:00:00Z"
    }))
    .unwrap();
    let segment = OutputSegment {
        node_did: "node".into(),
        requester_did: Some("reader".into()),
        session_id: "session".into(),
        request_doc_id: "request".into(),
        source: OutputSource::ToolCall {
            tool_call_doc_id: "child".into(),
        },
        writer: OutputWriter::ToolExecution {
            tool_call_doc_id: "child".into(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: 12,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolOutput,
            }),
        }],
        payload: "final output".into(),
        close: None,
        created_at: "2026-10-03T00:00:00Z".into(),
    };
    let mut close = segment.clone();
    close.ordinal = None;
    close.runs.clear();
    close.payload.clear();
    close.close = Some(SourceClose::Closed {
        outcome: OutputOutcome::Complete,
        segments: 1,
        stream_bytes: vec![12],
    });
    (
        tool,
        vec![
            OutputSegmentRow {
                doc_id: "flush".into(),
                segment,
            },
            OutputSegmentRow {
                doc_id: "close".into(),
                segment: close,
            },
        ],
    )
}

#[test]
fn completed_spawn_retains_output_and_bounds_the_display() {
    let (tool, mut rows) = fixture();
    assert_eq!(
        terminal_tool_output(&tool, &rows, &[], &[])
            .unwrap()
            .as_deref(),
        Some("final output")
    );
    let text = "λ🙂".repeat(10000) + " final output";
    rows[0].segment.payload = text.clone();
    rows[0].segment.runs[0].bytes = text.len().try_into().unwrap();
    rows[1].segment.close = Some(SourceClose::Closed {
        outcome: OutputOutcome::Complete,
        segments: 1,
        stream_bytes: vec![text.len() as u64],
    });
    let result = terminal_tool_output(&tool, &rows, &[], &[])
        .unwrap()
        .unwrap();
    assert!(result.ends_with(" final output"));
    assert!(result.len() <= gents::truncation::LIVE_STREAM_CAPACITY_BYTES);
    assert!(result.contains("[Showing last"));
}

#[test]
fn terminal_spawn_waits_for_exact_output_and_never_uses_a_foreign_source() {
    let (tool, rows) = fixture();
    assert!(terminal_tool_output(&tool, &rows[..1], &[], &[])
        .unwrap_err()
        .is_incomplete());
    for field in ["node", "reader", "session", "request", "source", "writer"] {
        let mut foreign = rows.clone();
        for row in &mut foreign {
            match field {
                "node" => row.segment.node_did = "other".into(),
                "reader" => row.segment.requester_did = None,
                "session" => row.segment.session_id = "other".into(),
                "request" => row.segment.request_doc_id = "other".into(),
                "source" => {
                    row.segment.source = OutputSource::Authored {
                        key: "receipt".into(),
                    }
                }
                "writer" => {
                    row.segment.writer = OutputWriter::ToolExecution {
                        tool_call_doc_id: "other".into(),
                    }
                }
                _ => unreachable!(),
            }
        }
        assert!(
            terminal_tool_output(&tool, &foreign, &[], &[]).is_err(),
            "{field}"
        );
    }
    let mut conflict = rows.clone();
    let mut other = rows[1].clone();
    other.doc_id = "other-close".into();
    conflict.push(other);
    assert!(terminal_tool_output(&tool, &conflict, &[], &[]).is_err());
}

#[test]
fn explicit_terminal_presentation_preserves_selection_and_denial() {
    use gents_protocol::output::PresentationPart;
    let (mut tool, rows) = fixture();
    tool.terminal_output = Some(PresentedPayload {
        output: PayloadRef {
            close_doc_id: "close".into(),
            stream: 0,
        },
        presentation: PayloadPresentation::Composed {
            parts: vec![PresentationPart::OutputRange {
                start_byte: 6,
                end_byte: 12,
            }],
        },
    });
    assert_eq!(
        terminal_tool_output(&tool, &rows, &[], &[])
            .unwrap()
            .as_deref(),
        Some("output")
    );
    assert!(matches!(
        terminal_tool_output(&tool, &rows, &["flush".into()], &[]),
        Err(ReconstructionError::AccessDenied { .. })
    ));
    assert!(matches!(
        terminal_tool_output(
            &tool,
            &[],
            &[],
            &[DependencyDenial {
                root_close_id: "close".into(),
                denied_doc_id: "flush".into()
            }]
        ),
        Err(ReconstructionError::AccessDenied { .. })
    ));
}

#[test]
fn running_and_never_started_cancelled_tools_do_not_invent_a_result() {
    let (mut tool, rows) = fixture();
    tool.lifecycle_state = Some("running".into());
    assert_eq!(terminal_tool_output(&tool, &rows, &[], &[]).unwrap(), None);
    tool.lifecycle_state = Some("cancelled".into());
    tool.started_at = None;
    assert_eq!(terminal_tool_output(&tool, &[], &[], &[]).unwrap(), None);
}
