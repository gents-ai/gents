use std::collections::BTreeMap;

use anyhow::{Context, Result};
use codex_app_server_protocol as codex;
use defra_agent::UpdateSubscriptionSource;
use serde_json::{json, Value};
use tokio::sync::watch;

use super::super::background::spawn_background_tool_watcher;
use super::super::command_projection::{
    tool_projection_status, update_running_background_tools, ToolProjectionStatus,
};
use super::super::progress::{
    content_delta, decode_defra_tool_call_progress, decode_defra_turn_progress,
    defra_turn_progress_query, response_field_is_blank, terminal_error_message,
    terminal_turn_status,
};
use super::super::protocol::send_committed_user_message;
use super::super::store::{hydrate_materialized_response_content, query_node_json};
use super::super::turn_projection::TurnProjection;
use super::super::{ConnectionState, ShimState};
use super::active::next_steering_request_after;
use crate::{is_terminal_lifecycle_state, request_diagnostic_hint, SubmittedRequest};

pub(super) async fn stream_defra_turn(
    connection: &ConnectionState,
    state: &ShimState,
    submitted: &SubmittedRequest,
    projection: &mut TurnProjection<'_>,
    mut cancel_rx: watch::Receiver<bool>,
) -> Result<()> {
    let outbound = &connection.outbound;
    let mut current = submitted.clone();
    let mut known_tool_calls: BTreeMap<String, ToolProjectionStatus> = BTreeMap::new();
    let mut running_background_tools: BTreeMap<String, codex::CommandExecutionStatus> =
        BTreeMap::new();
    let mut updates = state.node.subscribe_updates();
    let mut latest_content = String::new();
    let mut latest_reasoning = String::new();
    let mut latest_error_message: Option<String> = None;
    let mut latest_progress_signature: Option<String> = None;
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
            .and_then(|rows| rows.first())
            .cloned();
        let response_row = response
            .pointer("/data/AgentResponse")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .cloned();

        let signature = serde_json::to_string(&json!({
            "request": &request_row,
            "response": &response_row,
            "tools": response.pointer("/data/AgentToolCall"),
        }))
        .context("serializing DEFRA Codex shim progress signature")?;
        if latest_progress_signature.as_deref() != Some(signature.as_str()) {
            latest_progress_signature = Some(signature);
            last_progress_at = tokio::time::Instant::now();
        }

        let tool_rows = response
            .pointer("/data/AgentToolCall")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for tool in tool_rows.iter().filter_map(decode_defra_tool_call_progress) {
            let projection_status = tool_projection_status(&tool);
            let previous_status = known_tool_calls.get(&tool.tool_call_key).cloned();
            if previous_status.as_ref() == Some(&projection_status) {
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
        }

        let response_progress = response_row.as_ref().and_then(decode_defra_turn_progress);
        if let Some(progress) = response_progress.as_ref() {
            if progress.content != latest_content {
                let delta = content_delta(&latest_content, &progress.content);
                latest_content = progress.content.clone();
                projection.append_agent_delta(outbound, &delta).await?;
            }
            latest_reasoning = progress.reasoning.clone();
            latest_error_message = progress.error_message.clone();
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
        let response_status = response_progress
            .as_ref()
            .map(|progress| progress.status.as_str())
            .unwrap_or("");
        let terminal_by_request = is_terminal_lifecycle_state(lifecycle_state);
        let terminal_by_response = matches!(response_status, "complete" | "completed" | "error");

        if terminal_by_request || terminal_by_response {
            let mut terminal_response = response_row.unwrap_or_else(|| {
                json!({
                    "request_id": current.request_id,
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
                    .await?;
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

            let error_message = terminal_error_message(
                response_status,
                latest_error_message.as_deref(),
                lifecycle_state,
                failure_reason,
            );
            if let Some(error_message) = error_message.as_deref() {
                if !projection.rendered_agent_text().contains(error_message) {
                    projection
                        .append_agent_delta(outbound, &format!("\n[agent error] {error_message}\n"))
                        .await?;
                }
            }

            let turn_status = terminal_turn_status(lifecycle_state, response_status);
            if turn_status == codex::TurnStatus::Completed {
                if let Some(next_request) =
                    next_steering_request_after(state, &current.session_id, &current.request_id)
                        .await?
                {
                    if next_request.is_pending() {
                        tokio::time::sleep(state.poll_interval).await;
                        continue;
                    }
                    projection.finish_agent_message(outbound).await?;
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
                    known_tool_calls.clear();
                    latest_content.clear();
                    latest_reasoning.clear();
                    latest_error_message = None;
                    latest_progress_signature = None;
                    last_progress_at = tokio::time::Instant::now();
                    continue;
                }
            }
            projection
                .finish_turn(outbound, turn_status, error_message)
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

        if last_progress_at.elapsed() >= state.timeout {
            anyhow::bail!(
                "timed out waiting for AgentResponse {} after {}s of inactivity\n{}",
                current.request_id,
                state.timeout.as_secs(),
                request_diagnostic_hint(&current.request_id)
            );
        }

        let _ = &latest_reasoning;
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
