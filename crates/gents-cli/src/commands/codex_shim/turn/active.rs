use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use gents::graphql::escape_graphql_string;
use gents_protocol::client_protocol::{project_persisted_attempt, RequestLifecycleState};
use gents_protocol::row::AgentRequestRow;
use serde_json::{json, Value};
use tokio::sync::watch;

use super::super::store::query_node_json;
use super::super::{trace, ConnectionState, ShimState, TurnStreamControl};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ActiveCodexTurn {
    pub(super) turn_id: String,
    pub(super) interrupt_request_id: String,
    pub(super) current_request_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NextSteeringRequest {
    pub(super) request_id: String,
    pub(super) created_at: String,
    lifecycle_state: Option<RequestLifecycleState>,
}

impl NextSteeringRequest {
    pub(super) fn is_pending(&self) -> bool {
        self.lifecycle_state == Some(RequestLifecycleState::Pending)
    }
}

#[derive(Clone, Debug)]
struct RequestWithResponseStatus {
    request: AgentRequestRow,
    response_status: Option<String>,
}

impl std::ops::Deref for RequestWithResponseStatus {
    type Target = AgentRequestRow;

    fn deref(&self) -> &Self::Target {
        &self.request
    }
}

pub(in crate::commands::codex_shim) async fn install_stream_control(
    connection: &ConnectionState,
    thread_id: String,
    turn_id: String,
    owner_id: Option<&str>,
    cancel_tx: watch::Sender<bool>,
) -> TurnStreamRegistration {
    let stream_id = uuid::Uuid::new_v4().to_string();
    connection.turn_streams.lock().await.insert(
        stream_key(&thread_id, &turn_id),
        TurnStreamControl {
            stream_id: stream_id.clone(),
            owner_id: owner_id.map(ToOwned::to_owned),
            cancel_tx,
        },
    );
    TurnStreamRegistration {
        connection: connection.clone(),
        thread_id,
        turn_id,
        stream_id,
        armed: true,
    }
}

async fn clear_stream_control_if_current(
    connection: &ConnectionState,
    thread_id: &str,
    turn_id: &str,
    stream_id: &str,
) {
    let key = stream_key(thread_id, turn_id);
    let mut streams = connection.turn_streams.lock().await;
    if streams
        .get(&key)
        .is_some_and(|control| control.stream_id == stream_id)
    {
        streams.remove(&key);
    }
}

/// Owns one generation of a turn-stream control. Tokio abort drops the
/// streaming future, so Drop must arrange cleanup as well as the normal path.
/// The generation id prevents an old task from erasing its replacement.
pub(in crate::commands::codex_shim) struct TurnStreamRegistration {
    connection: ConnectionState,
    thread_id: String,
    turn_id: String,
    stream_id: String,
    armed: bool,
}

impl TurnStreamRegistration {
    pub(in crate::commands::codex_shim) async fn clear(mut self) {
        clear_stream_control_if_current(
            &self.connection,
            &self.thread_id,
            &self.turn_id,
            &self.stream_id,
        )
        .await;
        self.armed = false;
    }
}

impl Drop for TurnStreamRegistration {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let connection = self.connection.clone();
        let thread_id = self.thread_id.clone();
        let turn_id = self.turn_id.clone();
        let stream_id = self.stream_id.clone();
        if let Ok(mut streams) = connection.turn_streams.try_lock() {
            let key = stream_key(&thread_id, &turn_id);
            if streams
                .get(&key)
                .is_some_and(|control| control.stream_id == stream_id)
            {
                streams.remove(&key);
            }
            return;
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                clear_stream_control_if_current(&connection, &thread_id, &turn_id, &stream_id)
                    .await;
            });
        }
    }
}

pub(super) fn cancel_abandoned_steering_request(state: &ShimState, request_id: String) {
    let node = state.node.clone();
    tokio::spawn(async move {
        if let Err(error) = gents::interrupt_request(node.as_ref(), &request_id).await {
            tracing::warn!(
                %error,
                request_id,
                "Codex shim failed to interrupt abandoned steering request"
            );
        }
    });
}

pub(super) async fn load_active_codex_turn(
    state: &ShimState,
    thread_id: &str,
) -> Result<Option<ActiveCodexTurn>> {
    let rows = load_thread_request_rows(state, thread_id).await?;
    active_codex_turn_from_rows(&rows, None)
}

pub(super) async fn next_steering_request_after(
    state: &ShimState,
    thread_id: &str,
    queued_after_request_id: &str,
) -> Result<Option<NextSteeringRequest>> {
    let rows = load_thread_request_rows(state, thread_id).await?;
    Ok(next_steering_request_after_from_rows(
        &rows,
        queued_after_request_id,
    ))
}

pub(in crate::commands::codex_shim) async fn codex_turn_id_for_request(
    state: &ShimState,
    thread_id: &str,
    request_id: &str,
) -> Result<String> {
    let rows = load_thread_request_rows(state, thread_id).await?;
    let by_id = rows
        .iter()
        .map(|row| (row.request_id.as_str(), row))
        .collect::<BTreeMap<_, _>>();
    let Some(request) = by_id.get(request_id).copied() else {
        return Ok(request_id.to_string());
    };
    codex_turn_root_and_depth(request, &by_id).map(|(root_id, _)| root_id)
}

fn next_steering_request_after_from_rows(
    rows: &[RequestWithResponseStatus],
    queued_after_request_id: &str,
) -> Option<NextSteeringRequest> {
    rows.iter()
        .filter(|row| steering_parent_id(row).as_deref() == Some(queued_after_request_id))
        .min_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.request_id.cmp(&right.request_id))
        })
        .map(|row| NextSteeringRequest {
            request_id: row.request_id.clone(),
            created_at: row
                .created_at
                .clone()
                .expect("request row decoder requires created_at"),
            lifecycle_state: row.lifecycle_state.clone(),
        })
}

pub(super) async fn steering_request_ids_for_turn_interrupt_cleanup(
    state: &ShimState,
    thread_id: &str,
    turn_id: &str,
    interrupt_request_id: &str,
) -> Result<Vec<String>> {
    let rows = load_thread_request_rows(state, thread_id).await?;
    let by_id = rows
        .iter()
        .map(|row| (row.request_id.as_str(), row))
        .collect::<BTreeMap<_, _>>();
    let mut request_ids = Vec::new();
    for row in rows.iter().filter(|row| {
        row.request_id != interrupt_request_id
            && row.is_effectively_active()
            && steering_parent_id(row).is_some()
    }) {
        let (root, _) = codex_turn_root_and_depth(row, &by_id)?;
        if root == turn_id {
            request_ids.push(row.request_id.clone());
        }
    }
    Ok(request_ids)
}

pub(in crate::commands::codex_shim) async fn interrupt_active_turn(
    connection: &ConnectionState,
    state: &ShimState,
    thread_id: &str,
    turn_id: &str,
) -> Result<()> {
    let Some(active) = load_active_codex_turn(state, thread_id).await? else {
        let stream_control_cancelled = cancel_stream_control(connection, thread_id, turn_id).await;
        trace::shim_event_fields(
            &state.trace_path,
            "turn_interrupt_no_active_turn",
            json!({
                "thread_id": thread_id,
                "requested_turn_id": turn_id,
                "stream_control_cancelled": stream_control_cancelled,
            }),
        );
        return Ok(());
    };

    if active.turn_id != turn_id {
        trace::shim_event_fields(
            &state.trace_path,
            "turn_interrupt_turn_id_mismatch",
            json!({
                "thread_id": thread_id,
                "requested_turn_id": turn_id,
                "active_turn_id": active.turn_id,
            }),
        );
        tracing::warn!(
            active_turn_id = %active.turn_id,
            requested_turn_id = %turn_id,
            thread_id,
            "Codex shim interrupt turn id did not match active GENTS turn; interrupting active thread turn"
        );
    }

    let stream_control_cancelled =
        cancel_stream_control(connection, thread_id, &active.turn_id).await;
    trace::shim_event_fields(
        &state.trace_path,
        "turn_interrupt_active_selected",
        json!({
            "thread_id": thread_id,
            "requested_turn_id": turn_id,
            "active_turn_id": active.turn_id,
            "interrupt_request_id": active.interrupt_request_id,
            "current_request_id": active.current_request_id,
            "stream_control_cancelled": stream_control_cancelled,
        }),
    );
    let request_id = active.interrupt_request_id.clone();
    if let Err(error) = gents::interrupt_request(state.node.as_ref(), &request_id).await {
        trace::shim_event_fields(
            &state.trace_path,
            "turn_interrupt_latch_failed",
            json!({
                "thread_id": thread_id,
                "turn_id": active.turn_id,
                "request_id": request_id,
                "error": error.to_string(),
            }),
        );
        tracing::warn!(%error, request_id, "Codex shim failed to forward GENTS interrupt");
    } else {
        trace::shim_event_fields(
            &state.trace_path,
            "turn_interrupt_latch_succeeded",
            json!({
                "thread_id": thread_id,
                "turn_id": active.turn_id,
                "request_id": request_id,
            }),
        );
    }
    let cleanup_request_ids = steering_request_ids_for_turn_interrupt_cleanup(
        state,
        thread_id,
        &active.turn_id,
        &active.interrupt_request_id,
    )
    .await?;
    trace::shim_event_fields(
        &state.trace_path,
        "turn_interrupt_cleanup_candidates",
        json!({
            "thread_id": thread_id,
            "turn_id": active.turn_id,
            "request_ids": cleanup_request_ids,
        }),
    );
    for request_id in cleanup_request_ids {
        connection.take_steering_input(&request_id).await;
        if let Err(error) = gents::interrupt_request(state.node.as_ref(), &request_id).await {
            trace::shim_event_fields(
                &state.trace_path,
                "turn_interrupt_cleanup_latch_failed",
                json!({
                    "thread_id": thread_id,
                    "turn_id": active.turn_id,
                    "request_id": request_id,
                    "error": error.to_string(),
                }),
            );
            tracing::warn!(
                %error,
                request_id,
                "Codex shim failed to cancel queued GENTS steering request after interrupt"
            );
        } else {
            trace::shim_event_fields(
                &state.trace_path,
                "turn_interrupt_cleanup_latch_succeeded",
                json!({
                    "thread_id": thread_id,
                    "turn_id": active.turn_id,
                    "request_id": request_id,
                }),
            );
        }
    }
    Ok(())
}

async fn cancel_stream_control(
    connection: &ConnectionState,
    thread_id: &str,
    turn_id: &str,
) -> bool {
    if let Some(control) = connection
        .turn_streams
        .lock()
        .await
        .remove(&stream_key(thread_id, turn_id))
    {
        let _ = control.cancel_tx.send(true);
        true
    } else {
        false
    }
}

fn stream_key(thread_id: &str, turn_id: &str) -> String {
    format!("{thread_id}:{turn_id}")
}

async fn load_thread_request_rows(
    state: &ShimState,
    thread_id: &str,
) -> Result<Vec<RequestWithResponseStatus>> {
    let thread_id = escape_graphql_string(thread_id);
    let agent_did = escape_graphql_string(state.agent_did.as_ref());
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{thread_id}" }},
                    agent_did: {{ _eq: "{agent_did}" }}
                }},
                order: [{{ created_at: ASC }}, {{ request_id: ASC }}]
            ) {{
                request_id
                lifecycle_state
                superseded_by_request
                metadata
                created_at
            }}
            AgentResponse(
                filter: {{
                    session_id: {{ _eq: "{thread_id}" }},
                    agent_did: {{ _eq: "{agent_did}" }}
                }},
                order: {{ created_at: ASC }}
            ) {{
                request_id
                status
                created_at
            }}
        }}"#
    );
    let response = query_node_json(state.node.as_ref(), &query).await?;
    let mut rows = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(decode_request_row)
        .collect::<Result<Vec<_>>>()?;
    let latest_response_status = response
        .pointer("/data/AgentResponse")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .fold(
            BTreeMap::<String, (String, String)>::new(),
            |mut latest, row| {
                let Some(request_id) = row.get("request_id").and_then(Value::as_str) else {
                    return latest;
                };
                let Some(status) = row.get("status").and_then(Value::as_str) else {
                    return latest;
                };
                let created_at = row
                    .get("created_at")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let replace = latest
                    .get(request_id)
                    .is_none_or(|(_, previous_created_at)| created_at > *previous_created_at);
                if replace {
                    latest.insert(request_id.to_string(), (status.to_string(), created_at));
                }
                latest
            },
        );
    for row in &mut rows {
        row.response_status = latest_response_status
            .get(&row.request_id)
            .map(|(status, _)| status.clone());
    }
    Ok(rows)
}

fn decode_request_row(row: Value) -> Result<RequestWithResponseStatus> {
    let request: AgentRequestRow =
        serde_json::from_value(row).context("decoding canonical AgentRequest row")?;
    request
        .created_at
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .context("AgentRequest row missing created_at")?;
    Ok(RequestWithResponseStatus {
        request,
        response_status: None,
    })
}

fn active_codex_turn_from_rows(
    rows: &[RequestWithResponseStatus],
    expected_turn_id: Option<&str>,
) -> Result<Option<ActiveCodexTurn>> {
    let by_id = rows
        .iter()
        .map(|row| (row.request_id.as_str(), row))
        .collect::<BTreeMap<_, _>>();
    let active_rows = rows
        .iter()
        .filter(|row| row.is_effectively_active())
        .collect::<Vec<_>>();

    let mut candidates = Vec::<(&RequestWithResponseStatus, String, usize)>::new();
    for row in active_rows {
        let (root, depth) = codex_turn_root_and_depth(row, &by_id)?;
        if expected_turn_id.is_none_or(|expected| root == expected) {
            candidates.push((row, root, depth));
        }
    }
    candidates.sort_by(|(left, _, left_depth), (right, _, right_depth)| {
        left_depth
            .cmp(right_depth)
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.request_id.cmp(&right.request_id))
    });

    let Some((tail, root, _)) = candidates.pop() else {
        return Ok(None);
    };
    let interrupt_request_id = candidates
        .iter()
        .filter(|(_, candidate_root, _)| candidate_root == &root)
        .min_by(|(left, _, left_depth), (right, _, right_depth)| {
            left_depth
                .cmp(right_depth)
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.request_id.cmp(&right.request_id))
        })
        .map(|(row, _, _)| row.request_id.clone())
        .unwrap_or_else(|| tail.request_id.clone());
    Ok(Some(ActiveCodexTurn {
        turn_id: root,
        interrupt_request_id,
        current_request_id: tail.request_id.clone(),
    }))
}

fn codex_turn_root_and_depth<'a>(
    row: &'a RequestWithResponseStatus,
    by_id: &BTreeMap<&'a str, &'a RequestWithResponseStatus>,
) -> Result<(String, usize)> {
    let mut current = row;
    let mut seen = BTreeSet::<String>::new();
    let mut depth = 0usize;
    loop {
        if !seen.insert(current.request_id.clone()) {
            anyhow::bail!(
                "cycle in Codex steering queue ancestry at request {}",
                current.request_id
            );
        }

        let Some(parent_id) = steering_parent_id(current) else {
            return Ok((current.request_id.clone(), depth));
        };
        let Some(parent) = by_id.get(parent_id.as_str()).copied() else {
            return Ok((parent_id, depth + 1));
        };
        current = parent;
        depth += 1;
    }
}

fn steering_parent_id(row: &RequestWithResponseStatus) -> Option<String> {
    let metadata = row.metadata.as_deref()?.trim();
    if metadata.is_empty() {
        return None;
    }
    let value = serde_json::from_str::<Value>(metadata).ok()?;
    let queue = value.get("queue")?;
    let source = queue.get("source").and_then(Value::as_str)?;
    if source != "steering" {
        return None;
    }
    queue
        .get("queued_after_request_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|request_id| !request_id.is_empty())
        .map(ToOwned::to_owned)
}

impl RequestWithResponseStatus {
    fn is_effectively_active(&self) -> bool {
        project_persisted_attempt(
            self.lifecycle_state.map(|s| s.as_str()).unwrap_or(""),
            self.superseded_by_request.is_some(),
            self.response_status.as_deref(),
        )
        .is_some_and(|head| head.is_active())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        request_id: &str,
        lifecycle_state: &str,
        queued_after: Option<&str>,
    ) -> RequestWithResponseStatus {
        let metadata = queued_after.map(|parent| {
            serde_json::json!({
                "queue": {
                    "source": "steering",
                    "policy": "append",
                    "key": null,
                    "queued_after_request_id": parent
                }
            })
            .to_string()
        });
        RequestWithResponseStatus {
            request: serde_json::from_value(json!({
                "request_id": request_id,
                "lifecycle_state": lifecycle_state,
                "metadata": metadata,
                "created_at": request_id,
            }))
            .expect("canonical AgentRequest test row"),
            response_status: None,
        }
    }

    #[test]
    fn active_codex_turn_projects_deepest_gents_steering_tail() {
        let rows = vec![
            row("turn-1", "processing", None),
            row("steer-1", "pending", Some("turn-1")),
            row("steer-2", "pending", Some("steer-1")),
        ];

        let active = active_codex_turn_from_rows(&rows, Some("turn-1"))
            .unwrap()
            .unwrap();

        assert_eq!(
            active,
            ActiveCodexTurn {
                turn_id: "turn-1".to_string(),
                interrupt_request_id: "turn-1".to_string(),
                current_request_id: "steer-2".to_string(),
            }
        );
    }

    #[test]
    fn terminal_root_with_active_steering_still_projects_original_turn() {
        let rows = vec![
            row("turn-1", "completed", None),
            row("steer-1", "processing", Some("turn-1")),
        ];

        let active = active_codex_turn_from_rows(&rows, None).unwrap().unwrap();

        assert_eq!(active.turn_id, "turn-1");
        assert_eq!(active.interrupt_request_id, "steer-1");
        assert_eq!(active.current_request_id, "steer-1");
    }

    #[test]
    fn terminal_response_excludes_stale_processing_request_from_active_turn() {
        let mut completed = row("turn-1", "processing", None);
        completed.response_status = Some("complete".to_string());
        assert_eq!(
            active_codex_turn_from_rows(&[completed], None).unwrap(),
            None
        );

        let mut failed = row("turn-2", "processing", None);
        failed.response_status = Some("error".to_string());
        assert_eq!(active_codex_turn_from_rows(&[failed], None).unwrap(), None);
    }

    #[test]
    fn pending_steering_tail_does_not_become_interrupt_target() {
        let rows = vec![
            row("turn-1", "processing", None),
            row("steer-1", "pending", Some("turn-1")),
        ];

        let active = active_codex_turn_from_rows(&rows, None).unwrap().unwrap();

        assert_eq!(active.turn_id, "turn-1");
        assert_eq!(active.interrupt_request_id, "turn-1");
        assert_eq!(active.current_request_id, "steer-1");
    }

    #[test]
    fn next_steering_request_includes_already_completed_child() {
        let rows = vec![
            row("turn-1", "completed", None),
            row("steer-1", "completed", Some("turn-1")),
        ];

        let next = next_steering_request_after_from_rows(&rows, "turn-1").unwrap();

        assert_eq!(next.request_id, "steer-1");
        assert_eq!(next.lifecycle_state, Some(RequestLifecycleState::Completed));
        assert!(!next.is_pending());
    }

    #[test]
    fn next_steering_request_preserves_queue_order_across_terminal_children() {
        let rows = vec![
            row("turn-1", "completed", None),
            row("steer-2", "completed", Some("turn-1")),
            row("steer-1", "failed", Some("turn-1")),
        ];

        let next = next_steering_request_after_from_rows(&rows, "turn-1").unwrap();

        assert_eq!(next.request_id, "steer-1");
        assert_eq!(next.lifecycle_state, Some(RequestLifecycleState::Failed));
    }
}
