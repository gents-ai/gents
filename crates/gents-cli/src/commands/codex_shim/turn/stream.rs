use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::UpdateSubscriptionSource;
use gents_codex_protocol as codex;
use gents_codex_protocol::MessagePhase;
use gents_protocol::client_protocol::project_persisted_attempt;
use serde_json::{json, Value};
use tokio::sync::watch;

use super::super::background::spawn_background_tool_watcher;
use super::super::bound_behavior::load_bound_context_window;
use super::super::command_projection::{
    observed_command_status, observed_mcp_status, observed_patch_status, tool_projection_status,
    update_running_background_tools, ToolProjectionStatus,
};
use super::super::compaction_projection::decode_gents_compaction_progress;
use super::super::progress::{
    codex_turn_status, content_delta, decode_gents_tool_call_progress, gents_turn_progress_query,
    hydrate_gents_tool_call_progress, terminal_error_message, timestamp_millis,
};
use super::super::protocol::{
    send_committed_user_message, send_notification, send_thread_status_changed,
};
use super::super::store::query_node_json;
use super::super::thread_projection::{
    latest_inference_usage_observation, projected_thread_status, submitted_token_usage,
    thread_token_usage,
};
use super::super::turn_projection::TurnProjection;
use super::super::{ConnectionState, ShimState};
use super::active::next_steering_request_after;
use crate::{request_diagnostic_hint, SubmittedRequest};

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProgressMarker {
    request_lifecycle_state: Option<String>,
    request_interrupt_requested_at: Option<String>,
    request_valid_until: Option<String>,
    request_lease_expires_at: Option<String>,
    request_failure_reason: Option<String>,
    response_content_fingerprint: Option<(usize, u64)>,
    response_reasoning_fingerprint: Option<(usize, u64)>,
    selected_source: Option<gents::session::CanonicalSelectedSource>,
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
    selected_source: Option<gents::session::CanonicalSelectedSource>,
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

    pub(in crate::commands::codex_shim) fn fresh_background_completion(
        root_session_id: impl Into<String>,
    ) -> Self {
        Self {
            projection_root_session_id: root_session_id.into(),
            baseline_turn: None,
            follow_steering: true,
            enforce_timeout: false,
        }
    }

    pub(in crate::commands::codex_shim) fn resumed_background_completion(
        root_session_id: impl Into<String>,
        baseline_turn: codex::Turn,
    ) -> Self {
        Self {
            projection_root_session_id: root_session_id.into(),
            baseline_turn: Some(baseline_turn),
            follow_steering: true,
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
    let mut running_background_tools = BTreeSet::new();
    let mut updates = state.node.subscribe_updates();
    let mut updates_closed = false;
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

        let progress_query =
            gents_turn_progress_query(&current.request_doc_id, &current.session_id);
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
        let requests = response
            .pointer("/data/AgentRequest")
            .and_then(Value::as_array)
            .context("live request query omitted rows")?;
        anyhow::ensure!(
            requests.len() == 1,
            "missing or ambiguous physical live request"
        );
        let request_row = requests.first();
        for row in requests.iter() {
            anyhow::ensure!(
                row.get("agent_did").and_then(Value::as_str) == Some(current.agent_did.as_str())
                    && row.get("requester_did").and_then(Value::as_str)
                        == current.requester_did.as_deref()
                    && row.get("session_id").and_then(Value::as_str)
                        == Some(current.session_id.as_str())
                    && row.get("request_id").and_then(Value::as_str)
                        == Some(current.request_id.as_str()),
                "live projection crossed exact request scope"
            );
        }
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
        let lifecycle_state = request_row
            .as_ref()
            .and_then(|row| row.get("lifecycle_state"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let canonical_request: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(request_row.cloned().context("live request row missing")?)
                .context("decoding canonical Codex request")?;
        let canonical_output = gents::session::observe_request_output(
            &ConfigAccess::Local(state.node.clone()),
            &canonical_request,
        )
        .await
        .context("observing canonical Codex output")?;
        let presentation = match &canonical_output {
            gents::session::CanonicalRequestOutput::Live(value)
            | gents::session::CanonicalRequestOutput::Settling(value)
            | gents::session::CanonicalRequestOutput::Published {
                presentation: value,
                ..
            }
            | gents::session::CanonicalRequestOutput::TerminalMessage {
                presentation: value,
                ..
            } => Some(value),
            gents::session::CanonicalRequestOutput::Denied => {
                anyhow::bail!("canonical Codex output is denied")
            }
            gents::session::CanonicalRequestOutput::Conflicted => {
                anyhow::bail!("canonical Codex output is conflicted")
            }
            gents::session::CanonicalRequestOutput::Invalid => {
                anyhow::bail!("canonical Codex output is invalid")
            }
            _ => None,
        };
        // Only translate the owner's presentation for the existing wire delta
        // helpers. Never clone admission input into an output-shaped object.
        let rendered_output = presentation.map(|value| {
            json!({
                "content": value.body_markdown,
                "reasoning": value.reasoning_markdown,
            })
        });
        let response_row = rendered_output.as_ref();
        let response_started_at_ms = if projection.has_response_start() {
            None
        } else {
            canonical_output_started_at_ms(state, &canonical_output).await?
        };
        projection.observe_response_timing(
            response_started_at_ms,
            request_row
                .and_then(|row| nonempty_timestamp_field(row, "terminalized_at"))
                .and_then(timestamp_millis),
        );
        let client_head = project_persisted_attempt(lifecycle_state, false);
        let client_turn_state = client_head.map(|head| head.turn_state);
        let projection_settled = client_turn_state.is_some_and(|state| state.is_terminal());

        let mut marker = progress_marker(request_row, response_row, tool_rows, inference_call_rows);
        marker.selected_source = presentation.and_then(|value| value.selected_source.clone());
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
                send_thread_token_usage_update(outbound, state, projection, &current, usage.totals)
                    .await?;
            }
            known_inference_usage_call_id = Some(usage.call_id);
        }

        if marker_changed && !projection_settled {
            if let Some(value) = presentation {
                if let Some(source) = value.selected_source.as_ref() {
                    let observation = latest_reasoning_cursor.observe(
                        &current.request_id,
                        value.reasoning_markdown.as_deref().unwrap_or(""),
                        source,
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
        }
        // A published provider-turn header closes its selected source before
        // the request settles: complete that reasoning item at publication
        // with the header's durable text. Completion is idempotent per item,
        // so the later terminal pass does not replay it.
        if !projection_settled {
            if let gents::session::CanonicalRequestOutput::Published {
                header,
                presentation,
                ..
            } = &canonical_output
            {
                projection.observe_response_timing(None, timestamp_millis(&header.created_at));
                let item_id = latest_reasoning_cursor.active_item_id(&current.request_id);
                projection
                    .finish_reasoning(
                        outbound,
                        &item_id,
                        presentation.reasoning_markdown.as_deref(),
                    )
                    .await?;
            }
        }

        for row in tool_rows {
            let tool_marker = tool_progress_marker(row);
            let Some(tool_key) = tool_marker.tool_call_key.as_deref() else {
                continue;
            };
            if known_tool_markers.get(tool_key) == Some(&tool_marker) {
                continue;
            }
            let tool = match hydrate_gents_tool_call_progress(
                &ConfigAccess::Local(state.node.clone()),
                row,
                &current.agent_did,
                &current.session_id,
                current.requester_did.as_deref(),
                &current.request_doc_id,
            )
            .await
            {
                Ok(Some(tool)) => tool,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(%error, tool_key, "Codex live tool canonical payload not ready");
                    continue;
                }
            };
            let projection_status = tool_projection_status(&tool);
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
            latest_error_message = request_row
                .and_then(|row| row.get("failure_reason"))
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(ToOwned::to_owned);
        }

        let failure_reason = request_row
            .as_ref()
            .and_then(|row| row.get("failure_reason"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if projection_settled {
            // Terminal content comes only from the shared typed owner: it
            // classifies Loading/Denied/Conflicted/Invalid, distinguishes a
            // terminal NoMessage from missing dependencies, and never promotes
            // a retained partial attempt into the current answer. Request JSON
            // (admission prompt included) is never overlaid as response text.
            let terminal_presentation = match &canonical_output {
                gents::session::CanonicalRequestOutput::TerminalMessage {
                    presentation, ..
                } => Some(presentation),
                gents::session::CanonicalRequestOutput::Loading => {
                    if options.enforce_timeout && last_progress_at.elapsed() >= state.timeout {
                        anyhow::bail!(
                            "timed out waiting for terminal canonical output {} after {}s of inactivity\n{}",
                            current.request_id,
                            state.timeout.as_secs(),
                            request_diagnostic_hint(&current.request_id)
                        );
                    }
                    tokio::time::sleep(state.poll_interval).await;
                    continue;
                }
                gents::session::CanonicalRequestOutput::TerminalNoMessage => {
                    // Terminal without a selected message: no answer to catch
                    // up on; downstream projection surfaces the failure/empty
                    // turn honestly.
                    None
                }
                gents::session::CanonicalRequestOutput::Denied
                | gents::session::CanonicalRequestOutput::Conflicted
                | gents::session::CanonicalRequestOutput::Invalid
                | gents::session::CanonicalRequestOutput::Absent
                | gents::session::CanonicalRequestOutput::Live(_)
                | gents::session::CanonicalRequestOutput::Settling(_)
                | gents::session::CanonicalRequestOutput::Retracted
                | gents::session::CanonicalRequestOutput::RetainedPartial(_)
                | gents::session::CanonicalRequestOutput::Published { .. } => {
                    anyhow::bail!(
                        "settled Codex request has no valid terminal output: {canonical_output:?}"
                    )
                }
            };

            let completed_at_ms = request_row
                .and_then(|row| nonempty_timestamp_field(row, "terminalized_at"))
                .and_then(timestamp_millis);
            projection
                .set_completed_at(completed_at_ms.map(|timestamp| timestamp.div_euclid(1000)));
            projection.observe_response_timing(None, completed_at_ms);

            let durable_reasoning = terminal_presentation
                .as_ref()
                .and_then(|presentation| presentation.reasoning_markdown.as_deref())
                .filter(|text| !text.trim().is_empty());
            let reasoning_item_id = latest_reasoning_cursor
                .active_item_id(&current.request_id)
                .to_string();
            projection
                .finish_reasoning(outbound, &reasoning_item_id, durable_reasoning)
                .await?;

            if let Some(presentation) = terminal_presentation.as_ref() {
                let delta =
                    content_delta(projection.active_agent_text(), &presentation.body_markdown);
                projection.append_agent_delta(outbound, &delta).await?;
            }

            let turn_status = codex_turn_status(
                client_turn_state.expect("settled client turn state must be present"),
            );
            let error_message = if turn_status == codex::TurnStatus::Failed {
                terminal_error_message(
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
                            cancel_pending_steering_request(connection, state, &next_request).await;
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
                        current.request_doc_id.clone(),
                        current.session_id.clone(),
                        projection.thread_id.to_string(),
                        projection.turn_id.to_string(),
                        projection.cwd.clone(),
                        std::mem::take(&mut running_background_tools),
                    );
                    let next_input = steering_input_for_request(
                        connection,
                        state,
                        &next_request.request_id,
                        &next_request.request_doc_id,
                    )
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
                    current.request_doc_id = next_request.request_doc_id;
                    current.request_id = next_request.request_id;
                    turn_request_ids.push(current.request_id.clone());
                    known_tool_calls.clear();
                    known_tool_markers.clear();
                    known_compaction_states.clear();
                    known_inference_usage_call_id = None;
                    latest_content_cursor.reset();
                    latest_reasoning_cursor.reset();
                    projection.reset_response_timing();
                    latest_error_message = None;
                    latest_progress_marker = None;
                    last_progress_at = tokio::time::Instant::now();
                    continue;
                }
            }
            let last_usage =
                submitted_token_usage(state, &current, Some(&turn_request_ids)).await?;
            send_thread_token_usage_update(outbound, state, projection, &current, last_usage)
                .await?;

            projection
                .finish_turn(outbound, turn_status, error_message)
                .await
                .context("sending terminal Codex turn notification")?;
            send_thread_status_changed(
                outbound,
                state,
                projection.thread_id,
                projected_thread_status(client_head),
            )
            .await?;
            spawn_background_tool_watcher(
                connection.clone(),
                state.clone(),
                current.request_doc_id.clone(),
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
                "timed out waiting for canonical output for request {} after {}s of inactivity\n{}",
                current.request_id,
                state.timeout.as_secs(),
                request_diagnostic_hint(&current.request_id)
            );
        }

        tokio::select! {
            _ = tokio::time::sleep(state.poll_interval) => {}
            msg = updates.recv(), if !updates_closed => {
                if msg.is_none() {
                    tracing::warn!("Codex shim embedded-node update subscription closed");
                    updates_closed = true;
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

fn nonempty_timestamp_field<'a>(row: &'a Value, field: &str) -> Option<&'a str> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// When the owner-selected output began: the first segment of the selected
/// live source, or the selected header's creation for a native terminal
/// message. Request admission time is not response start.
async fn canonical_output_started_at_ms(
    state: &ShimState,
    output: &gents::session::CanonicalRequestOutput,
) -> Result<Option<i64>> {
    use gents::session::CanonicalRequestOutput;
    let (presentation, header) = match output {
        CanonicalRequestOutput::Live(presentation)
        | CanonicalRequestOutput::Settling(presentation) => (presentation, None),
        CanonicalRequestOutput::Published {
            header,
            presentation,
            ..
        }
        | CanonicalRequestOutput::TerminalMessage {
            header,
            presentation,
            ..
        } => (presentation, Some(header)),
        _ => return Ok(None),
    };
    let Some(selected) = presentation.selected_source.as_ref() else {
        return Ok(header.and_then(|header| timestamp_millis(&header.created_at)));
    };
    let response = query_node_json(
        state.node.as_ref(),
        &format!(
            r#"{{ AgentOutputSegment(
                filter: {{ request_doc_id: {{ _eq: "{}" }} }},
                order: {{ created_at: ASC }}
            ) {{ source writer created_at }} }}"#,
            gents::graphql::escape_graphql_string(&selected.request_doc_id),
        ),
    )
    .await
    .context("loading selected Codex output segments")?;
    let is_selected = |row: &Value| {
        let source = row
            .get("source")
            .cloned()
            .map(serde_json::from_value::<gents_protocol::output::OutputSource>);
        let writer = row
            .get("writer")
            .cloned()
            .map(serde_json::from_value::<gents_protocol::output::OutputWriter>);
        matches!(source, Some(Ok(source)) if source == selected.source)
            && matches!(writer, Some(Ok(writer)) if writer == selected.writer)
    };
    Ok(response
        .pointer("/data/AgentOutputSegment")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|row| is_selected(row))
        .filter_map(|row| nonempty_timestamp_field(row, "created_at"))
        .filter_map(timestamp_millis)
        .min())
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
        request_lease_expires_at: scalar_marker(request_row, "execution_lease_expires_at"),
        request_failure_reason: scalar_marker(request_row, "failure_reason"),
        response_content_fingerprint: string_fingerprint_marker(response_row, "content"),
        response_reasoning_fingerprint: string_fingerprint_marker(response_row, "reasoning"),
        selected_source: None,
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
                known_tool_calls.insert(
                    id.clone(),
                    ToolProjectionStatus::Mcp(observed_mcp_status(status)),
                );
            }
            codex::ThreadItem::CommandExecution { id, status, .. } => {
                known_tool_calls.insert(
                    id.clone(),
                    ToolProjectionStatus::Command(observed_command_status(status)),
                );
            }
            codex::ThreadItem::FileChange { id, status, .. } => {
                known_tool_calls.insert(
                    id.clone(),
                    ToolProjectionStatus::FileChange(observed_patch_status(status)),
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
        source: &gents::session::CanonicalSelectedSource,
    ) -> ReasoningObservation {
        let changed_source = self
            .selected_source
            .as_ref()
            .is_some_and(|previous| previous != source);
        let completed_item_id = (changed_source && !self.observed_preview.is_empty())
            .then(|| self.active_item_id(request_id));
        if changed_source {
            let had_reasoning = !self.observed_preview.is_empty();
            self.observed_preview.clear();
            self.active_item_id = None;
            if had_reasoning {
                self.segment = self.segment.saturating_add(1);
            }
        }
        self.selected_source = Some(source.clone());

        if current.is_empty() || current == self.observed_preview {
            return ReasoningObservation {
                completed_item_id,
                delta: None,
            };
        }

        let delta = if self.observed_preview.is_empty() {
            current
        } else if let Some(delta) = current.strip_prefix(&self.observed_preview) {
            delta
        } else {
            let overlap = suffix_prefix_overlap(&self.observed_preview, current);
            &current[overlap..]
        };
        self.observed_preview = current.to_string();
        if delta.is_empty() {
            return ReasoningObservation {
                completed_item_id,
                delta: None,
            };
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
        self.selected_source = None;
    }

    fn active_item_id(&self, request_id: &str) -> String {
        self.active_item_id
            .clone()
            .unwrap_or_else(|| reasoning_item_id(request_id, self.segment))
    }

    fn reset(&mut self) {
        self.observed_preview.clear();
        self.active_item_id = None;
        self.selected_source = None;
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
    request: &SubmittedRequest,
    last_usage: super::super::thread_projection::TokenTotals,
) -> Result<()> {
    let total_usage = submitted_token_usage(state, request, None).await?;
    let model_context_window = load_bound_context_window(
        state.node.as_ref(),
        &request.agent_did,
        request
            .behavior_id
            .as_deref()
            .context("committed request missing behavior")?,
    )
    .await?;
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
    running_background_tools: BTreeSet<String>,
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
        submitted.request_doc_id.clone(),
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
    request: &super::active::NextSteeringRequest,
) {
    connection.take_steering_input(&request.request_id).await;
    if let Err(error) = gents::interrupt_request_by_doc_id(
        state.node.as_ref(),
        &request.request_doc_id,
        &state.agent_did,
        Some(state.local_requester_did()),
    )
    .await
    {
        tracing::warn!(
            %error,
            request_id=%request.request_id,
            "Codex shim failed to interrupt timed-out queued steering request"
        );
    }
}

async fn steering_input_for_request(
    connection: &ConnectionState,
    state: &ShimState,
    request_id: &str,
    request_doc_id: &str,
) -> Result<Vec<codex::UserInput>> {
    if let Some(input) = connection.take_steering_input(request_id).await {
        return Ok(input);
    }
    let physical = gents::graphql::escape_graphql_string(request_doc_id);
    let owner = gents::graphql::escape_graphql_string(state.agent_did.as_ref());
    let logical = gents::graphql::escape_graphql_string(request_id);
    let query = format!(
        r#"{{ AgentRequest(filter: {{_docID: {{_eq: "{physical}"}}, agent_did: {{_eq: "{owner}"}}, requester_did: {{_eq: "{owner}"}}, request_id: {{_eq: "{logical}"}}}}, limit: 2) {{content}} }}"#
    );
    let response = query_node_json(state.node.as_ref(), &query).await?;
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .context("steering request query omitted rows")?;
    anyhow::ensure!(
        rows.len() == 1,
        "steering physical request missing or ambiguous"
    );
    let content = rows[0]
        .get("content")
        .and_then(Value::as_str)
        .context("steering request omitted content")?
        .to_string();
    Ok(vec![codex::UserInput::Text {
        text: content,
        text_elements: Vec::new(),
    }])
}

#[cfg(test)]
mod tests {
    use super::{
        content_delta, content_delta_from_cursor, progress_marker, suffix_prefix_overlap,
        ContentCursor, ReasoningCursor, ReasoningObservation,
    };
    use gents_codex_protocol as codex;
    use serde_json::json;

    #[test]
    fn silent_owner_renewal_and_request_failure_change_progress() {
        let initial = json!({
            "lifecycle_state": "processing",
            "execution_lease_expires_at": "2026-09-22T00:00:30Z",
            "failure_reason": null
        });
        let mut renewed = initial.clone();
        renewed["execution_lease_expires_at"] = json!("2026-09-22T00:00:45Z");
        let mut failed = initial.clone();
        failed["failure_reason"] = json!("provider unavailable");
        let marker = progress_marker(Some(&initial), None, &[], &[]);
        assert_ne!(marker, progress_marker(Some(&renewed), None, &[], &[]));
        assert_ne!(marker, progress_marker(Some(&failed), None, &[], &[]));
        assert_eq!(marker, progress_marker(Some(&initial), None, &[], &[]));
    }

    #[test]
    fn live_preview_changes_are_observed_and_terminal_text_catches_up() {
        let request = json!({"lifecycle_state": "processing"});
        let first = json!({"content": "hello", "reasoning": "thinking"});
        let second = json!({"content": "hello!", "reasoning": "thinking"});
        assert_ne!(
            progress_marker(Some(&request), Some(&first), &[], &[]),
            progress_marker(Some(&request), Some(&second), &[], &[]),
        );
        let mut cursor = ContentCursor::default();
        assert_eq!(content_delta_from_cursor(&mut cursor, "hello"), "hello");
        assert_eq!(content_delta_from_cursor(&mut cursor, "hello!"), "!");
        assert_eq!(content_delta("hello!", "hello! world"), " world");
    }

    #[tokio::test]
    async fn reasoning_cursor_oversized_no_overlap_tail_terminally_completes_with_durable_text() {
        use std::sync::{atomic::AtomicU64, Arc};
        use std::time::Duration;

        use tokio::sync::{mpsc, Mutex};

        use super::super::super::turn_projection::TurnProjection;
        use super::super::super::{CodexSidecar, ShimState};

        let temp = tempfile::tempdir().expect("reasoning projection directory");
        let node = Arc::new(
            gents::defra_node::EmbeddedNode::builder()
                .data_path(temp.path().join("node"))
                .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
                .build()
                .await
                .expect("embedded node"),
        );
        let state = ShimState {
            codex_home: temp.path().to_path_buf(),
            trace_path: temp.path().join("reasoning.jsonl"),
            cwd: temp.path().to_path_buf(),
            fs_root: None,
            node,
            background_execution_registry: gents::BackgroundExecutionRegistry::default(),
            graphql: Arc::from("http://127.0.0.1/graphql"),
            agent_did: Arc::from("did:test:reasoning"),
            behavior_id: Arc::from("reasoning"),
            id_counter: Arc::new(AtomicU64::new(1)),
            timeout: Duration::from_secs(5),
            poll_interval: Duration::from_millis(10),
            sidecar: Arc::new(Mutex::new(CodexSidecar::default())),
            auth_token: None,
        };
        let (outbound, mut notifications) = mpsc::unbounded_channel();
        let mut projection =
            TurnProjection::new(&state, "thread", "turn", temp.path().to_path_buf(), None);
        let mut cursor = ReasoningCursor::default();
        let source = reasoning_source(0);
        let first_tail = "a".repeat(gents::MAX_LIVE_REASONING_BYTES);
        let second_tail = "b".repeat(gents::MAX_LIVE_REASONING_BYTES);
        let durable_text = format!("{first_tail} omitted middle {second_tail}");
        assert!(durable_text.len() > gents::MAX_LIVE_REASONING_BYTES);

        let first = cursor
            .observe("request-1", &first_tail, &source)
            .delta
            .expect("first bounded-tail delta");
        projection
            .append_reasoning_delta(&outbound, &first.item_id, &first.text)
            .await
            .expect("project first reasoning delta");

        // A poll gap larger than the bounded preview has no overlap. The
        // source is still the same provider turn, so the latest tail extends
        // its existing item. Terminal materialization replaces that item's
        // incomplete streamed text with the exact durable text.
        let observation = cursor.observe("request-1", &second_tail, &source);
        assert!(observation.completed_item_id.is_none());
        let second = observation.delta.expect("unrecoverable bounded-tail delta");
        assert_eq!(second.item_id, first.item_id);
        assert_eq!(second.text, second_tail);
        projection
            .append_reasoning_delta(&outbound, &second.item_id, &second.text)
            .await
            .expect("project latest reasoning tail");
        projection
            .finish_reasoning(
                &outbound,
                &cursor.active_item_id("request-1"),
                Some(&durable_text),
            )
            .await
            .expect("terminal durable reasoning reconciliation");

        let mut completed = Vec::new();
        while let Ok(payload) = notifications.try_recv() {
            let notification: codex::ServerNotification =
                serde_json::from_str(&payload).expect("Codex notification");
            if let codex::ServerNotification::ItemCompleted(completed_item) = notification {
                let codex::ThreadItem::Reasoning { id, content, .. } = completed_item.item else {
                    panic!("expected reasoning completion");
                };
                completed.push((id, content.concat()));
            }
        }
        assert_eq!(completed, vec![(first.item_id, durable_text)]);
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

    fn reasoning_source(turn_index: u32) -> gents::session::CanonicalSelectedSource {
        gents::session::CanonicalSelectedSource {
            request_doc_id: "request-doc-1".to_string(),
            source: gents_protocol::output::OutputSource::ProviderTurn {
                scope: gents_protocol::rendered_request::CaptureScope {
                    kind: gents_protocol::rendered_request::CaptureScopeKind::Inference,
                    seq: 0,
                },
                turn_index,
                attempt: 0,
            },
            writer: gents_protocol::output::OutputWriter::RequestExecution {
                execution_generation: "generation-1".to_string(),
            },
        }
    }

    #[test]
    fn reasoning_cursor_appends_and_suppresses_same_source_replay() {
        let mut cursor = ReasoningCursor::default();
        let source = reasoning_source(0);
        let first = cursor
            .observe("request-1", "inspect", &source)
            .delta
            .unwrap();
        assert_eq!(first.item_id, "gents-reasoning-request-1");
        assert_eq!(first.text, "inspect");
        let appended = cursor
            .observe("request-1", "inspect then test", &source)
            .delta
            .unwrap();
        assert_eq!(appended.item_id, first.item_id);
        assert_eq!(appended.text, " then test");
        assert_eq!(
            cursor.observe("request-1", "inspect then test", &source),
            ReasoningObservation::default()
        );
    }

    #[test]
    fn reasoning_cursor_identical_next_source_survives_missed_empty_boundary() {
        let mut cursor = ReasoningCursor::default();
        let first_source = reasoning_source(0);
        let next_source = reasoning_source(1);
        cursor
            .observe("request-1", "same thought", &first_source)
            .delta
            .unwrap();
        let next = cursor.observe("request-1", "same thought", &next_source);
        assert_eq!(
            next.completed_item_id.as_deref(),
            Some("gents-reasoning-request-1")
        );
        let delta = next.delta.unwrap();
        assert_eq!(delta.item_id, "gents-reasoning-request-1-segment-1");
        assert_eq!(delta.text, "same thought");
        assert_eq!(
            cursor.observe("request-1", "same thought", &next_source),
            ReasoningObservation::default()
        );
    }

    #[test]
    fn reasoning_cursor_writer_change_is_a_distinct_owner() {
        let mut cursor = ReasoningCursor::default();
        let first_source = reasoning_source(0);
        let mut next_owner = first_source.clone();
        next_owner.writer = gents_protocol::output::OutputWriter::RequestExecution {
            execution_generation: "generation-2".to_string(),
        };
        cursor
            .observe("request-1", "same thought", &first_source)
            .delta
            .unwrap();
        let next = cursor.observe("request-1", "same thought", &next_owner);
        assert_eq!(
            next.completed_item_id.as_deref(),
            Some("gents-reasoning-request-1")
        );
        assert_eq!(next.delta.unwrap().text, "same thought");
    }

    #[test]
    fn reasoning_cursor_live_to_settling_keeps_same_source() {
        let mut cursor = ReasoningCursor::default();
        let source = reasoning_source(0);
        cursor.observe("request-1", "live", &source).delta.unwrap();
        assert_eq!(
            cursor.observe("request-1", "live", &source),
            ReasoningObservation::default()
        );
        let settling = cursor.observe("request-1", "live and settling", &source);
        assert!(settling.completed_item_id.is_none());
        assert_eq!(settling.delta.unwrap().text, " and settling");
    }

    #[test]
    fn reasoning_cursor_empty_source_does_not_consume_item_number() {
        let mut cursor = ReasoningCursor::default();
        assert_eq!(
            cursor.observe("request-1", "", &reasoning_source(0)),
            ReasoningObservation::default()
        );
        let first = cursor
            .observe("request-1", "first actual thought", &reasoning_source(1))
            .delta
            .unwrap();
        assert_eq!(first.item_id, "gents-reasoning-request-1");
    }

    #[test]
    fn reasoning_cursor_recovers_rolled_tail_without_new_source() {
        let mut cursor = ReasoningCursor::default();
        let source = reasoning_source(0);
        cursor
            .observe("request-1", "first middle", &source)
            .delta
            .unwrap();
        let rolled = cursor
            .observe("request-1", "middle last", &source)
            .delta
            .unwrap();
        assert_eq!(rolled.item_id, "gents-reasoning-request-1");
        assert_eq!(rolled.text, " last");
    }

    #[test]
    fn reasoning_cursor_does_not_infer_new_source_from_disjoint_tail() {
        let mut cursor = ReasoningCursor::default();
        let source = reasoning_source(0);
        cursor
            .observe("request-1", "first window", &source)
            .delta
            .unwrap();
        let disjoint = cursor
            .observe("request-1", "later window", &source)
            .delta
            .unwrap();
        assert_eq!(disjoint.item_id, "gents-reasoning-request-1");
        assert_eq!(disjoint.text, "later window");
    }

    #[test]
    fn reasoning_cursor_primes_resume_without_replay() {
        let mut cursor = ReasoningCursor::default();
        let source = reasoning_source(0);
        cursor.prime("gents-reasoning-request-1".to_string(), "already visible");
        assert!(cursor
            .observe("request-1", "already visible", &source)
            .delta
            .is_none());
        let delta = cursor
            .observe("request-1", "already visible plus new", &source)
            .delta
            .unwrap();
        assert_eq!(delta.text, " plus new");
    }

    #[test]
    fn reasoning_cursor_reset_allows_identical_first_source() {
        let mut cursor = ReasoningCursor::default();
        let source = reasoning_source(0);
        cursor
            .observe("request-1", "same thought", &source)
            .delta
            .unwrap();
        cursor.reset();
        assert_eq!(
            cursor
                .observe("request-1", "same thought", &source)
                .delta
                .unwrap()
                .text,
            "same thought"
        );
    }
    #[test]
    fn suffix_prefix_overlap_is_linear_and_utf8_safe() {
        assert_eq!(suffix_prefix_overlap("abc middle", "middle xyz"), 6);
        assert_eq!(suffix_prefix_overlap("reason ☕", "☕ next"), "☕".len());
        assert_eq!(suffix_prefix_overlap("no overlap", "fresh"), 0);
    }
}
