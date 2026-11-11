mod final_output;
pub mod status_projection;

use crate::inference::llm::message::{AssistantContent, Message, Text};
use crate::session::tool_output_rows::observe_canonical_tool_output_with_access;
use anyhow::{Context, Result};
use chrono::Utc;
use serde::Deserialize;

use crate::tools::native::background_args::{
    ListBackgroundToolsArgs, ListBackgroundToolsEntry, ListBackgroundToolsResponse,
    ListStatusFilter, ReadToolOutputArgs, ReadToolOutputResponse,
};
use gents_loop::live_output::LiveToolOutputRegistry;
use gents_protocol::wait_status::WaitLifecycle;
pub use status_projection::OutputEvidence;
use status_projection::WaitStatusProjection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessControlScope {
    pub request_id: String,
    pub session_id: String,
    pub node_did: String,
    pub requester_did: Option<String>,
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
    started_at: Option<String>,
    completed_at: Option<String>,
}

pub(crate) enum ReadToolOutputOutcome {
    Found(ReadToolOutputResponse),
    NotAuthorized,
    NotBackgrounded,
}

pub(crate) async fn handle_list_background_tools_with_access(
    access: &crate::ConfigAccess,
    caller: &ProcessControlScope,
    local_deployment_id: &str,
    // Unused here by design: list rows project status from the persisted
    // lifecycle and read output evidence from the canonical output owner,
    // not the live-output registry.
    _live_outputs: &LiveToolOutputRegistry,
    args: ListBackgroundToolsArgs,
) -> Result<ListBackgroundToolsResponse> {
    let limit = args.validated_limit() as usize;
    use gents_protocol::document_query::{
        fields::agent_tool_call as fields, Direction, Filter, Query, Selection,
    };
    let query = Query::select(
        Selection::doc_id()
            .field(fields::TOOL_CALL_ID)
            .field(fields::TOOL_NAME)
            .field(fields::REQUEST_DOC_ID)
            .field(fields::SESSION_ID)
            .field(fields::NODE_DID)
            .field(fields::REQUESTER_DID)
            .field(fields::AWAIT_MODE)
            .field(fields::LIFECYCLE_STATE)
            .field(fields::STARTED_AT)
            .field(fields::COMPLETED_AT),
    )
    .filter(Filter::eq(fields::SESSION_ID, &caller.session_id))
    .filter(Filter::eq(fields::AWAIT_MODE, "background"))
    .order_by(fields::STARTED_AT, Direction::Asc)
    .render();
    let response = access.read(&query).await?;
    let rows: Vec<ListBackgroundToolRow> =
        rows_skipping_malformed(response.get("data"), "AgentToolCall")?;

    let mut entries = Vec::new();
    for row in rows {
        if !caller.authorizes(&row.session_id, &row.node_did, row.requester_did.as_deref()) {
            continue;
        }
        if row.await_mode.as_deref() != Some("background") {
            continue;
        }
        let projection = WaitStatusProjection::from_parts(
            row.lifecycle_state.as_deref(),
            row.started_at.as_deref(),
            row.completed_at.as_deref(),
            Utc::now(),
        );
        if !list_projection_matches(args.status, &projection) {
            continue;
        }
        let Some(tool_call_id) = non_empty_string(Some(&row.tool_call_id)) else {
            continue;
        };
        let output = observe_canonical_tool_output_with_access(
            access,
            &row.doc_id,
            &row.request_doc_id,
            &row.session_id,
            &row.node_did,
            row.requester_did.as_deref(),
        )
        .await?
        .into_text();
        let evidence = OutputEvidence::from_text(&output);

        entries.push(ListBackgroundToolsEntry {
            tool_call_id,
            tool_name: row.tool_name,
            deployment_id: local_deployment_id.to_string(),
            await_mode: "background".to_string(),
            status: projection.status_token().to_string(),
            lifecycle_state_raw: projection.lifecycle_state_raw.clone(),
            started_at: projection.started_at,
            completed_at: projection.completed_at,
            elapsed_seconds: projection.running_elapsed_seconds,
            terminal_elapsed_seconds: projection.terminal_elapsed_seconds,
            available_bytes: evidence.total_bytes,
            preview: evidence.preview,
            preview_truncated: evidence.preview_truncated,
            stdout_bytes: evidence.total_bytes,
            stderr_bytes: 0,
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

pub(crate) async fn handle_read_tool_output_with_access(
    access: &crate::ConfigAccess,
    caller: &ProcessControlScope,
    live_outputs: &LiveToolOutputRegistry,
    args: ReadToolOutputArgs,
) -> Result<ReadToolOutputOutcome> {
    read_tool_output_slice_with_access(
        access,
        caller,
        live_outputs,
        &args.tool_call_id,
        args.offset,
        args.validated_max_bytes(),
    )
    .await
}

pub(crate) async fn read_tool_output_slice_with_access(
    access: &crate::ConfigAccess,
    caller: &ProcessControlScope,
    live_outputs: &LiveToolOutputRegistry,
    tool_call_id: &str,
    offset: u64,
    max_bytes: usize,
) -> Result<ReadToolOutputOutcome> {
    read_tool_output_slice_inner(
        access,
        caller,
        live_outputs,
        tool_call_id,
        offset,
        max_bytes,
    )
    .await
}

async fn read_tool_output_slice_inner(
    access: &crate::ConfigAccess,
    caller: &ProcessControlScope,
    _live_outputs: &LiveToolOutputRegistry,
    tool_call_id: &str,
    offset: u64,
    max_bytes: usize,
) -> Result<ReadToolOutputOutcome> {
    let tool_call_id = tool_call_id.trim();
    if tool_call_id.is_empty() {
        return Ok(ReadToolOutputOutcome::NotAuthorized);
    }

    use gents_protocol::document_query::{
        fields::agent_tool_call as fields, Filter, Query, Selection,
    };
    let query = Query::select(
        Selection::doc_id()
            .field(fields::TOOL_CALL_ID)
            .field(fields::TOOL_NAME)
            .field(fields::REQUEST_DOC_ID)
            .field(fields::SESSION_ID)
            .field(fields::NODE_DID)
            .field(fields::REQUESTER_DID)
            .field(fields::AWAIT_MODE)
            .field(fields::LIFECYCLE_STATE)
            .field(fields::STARTED_AT)
            .field(fields::COMPLETED_AT),
    )
    .filter(Filter::eq(fields::SESSION_ID, &caller.session_id))
    .filter(Filter::eq(fields::TOOL_CALL_ID, tool_call_id))
    .limit(1)
    .render();
    let data = access.read(&query).await?.get("data").cloned();
    let Some(row) = first_row::<ReadToolOutputRow>(data.as_ref(), "AgentToolCall") else {
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

    let projection = WaitStatusProjection::from_parts(
        row.lifecycle_state.as_deref(),
        row.started_at.as_deref(),
        row.completed_at.as_deref(),
        Utc::now(),
    );
    let status = projection.status_token().to_string();
    let exited = projection.lifecycle.is_terminal();
    let request_doc_id = row
        .request_doc_id
        .as_deref()
        .context("tool output row lacks request binding")?;
    let session_id = row
        .session_id
        .as_deref()
        .context("tool output row lacks session binding")?;
    let node_did = row
        .node_did
        .as_deref()
        .context("tool output row lacks agent binding")?;
    let combined = observe_canonical_tool_output_with_access(
        access,
        &row.doc_id,
        request_doc_id,
        session_id,
        node_did,
        row.requester_did.as_deref(),
    )
    .await?
    .into_text();
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

struct CombinedOutputSlice {
    output: String,
    next_offset: u64,
    /// Byte offset of the earliest still-readable byte; > 0 only when a live
    /// ring buffer evicted a prefix.
    first_available_offset: u64,
    total_bytes: u64,
    has_more: bool,
}

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
    while start < bytes.len() && !combined.is_char_boundary(start) {
        start += 1;
    }
    let mut end = start.saturating_add(max_bytes).min(bytes.len());
    while end > start && !combined.is_char_boundary(end) {
        end -= 1;
    }
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

/// Filter against the shared projection: `running` matches only a parsed
/// Running lifecycle, `terminal` only a terminal one; unknown states surface
/// under `All` (projected as `unknown`, never relabeled `running`).
fn list_projection_matches(filter: ListStatusFilter, projection: &WaitStatusProjection) -> bool {
    match filter {
        ListStatusFilter::Running => projection.lifecycle == WaitLifecycle::Running,
        ListStatusFilter::Terminal => projection.lifecycle.is_terminal(),
        ListStatusFilter::All => true,
    }
}

#[cfg(test)]
pub(crate) use final_output::select_folded_outcome;
pub(crate) use final_output::{
    caused_request_terminals_in_txn, load_caused_request_terminals, load_session_message_terminal,
};

fn render_assistant_message_text(message: &Message) -> Result<String> {
    let Message::Assistant { content, .. } = message else {
        anyhow::bail!("materialized child response is not an assistant message");
    };

    // Render only `Text` content so no provider reasoning trace can leak into
    // a downstream prompt; never serialize reasoning/tool blocks as fallback.
    let text_parts: Vec<String> = content
        .iter()
        .filter_map(|item| match item {
            AssistantContent::Text(Text { text }) => Some(text.clone()),
            _ => None,
        })
        .collect();
    Ok(text_parts.join("\n"))
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

pub fn supports_foreground_backgrounding(name: &str) -> bool {
    crate::tools::native::is_bash_tool_name(name)
}

/// Requests a handoff of one exact native invocation. The runtime publishes
/// its background receipt only after assuming responsibility for completion.
pub async fn request_foreground_backgrounding(
    access: &crate::store::config_client::ConfigAccess,
    node_did: &str,
    session_id: &str,
    request_id: &str,
    tool_call_id: &str,
) -> Result<bool> {
    use gents_protocol::document_query::{
        fields::agent_tool_call as fields, Filter, Input, Query, Selection, Update,
    };
    let lookup = Query::select(
        Selection::doc_id()
            .field(fields::REQUEST_DOC_ID)
            .field(fields::TOOL_NAME)
            .field(fields::LIFECYCLE_STATE)
            .field(fields::AWAIT_MODE)
            .field(fields::PLUGIN_PARENT_TOOL_CALL_DOC_ID)
            .field(fields::BACKGROUND_REQUESTED_AT)
            .field(fields::STARTED_AT)
            .field(fields::DEADLINE_AT),
    )
    .filter(Filter::eq(fields::NODE_DID, node_did))
    .filter(Filter::eq(fields::SESSION_ID, session_id))
    .filter(Filter::eq(fields::REQUEST_ID, request_id))
    .filter(Filter::eq(fields::TOOL_CALL_ID, tool_call_id))
    .limit(2)
    .render();
    let response = access.read(&lookup).await?;
    let rows = response
        .pointer("/data/AgentToolCall")
        .and_then(serde_json::Value::as_array)
        .context("background request lookup omitted tool rows")?;
    anyhow::ensure!(
        rows.len() == 1,
        "background request requires one exact tool invocation"
    );
    let row = &rows[0];
    anyhow::ensure!(
        row["tool_name"]
            .as_str()
            .is_some_and(crate::tools::native::is_bash_tool_name)
            && row["plugin_parent_tool_call_doc_id"].is_null(),
        "only a direct native Bash command can be backgrounded"
    );
    if row["lifecycle_state"].as_str() != Some("running") {
        return Ok(false);
    }
    if row["await_mode"].as_str() == Some("background") || !row["background_requested_at"].is_null()
    {
        return Ok(true);
    }
    anyhow::ensure!(
        crate::tools::call::hook::persistence::count_live_backgrounded_rows_in(
            access,
            row["request_doc_id"]
                .as_str()
                .context("background request missing request document identity")?,
        )
        .await?
            < gents_protocol::job::MAX_BACKGROUNDED_TOOLS_PER_PARENT,
        "background process limit reached; wait for a background process to finish"
    );
    let doc = row["_docID"]
        .as_str()
        .context("tool missing physical identity")?;
    let started = row["started_at"]
        .as_str()
        .context("tool missing start time")?;
    let deadline = row["deadline_at"]
        .as_str()
        .context("tool missing deadline")?;
    let now = Utc::now().to_rfc3339();
    let update = Update::doc(
        doc,
        Input::new()
            .set(fields::BACKGROUND_REQUESTED_AT, &now)
            .set(fields::STARTED_AT, started)
            .set(fields::DEADLINE_AT, deadline),
    )
    .filter(Filter::eq(fields::DOC_ID, doc))
    .filter(Filter::eq(fields::NODE_DID, node_did))
    .filter(Filter::eq(fields::SESSION_ID, session_id))
    .filter(Filter::eq(fields::REQUEST_ID, request_id))
    .filter(Filter::eq(fields::TOOL_CALL_ID, tool_call_id))
    .filter(Filter::eq(fields::LIFECYCLE_STATE, "running"))
    .filter(Filter::eq(fields::AWAIT_MODE, "foreground"))
    .render();
    let result = access
        .write_rendered("tool.request_background", &update)
        .await?;
    Ok(result
        .pointer("/data/update_AgentToolCall")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|rows| !rows.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_tool_output_row_decodes_missing_and_malformed_timestamps() {
        // Missing started_at/completed_at must decode as None (not drop the
        // row); the projection then renders elapsed as unknown truthfully.
        let row: ReadToolOutputRow = serde_json::from_value(serde_json::json!({
            "_docID": "tool-doc-1",
            "tool_call_id": "call-1",
            "tool_name": "spawn_process",
            "lifecycle_state": "running"
        }))
        .expect("missing timestamp fields decode to None");
        assert_eq!(row.started_at, None);
        assert_eq!(row.completed_at, None);
        assert_eq!(row.lifecycle_state.as_deref(), Some("running"));

        // Malformed (non-RFC3339) timestamps must decode verbatim as
        // strings, preserving the lifecycle state so the projection can
        // truthfully render elapsed as unknown instead of dropping the row.
        let malformed: ReadToolOutputRow = serde_json::from_value(serde_json::json!({
            "_docID": "tool-doc-2",
            "tool_call_id": "call-2",
            "tool_name": "spawn_process",
            "lifecycle_state": "failed",
            "started_at": "not-a-timestamp",
            "await_mode": "background"
        }))
        .expect("malformed timestamp string must not drop the row");
        assert_eq!(malformed.started_at.as_deref(), Some("not-a-timestamp"));
        assert_eq!(malformed.completed_at, None);
        assert_eq!(malformed.lifecycle_state.as_deref(), Some("failed"));
    }

    /// Drives the Lean `tool_output_paging_cases` (#937) through the real
    /// `read_retained_output_slice`; the rows are computed from the Lean
    /// `ToolOutput.readSlice` model, so paging drift in either direction
    /// fails here. ASCII payloads keep byte and UTF-8 boundaries identical.

    #[test]
    fn generated_tool_output_paging_cases_match_slice_function() {
        let cases = gents_lean_vocab::lean_tool_output_paging_cases();
        assert!(
            !cases.is_empty(),
            "Lean emitted no tool-output paging cases"
        );

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
