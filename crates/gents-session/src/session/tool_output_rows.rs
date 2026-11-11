use anyhow::{Context, Result};
use gents_protocol::output::reconstruction::{reconstruct_stream, ObservedSegment};
use gents_protocol::output::rows::{
    decode_scoped_request_output_segments, request_output_segments_query,
};
use gents_protocol::output::{OutputSource, OutputWriter, PayloadRef};
use serde::Deserialize;

/// Reads without an actor. Output segments are Agent-owned and invisible to
/// it, so it exists only for unowned fixtures; runtime readers use
/// [`observe_canonical_tool_output_with_access`] with the owning Agent.
#[cfg(test)]
pub async fn canonical_tool_output(
    node: &defra_node::Node,
    tool_doc_id: &str,
    request_doc_id: &str,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<String> {
    let response = gents_store::graphql::graphql_with_transaction_retry(
        node,
        &request_output_segments_query(request_doc_id),
        "query canonical tool output",
    )
    .await?;
    let values = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentOutputSegment"))
        .and_then(serde_json::Value::as_array)
        .context("canonical tool output query omitted segments")?;
    let rows =
        decode_scoped_request_output_segments(values, node_did, Some(session_id), requester_did)?;
    Ok(canonical_tool_output_from_rows(rows, tool_doc_id, request_doc_id)?.into_text())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CanonicalToolOutputObservation {
    Open(String),
    Closed(String),
}

impl CanonicalToolOutputObservation {
    pub fn into_text(self) -> String {
        match self {
            Self::Open(text) | Self::Closed(text) => text,
        }
    }
}

pub async fn observe_canonical_tool_output_with_access(
    access: &gents_store::config_client::ConfigAccess,
    tool_doc_id: &str,
    request_doc_id: &str,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<CanonicalToolOutputObservation> {
    let response = access
        .execute(&request_output_segments_query(request_doc_id))
        .await?;
    let values = response
        .pointer("/data/AgentOutputSegment")
        .and_then(serde_json::Value::as_array)
        .context("canonical tool output query omitted segments")?;
    let rows =
        decode_scoped_request_output_segments(values, node_did, Some(session_id), requester_did)?;
    canonical_tool_output_from_rows(rows, tool_doc_id, request_doc_id)
}

pub fn canonical_tool_output_from_rows(
    rows: Vec<gents_protocol::output::rows::OutputSegmentRow>,
    tool_doc_id: &str,
    request_doc_id: &str,
) -> Result<CanonicalToolOutputObservation> {
    let source = OutputSource::ToolCall {
        tool_call_doc_id: tool_doc_id.to_owned(),
    };
    let rows = rows
        .into_iter()
        .filter(|row| row.segment.source == source)
        .collect::<Vec<_>>();
    let close = rows
        .iter()
        .filter(|row| row.segment.close.is_some())
        .collect::<Vec<_>>();
    let observations = rows
        .iter()
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    if close.is_empty() {
        let writer = OutputWriter::ToolExecution {
            tool_call_doc_id: tool_doc_id.to_owned(),
        };
        let extent = gents_protocol::output::extent::inspect_open_source(
            &observations,
            request_doc_id,
            &source,
            &writer,
        )?;
        if extent.streams.is_empty() {
            return Ok(CanonicalToolOutputObservation::Open(String::new()));
        }
        anyhow::ensure!(
            extent.streams.len() == 1
                && matches!(
                    extent.streams[0].declaration.payload,
                    gents_protocol::output::StreamPayload::ToolOutput
                ),
            "open tool output source does not have one ToolOutput stream"
        );
        return Ok(CanonicalToolOutputObservation::Open(
            extent.streams[0].text.clone(),
        ));
    }
    anyhow::ensure!(
        close.len() == 1,
        "tool output is unresolved or has conflicting closures"
    );
    let stream = reconstruct_stream(
        &observations,
        &[],
        &[],
        &PayloadRef {
            close_doc_id: close[0].doc_id.clone(),
            stream: 0,
        },
    )
    .map_err(anyhow::Error::from)?;
    anyhow::ensure!(
        matches!(
            stream.declaration.payload,
            gents_protocol::output::StreamPayload::ToolOutput
        ),
        "tool output source stream is not a ToolOutput stream"
    );
    Ok(CanonicalToolOutputObservation::Closed(stream.text))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackgroundToolArgs {
    pub tool_name: String,
    pub args: serde_json::Map<String, serde_json::Value>,
}
