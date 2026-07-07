use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use anyhow::{Context, Result};
use codex_app_server_protocol as codex;
use codex_protocol::models::MessagePhase;
use defra_agent::UpdateSubscriptionSource;
use serde_json::{json, Value};
use tokio::sync::watch;

use super::super::background::spawn_background_tool_watcher;
use super::super::command_projection::{
    tool_projection_status, update_running_background_tools, ToolProjectionStatus,
};
use super::super::progress::{
    content_delta, decode_defra_tool_call_progress, defra_turn_progress_query,
    response_field_is_blank, terminal_error_message, terminal_turn_status,
};
use super::super::protocol::{send_committed_user_message, send_notification};
use super::super::store::{hydrate_materialized_response_content, query_node_json};
use super::super::thread_projection::{
    requests_token_usage, session_token_usage, thread_token_usage,
};
use super::super::turn_projection::TurnProjection;
use super::super::{ConnectionState, ShimState};
use super::active::next_steering_request_after;
use crate::{is_terminal_lifecycle_state, request_diagnostic_hint, SubmittedRequest};

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProgressMarker {
    request_lifecycle_state: Option<String>,
    request_interrupt_requested_at: Option<String>,
    request_valid_until: Option<String>,
    response_doc_id: Option<String>,
    response_status: Option<String>,
    response_token_count: Option<String>,
    response_progress_seq: Option<String>,
    response_content_len: Option<usize>,
    response_reasoning_fingerprint: Option<(usize, u64)>,
    response_error_len: Option<usize>,
    response_materialized_message_sequence: Option<String>,
    response_materialized_at: Option<String>,
    response_completed_at: Option<String>,
    response_interrupted_at: Option<String>,
    tools: Vec<ToolProgressMarker>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ToolProgressMarker {
    tool_call_key: Option<String>,
    tool_name: Option<String>,
    status: Option<String>,
    lifecycle_state: Option<String>,
    await_mode: Option<String>,
    child_request_id: Option<String>,
    args_len: Option<usize>,
    result_len: Option<usize>,
    started_at: Option<String>,
    completed_at: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct ContentCursor {
    rendered_len: usize,
    head: String,
    tail: String,
}

pub(super) async fn stream_defra_turn(
    connection: &ConnectionState,
    state: &ShimState,
    submitted: &SubmittedRequest,
    projection: &mut TurnProjection<'_>,
    mut cancel_rx: watch::Receiver<bool>,
) -> Result<()> {
    let outbound = &connection.outbound;
    let mut current = submitted.clone();
    let mut turn_request_ids = vec![current.request_id.clone()];
    let mut known_tool_calls: BTreeMap<String, ToolProjectionStatus> = BTreeMap::new();
    let mut known_tool_markers: BTreeMap<String, ToolProgressMarker> = BTreeMap::new();
    let mut running_background_tools: BTreeMap<String, codex::CommandExecutionStatus> =
        BTreeMap::new();
    let mut updates = state.node.subscribe_updates();
    let mut latest_content_cursor = ContentCursor::default();
    let mut latest_error_message: Option<String> = None;
    let mut latest_progress_marker: Option<ProgressMarker> = None;
    let mut last_progress_at = tokio::time::Instant::now();

    loop {
        if *cancel_rx.borrow() {
            return finish_interrupted_turn(
                connection,
                state,
                &current,
                projection,
                running_background_tools,
            )
            .await;
        }

        let progress_query = defra_turn_progress_query(&current.request_id, &current.session_id);
        let response = tokio::select! {
            response = query_node_json(state.node.as_ref(), &progress_query) => response?,
            changed = cancel_rx.changed() => {
                if changed.is_ok() && *cancel_rx.borrow() {
                    return finish_interrupted_turn(
                        connection,
                        state,
                        &current,
                        projection,
                        running_background_tools,
                    )
                    .await;
                }
                continue;
            }
        };
        let request_row = response
            .pointer("/data/AgentRequest")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first());
        let response_row = response
            .pointer("/data/AgentResponse")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first());
        let tool_rows = response
            .pointer("/data/AgentToolCall")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);

        let marker = progress_marker(request_row, response_row, tool_rows);
        let marker_changed = latest_progress_marker.as_ref() != Some(&marker);
        if marker_changed {
            latest_progress_marker = Some(marker);
            last_progress_at = tokio::time::Instant::now();
        }

        for row in tool_rows {
            let tool_marker = tool_progress_marker(row);
            let Some(tool_key) = tool_marker.tool_call_key.as_deref() else {
                continue;
            };
            if known_tool_markers.get(tool_key) == Some(&tool_marker) {
                continue;
            }
            let Some(tool) = decode_defra_tool_call_progress(row) else {
                continue;
            };
            let projection_status = tool_projection_status(&tool);
            let previous_status = known_tool_calls.get(&tool.tool_call_key).cloned();
            if previous_status.as_ref() == Some(&projection_status) {
                known_tool_markers.insert(tool.tool_call_key.clone(), tool_marker);
                continue;
            }

            projection
                .send_tool_projection_update(
                    outbound,
                    &tool,
                    previous_status.as_ref(),
                    &projection_status,
                )
                .await?;

            update_running_background_tools(
                &mut running_background_tools,
                &tool,
                &projection_status,
            );
            known_tool_calls.insert(tool.tool_call_key.clone(), projection_status);
            known_tool_markers.insert(tool.tool_call_key.clone(), tool_marker);
        }

        let response_status = response_row
            .and_then(|row| row.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if marker_changed {
            if let Some(content) = response_row
                .and_then(|row| row.get("content"))
                .and_then(Value::as_str)
            {
                let delta = content_delta_from_cursor(&mut latest_content_cursor, content);
                projection.append_agent_delta(outbound, &delta).await?;
            }
            latest_error_message = response_row
                .and_then(|row| row.get("error_message"))
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(ToOwned::to_owned);
        }

        let lifecycle_state = request_row
            .as_ref()
            .and_then(|row| row.get("lifecycle_state"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let failure_reason = request_row
            .as_ref()
            .and_then(|row| row.get("failure_reason"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let terminal_by_request = is_terminal_lifecycle_state(lifecycle_state);
        let terminal_by_response = matches!(response_status, "complete" | "completed" | "error");

        if terminal_by_request || terminal_by_response {
            let mut terminal_response = response_row.cloned().unwrap_or_else(|| {
                json!({
                    "request_id": current.request_id.clone(),
                    "status": null,
                    "content": null,
                })
            });
            let should_wait_for_materialized_content =
                matches!(response_status, "complete" | "completed")
                    && response_field_is_blank(&terminal_response, "content")
                    && terminal_response
                        .get("materialized_message_sequence")
                        .is_some_and(|value| !value.is_null());
            let hydrated =
                hydrate_materialized_response_content(state.node.as_ref(), &mut terminal_response)
                    .await
                    .context("hydrating materialized response content for terminal Codex turn")?;
            if should_wait_for_materialized_content && !hydrated {
                if last_progress_at.elapsed() >= state.timeout {
                    anyhow::bail!(
                        "timed out waiting for materialized AgentMessage {} after {}s of inactivity\n{}",
                        current.request_id,
                        state.timeout.as_secs(),
                        request_diagnostic_hint(&current.request_id)
                    );
                }
                tokio::time::sleep(state.poll_interval).await;
                continue;
            }

            if let Some(content) = terminal_response.get("content").and_then(Value::as_str) {
                let delta = content_delta(projection.active_agent_text(), content);
                projection.append_agent_delta(outbound, &delta).await?;
            }

            let turn_status = terminal_turn_status(lifecycle_state, response_status);
            let error_message = if turn_status == codex::TurnStatus::Failed {
                terminal_error_message(
                    response_status,
                    latest_error_message.as_deref(),
                    lifecycle_state,
                    failure_reason,
                )
            } else {
                None
            };
            if let Some(error_message) = error_message.as_deref() {
                if !projection.rendered_agent_text().contains(error_message) {
                    projection
                        .append_agent_delta(outbound, &format!("\n[agent error] {error_message}\n"))
                        .await?;
                }
            }
            if turn_status == codex::TurnStatus::Completed {
                if let Some(next_request) =
                    next_steering_request_after(state, &current.session_id, &current.request_id)
                        .await
                        .context("loading next Codex steering request")?
                {
                    if next_request.is_pending() {
                        if last_progress_at.elapsed() >= state.timeout {
                            anyhow::bail!(
                                "timed out waiting for queued Codex steering request {} after {}s of inactivity\n{}",
                                next_request.request_id,
                                state.timeout.as_secs(),
                                request_diagnostic_hint(&next_request.request_id)
                            );
                        }
                        tokio::select! {
                            _ = tokio::time::sleep(state.poll_interval) => {}
                            changed = cancel_rx.changed() => {
                                if changed.is_ok() && *cancel_rx.borrow() {
                                    return finish_interrupted_turn(
                                        connection,
                                        state,
                                        &current,
                                        projection,
                                        running_background_tools,
                                    )
                                    .await;
                                }
                            }
                        }
                        continue;
                    }
                    projection
                        .finish_agent_message_with_phase(outbound, Some(MessagePhase::FinalAnswer))
                        .await?;
                    spawn_background_tool_watcher(
                        connection.clone(),
                        state.clone(),
                        current.request_id.clone(),
                        current.session_id.clone(),
                        projection.thread_id.to_string(),
                        projection.turn_id.to_string(),
                        projection.cwd.clone(),
                        std::mem::take(&mut running_background_tools),
                    );
                    let next_input =
                        steering_input_for_request(connection, state, &next_request.request_id)
                            .await?;
                    send_committed_user_message(
                        outbound,
                        state,
                        projection.thread_id,
                        projection.turn_id,
                        &next_input,
                    )
                    .await?;
                    current.request_id = next_request.request_id;
                    turn_request_ids.push(current.request_id.clone());
                    known_tool_calls.clear();
                    known_tool_markers.clear();
                    latest_content_cursor.reset();
                    latest_error_message = None;
                    latest_progress_marker = None;
                    last_progress_at = tokio::time::Instant::now();
                    continue;
                }
            }
            let last_usage = requests_token_usage(state, &turn_request_ids)
                .await
                .unwrap_or_default();
            let total_usage = session_token_usage(state, &current.session_id)
                .await
                .unwrap_or_default();
            send_notification(
                outbound,
                state,
                codex::ServerNotification::ThreadTokenUsageUpdated(
                    codex::ThreadTokenUsageUpdatedNotification {
                        thread_id: projection.thread_id.to_string(),
                        turn_id: projection.turn_id.to_string(),
                        token_usage: thread_token_usage(total_usage, last_usage),
                    },
                ),
            )
            .await?;

            projection
                .finish_turn(outbound, turn_status, error_message)
                .await
                .context("sending terminal Codex turn notification")?;
            spawn_background_tool_watcher(
                connection.clone(),
                state.clone(),
                current.request_id.clone(),
                current.session_id.clone(),
                projection.thread_id.to_string(),
                projection.turn_id.to_string(),
                projection.cwd.clone(),
                running_background_tools,
            );
            return Ok(());
        }

        if last_progress_at.elapsed() >= state.timeout {
            anyhow::bail!(
                "timed out waiting for AgentResponse {} after {}s of inactivity\n{}",
                current.request_id,
                state.timeout.as_secs(),
                request_diagnostic_hint(&current.request_id)
            );
        }

        tokio::select! {
            _ = tokio::time::sleep(state.poll_interval) => {}
            msg = updates.recv() => {
                if msg.is_none() {
                    tracing::warn!("Codex shim embedded-node update subscription closed");
                }
                let dropped = updates.check_and_reset_dropped();
                if dropped > 0 {
                    tracing::warn!(dropped, "Codex shim update subscription dropped messages");
                }
            }
            changed = cancel_rx.changed() => {
                if changed.is_ok() && *cancel_rx.borrow() {
                    return finish_interrupted_turn(
                        connection,
                        state,
                        &current,
                        projection,
                        running_background_tools,
                    )
                    .await;
                }
            }
        }
    }
}

fn progress_marker(
    request_row: Option<&Value>,
    response_row: Option<&Value>,
    tool_rows: &[Value],
) -> ProgressMarker {
    ProgressMarker {
        request_lifecycle_state: scalar_marker(request_row, "lifecycle_state"),
        request_interrupt_requested_at: scalar_marker(request_row, "interrupt_requested_at"),
        request_valid_until: scalar_marker(request_row, "valid_until"),
        response_doc_id: scalar_marker(response_row, "_docID"),
        response_status: scalar_marker(response_row, "status"),
        response_token_count: scalar_marker(response_row, "token_count"),
        response_progress_seq: scalar_marker(response_row, "progress_seq"),
        response_content_len: string_len_marker(response_row, "content"),
        response_reasoning_fingerprint: string_fingerprint_marker(response_row, "reasoning"),
        response_error_len: string_len_marker(response_row, "error_message"),
        response_materialized_message_sequence: scalar_marker(
            response_row,
            "materialized_message_sequence",
        ),
        response_materialized_at: scalar_marker(response_row, "materialized_at"),
        response_completed_at: scalar_marker(response_row, "completed_at"),
        response_interrupted_at: scalar_marker(response_row, "interrupted_at"),
        tools: tool_rows.iter().map(tool_progress_marker).collect(),
    }
}

fn content_delta_from_cursor(cursor: &mut ContentCursor, current: &str) -> String {
    if current.is_empty() {
        cursor.reset();
        return String::new();
    }
    let current_len = current.len();
    if current_len > cursor.rendered_len
        && current.is_char_boundary(cursor.rendered_len)
        && cursor.tail_matches_at_rendered_len(current)
    {
        let delta = current[cursor.rendered_len..].to_string();
        cursor.observe(current);
        return delta;
    }
    if current_len == cursor.rendered_len && cursor.tail_matches_at_end(current) {
        return String::new();
    }
    cursor.observe(current);
    current.to_string()
}

impl ContentCursor {
    const TAIL_BYTES: usize = 64;

    fn observe(&mut self, current: &str) {
        self.rendered_len = current.len();
        self.head = head_window(current, Self::TAIL_BYTES).to_string();
        self.tail = tail_window(current, Self::TAIL_BYTES).to_string();
    }

    fn reset(&mut self) {
        self.rendered_len = 0;
        self.head.clear();
        self.tail.clear();
    }

    fn tail_matches_at_rendered_len(&self, current: &str) -> bool {
        if self.rendered_len == 0 {
            return true;
        }
        if !self.head_matches_start(current) {
            return false;
        }
        let tail_len = self.tail.len();
        if tail_len == 0 || self.rendered_len < tail_len {
            return false;
        }
        let start = self.rendered_len - tail_len;
        current.get(start..self.rendered_len) == Some(self.tail.as_str())
    }

    fn tail_matches_at_end(&self, current: &str) -> bool {
        let tail_len = self.tail.len();
        if tail_len == 0 {
            return current.is_empty();
        }
        if !self.head_matches_start(current) {
            return false;
        }
        current
            .len()
            .checked_sub(tail_len)
            .and_then(|start| current.get(start..))
            == Some(self.tail.as_str())
    }

    fn head_matches_start(&self, current: &str) -> bool {
        let head_len = self.head.len();
        head_len > 0 && current.get(..head_len) == Some(self.head.as_str())
    }
}

fn head_window(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn tail_window(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut start = value.len() - max_bytes;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    &value[start..]
}

fn tool_progress_marker(row: &Value) -> ToolProgressMarker {
    ToolProgressMarker {
        tool_call_key: scalar_marker(Some(row), "tool_call_key"),
        tool_name: scalar_marker(Some(row), "tool_name"),
        status: scalar_marker(Some(row), "status"),
        lifecycle_state: scalar_marker(Some(row), "lifecycle_state"),
        await_mode: scalar_marker(Some(row), "await_mode"),
        child_request_id: scalar_marker(Some(row), "child_request_id"),
        args_len: string_len_marker(Some(row), "args"),
        result_len: string_len_marker(Some(row), "result"),
        started_at: scalar_marker(Some(row), "started_at"),
        completed_at: scalar_marker(Some(row), "completed_at"),
    }
}

fn scalar_marker(row: Option<&Value>, field: &str) -> Option<String> {
    let value = row?.get(field)?;
    if value.is_null() {
        return None;
    }
    value
        .as_str()
        .map(ToOwned::to_owned)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .or_else(|| value.as_bool().map(|value| value.to_string()))
}

fn string_len_marker(row: Option<&Value>, field: &str) -> Option<usize> {
    row?.get(field)?.as_str().map(str::len)
}

fn string_fingerprint_marker(row: Option<&Value>, field: &str) -> Option<(usize, u64)> {
    let value = row?.get(field)?.as_str()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    Some((value.len(), hasher.finish()))
}

async fn finish_interrupted_turn(
    connection: &ConnectionState,
    state: &ShimState,
    submitted: &SubmittedRequest,
    projection: &mut TurnProjection<'_>,
    running_background_tools: BTreeMap<String, codex::CommandExecutionStatus>,
) -> Result<()> {
    projection
        .finish_turn(&connection.outbound, codex::TurnStatus::Interrupted, None)
        .await?;
    spawn_background_tool_watcher(
        connection.clone(),
        state.clone(),
        submitted.request_id.clone(),
        submitted.session_id.clone(),
        projection.thread_id.to_string(),
        projection.turn_id.to_string(),
        projection.cwd.clone(),
        running_background_tools,
    );
    Ok(())
}

async fn steering_input_for_request(
    connection: &ConnectionState,
    state: &ShimState,
    request_id: &str,
) -> Result<Vec<codex::UserInput>> {
    if let Some(input) = connection.take_steering_input(request_id).await {
        return Ok(input);
    }

    let request_id_escaped = defra_agent::graphql::escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id_escaped}" }} }},
                limit: 1
            ) {{
                content
            }}
        }}"#
    );
    let response = query_node_json(state.node.as_ref(), &query).await?;
    let content = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(vec![codex::UserInput::Text {
        text: content,
        text_elements: Vec::new(),
    }])
}

#[cfg(test)]
mod tests {
    use super::{content_delta_from_cursor, ContentCursor};

    #[test]
    fn content_cursor_emits_only_appended_suffix() {
        let mut cursor = ContentCursor::default();

        assert_eq!(
            content_delta_from_cursor(&mut cursor, "first chunk"),
            "first chunk"
        );
        assert_eq!(cursor.rendered_len, "first chunk".len());
        assert_eq!(
            content_delta_from_cursor(&mut cursor, "first chunk and second"),
            " and second"
        );
        assert_eq!(cursor.rendered_len, "first chunk and second".len());
        assert_eq!(
            content_delta_from_cursor(&mut cursor, "first chunk and second"),
            ""
        );
    }

    #[test]
    fn content_cursor_falls_back_to_full_text_on_rewrite() {
        let mut cursor = ContentCursor::default();

        assert_eq!(
            content_delta_from_cursor(&mut cursor, "draft answer"),
            "draft answer"
        );
        assert_eq!(content_delta_from_cursor(&mut cursor, "final"), "final");
        assert_eq!(cursor.rendered_len, "final".len());
    }

    #[test]
    fn content_cursor_falls_back_when_tail_reset_was_missed() {
        let mut cursor = ContentCursor::default();

        assert_eq!(
            content_delta_from_cursor(&mut cursor, "previous assistant text"),
            "previous assistant text"
        );
        assert_eq!(
            content_delta_from_cursor(&mut cursor, "new assistant text after reset"),
            "new assistant text after reset"
        );
    }

    #[test]
    fn content_cursor_empty_current_resets_boundary() {
        let mut cursor = ContentCursor::default();

        assert_eq!(
            content_delta_from_cursor(&mut cursor, "old tail"),
            "old tail"
        );
        assert_eq!(content_delta_from_cursor(&mut cursor, ""), "");
        assert_eq!(cursor.rendered_len, 0);
        assert_eq!(
            content_delta_from_cursor(&mut cursor, "new tail"),
            "new tail"
        );
    }

    #[test]
    fn content_cursor_uses_utf8_byte_boundaries_from_prior_content() {
        let mut cursor = ContentCursor::default();

        assert_eq!(
            content_delta_from_cursor(&mut cursor, "hello ☕"),
            "hello ☕"
        );
        assert_eq!(
            content_delta_from_cursor(&mut cursor, "hello ☕ done"),
            " done"
        );
    }
}
