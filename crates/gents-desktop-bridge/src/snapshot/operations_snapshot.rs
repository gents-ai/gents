use super::super::types::{
    ActiveToolCallView, BackgroundedToolView, NativeExecutorStatusView, RuntimeLivenessView,
    StuckWorkDiagnosticView, WaitStatusProjectionView,
};

#[derive(Debug, Clone)]
pub struct ToolCallRow {
    pub tool_call_doc_id: Option<String>,
    pub request_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub lifecycle_state: Option<String>,
    pub status: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub deadline_at: Option<String>,
    pub await_mode: Option<String>,
    pub stuck_since: Option<String>,
    pub job: bool,
}

const CORRELATION_WINDOW_MS: i64 = 1_000;

pub fn project_backgrounded_tools(
    rows: &[ToolCallRow],
    liveness: &RuntimeLivenessView,
) -> Vec<BackgroundedToolView> {
    let now = chrono::Utc::now();
    let mut labels = rapidhash::RapidHashMap::default();
    for row in rows {
        *labels.entry(row.tool_call_id.as_str()).or_insert(0_usize) += 1;
    }
    rows.iter()
        .filter(|r| r.await_mode.as_deref() == Some("background"))
        // Preserve existing panel semantics: terminal calls (per the shared
        // projection) and legacy `superseded` rows are not active workers.
        .filter(|r| {
            let projection = gents_protocol::wait_status::WaitStatusProjection::from_parts(
                r.lifecycle_state.as_deref(),
                r.started_at.as_deref(),
                r.completed_at.as_deref(),
                now,
            );
            !projection.lifecycle.is_terminal()
                && r.lifecycle_state.as_deref() != Some("superseded")
        })
        .map(|r| {
            let projection = gents_protocol::wait_status::WaitStatusProjection::from_parts(
                r.lifecycle_state.as_deref(),
                r.started_at.as_deref(),
                r.completed_at.as_deref(),
                now,
            );
            let unique_label = labels.get(r.tool_call_id.as_str()).copied() == Some(1);
            let age_ms = unique_label
                .then(|| {
                    age_from_live_snapshot(r.tool_call_id.as_str(), &liveness.active_tool_calls)
                })
                .flatten();
            let deadline_expired = unique_label
                && liveness
                    .active_tool_calls
                    .iter()
                    .find(|tc| tc.tool_call_id == r.tool_call_id)
                    .map(|tc| tc.deadline_expired)
                    .unwrap_or(false);
            let native_executor = unique_label
                .then(|| correlate_native_executor(r, &liveness.active_native_executors))
                .flatten();
            let wait_status = wait_status_view(&projection);
            BackgroundedToolView {
                tool_call_doc_id: r.tool_call_doc_id.clone(),
                request_id: r.request_id.clone(),
                tool_call_id: r.tool_call_id.clone(),
                tool_name: r.tool_name.clone(),
                starts_session: gents::tools::native::is_session_message_tool(&r.tool_name),
                job: r.job,
                lifecycle_state: r.lifecycle_state.clone(),
                status: r.status.clone(),
                started_at: r.started_at.clone(),
                age_ms,
                deadline_at: r.deadline_at.clone(),
                deadline_expired,
                await_mode: r.await_mode.clone(),
                stuck_since: r.stuck_since.clone(),
                native_executor,
                wait_status,
            }
        })
        .collect()
}

fn wait_status_view(
    projection: &gents_protocol::wait_status::WaitStatusProjection,
) -> Option<WaitStatusProjectionView> {
    Some(WaitStatusProjectionView {
        lifecycle: projection.status_token().to_string(),
        lifecycle_state_raw: projection.lifecycle_state_raw.clone(),
        started_at: projection
            .started_at
            .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)),
        completed_at: projection
            .completed_at
            .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)),
        running_elapsed_seconds: projection.running_elapsed_seconds,
        terminal_elapsed_seconds: projection.terminal_elapsed_seconds,
    })
}

pub fn stuck_diagnostics_from_tool_calls(rows: &[ToolCallRow]) -> Vec<StuckWorkDiagnosticView> {
    let now = chrono::Utc::now();
    rows.iter()
        .filter(|r| r.await_mode.as_deref() == Some("background"))
        .filter(|r| {
            let projection = gents_protocol::wait_status::WaitStatusProjection::from_parts(
                r.lifecycle_state.as_deref(),
                r.started_at.as_deref(),
                r.completed_at.as_deref(),
                now,
            );
            !projection.lifecycle.is_terminal()
                && r.lifecycle_state.as_deref() != Some("superseded")
        })
        .filter(|r| r.stuck_since.is_some())
        .map(|r| StuckWorkDiagnosticView {
            request_id: r.request_id.clone(),
            session_id: None,
            severity: "warning".to_string(),
            reason: "stuckTool".to_string(),
            deadline_age_ms: None,
            last_progress_age_ms: None,
            tool_call_id: Some(r.tool_call_id.clone()),
            tool_name: Some(r.tool_name.clone()),
            stuck_since: r.stuck_since.clone(),
        })
        .collect()
}

fn age_from_live_snapshot(tool_call_id: &str, live: &[ActiveToolCallView]) -> Option<i64> {
    live.iter()
        .find(|tc| tc.tool_call_id == tool_call_id)
        .map(|tc| tc.running_age_ms)
}

fn correlate_native_executor(
    row: &ToolCallRow,
    execs: &[NativeExecutorStatusView],
) -> Option<NativeExecutorStatusView> {
    let started_at = row.started_at.as_deref()?;
    let started_ms = parse_iso_ms(started_at)?;
    execs
        .iter()
        .find(|ne| {
            ne.tool_name.as_deref() == Some(row.tool_name.as_str())
                && parse_iso_ms(&ne.started_at)
                    .map(|m| (m - started_ms).abs() <= CORRELATION_WINDOW_MS)
                    .unwrap_or(false)
        })
        .cloned()
}

fn parse_iso_ms(iso: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

#[cfg(test)]
#[path = "operations_snapshot/tests.rs"]
mod tests;
