use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use defra_agent::graphql::escape_graphql_string;
use serde_json::Value;
use tokio::sync::watch;

use super::super::store::query_node_json;
use super::super::{ConnectionState, ShimState, TurnStreamControl};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ActiveCodexTurn {
    pub(super) turn_id: String,
    pub(super) current_request_id: String,
}

#[derive(Clone, Debug)]
struct RequestRow {
    request_id: String,
    lifecycle_state: String,
    metadata: Option<String>,
    created_at: String,
}

pub(super) async fn install_stream_control(
    connection: &ConnectionState,
    thread_id: String,
    turn_id: String,
    cancel_tx: watch::Sender<bool>,
) {
    connection.turn_streams.lock().await.insert(
        stream_key(&thread_id, &turn_id),
        TurnStreamControl { cancel_tx },
    );
}

pub(super) async fn clear_stream_control_if_current(
    connection: &ConnectionState,
    thread_id: &str,
    turn_id: &str,
) {
    connection
        .turn_streams
        .lock()
        .await
        .remove(&stream_key(thread_id, turn_id));
}

pub(super) fn cancel_abandoned_steering_request(state: &ShimState, request_id: String) {
    let node = state.node.clone();
    tokio::spawn(async move {
        if let Err(error) = defra_agent::interrupt_request(node.as_ref(), &request_id).await {
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
) -> Result<Option<String>> {
    let rows = load_thread_request_rows(state, thread_id).await?;
    Ok(rows
        .iter()
        .filter(|row| row.lifecycle_state_is_active())
        .filter(|row| steering_parent_id(row).as_deref() == Some(queued_after_request_id))
        .min_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.request_id.cmp(&right.request_id))
        })
        .map(|row| row.request_id.clone()))
}

pub(in crate::commands::codex_shim) async fn interrupt_active_turn(
    connection: &ConnectionState,
    state: &ShimState,
    thread_id: &str,
    turn_id: &str,
) -> Result<()> {
    let Some(active) = load_active_codex_turn(state, thread_id).await? else {
        cancel_stream_control(connection, thread_id, turn_id).await;
        return Ok(());
    };

    if active.turn_id != turn_id {
        tracing::warn!(
            active_turn_id = %active.turn_id,
            requested_turn_id = %turn_id,
            thread_id,
            "Codex shim interrupt turn id did not match active DEFRA turn; interrupting active thread turn"
        );
    }

    cancel_stream_control(connection, thread_id, &active.turn_id).await;
    let node = state.node.clone();
    let request_id = active.current_request_id.clone();
    tokio::spawn(async move {
        if let Err(error) = defra_agent::interrupt_request(node.as_ref(), &request_id).await {
            tracing::warn!(%error, request_id, "Codex shim failed to forward DEFRA interrupt");
        }
    });
    Ok(())
}

async fn cancel_stream_control(connection: &ConnectionState, thread_id: &str, turn_id: &str) {
    if let Some(control) = connection
        .turn_streams
        .lock()
        .await
        .remove(&stream_key(thread_id, turn_id))
    {
        let _ = control.cancel_tx.send(true);
    }
}

fn stream_key(thread_id: &str, turn_id: &str) -> String {
    format!("{thread_id}:{turn_id}")
}

async fn load_thread_request_rows(state: &ShimState, thread_id: &str) -> Result<Vec<RequestRow>> {
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
                metadata
                created_at
            }}
        }}"#
    );
    let response = query_node_json(state.node.as_ref(), &query).await?;
    response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(decode_request_row)
        .collect()
}

fn decode_request_row(row: Value) -> Result<RequestRow> {
    let request_id = row
        .get("request_id")
        .and_then(Value::as_str)
        .context("AgentRequest row missing request_id")?
        .to_string();
    let lifecycle_state = row
        .get("lifecycle_state")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let metadata = row
        .get("metadata")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let created_at = row
        .get("created_at")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok(RequestRow {
        request_id,
        lifecycle_state,
        metadata,
        created_at,
    })
}

fn active_codex_turn_from_rows(
    rows: &[RequestRow],
    expected_turn_id: Option<&str>,
) -> Result<Option<ActiveCodexTurn>> {
    let by_id = rows
        .iter()
        .map(|row| (row.request_id.as_str(), row))
        .collect::<BTreeMap<_, _>>();
    let active_rows = rows
        .iter()
        .filter(|row| row.lifecycle_state_is_active())
        .collect::<Vec<_>>();

    let mut candidates = Vec::<(&RequestRow, String, usize)>::new();
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
    Ok(Some(ActiveCodexTurn {
        turn_id: root,
        current_request_id: tail.request_id.clone(),
    }))
}

fn codex_turn_root_and_depth<'a>(
    row: &'a RequestRow,
    by_id: &BTreeMap<&'a str, &'a RequestRow>,
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

fn steering_parent_id(row: &RequestRow) -> Option<String> {
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

impl RequestRow {
    fn lifecycle_state_is_active(&self) -> bool {
        matches!(
            self.lifecycle_state.as_str(),
            "pending" | "claimed" | "processing" | "inputRequired"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(request_id: &str, lifecycle_state: &str, queued_after: Option<&str>) -> RequestRow {
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
        RequestRow {
            request_id: request_id.to_string(),
            lifecycle_state: lifecycle_state.to_string(),
            metadata,
            created_at: request_id.to_string(),
        }
    }

    #[test]
    fn active_codex_turn_projects_deepest_defra_steering_tail() {
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
        assert_eq!(active.current_request_id, "steer-1");
    }
}
