use std::sync::Arc;

use anyhow::{anyhow, Result};
use chrono::Utc;
use gents_desktop_bridge::snapshot::operations_snapshot::{
    project_backgrounded_tools, stuck_diagnostics_from_tool_calls, ToolCallRow,
};
use gents_desktop_bridge::types::{
    DesktopOperationsSnapshot, DesktopOperationsSnapshotRequest, NativeExecutorStatusView,
    RuntimeLivenessView,
};
use gents_desktop_core::client::ClientCore;

pub(super) async fn operations_snapshot_response(
    core: Arc<ClientCore>,
    request: DesktopOperationsSnapshotRequest,
) -> Result<DesktopOperationsSnapshot> {
    let native_executors = gents::tools::exec::status::active_native_executors()
        .into_iter()
        .map(|executor| NativeExecutorStatusView {
            id: executor.id as i64,
            pid: executor.pid as u32,
            argv0: executor.argv0,
            tool_name: executor.tool_name,
            started_at: executor.started_at,
            age_ms: executor.age_ms,
        })
        .collect();
    let tool_call_rows = fetch_background_tool_calls(&core)
        .await
        .map_err(|error| anyhow!("failed to query AgentToolCall: {error}"))?;
    let liveness = RuntimeLivenessView {
        expired_processing_count: 0,
        requests: Vec::new(),
        active_tool_calls: Vec::new(),
        active_native_executors_available: true,
        active_native_executors: native_executors,
    };
    let backgrounded_tools = project_backgrounded_tools(&tool_call_rows, &liveness);
    let stuck_diagnostics = stuck_diagnostics_from_tool_calls(&tool_call_rows);

    Ok(DesktopOperationsSnapshot {
        fetched_at: Utc::now().to_rfc3339(),
        node_did: request.node_did,
        liveness: Some(liveness),
        liveness_unavailable_reason: None,
        backgrounded_tools,
        stuck_diagnostics,
    })
}

async fn fetch_background_tool_calls(core: &Arc<ClientCore>) -> Result<Vec<ToolCallRow>, String> {
    let query = r#"
        query {
            AgentToolCall(
                filter: { await_mode: { _eq: "background" } }
            ) {
                _docID
                request_id
                tool_call_id
                tool_name
                lifecycle_state
                status
                started_at
                completed_at
                deadline_at
                await_mode
                stuck_since
                job_pin
            }
        }
    "#;

    let response =
        gents::graphql::graphql_with_transaction_retry(core.node(), query, "AgentToolCall query")
            .await
            .map_err(|error| format!("{error:#}"))?;

    let data = response
        .data
        .ok_or_else(|| "AgentToolCall query returned no data".to_string())?;
    let rows = data
        .get("AgentToolCall")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(rows
        .into_iter()
        .map(|row| ToolCallRow {
            tool_call_doc_id: row
                .get("_docID")
                .and_then(|value| value.as_str())
                .map(str::to_owned),
            request_id: row
                .get("request_id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            tool_call_id: row
                .get("tool_call_id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            tool_name: row
                .get("tool_name")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            lifecycle_state: row
                .get("lifecycle_state")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            status: row
                .get("status")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            started_at: row
                .get("started_at")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            completed_at: row
                .get("completed_at")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            deadline_at: row
                .get("deadline_at")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            await_mode: row
                .get("await_mode")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            stuck_since: row
                .get("stuck_since")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            job: row.get("job_pin").is_some_and(|pin| !pin.is_null()),
        })
        .collect())
}
