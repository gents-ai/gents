use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use anyhow::{Context, Result};
use codex_app_server_protocol as codex;
use codex_protocol::models::MessagePhase;
use gents::UpdateSubscriptionSource;
use serde_json::{json, Value};
use tokio::sync::watch;

use super::super::background::spawn_background_tool_watcher;
use super::super::bound_behavior::load_bound_context_window;
use super::super::command_projection::{
    tool_projection_status_with_settled, update_running_background_tools, ToolProjectionStatus,
};
use super::super::compaction_projection::decode_gents_compaction_progress;
use super::super::progress::{
    content_delta, decode_gents_tool_call_progress, gents_turn_progress_query,
    response_field_is_blank, terminal_error_message, terminal_turn_status, timestamp_millis,
};
use super::super::protocol::{
    send_committed_user_message, send_notification, send_thread_status_changed,
};
use super::super::store::{hydrate_materialized_response_content, query_node_json};
use super::super::subagent_projection::{
    attach_subagent_link, is_subagent_control_tool, load_authorized_subagent_threads_for_root,
    SubagentProjectionUpdateFilter,
};
use super::super::thread_projection::{
    latest_inference_usage_observation, latest_requests_token_usage, projected_thread_status,
    session_token_usage, thread_token_usage,
};
use super::super::turn_projection::TurnProjection;
use super::super::{ConnectionState, ShimState};
use super::active::next_steering_request_after;
use crate::{is_terminal_lifecycle_state, request_diagnostic_hint, SubmittedRequest};

const SUBAGENT_LINK_SETTLE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProgressMarker {
    request_lifecycle_state: Option<String>,
    request_interrupt_requested_at: Option<String>,
    request_valid_until: Option<String>,
    response_doc_id: Option<String>,
    response_status: Option<String>,
    response_token_count: Option<String>,
    response_progress_seq: Option<String>,
    response_reasoning_progress_seq: Option<String>,
    response_content_len: Option<usize>,
    response_reasoning_fingerprint: Option<(usize, u64)>,
    response_error_len: Option<usize>,
    response_materialized_message_sequence: Option<String>,
    response_materialized_at: Option<String>,
    response_completed_at: Option<String>,
    response_interrupted_at: Option<String>,
    tools: Vec<ToolProgressMarker>,
    inference_calls: Vec<InferenceCallProgressMarker>,
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
    selected_service_id: Option<String>,
    selected_tool_name: Option<String>,
    tool_failure_class: Option<String>,
    denial_reason: Option<String>,
    cancel_cause: Option<String>,
    latency_ms: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InferenceCallProgressMarker {
    call_id: Option<String>,
    call_kind: Option<String>,
    call_state: Option<String>,
    queued_at: Option<String>,
    started_at: Option<String>,
    ended_at: Option<String>,
    prompt_tokens: Option<String>,
    completion_tokens: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct ContentCursor {
    rendered_len: usize,
    head: String,
    tail: String,
}

#[derive(Clone, Debug, Default)]
struct ReasoningCursor {
    observed_preview: String,
    active_item_id: Option<String>,
    progress_seq: Option<String>,
    segment: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReasoningDelta {
    item_id: String,
    text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ReasoningObservation {
    completed_item_id: Option<String>,
    delta: Option<ReasoningDelta>,
}

#[derive(Clone, Debug)]
pub(in crate::commands::codex_shim) struct TurnStreamOptions {
    pub(super) projection_root_session_id: String,
    pub(super) baseline_turn: Option<codex::Turn>,
    pub(super) follow_steering: bool,
    pub(super) enforce_timeout: bool,
}

impl TurnStreamOptions {
    pub(in crate::commands::codex_shim) fn fresh(root_session_id: impl Into<String>) -> Self {
        Self {
            projection_root_session_id: root_session_id.into(),
            baseline_turn: None,
            follow_steering: true,
            enforce_timeout: true,
        }
    }

    pub(in crate::commands::codex_shim) fn resumed_subagent(
        root_session_id: impl Into<String>,
        baseline_turn: codex::Turn,
    ) -> Self {
        Self {
            projection_root_session_id: root_session_id.into(),
            baseline_turn: Some(baseline_turn),
            follow_steering: false,
            enforce_timeout: false,
        }
    }

    pub(in crate::commands::codex_shim) fn fresh_subagent(
        root_session_id: impl Into<String>,
    ) -> Self {
        Self {
            projection_root_session_id: root_session_id.into(),
            baseline_turn: None,
            follow_steering: false,
            enforce_timeout: false,
        }
    }
}

pub(in crate::commands::codex_shim) async fn stream_gents_turn(
    connection: &ConnectionState,
    state: &ShimState,
    submitted: &SubmittedRequest,
    projection: &mut TurnProjection<'_>,
    mut cancel_rx: watch::Receiver<bool>,
    mut options: TurnStreamOptions,
) -> Result<()> {
    let outbound = &connection.outbound;
    let mut current = submitted.clone();
    let mut turn_request_ids = vec![current.request_id.clone()];
    let mut known_tool_calls: BTreeMap<String, ToolProjectionStatus> = BTreeMap::new();
    let mut known_tool_markers: BTreeMap<String, ToolProgressMarker> = BTreeMap::new();
    let mut known_compaction_states: BTreeMap<String, String> = BTreeMap::new();
    let mut known_inference_usage_call_id: Option<String> = None;
    let mut running_background_tools: BTreeMap<String, codex::CommandExecutionStatus> =
        BTreeMap::new();
    let mut updates = state.node.subscribe_updates();
    let mut updates_closed = false;
    let subagent_update_filter = SubagentProjectionUpdateFilter::from_state(state);
    let mut subagent_links = Vec::new();
    let mut subagent_links_dirty = true;
    let mut subagent_link_settle_started_at = None;
    let mut latest_content_cursor = ContentCursor::default();
    let mut latest_reasoning_cursor = ReasoningCursor::default();
    let mut latest_error_message: Option<String> = None;
    let mut latest_progress_marker: Option<ProgressMarker> = None;
    let mut last_progress_at = tokio::time::Instant::now();
    if let Some(baseline_turn) = options.baseline_turn.take() {
        prime_projection_from_turn(
            projection,
            &baseline_turn,
            &current.request_id,
            &mut latest_content_cursor,
            &mut latest_reasoning_cursor,
            &mut known_tool_calls,
            &mut known_compaction_states,
        );
    }

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

        let progress_query = gents_turn_progress_query(&current.request_id, &current.session_id);
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
        let inference_call_rows = response
            .pointer("/data/InferenceCall")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let response_status = response_row
            .and_then(|row| row.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let lifecycle_state = request_row
            .as_ref()
            .and_then(|row| row.get("lifecycle_state"))
            .and_then(Value::as_str)
            .unwrap_or("");
        projection.observe_response_timing(
            response_row
                .and_then(|row| nonempty_timestamp_field(row, "created_at"))
                .and_then(timestamp_millis),
            response_row
                .and_then(response_terminal_timestamp)
                .and_then(timestamp_millis),
        );
        let projection_settled = is_terminal_lifecycle_state(lifecycle_state)
            || matches!(response_status, "complete" | "completed" | "error");

        let marker = progress_marker(request_row, response_row, tool_rows, inference_call_rows);
        let marker_changed = latest_progress_marker.as_ref() != Some(&marker);
        if marker_changed {
            latest_progress_marker = Some(marker);
            last_progress_at = tokio::time::Instant::now();
        }

        for row in inference_call_rows {
            let Some(compaction) = decode_gents_compaction_progress(row) else {
                continue;
            };
            let previous_state = known_compaction_states
                .get(&compaction.call_id)
                .map(String::as_str);
            projection
                .send_compaction_projection_update(outbound, &compaction, previous_state)
                .await?;
            known_compaction_states.insert(compaction.call_id, compaction.call_state);
        }

        if let Some(usage) = latest_inference_usage_observation(inference_call_rows) {
            if known_inference_usage_call_id.as_deref() != Some(&usage.call_id) {
                send_thread_token_usage_update(
                    outbound,
                    state,
                    projection,
                    &current.session_id,
                    usage.totals,
                )
                .await?;
            }
            known_inference_usage_call_id = Some(usage.call_id);
        }

        let has_subagent_control = tool_rows.iter().any(|row| {
            row.get("tool_name")
                .and_then(Value::as_str)
                .is_some_and(is_subagent_control_tool)
        });
        let subagent_tool_marker_changed = tool_rows.iter().any(|row| {
            let marker = tool_progress_marker(row);
            marker
                .tool_name
                .as_deref()
                .is_some_and(is_subagent_control_tool)
                && marker
                    .tool_call_key
                    .as_deref()
                    .is_some_and(|tool_key| known_tool_markers.get(tool_key) != Some(&marker))
        });
        let settling_deferred_control = projection_settled
            && known_tool_calls
                .values()
                .any(|status| status == &ToolProjectionStatus::DeferredCollab);
        if subagent_tool_marker_changed || settling_deferred_control || updates_closed {
            subagent_links_dirty = true;
        }
        let mut subagent_links_refreshed = false;
        if has_subagent_control && subagent_links_dirty {
            subagent_links = load_authorized_subagent_threads_for_root(
                state,
                &options.projection_root_session_id,
            )
            .await?;
            subagent_links_dirty = false;
            subagent_links_refreshed = true;
        }
        let unresolved_terminal_control = projection_settled
            && tool_rows.iter().any(|row| {
                let Some(mut tool) = decode_gents_tool_call_progress(row) else {
                    return false;
                };
                attach_subagent_link(&mut tool, &subagent_links);
                matches!(
                    tool_projection_status_with_settled(&tool, false, false),
                    ToolProjectionStatus::DeferredCollab
                )
            });
        let link_settle_expired = observe_subagent_link_settle_window(
            &mut subagent_link_settle_started_at,
            unresolved_terminal_control,
            tokio::time::Instant::now(),
            SUBAGENT_LINK_SETTLE_TIMEOUT,
        );
        let waiting_for_subagent_links = unresolved_terminal_control && !link_settle_expired;

        if marker_changed && !projection_settled {
            if let Some(reasoning) = response_row
                .and_then(|row| row.get("reasoning"))
                .and_then(Value::as_str)
            {
                let observation = latest_reasoning_cursor.observe(
                    &current.request_id,
                    reasoning,
                    response_row.and_then(|row| scalar_marker(Some(row), "progress_seq")),
                );
                if let Some(item_id) = observation.completed_item_id {
                    projection
                        .finish_reasoning(outbound, &item_id, None)
                        .await?;
                }
                if let Some(delta) = observation.delta {
                    projection
                        .append_reasoning_delta(outbound, &delta.item_id, &delta.text)
                        .await?;
                }
            }
        }

        for row in tool_rows {
            let tool_marker = tool_progress_marker(row);
            let Some(tool_key) = tool_marker.tool_call_key.as_deref() else {
                continue;
            };
            let retry_subagent_projection = tool_marker
                .tool_name
                .as_deref()
                .is_some_and(is_subagent_control_tool)
                && subagent_links_refreshed;
            if known_tool_markers.get(tool_key) == Some(&tool_marker) && !retry_subagent_projection
            {
                continue;
            }
            let Some(mut tool) = decode_gents_tool_call_progress(row) else {
                continue;
            };
            if has_subagent_control {
                attach_subagent_link(&mut tool, &subagent_links);
            }
            let projection_status =
                tool_projection_status_with_settled(&tool, projection_settled, link_settle_expired);
            let previous_status = known_tool_calls.get(&tool.tool_call_key).cloned();
            update_running_background_tools(
                &mut running_background_tools,
                &tool,
                &projection_status,
            );
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
            last_progress_at = tokio::time::Instant::now();

            known_tool_calls.insert(tool.tool_call_key.clone(), projection_status);
            known_tool_markers.insert(tool.tool_call_key.clone(), tool_marker);
        }

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

        let failure_reason = request_row
            .as_ref()
            .and_then(|row| row.get("failure_reason"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let terminal_by_request = is_terminal_lifecycle_state(lifecycle_state);
        let terminal_by_response = matches!(response_status, "complete" | "completed" | "error");

        if (terminal_by_request || terminal_by_response) && !waiting_for_subagent_links {
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
                if options.enforce_timeout && last_progress_at.elapsed() >= state.timeout {
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

            let completed_at_ms = response_terminal_timestamp(&terminal_response)
                .or_else(|| {
                    request_row.and_then(|row| nonempty_timestamp_field(row, "terminalized_at"))
                })
                .and_then(timestamp_millis);
            projection
                .set_completed_at(completed_at_ms.map(|timestamp| timestamp.div_euclid(1000)));
            projection.observe_response_timing(None, completed_at_ms);

            let durable_reasoning = terminal_response
                .get("reasoning")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty());
            let reasoning_item_id = latest_reasoning_cursor
                .active_item_id(&current.request_id)
                .to_string();
            projection
                .finish_reasoning(outbound, &reasoning_item_id, durable_reasoning)
                .await?;

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
            if options.follow_steering && turn_status == codex::TurnStatus::Completed {
                if let Some(next_request) =
                    next_steering_request_after(state, &current.session_id, &current.request_id)
                        .await
                        .context("loading next Codex steering request")?
                {
                    if next_request.is_pending() {
                        if last_progress_at.elapsed() >= state.timeout {
                            cancel_pending_steering_request(
                                connection,
                                state,
                                &next_request.request_id,
                            )
                            .await;
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
                        timestamp_millis(&next_request.created_at),
                    )
                    .await?;
                    current.request_id = next_request.request_id;
                    turn_request_ids.push(current.request_id.clone());
                    known_tool_calls.clear();
                    known_tool_markers.clear();
                    known_compaction_states.clear();
                    known_inference_usage_call_id = None;
                    subagent_links_dirty = true;
                    subagent_link_settle_started_at = None;
                    latest_content_cursor.reset();
                    latest_reasoning_cursor.reset();
                    projection.reset_response_timing();
                    latest_error_message = None;
                    latest_progress_marker = None;
                    last_progress_at = tokio::time::Instant::now();
                    continue;
                }
            }
            let last_usage = latest_requests_token_usage(state, &turn_request_ids)
                .await
                .unwrap_or_default();
            send_thread_token_usage_update(
                outbound,
                state,
                projection,
                &current.session_id,
                last_usage,
            )
            .await?;

            projection
                .finish_turn(outbound, turn_status, error_message)
                .await
                .context("sending terminal Codex turn notification")?;
            send_thread_status_changed(
                outbound,
                state,
                projection.thread_id,
                projected_thread_status(Some(lifecycle_state), ""),
            )
            .await?;
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

        if options.enforce_timeout && last_progress_at.elapsed() >= state.timeout {
            anyhow::bail!(
                "timed out waiting for AgentResponse {} after {}s of inactivity\n{}",
                current.request_id,
                state.timeout.as_secs(),
                request_diagnostic_hint(&current.request_id)
            );
        }

        tokio::select! {
            _ = tokio::time::sleep(state.poll_interval) => {
                if updates_closed {
                    subagent_links_dirty = true;
                }
            }
            msg = updates.recv(), if !updates_closed => {
                match msg {
                    Some(message) => {
                        if message.as_update().is_some_and(|update| {
                            subagent_update_filter.affects_collection_id(&update.collection_id)
                        }) {
                            subagent_links_dirty = true;
                        }
                    }
                    None => {
                        tracing::warn!("Codex shim embedded-node update subscription closed");
                        updates_closed = true;
                        subagent_links_dirty = true;
                    }
                }
                let dropped = updates.check_and_reset_dropped();
                if dropped > 0 {
                    tracing::warn!(dropped, "Codex shim update subscription dropped messages");
                    subagent_links_dirty = true;
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

fn observe_subagent_link_settle_window(
    started_at: &mut Option<tokio::time::Instant>,
    unresolved: bool,
    now: tokio::time::Instant,
    timeout: Duration,
) -> bool {
    if !unresolved {
        *started_at = None;
        return false;
    }
    let started_at = *started_at.get_or_insert(now);
    now.duration_since(started_at) >= timeout
}

fn nonempty_timestamp_field<'a>(row: &'a Value, field: &str) -> Option<&'a str> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn response_terminal_timestamp(row: &Value) -> Option<&str> {
    ["completed_at", "interrupted_at", "materialized_at"]
        .into_iter()
        .find_map(|field| nonempty_timestamp_field(row, field))
}

fn progress_marker(
    request_row: Option<&Value>,
    response_row: Option<&Value>,
    tool_rows: &[Value],
    inference_call_rows: &[Value],
) -> ProgressMarker {
    ProgressMarker {
        request_lifecycle_state: scalar_marker(request_row, "lifecycle_state"),
        request_interrupt_requested_at: scalar_marker(request_row, "interrupt_requested_at"),
        request_valid_until: scalar_marker(request_row, "valid_until"),
        response_doc_id: scalar_marker(response_row, "_docID"),
        response_status: scalar_marker(response_row, "status"),
        response_token_count: scalar_marker(response_row, "token_count"),
        response_progress_seq: scalar_marker(response_row, "progress_seq"),
        response_reasoning_progress_seq: scalar_marker(response_row, "reasoning_progress_seq"),
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
        inference_calls: inference_call_rows
            .iter()
            .map(inference_call_progress_marker)
            .collect(),
    }
}

fn prime_projection_from_turn(
    projection: &mut TurnProjection<'_>,
    turn: &codex::Turn,
    request_id: &str,
    content_cursor: &mut ContentCursor,
    reasoning_cursor: &mut ReasoningCursor,
    known_tool_calls: &mut BTreeMap<String, ToolProjectionStatus>,
    known_compaction_states: &mut BTreeMap<String, String>,
) {
    let preferred_agent_id = format!("gents-{request_id}");
    let preferred_reasoning_id = reasoning_item_id(request_id, 0);
    let mut resumed_agent = None;
    let mut found_preferred_agent = false;
    let resumed_reasoning = resumable_reasoning_item(turn, &preferred_reasoning_id);
    for item in &turn.items {
        match item {
            codex::ThreadItem::AgentMessage { id, text, .. } => {
                if id == &preferred_agent_id {
                    resumed_agent = Some((id.clone(), text.clone()));
                    found_preferred_agent = true;
                } else if !found_preferred_agent {
                    resumed_agent = Some((id.clone(), text.clone()));
                }
            }
            codex::ThreadItem::Reasoning { .. } => {}
            codex::ThreadItem::McpToolCall { id, status, .. } => {
                known_tool_calls.insert(id.clone(), ToolProjectionStatus::Mcp(status.clone()));
            }
            codex::ThreadItem::CommandExecution { id, status, .. } => {
                known_tool_calls.insert(id.clone(), ToolProjectionStatus::Command(status.clone()));
            }
            codex::ThreadItem::FileChange { id, status, .. } => {
                known_tool_calls
                    .insert(id.clone(), ToolProjectionStatus::FileChange(status.clone()));
            }
            codex::ThreadItem::CollabAgentToolCall {
                id,
                tool,
                status,
                receiver_thread_ids,
                model,
                agents_states,
                ..
            } => {
                let Some(receiver_thread_id) = receiver_thread_ids.first() else {
                    continue;
                };
                let child = agents_states.get(receiver_thread_id);
                known_tool_calls.insert(
                    id.clone(),
                    ToolProjectionStatus::Collab(
                        super::super::subagent_projection::CollabProjection {
                            status: status.clone(),
                            tool: tool.clone(),
                            receiver_thread_id: receiver_thread_id.clone(),
                            child_model: model.clone(),
                            child_lifecycle_state: child
                                .map(|state| collab_lifecycle_state(&state.status))
                                .unwrap_or("")
                                .to_string(),
                            child_failure_reason: child.and_then(|state| state.message.clone()),
                        },
                    ),
                );
            }
            codex::ThreadItem::ContextCompaction { id } => {
                known_compaction_states.insert(id.clone(), "completed".to_string());
            }
            _ => {}
        }
    }
    if let Some((item_id, text)) = resumed_agent.filter(|(_, text)| !text.trim().is_empty()) {
        content_cursor.prime(&text);
        projection.resume_agent_message(item_id, &text);
    }
    if let Some((item_id, text)) = resumed_reasoning {
        reasoning_cursor.prime(item_id.clone(), &text);
        projection.resume_reasoning(item_id, &text);
    }
}

fn resumable_reasoning_item(turn: &codex::Turn, preferred_id: &str) -> Option<(String, String)> {
    turn.items.iter().find_map(|item| {
        let codex::ThreadItem::Reasoning {
            id,
            summary,
            content,
        } = item
        else {
            return None;
        };
        if id != preferred_id {
            return None;
        }
        let text = if content.is_empty() {
            summary.concat()
        } else {
            content.concat()
        };
        (!text.trim().is_empty()).then(|| (id.clone(), text))
    })
}

fn collab_lifecycle_state(status: &codex::CollabAgentStatus) -> &'static str {
    match status {
        codex::CollabAgentStatus::PendingInit => "pending",
        codex::CollabAgentStatus::Running => "processing",
        codex::CollabAgentStatus::Completed | codex::CollabAgentStatus::Shutdown => "completed",
        codex::CollabAgentStatus::Errored | codex::CollabAgentStatus::NotFound => "failed",
        codex::CollabAgentStatus::Interrupted => "interrupted",
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

    fn prime(&mut self, current: &str) {
        self.observe(current);
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

impl ReasoningCursor {
    fn observe(
        &mut self,
        request_id: &str,
        current: &str,
        progress_seq: Option<String>,
    ) -> ReasoningObservation {
        let progress_boundary = self.progress_seq.is_some()
            && progress_seq.is_some()
            && self.progress_seq != progress_seq;
        self.progress_seq = progress_seq;
        let explicit_boundary = current.is_empty() && !self.observed_preview.is_empty();
        let previous_preview = self.observed_preview.clone();
        let completed_item_id = ((progress_boundary || explicit_boundary)
            && !self.observed_preview.is_empty())
        .then(|| self.active_item_id(request_id));
        if completed_item_id.is_some() {
            self.observed_preview.clear();
            self.active_item_id = None;
            self.segment = self.segment.saturating_add(1);
        }

        if current.is_empty()
            || (progress_boundary && current == previous_preview)
            || current == self.observed_preview
        {
            return ReasoningObservation {
                completed_item_id,
                delta: None,
            };
        }

        let (delta, discontinuity) = if self.observed_preview.is_empty() {
            (current, false)
        } else if let Some(delta) = current.strip_prefix(&self.observed_preview) {
            (delta, false)
        } else {
            let overlap = suffix_prefix_overlap(&self.observed_preview, current);
            if overlap == 0 {
                (current, true)
            } else {
                (&current[overlap..], false)
            }
        };
        self.observed_preview = current.to_string();
        if delta.is_empty() {
            return ReasoningObservation {
                completed_item_id,
                delta: None,
            };
        }
        if discontinuity {
            self.segment = self.segment.saturating_add(1);
            self.active_item_id = Some(reasoning_item_id(request_id, self.segment));
        }
        let item_id = self
            .active_item_id
            .get_or_insert_with(|| reasoning_item_id(request_id, self.segment))
            .clone();
        ReasoningObservation {
            completed_item_id,
            delta: Some(ReasoningDelta {
                item_id,
                text: delta.to_string(),
            }),
        }
    }

    fn prime(&mut self, item_id: String, text: &str) {
        self.observed_preview = text.to_string();
        self.active_item_id = Some(item_id);
    }

    fn active_item_id(&self, request_id: &str) -> String {
        self.active_item_id
            .clone()
            .unwrap_or_else(|| reasoning_item_id(request_id, self.segment))
    }

    fn reset(&mut self) {
        self.observed_preview.clear();
        self.active_item_id = None;
        self.progress_seq = None;
        self.segment = 0;
    }
}

fn reasoning_item_id(request_id: &str, segment: u64) -> String {
    if segment == 0 {
        format!("gents-reasoning-{request_id}")
    } else {
        format!("gents-reasoning-{request_id}-segment-{segment}")
    }
}

/// Longest byte length which is both a suffix of `previous` and a prefix of
/// `current`. This is the KMP prefix function over `current + sentinel +
/// previous`; 0xFF cannot occur in valid UTF-8, so it is an unambiguous
/// separator. The result is a UTF-8 boundary because the matching suffix ends
/// at the boundary at the end of `previous`.
fn suffix_prefix_overlap(previous: &str, current: &str) -> usize {
    if previous.is_empty() || current.is_empty() {
        return 0;
    }
    let mut combined = Vec::with_capacity(current.len() + 1 + previous.len());
    combined.extend_from_slice(current.as_bytes());
    combined.push(0xff);
    combined.extend_from_slice(previous.as_bytes());
    let mut prefix = vec![0usize; combined.len()];
    for index in 1..combined.len() {
        let mut candidate = prefix[index - 1];
        while candidate > 0 && combined[index] != combined[candidate] {
            candidate = prefix[candidate - 1];
        }
        if combined[index] == combined[candidate] {
            candidate += 1;
        }
        prefix[index] = candidate.min(current.len());
    }
    prefix.last().copied().unwrap_or_default()
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
        selected_service_id: scalar_marker(Some(row), "selected_service_id"),
        selected_tool_name: scalar_marker(Some(row), "selected_tool_name"),
        tool_failure_class: scalar_marker(Some(row), "tool_failure_class"),
        denial_reason: scalar_marker(Some(row), "denial_reason"),
        cancel_cause: scalar_marker(Some(row), "cancel_cause"),
        latency_ms: scalar_marker(Some(row), "latency_ms"),
    }
}

fn inference_call_progress_marker(row: &Value) -> InferenceCallProgressMarker {
    InferenceCallProgressMarker {
        call_id: scalar_marker(Some(row), "call_id"),
        call_kind: scalar_marker(Some(row), "call_kind"),
        call_state: scalar_marker(Some(row), "call_state"),
        queued_at: scalar_marker(Some(row), "queued_at"),
        started_at: scalar_marker(Some(row), "started_at"),
        ended_at: scalar_marker(Some(row), "ended_at"),
        prompt_tokens: scalar_marker(Some(row), "prompt_tokens"),
        completion_tokens: scalar_marker(Some(row), "completion_tokens"),
    }
}

async fn send_thread_token_usage_update(
    outbound: &super::super::Outbound,
    state: &ShimState,
    projection: &TurnProjection<'_>,
    session_id: &str,
    last_usage: super::super::thread_projection::TokenTotals,
) -> Result<()> {
    let total_usage = session_token_usage(state, session_id)
        .await
        .unwrap_or_default();
    let model_context_window = load_bound_context_window(
        state.node.as_ref(),
        state.behavior_id.as_ref(),
    )
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(
            %error,
            behavior_id = %state.behavior_id,
            "Codex shim could not load the effective context window; using the runtime default"
        );
        gents::DEFAULT_CONTEXT_WINDOW as i64
    });
    send_notification(
        outbound,
        state,
        codex::ServerNotification::ThreadTokenUsageUpdated(
            codex::ThreadTokenUsageUpdatedNotification {
                thread_id: projection.thread_id.to_string(),
                turn_id: projection.turn_id.to_string(),
                token_usage: thread_token_usage(total_usage, last_usage, model_context_window),
            },
        ),
    )
    .await
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
    send_thread_status_changed(
        &connection.outbound,
        state,
        projection.thread_id,
        codex::ThreadStatus::Idle,
    )
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

async fn cancel_pending_steering_request(
    connection: &ConnectionState,
    state: &ShimState,
    request_id: &str,
) {
    connection.take_steering_input(request_id).await;
    if let Err(error) = gents::interrupt_request(state.node.as_ref(), request_id).await {
        tracing::warn!(
            %error,
            request_id,
            "Codex shim failed to interrupt timed-out queued steering request"
        );
    }
}

async fn steering_input_for_request(
    connection: &ConnectionState,
    state: &ShimState,
    request_id: &str,
) -> Result<Vec<codex::UserInput>> {
    if let Some(input) = connection.take_steering_input(request_id).await {
        return Ok(input);
    }

    let request_id_escaped = gents::graphql::escape_graphql_string(request_id);
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
    use codex_app_server_protocol as codex;

    use super::{
        content_delta_from_cursor, observe_subagent_link_settle_window, resumable_reasoning_item,
        suffix_prefix_overlap, ContentCursor, ReasoningCursor,
    };

    #[test]
    fn terminal_subagent_link_gets_a_bounded_replication_window() {
        let start = tokio::time::Instant::now();
        let timeout = std::time::Duration::from_secs(5);
        let mut started_at = None;

        assert!(!observe_subagent_link_settle_window(
            &mut started_at,
            true,
            start,
            timeout
        ));
        assert!(!observe_subagent_link_settle_window(
            &mut started_at,
            true,
            start + std::time::Duration::from_secs(4),
            timeout
        ));
        assert!(observe_subagent_link_settle_window(
            &mut started_at,
            true,
            start + timeout,
            timeout
        ));
        assert!(!observe_subagent_link_settle_window(
            &mut started_at,
            false,
            start + timeout,
            timeout
        ));
        assert!(started_at.is_none());
    }

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
    fn primed_content_cursor_emits_updates_after_resume_snapshot() {
        let mut cursor = ContentCursor::default();
        cursor.prime("visible in thread/resume");

        assert_eq!(
            content_delta_from_cursor(&mut cursor, "visible in thread/resume plus live delta"),
            " plus live delta"
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

    #[test]
    fn reasoning_cursor_emits_live_append_without_duplication() {
        let mut cursor = ReasoningCursor::default();
        let first = cursor
            .observe("request-1", "inspect", Some("1".to_string()))
            .delta
            .expect("first reasoning delta");
        assert_eq!(first.item_id, "gents-reasoning-request-1");
        assert_eq!(first.text, "inspect");

        let appended = cursor
            .observe("request-1", "inspect then test", Some("1".to_string()))
            .delta
            .expect("appended reasoning delta");
        assert_eq!(appended.item_id, first.item_id);
        assert_eq!(appended.text, " then test");
        assert_eq!(
            cursor.observe("request-1", "inspect then test", Some("1".to_string())),
            Default::default()
        );
    }

    #[test]
    fn reasoning_cursor_recovers_delta_after_bounded_tail_rolls() {
        let mut cursor = ReasoningCursor::default();
        cursor
            .observe("request-1", "first middle", Some("1".to_string()))
            .delta
            .expect("first reasoning delta");
        let rolled = cursor
            .observe("request-1", "middle last", Some("1".to_string()))
            .delta
            .expect("rolled reasoning delta");
        assert_eq!(rolled.item_id, "gents-reasoning-request-1");
        assert_eq!(rolled.text, " last");
    }

    #[test]
    fn reasoning_cursor_primes_resume_without_replay() {
        let mut cursor = ReasoningCursor::default();
        cursor.prime("gents-reasoning-request-1".to_string(), "already visible");
        assert!(cursor
            .observe("request-1", "already visible", Some("1".to_string()))
            .delta
            .is_none());
        let delta = cursor
            .observe(
                "request-1",
                "already visible plus new",
                Some("1".to_string()),
            )
            .delta
            .expect("new reasoning after resume");
        assert_eq!(delta.text, " plus new");
    }

    #[test]
    fn resume_never_binds_current_cursor_to_foreign_reasoning_item() {
        let turn = codex::Turn {
            id: "request-2".to_string(),
            items: vec![codex::ThreadItem::Reasoning {
                id: "gents-reasoning-message-1".to_string(),
                summary: Vec::new(),
                content: vec!["reasoning from an earlier model turn".to_string()],
            }],
            items_view: codex::TurnItemsView::Full,
            status: codex::TurnStatus::InProgress,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        };
        assert!(resumable_reasoning_item(&turn, "gents-reasoning-request-2").is_none());
    }

    #[test]
    fn reasoning_cursor_starts_new_item_after_unrecoverable_gap() {
        let mut cursor = ReasoningCursor::default();
        cursor
            .observe("request-1", "old preview", Some("1".to_string()))
            .delta
            .expect("first reasoning delta");
        let replacement = cursor
            .observe("request-1", "entirely new preview", Some("1".to_string()))
            .delta
            .expect("replacement reasoning delta");
        assert_eq!(replacement.item_id, "gents-reasoning-request-1-segment-1");
        assert_eq!(replacement.text, "entirely new preview");
    }

    #[test]
    fn reasoning_cursor_segments_on_observed_empty_runtime_boundary() {
        let mut cursor = ReasoningCursor::default();
        cursor
            .observe("request-1", "first turn", Some("1".to_string()))
            .delta
            .expect("first reasoning delta");

        let boundary = cursor.observe("request-1", "", Some("2".to_string()));
        assert_eq!(
            boundary.completed_item_id.as_deref(),
            Some("gents-reasoning-request-1")
        );
        assert!(boundary.delta.is_none());

        let next = cursor
            .observe("request-1", "second turn", Some("2".to_string()))
            .delta
            .expect("second reasoning delta");
        assert_eq!(next.item_id, "gents-reasoning-request-1-segment-1");
        assert_eq!(next.text, "second turn");
    }

    #[test]
    fn reasoning_cursor_uses_progress_boundary_when_empty_write_was_missed() {
        let mut cursor = ReasoningCursor::default();
        cursor
            .observe("request-1", "tail shared", Some("1".to_string()))
            .delta
            .expect("first reasoning delta");

        let next = cursor.observe(
            "request-1",
            "shared but belongs to the next turn",
            Some("2".to_string()),
        );
        assert_eq!(
            next.completed_item_id.as_deref(),
            Some("gents-reasoning-request-1")
        );
        let delta = next.delta.expect("next-turn reasoning delta");
        assert_eq!(delta.item_id, "gents-reasoning-request-1-segment-1");
        assert_eq!(delta.text, "shared but belongs to the next turn");
    }

    #[test]
    fn suffix_prefix_overlap_is_linear_and_utf8_safe() {
        assert_eq!(suffix_prefix_overlap("abc middle", "middle xyz"), 6);
        assert_eq!(suffix_prefix_overlap("reason ☕", "☕ next"), "☕".len());
        assert_eq!(suffix_prefix_overlap("no overlap", "fresh"), 0);
    }
}
