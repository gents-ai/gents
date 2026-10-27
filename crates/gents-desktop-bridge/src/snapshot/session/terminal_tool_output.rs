use gents::session::canonical_rows::OutputSegmentRow;
use gents_protocol::output::reconstruction::{
    reconstruct_presented_payload, reconstruct_stream, tool_output_reference_owned,
    DependencyDenial, ObservedSegment,
};
use gents_protocol::output::{
    OutputSource, OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload,
    ReconstructionError, StreamPayload,
};
use gents_protocol::row::AgentToolCallRow;
use gents_protocol::wait_status::WaitLifecycle;

/// Spawned executions have no provider ToolResult header. Their closed physical
/// ToolOutput source remains the result owner after the live tail stops.
pub(super) fn terminal_tool_output(
    tool: &AgentToolCallRow,
    rows: &[OutputSegmentRow],
    denied: &[String],
    denials: &[DependencyDenial],
) -> Result<Option<String>, ReconstructionError> {
    if !WaitLifecycle::from_persisted(tool.lifecycle_state.as_deref()).is_terminal() {
        return Ok(None);
    }
    let invalid = || ReconstructionError::InvalidStructure {
        detail: "terminal tool output has conflicting or foreign ownership".to_owned(),
    };
    let (Some(tool_id), Some(request), Some(node), Some(session)) = (
        tool.doc_id.as_deref(),
        tool.request_doc_id.as_deref(),
        tool.node_did.as_deref(),
        tool.session_id.as_deref(),
    ) else {
        return Err(invalid());
    };
    let source = OutputSource::ToolCall {
        tool_call_doc_id: tool_id.to_owned(),
    };
    let writer = OutputWriter::ToolExecution {
        tool_call_doc_id: tool_id.to_owned(),
    };
    let scoped = rows
        .iter()
        .filter(|row| {
            row.segment.node_did == node
                && row.segment.requester_did == tool.requester_did
                && row.segment.session_id == session
                && row.segment.request_doc_id == request
                && row.segment.source == source
                && row.segment.writer == writer
        })
        .collect::<Vec<_>>();
    let fallback;
    let payload = if let Some(payload) = &tool.terminal_output {
        payload
    } else {
        let mut closes = scoped.iter().filter(|row| row.segment.close.is_some());
        let Some(close) = closes.next() else {
            if tool.lifecycle_state.as_deref() == Some("cancelled") && tool.started_at.is_none() {
                return Ok(None);
            }
            return Err(ReconstructionError::UnresolvedClose {
                close_doc_id: tool_id.to_owned(),
            });
        };
        if closes.any(|other| other.doc_id != close.doc_id || other.segment != close.segment) {
            return Err(invalid());
        }
        fallback = PresentedPayload {
            output: PayloadRef {
                close_doc_id: close.doc_id.clone(),
                stream: 0,
            },
            presentation: PayloadPresentation::Full,
        };
        &fallback
    };
    let observed = scoped
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    let stream = reconstruct_stream(&observed, denied, denials, &payload.output)?;
    if stream.declaration.payload != StreamPayload::ToolOutput
        || !tool_output_reference_owned(&observed, &payload.output, request, tool_id)
    {
        return Err(invalid());
    }
    let text = reconstruct_presented_payload(&observed, denied, denials, payload)?;
    Ok(Some(
        gents::truncation::truncate(
            &text,
            gents::truncation::TruncationMode::Tail,
            &gents::truncation::TruncationLimits::default(),
        )
        .text,
    ))
}

#[cfg(test)]
#[path = "terminal_tool_output/tests.rs"]
mod tests;
