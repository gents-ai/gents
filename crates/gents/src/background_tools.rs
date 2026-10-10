mod final_output;
pub(crate) mod r4c_args;

use crate::llm::message::{AssistantContent, Message, Text};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use crate::graphql::escape_graphql_string;

use crate::session::canonical_rows::{
    decode_scoped_request_output_segments, request_output_segments_query,
};
use gents_protocol::output::reconstruction::{reconstruct_stream, ObservedSegment};
use gents_protocol::output::{OutputSource, OutputWriter, PayloadRef};

use self::r4c_args::{
    ListBackgroundToolsArgs, ListBackgroundToolsEntry, ListBackgroundToolsResponse,
    ListStatusFilter, ReadToolOutputArgs, ReadToolOutputResponse,
};
pub(crate) use gents_loop::live_output::{
    LiveOutputStream, LiveToolOutputRegistry, LiveToolOutputWriter,
};

/// Immutable identity boundary used by `list_processes`, `read_process`,
/// `wait_process`, and `cancel_process`. A handle is usable on a later request
/// in the same session when the agent and requester DIDs still match.
/// Two absent requester identities are the same anonymous scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessControlScope {
    pub(crate) request_id: String,
    pub(crate) session_id: String,
    pub(crate) node_did: String,
    pub(crate) requester_did: Option<String>,
}

impl ProcessControlScope {
    pub(crate) fn authorizes(
        &self,
        owner_session_id: &str,
        owner_node_did: &str,
        owner_requester_did: Option<&str>,
    ) -> bool {
        self.session_id == owner_session_id
            && self.node_did == owner_node_did
            && self.requester_did.as_deref() == owner_requester_did
    }
}

/// `spawn_process` input, decoded exactly as its advertised schema states:
/// both fields required, `args` an object, nothing else. A wider decoder
/// returns a running receipt for input the target cannot decode, and that
/// target's rejection reaches the model only later, as a failed completion.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BackgroundToolArgs {
    pub tool_name: String,
    pub args: serde_json::Map<String, serde_json::Value>,
}

/// `wait_process` wait when neither the call nor the handle's Tools group
/// (`wait_timeout_secs`) sets one; codex `wait_agent` also defaults to 30s. A
/// wait that times out reports the process as still running without
/// cancelling it (#985).
pub(crate) const DEFAULT_WAIT_PROCESS_TIMEOUT_SECS: u64 = 30;
/// Ceiling on any `wait_process` wait, configured or requested. A longer
/// block holds the caller's turn; the completion notification is the
/// mechanism for long work (grok-build caps blocking waits at 10 minutes).
pub(crate) const MAX_WAIT_PROCESS_TIMEOUT_SECS: u64 = 600;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct WaitToolArgs {
    pub tool_call_id: String,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CancelToolArgs {
    pub tool_call_id: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListBackgroundToolRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    tool_call_id: String,
    tool_name: String,
    request_doc_id: String,
    session_id: String,
    node_did: String,
    requester_did: Option<String>,
    await_mode: Option<String>,
    lifecycle_state: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReadToolOutputRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    tool_call_id: String,
    tool_name: String,
    session_id: Option<String>,
    node_did: Option<String>,
    requester_did: Option<String>,
    await_mode: Option<String>,
    lifecycle_state: Option<String>,
    request_doc_id: Option<String>,
}

pub(crate) enum ReadToolOutputOutcome {
    Found(ReadToolOutputResponse),
    NotAuthorized,
    NotBackgrounded,
}

pub(crate) async fn handle_list_background_tools(
    node: &std::sync::Arc<EmbeddedNode>,
    caller: &ProcessControlScope,
    local_deployment_id: &str,
    live_outputs: &LiveToolOutputRegistry,
    args: ListBackgroundToolsArgs,
) -> Result<ListBackgroundToolsResponse> {
    let limit = args.validated_limit() as usize;
    let escaped_session = escape_graphql_string(&caller.session_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{escaped_session}" }},
                    await_mode: {{ _eq: "background" }}
                }},
                order: {{ started_at: ASC }}
            ) {{
                _docID
                tool_call_id
                tool_name
                request_doc_id
                session_id
                node_did
                requester_did
                await_mode
                lifecycle_state
                started_at
                completed_at
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!("list_background_tools query failed: {:?}", response.errors);
    }
    let rows: Vec<ListBackgroundToolRow> =
        rows_skipping_malformed(response.data.as_ref(), "AgentToolCall")?;

    let mut entries = Vec::new();
    for row in rows {
        if !caller.authorizes(&row.session_id, &row.node_did, row.requester_did.as_deref()) {
            continue;
        }
        if row.await_mode.as_deref() != Some("background") {
            continue;
        }
        let Some(tool_call_id) = non_empty_string(Some(&row.tool_call_id)) else {
            continue;
        };
        let status = row
            .lifecycle_state
            .as_deref()
            .filter(|state| !state.trim().is_empty())
            .unwrap_or("running");
        if !list_status_matches(args.status, status) {
            continue;
        }
        let Some(created_at) = parse_rfc3339(row.started_at.as_deref()) else {
            tracing::warn!(
                tool_call_id,
                started_at = ?row.started_at,
                "skipping malformed background tool call with invalid started_at"
            );
            continue;
        };
        let last_update = parse_rfc3339(row.completed_at.as_deref()).unwrap_or(created_at);
        let _ = live_outputs;
        let output = canonical_tool_output(
            node,
            &row.doc_id,
            &row.request_doc_id,
            &row.session_id,
            &row.node_did,
            row.requester_did.as_deref(),
        )
        .await?;
        let (stdout_bytes, stderr_bytes) = (output.len() as u64, 0);

        entries.push(ListBackgroundToolsEntry {
            tool_call_id,
            tool_name: row.tool_name,
            deployment_id: local_deployment_id.to_string(),
            await_mode: "background".to_string(),
            status: status.to_string(),
            created_at,
            last_update,
            stdout_bytes,
            stderr_bytes,
        });
    }

    let truncated = entries.len() > limit;
    entries.truncate(limit);
    Ok(ListBackgroundToolsResponse {
        read_at: Utc::now(),
        truncated,
        entries,
    })
}

pub(crate) async fn handle_read_tool_output(
    node: &EmbeddedNode,
    caller: &ProcessControlScope,
    live_outputs: &LiveToolOutputRegistry,
    args: ReadToolOutputArgs,
) -> Result<ReadToolOutputOutcome> {
    read_tool_output_slice(
        node,
        caller,
        live_outputs,
        &args.tool_call_id,
        args.offset,
        args.validated_max_bytes(),
    )
    .await
}

/// Shared authorized output observation. Client snapshots read the retained
/// buffer in one observation; model tools keep their configured page budget.
pub(crate) async fn read_tool_output_slice(
    node: &EmbeddedNode,
    caller: &ProcessControlScope,
    live_outputs: &LiveToolOutputRegistry,
    tool_call_id: &str,
    offset: u64,
    max_bytes: usize,
) -> Result<ReadToolOutputOutcome> {
    let tool_call_id = tool_call_id.trim();
    if tool_call_id.is_empty() {
        return Ok(ReadToolOutputOutcome::NotAuthorized);
    }

    let escaped_tool_call_id = escape_graphql_string(tool_call_id);
    let escaped_session = escape_graphql_string(&caller.session_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{escaped_session}" }},
                    tool_call_id: {{ _eq: "{escaped_tool_call_id}" }}
                }},
                limit: 1
            ) {{
                _docID
                tool_call_id
                tool_name
                request_doc_id
                session_id
                node_did
                requester_did
                await_mode
                lifecycle_state
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!("read_tool_output query failed: {:?}", response.errors);
    }
    let Some(row) = first_row::<ReadToolOutputRow>(response.data.as_ref(), "AgentToolCall") else {
        return Ok(ReadToolOutputOutcome::NotAuthorized);
    };
    if !caller.authorizes(
        row.session_id.as_deref().unwrap_or_default(),
        row.node_did.as_deref().unwrap_or_default(),
        row.requester_did.as_deref(),
    ) {
        return Ok(ReadToolOutputOutcome::NotAuthorized);
    }
    if row.await_mode.as_deref() != Some("background") {
        return Ok(ReadToolOutputOutcome::NotBackgrounded);
    }

    let status = row
        .lifecycle_state
        .as_deref()
        .filter(|state| !state.trim().is_empty())
        .unwrap_or("running")
        .to_string();
    let exited = status != "running";
    let _ = live_outputs;
    let combined = canonical_tool_output(
        node,
        &row.doc_id,
        row.request_doc_id
            .as_deref()
            .context("tool output row lacks request binding")?,
        row.session_id
            .as_deref()
            .context("tool output row lacks session binding")?,
        row.node_did
            .as_deref()
            .context("tool output row lacks agent binding")?,
        row.requester_did.as_deref(),
    )
    .await?;
    let slice = read_combined_output_slice(&combined, offset, max_bytes);
    let exit_code = None;

    Ok(ReadToolOutputOutcome::Found(ReadToolOutputResponse {
        tool_call_id: row.tool_call_id,
        tool_name: row.tool_name,
        status,
        output: slice.output,
        next_offset: slice.next_offset,
        first_available_offset: slice.first_available_offset,
        total_bytes: slice.total_bytes,
        has_more: slice.has_more,
        exited,
        exit_code,
    }))
}

/// Resolve one physical tool source through canonical segment reconstruction.
/// There is deliberately no mutable result-column or volatile ring-buffer
/// fallback.  An open source is explicit: callers must retry after a segment
/// close is visible rather than treating absence as an empty result.
pub(crate) async fn canonical_tool_output(
    node: &EmbeddedNode,
    tool_doc_id: &str,
    request_doc_id: &str,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<String> {
    let response = crate::graphql::graphql_with_transaction_retry(
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
pub(crate) enum CanonicalToolOutputObservation {
    Open(String),
    Closed(String),
}

impl CanonicalToolOutputObservation {
    fn into_text(self) -> String {
        match self {
            Self::Open(text) | Self::Closed(text) => text,
        }
    }
}

pub(crate) async fn observe_canonical_tool_output_with_access(
    access: &crate::config_client::ConfigAccess,
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

pub(crate) fn canonical_tool_output_from_rows(
    rows: Vec<crate::session::canonical_rows::OutputSegmentRow>,
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

struct CombinedOutputSlice {
    output: String,
    next_offset: u64,
    /// Earliest byte offset still readable. 0 for terminal/persisted output
    /// (nothing is ever evicted); for a running tool whose live ring buffer
    /// has overflowed, this is > 0 and a caller can detect that bytes in
    /// `[requested offset, first_available_offset)` were dropped.
    first_available_offset: u64,
    total_bytes: u64,
    has_more: bool,
}

/// Read a contiguous byte slice of the combined buffer starting at `offset`,
/// capped at `max_bytes`. Pages are contiguous from the cursor (no head/tail
/// drop): `next_offset = offset + bytes_returned`, `has_more` is true iff
/// `next_offset < total_bytes`. `offset` past the end yields an empty slice
/// with `next_offset == total_bytes` and `has_more == false`.
fn read_combined_output_slice(
    combined: &str,
    offset: u64,
    max_bytes: usize,
) -> CombinedOutputSlice {
    read_retained_output_slice(combined, 0, combined.len() as u64, offset, max_bytes)
}

fn read_retained_output_slice(
    combined: &str,
    first_offset: u64,
    total_bytes: u64,
    offset: u64,
    max_bytes: usize,
) -> CombinedOutputSlice {
    let bytes = combined.as_bytes();
    let retained_end = first_offset.saturating_add(bytes.len() as u64);
    let total_bytes = total_bytes.max(retained_end);
    let start_offset = offset.clamp(first_offset, retained_end);
    let start = start_offset.saturating_sub(first_offset) as usize;
    let mut start = start;
    // Snap forward to a UTF-8 char boundary so slicing never splits a codepoint.
    while start < bytes.len() && !combined.is_char_boundary(start) {
        start += 1;
    }
    let mut end = start.saturating_add(max_bytes).min(bytes.len());
    while end > start && !combined.is_char_boundary(end) {
        end -= 1;
    }
    // Progress guard: if snapping back a multi-byte codepoint collapses the
    // slice to empty yet more bytes remain, advance `end` past that one
    // codepoint so every read makes progress.  In practice the 256-byte floor
    // in `validated_max_bytes` makes this unreachable today, but the guard
    // keeps the invariant explicit and safe against future budget changes.
    if end == start && start < bytes.len() {
        end = start + 1;
        while end < bytes.len() && !combined.is_char_boundary(end) {
            end += 1;
        }
    }
    let output = combined[start..end].to_string();
    let next_offset = first_offset.saturating_add(end as u64);
    CombinedOutputSlice {
        output,
        next_offset,
        first_available_offset: first_offset,
        total_bytes,
        has_more: next_offset < total_bytes,
    }
}

fn list_status_matches(filter: ListStatusFilter, status: &str) -> bool {
    match filter {
        ListStatusFilter::Running => status == "running",
        ListStatusFilter::Terminal => bridge_state_is_terminal(status),
        ListStatusFilter::All => !status.trim().is_empty(),
    }
}

fn bridge_state_is_terminal(status: &str) -> bool {
    matches!(
        status,
        "completed"
            | "complete"
            | "failed"
            | "error"
            | "timedOut"
            | "cancelled"
            | "dead"
            | "interrupted"
            | "superseded"
    )
}

pub use final_output::load_caused_request_terminal;

fn render_assistant_message_text(message: &Message) -> Result<String> {
    let Message::Assistant { content, .. } = message else {
        anyhow::bail!("materialized child response is not an assistant message");
    };

    // A materialized final response handed to a waiting parent should be the
    // assistant's ANSWER TEXT — never its chain-of-thought. Render only `Text`
    // content; drop reasoning/tool-call/image items so no provider's reasoning
    // trace can leak into a downstream prompt.
    let text_parts: Vec<String> = content
        .iter()
        .filter_map(|item| match item {
            AssistantContent::Text(Text { text }) => Some(text.clone()),
            _ => None,
        })
        .collect();
    // A reasoning-only or tool-only message has no answer text. Never serialize
    // those blocks into the parent's prompt as a fallback.
    Ok(text_parts.join("\n"))
}

fn parse_rfc3339(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .filter(|value| !value.trim().is_empty())
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
}

fn first_row<T>(data: Option<&serde_json::Value>, collection: &str) -> Option<T>
where
    T: for<'de> Deserialize<'de>,
{
    data.and_then(|data| data.get(collection))
        .and_then(|value| serde_json::from_value::<Vec<T>>(value.clone()).ok())
        .and_then(|mut rows| rows.pop())
}

fn rows_skipping_malformed<T>(data: Option<&serde_json::Value>, collection: &str) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let Some(value) = data.and_then(|data| data.get(collection)) else {
        anyhow::bail!("{collection} field missing from query response");
    };
    let Some(values) = value.as_array() else {
        anyhow::bail!("parse {collection}: expected an array");
    };

    let mut parsed = Vec::with_capacity(values.len());
    for (row_index, value) in values.iter().enumerate() {
        match serde_json::from_value(value.clone()) {
            Ok(row) => parsed.push(row),
            Err(error) => {
                tracing::warn!(
                    collection,
                    row_index,
                    error = %error,
                    "skipping malformed control-plane row"
                );
            }
        }
    }
    Ok(parsed)
}

fn non_empty_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_tool_output_requires_a_close_before_finalizing() {
        let segment = gents_protocol::output::OutputSegment {
            node_did: "did:test:owner".into(),
            requester_did: None,
            session_id: "session".into(),
            request_doc_id: "request-doc".into(),
            source: OutputSource::ToolCall {
                tool_call_doc_id: "tool-doc".into(),
            },
            writer: OutputWriter::ToolExecution {
                tool_call_doc_id: "tool-doc".into(),
            },
            ordinal: Some(0),
            runs: vec![gents_protocol::output::SegmentRun {
                stream: 0,
                bytes: 6,
                declaration: Some(gents_protocol::output::StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: gents_protocol::output::StreamPayload::ToolOutput,
                }),
            }],
            payload: "prefix".into(),
            close: None,
            created_at: "2026-09-22T00:00:00Z".into(),
        };
        let row = crate::session::canonical_rows::OutputSegmentRow {
            doc_id: "segment-open".into(),
            segment,
        };
        assert_eq!(
            canonical_tool_output_from_rows(vec![row], "tool-doc", "request-doc").unwrap(),
            CanonicalToolOutputObservation::Open("prefix".into()),
            "even when a lifecycle replica is terminal, an open source remains loading"
        );
    }

    #[test]
    fn canonical_tool_output_preserves_a_valid_empty_close() {
        let segment = gents_protocol::output::OutputSegment {
            node_did: "did:test:owner".into(),
            requester_did: None,
            session_id: "session".into(),
            request_doc_id: "request-doc".into(),
            source: OutputSource::ToolCall {
                tool_call_doc_id: "tool-doc".into(),
            },
            writer: OutputWriter::ToolExecution {
                tool_call_doc_id: "tool-doc".into(),
            },
            ordinal: Some(0),
            runs: vec![gents_protocol::output::SegmentRun {
                stream: 0,
                bytes: 0,
                declaration: Some(gents_protocol::output::StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: gents_protocol::output::StreamPayload::ToolOutput,
                }),
            }],
            payload: String::new(),
            close: Some(gents_protocol::output::SourceClose::Closed {
                outcome: gents_protocol::output::OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![0],
            }),
            created_at: "2026-09-22T00:00:00Z".into(),
        };
        let row = crate::session::canonical_rows::OutputSegmentRow {
            doc_id: "segment-closed".into(),
            segment,
        };
        assert_eq!(
            canonical_tool_output_from_rows(vec![row], "tool-doc", "request-doc").unwrap(),
            CanonicalToolOutputObservation::Closed(String::new())
        );
    }
    use crate::llm::message::{
        AssistantContent, Reasoning, Text, ToolCall, ToolFunction, UserContent,
    };

    #[test]
    fn process_control_requester_absence_is_exact_not_empty_string() {
        let owner = ProcessControlScope {
            request_id: "request-1".to_string(),
            session_id: "session-1".to_string(),
            node_did: "did:agent".to_string(),
            requester_did: None,
        };
        let absent_next_turn = ProcessControlScope {
            request_id: "request-2".to_string(),
            ..owner.clone()
        };
        assert!(absent_next_turn.authorizes(
            &owner.session_id,
            &owner.node_did,
            owner.requester_did.as_deref(),
        ));

        let empty_next_turn = ProcessControlScope {
            requester_did: Some(String::new()),
            ..absent_next_turn
        };
        assert!(!empty_next_turn.authorizes(
            &owner.session_id,
            &owner.node_did,
            owner.requester_did.as_deref(),
        ));
    }

    #[test]
    fn render_assistant_message_text_prefers_text_over_reasoning_and_tools() {
        let message = Message::Assistant {
            id: None,
            content: vec![
                AssistantContent::Reasoning(Reasoning::new("chain-of-thought trace")),
                AssistantContent::ToolCall(ToolCall::new(
                    "call-1".to_string(),
                    ToolFunction::new("bash".to_string(), serde_json::json!({"command": "ls"})),
                )),
                AssistantContent::Text(Text {
                    text: "final answer".to_string(),
                }),
            ],
        };
        assert_eq!(
            render_assistant_message_text(&message).unwrap(),
            "final answer"
        );
    }

    #[test]
    fn child_answer_without_text_does_not_serialize_reasoning() {
        let message = Message::Assistant {
            id: None,
            content: vec![AssistantContent::Reasoning(Reasoning::new(
                "private reasoning",
            ))],
        };
        assert_eq!(render_assistant_message_text(&message).unwrap(), "");
        assert!(render_assistant_message_text(&Message::User {
            content: vec![UserContent::Text(Text {
                text: "not an answer".into()
            })],
        })
        .is_err());
    }

    #[test]
    fn combined_slice_reports_zero_first_available_offset() {
        // Terminal/persisted output is never evicted, so the earliest readable
        // byte is always 0.
        let slice = read_combined_output_slice("hello world", 0, 1024);
        assert_eq!(slice.first_available_offset, 0);
        assert_eq!(slice.output, "hello world");
        assert_eq!(slice.total_bytes, 11);
        assert!(!slice.has_more);
    }

    #[test]
    fn retained_slice_surfaces_dropped_prefix() {
        // Simulate a live ring buffer that produced 1000 bytes but retains only
        // the last 4 (tail): first_offset = 996, total_bytes_seen = 1000.
        let slice = read_retained_output_slice("tail", 996, 1000, 0, 1024);
        // A read from offset 0 is clamped forward to the earliest retained
        // byte; first_available_offset (996) exceeding the requested offset (0)
        // is how a caller detects bytes [0, 996) were produced then evicted.
        assert_eq!(slice.first_available_offset, 996);
        assert_eq!(slice.output, "tail");
        assert_eq!(slice.next_offset, 1000);
        assert_eq!(slice.total_bytes, 1000);
        assert!(!slice.has_more);
    }

    /// Drives the Lean `tool_output_paging_cases` (#937) through the real
    /// `read_retained_output_slice`. The rows are computed from the Lean
    /// `ToolOutput.readSlice` model, so paging drift in either
    /// direction (model or implementation) fails here. ASCII payloads keep
    /// byte and UTF-8 character boundaries identical, so the Rust boundary
    /// snapping is inert for these rows.
    #[test]
    fn generated_tool_output_paging_cases_match_slice_function() {
        let cases = crate::lean_vocab_test::lean_tool_output_paging_cases();
        assert!(
            !cases.is_empty(),
            "Lean emitted no tool-output paging cases"
        );
        // The canonical model retains the full stream, so its former
        // evicted-prefix case is no longer part of this contract family.
        // Exercise every emitted case without maintaining a second case count.

        for case in cases {
            let retained = "x".repeat(case.retained_len as usize);
            let slice = read_retained_output_slice(
                &retained,
                case.first_offset,
                case.total_bytes,
                case.offset,
                case.max_bytes as usize,
            );
            assert_eq!(
                slice.output.len() as u64,
                case.slice_len,
                "paging case {} returned the wrong slice length",
                case.name
            );
            assert_eq!(
                slice.next_offset, case.next_offset,
                "paging case {} continuation cursor drifted",
                case.name
            );
            assert_eq!(
                slice.first_available_offset, case.first_available_offset,
                "paging case {} eviction floor drifted",
                case.name
            );
            assert_eq!(
                slice.total_bytes, case.total_bytes_out,
                "paging case {} total drifted",
                case.name
            );
            assert_eq!(
                slice.has_more, case.has_more,
                "paging case {} has_more drifted",
                case.name
            );
            assert_eq!(
                slice.next_offset,
                case.start + case.slice_len,
                "paging case {} pages must be contiguous from the clamped start",
                case.name
            );
        }
    }
}
