use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use gents::drop_cleanup::DropCleanup;
use gents_protocol::client_protocol::{project_persisted_attempt, RequestLifecycleState};
use gents_protocol::row::AgentRequestRow;
use serde_json::{json, Value};
use tokio::sync::watch;

use super::super::caused_threads::CausedThread;
use super::super::CodexShimError;
use super::super::{trace, ConnectionState, ShimState, TurnStreamControl};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ActiveCodexTurn {
    pub(super) turn_id: String,
    pub(super) interrupt_request_id: String,
    pub(super) current_request_id: String,
    pub(super) current_request_doc_id: String,
    pub(super) interrupt_request_doc_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NextSteeringRequest {
    pub(super) request_id: String,
    pub(super) request_doc_id: String,
    pub(super) created_at: String,
    lifecycle_state: Option<RequestLifecycleState>,
}

impl NextSteeringRequest {
    pub(super) fn is_pending(&self) -> bool {
        self.lifecycle_state == Some(RequestLifecycleState::Pending)
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
        runtime: DropCleanup::current(),
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

/// Drop-cleanup and generation id must hold: Tokio abort drops the streaming
/// future, and the generation id stops an old task from erasing its replacement.
pub(in crate::commands::codex_shim) struct TurnStreamRegistration {
    connection: ConnectionState,
    thread_id: String,
    turn_id: String,
    stream_id: String,
    runtime: DropCleanup,
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
        self.runtime.spawn(async move {
            clear_stream_control_if_current(&connection, &thread_id, &turn_id, &stream_id).await;
        });
    }
}

pub(super) fn cancel_abandoned_steering_request(
    state: &ShimState,
    request: &gents::EnqueuedAgentRequest,
) {
    let node = state.node.clone();
    let request = request.clone();
    let node_did = state.node_did.clone();
    tokio::spawn(async move {
        if let Err(error) = gents::interrupt_request_by_doc_id(
            &node,
            &request.doc_id,
            node_did.as_ref(),
            Some(node_did.as_ref()),
        )
        .await
        {
            tracing::warn!(%error,request_id=%request.request_id,"Codex shim failed to interrupt abandoned steering request");
        }
    });
}

pub(super) async fn load_active_codex_turn(
    state: &ShimState,
    thread_id: &str,
) -> Result<Option<ActiveCodexTurn>> {
    let rows = load_thread_request_rows(
        state,
        &state.node_did,
        Some(state.local_requester_did()),
        thread_id,
    )
    .await?;
    active_codex_turn_from_rows(&rows, None)
}

pub(super) async fn next_steering_request_after(
    state: &ShimState,
    thread_id: &str,
    queued_after_request_id: &str,
) -> Result<Option<NextSteeringRequest>> {
    let rows = load_thread_request_rows(
        state,
        &state.node_did,
        Some(state.local_requester_did()),
        thread_id,
    )
    .await?;
    let candidates = steering_successor_candidates(&rows, queued_after_request_id);
    if candidates.len() < 2 {
        return Ok(candidates.first().map(|row| next_steering_request(row)));
    }
    let ids = candidates
        .iter()
        .map(|row| row.doc_id.clone().expect("decoded physical identity"))
        .collect::<Vec<_>>();
    let order = gents::config_client::ConfigAccess::transact_local_write(
        &state.node,
        None,
        "codex.active.steering_order",
        |txn| {
            let ids = &ids;
            Box::pin(async move { gents::lifecycle::request_arrival_order(txn, ids).await })
        },
    )
    .await?;
    Ok(order.first().and_then(|first| {
        candidates
            .iter()
            .find(|row| row.doc_id.as_deref() == Some(first.as_str()))
            .map(|row| next_steering_request(row))
    }))
}

fn steering_successor_candidates<'a>(
    rows: &'a [AgentRequestRow],
    queued_after_request_id: &str,
) -> Vec<&'a AgentRequestRow> {
    let mut transparent_parents = BTreeSet::from([queued_after_request_id]);
    loop {
        let added = rows
            .iter()
            .filter(|row| steering_request_is_transparent(row))
            .filter(|row| {
                steering_parent_id(row)
                    .as_deref()
                    .is_some_and(|parent| transparent_parents.contains(parent))
            })
            .map(|row| row.request_id.as_str())
            .filter(|id| !transparent_parents.contains(id))
            .collect::<Vec<_>>();
        if added.is_empty() {
            break;
        }
        transparent_parents.extend(added);
    }
    rows.iter()
        .filter(|row| !steering_request_is_transparent(row))
        .filter(|row| {
            steering_parent_id(row)
                .as_deref()
                .is_some_and(|parent| transparent_parents.contains(parent))
        })
        .collect()
}

fn next_steering_request(row: &AgentRequestRow) -> NextSteeringRequest {
    NextSteeringRequest {
        request_id: row.request_id.clone(),
        request_doc_id: row.doc_id.clone().expect("validated physical request"),
        created_at: row
            .created_at
            .clone()
            .expect("request row decoder requires created_at"),
        lifecycle_state: row.lifecycle_state,
    }
}

fn steering_request_is_transparent(row: &AgentRequestRow) -> bool {
    gents::lifecycle::folded_into(row).is_some() || gents::lifecycle::replaced_by(row).is_some()
}

pub(super) async fn steering_request_ids_for_turn_interrupt_cleanup(
    state: &ShimState,
    thread_id: &str,
    turn_id: &str,
    interrupt_request_id: &str,
) -> Result<Vec<(String, String)>> {
    let rows = load_thread_request_rows(
        state,
        &state.node_did,
        Some(state.local_requester_did()),
        thread_id,
    )
    .await?;
    let by_id = rows
        .iter()
        .map(|row| (row.request_id.as_str(), row))
        .collect::<BTreeMap<_, _>>();
    let mut request_ids = Vec::new();
    for row in rows.iter().filter(|row| {
        row.request_id != interrupt_request_id
            && row_is_effectively_active(row)
            && steering_parent_id(row).is_some()
    }) {
        let (root, _) = codex_turn_root_and_depth(row, &by_id)?;
        if root == turn_id {
            request_ids.push((
                row.request_id.clone(),
                row.doc_id
                    .clone()
                    .ok_or(CodexShimError::SteeringRequestMissingPhysicalIdentity)?,
            ));
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
    if let Err(error) = gents::interrupt_request_by_doc_id(
        &state.node,
        &active.interrupt_request_doc_id,
        &state.node_did,
        Some(state.local_requester_did()),
    )
    .await
    {
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
    for (request_id, request_doc_id) in cleanup_request_ids {
        connection.take_steering_input(&request_id).await;
        if let Err(error) = gents::interrupt_request_by_doc_id(
            &state.node,
            &request_doc_id,
            &state.node_did,
            Some(state.local_requester_did()),
        )
        .await
        {
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

pub(in crate::commands::codex_shim) async fn interrupt_caused_thread_turn(
    connection: &ConnectionState,
    state: &ShimState,
    thread: &CausedThread,
    turn_id: &str,
) -> Result<()> {
    cancel_stream_control(connection, &thread.session_id, turn_id).await;
    let interrupted =
        gents::session_message::interrupt_session(&state.node, &thread.scope()).await?;
    trace::shim_event_fields(
        &state.trace_path,
        "turn_interrupt_caused_thread",
        json!({
            "thread_id": thread.session_id,
            "requested_turn_id": turn_id,
            "interrupted_request_id": interrupted,
        }),
    );
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
    node_did: &str,
    requester_did: Option<&str>,
    thread_id: &str,
) -> Result<Vec<AgentRequestRow>> {
    let scope = gents::session::public_request_filter(&gents::session::session_scope_filter(
        node_did,
        thread_id,
        requester_did,
    ));
    gents::config_client::ConfigAccess::transact_local_write(&state.node,None,"codex.active.rows",|txn| {
        let scope=&scope;
        Box::pin(async move {
            let response=txn.execute(&format!(r#"{{
                AgentRequest(filter:{{{scope}}},order:[{{created_at:ASC}},{{request_id:ASC}}]){{
                    _docID request_id node_did requester_did session_id agent_id lifecycle_state superseded_by_request failure_reason input created_at
                }}
            }}"#)).await?;
            let values=response.pointer("/data/AgentRequest").and_then(Value::as_array).ok_or(CodexShimError::ActiveRequestRowsOmitted)?;
            let rows=values.iter().cloned().map(decode_request_row).collect::<Result<Vec<_>>>()?;
            let mut labels=BTreeSet::new();
            for row in &rows {
                if !labels.insert(row.request_id.clone()) {
                    return Err(CodexShimError::AmbiguousScopedActiveRequestLabel.into());
                }
            }
            Ok(rows)
        })
    }).await
}

fn decode_request_row(row: Value) -> Result<AgentRequestRow> {
    let request: AgentRequestRow =
        serde_json::from_value(row).map_err(CodexShimError::DecodeAgentRequestRow)?;
    request
        .doc_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .ok_or(CodexShimError::AgentRequestMissingPhysicalIdentity)?;
    request
        .created_at
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or(CodexShimError::AgentRequestMissingCreatedAt)?;
    Ok(request)
}

fn active_codex_turn_from_rows(
    rows: &[AgentRequestRow],
    expected_turn_id: Option<&str>,
) -> Result<Option<ActiveCodexTurn>> {
    let by_id = rows
        .iter()
        .map(|row| (row.request_id.as_str(), row))
        .collect::<BTreeMap<_, _>>();
    let active_rows = rows
        .iter()
        .filter(|row| row_is_effectively_active(row))
        .collect::<Vec<_>>();

    let mut candidates = Vec::<(&AgentRequestRow, String, usize)>::new();
    for row in active_rows {
        if steering_lineage_has_superseded_request(row, &by_id)? {
            continue;
        }
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
    let interrupt_request_doc_id = by_id
        .get(interrupt_request_id.as_str())
        .and_then(|row| row.doc_id.clone())
        .ok_or(CodexShimError::InterruptRequestMissingPhysicalIdentity)?;
    Ok(Some(ActiveCodexTurn {
        turn_id: root,
        interrupt_request_id,
        current_request_id: tail.request_id.clone(),
        current_request_doc_id: tail
            .doc_id
            .clone()
            .ok_or(CodexShimError::ActiveRequestMissingPhysicalIdentity)?,
        interrupt_request_doc_id,
    }))
}

fn steering_lineage_has_superseded_request<'a>(
    row: &'a AgentRequestRow,
    by_id: &BTreeMap<&'a str, &'a AgentRequestRow>,
) -> Result<bool> {
    let mut current = row;
    let mut seen = BTreeSet::<String>::new();
    loop {
        if !seen.insert(current.request_id.clone()) {
            return Err(CodexShimError::SteeringQueueCycle {
                request_id: current.request_id.clone(),
            }
            .into());
        }
        if current
            .superseded_by_request
            .as_deref()
            .is_some_and(|id| !id.trim().is_empty())
        {
            return Ok(true);
        }
        let Some(parent_id) = steering_parent_id(current) else {
            return Ok(false);
        };
        let Some(parent) = by_id.get(parent_id.as_str()).copied() else {
            return Ok(false);
        };
        current = parent;
    }
}

fn codex_turn_root_and_depth<'a>(
    row: &'a AgentRequestRow,
    by_id: &BTreeMap<&'a str, &'a AgentRequestRow>,
) -> Result<(String, usize)> {
    let mut current = row;
    let mut seen = BTreeSet::<String>::new();
    let mut depth = 0usize;
    loop {
        if !seen.insert(current.request_id.clone()) {
            return Err(CodexShimError::SteeringQueueCycle {
                request_id: current.request_id.clone(),
            }
            .into());
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

fn steering_parent_id(row: &AgentRequestRow) -> Option<String> {
    let queue = row.input.as_ref()?.queue.as_ref()?;
    matches!(
        queue.source,
        gents_protocol::request_input::QueueSource::User
            | gents_protocol::request_input::QueueSource::Steering
    )
    .then(|| queue.queued_after_request_id.clone())
    .flatten()
}

fn row_is_effectively_active(row: &AgentRequestRow) -> bool {
    project_persisted_attempt(
        row.lifecycle_state
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
        row.superseded_by_request
            .as_deref()
            .is_some_and(|id| !id.trim().is_empty()),
    )
    .is_some_and(|head| head.is_active())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn active_turn_ignores_newer_title_audit_request() {
        let temp = tempfile::tempdir().expect("active turn directory");
        let state =
            super::super::super::turn_projection::tests::notification_test_state(temp.path()).await;
        let node = state.node.clone();
        let node_did = gents::graphql::escape_graphql_string(state.node_did.as_ref());
        let agent_id = gents::graphql::escape_graphql_string(state.agent_id.as_ref());
        for (id, purpose, created_at) in [
            ("normal-turn", "normal", "2026-09-25T00:00:00Z"),
            ("title-audit", "title-audit", "2026-09-25T00:00:01Z"),
        ] {
            let id = gents::graphql::escape_graphql_string(id);
            let purpose = gents::graphql::escape_graphql_string(purpose);
            let created_at = gents::graphql::escape_graphql_string(created_at);
            let mutation = format!(
                r#"mutation {{ create_AgentRequest(input: {{request_id: "{id}", purpose: "{purpose}", session_id: "thread", node_did: "{node_did}", requester_did: "{node_did}", agent_id: "{agent_id}", content: "prompt", lifecycle_state: "processing", created_at: "{created_at}"}}) {{_docID}} }}"#
            );
            gents::config_client::ConfigAccess::write_local(
                &node,
                "codex.active.test_request",
                &mutation,
            )
            .await
            .expect("create request");
        }

        let active = super::load_active_codex_turn(&state, "thread")
            .await
            .expect("active query")
            .expect("normal active turn");
        assert_eq!(active.turn_id, "normal-turn");
        assert_eq!(active.interrupt_request_id, "normal-turn");
        assert_eq!(active.current_request_id, "normal-turn");
        gents::interrupt_request_by_doc_id(
            &node,
            &active.interrupt_request_doc_id,
            state.node_did.as_ref(),
            Some(state.node_did.as_ref()),
        )
        .await
        .expect("interrupt selected normal turn");
        let response = super::super::super::store::query_node_json(
            &node,
            r#"{ AgentRequest(filter: { session_id: {_eq: "thread"} }) {request_id interrupt_requested_at} }"#,
        )
        .await
        .expect("read interrupt latches");
        let rows = response["data"]["AgentRequest"]
            .as_array()
            .expect("request rows");
        assert_eq!(rows.len(), 2);
        for row in rows {
            let latched = row["interrupt_requested_at"].as_str().is_some();
            assert_eq!(latched, row["request_id"] == "normal-turn");
        }
    }

    #[tokio::test]
    async fn steering_successor_survives_threads_longer_than_one_arrival_page() {
        let temp = tempfile::tempdir().expect("active turn directory");
        let state =
            super::super::super::turn_projection::tests::notification_test_state(temp.path()).await;
        let node_did = state.node_did.to_string();
        let agent_id = state.agent_id.to_string();
        let request = |request_id: String, created_at: String, input: Value| {
            json!({
                "request_id": request_id, "purpose": "normal", "session_id": "thread",
                "node_did": node_did, "requester_did": node_did, "agent_id": agent_id,
                "content": "prompt", "lifecycle_state": "completed",
                "created_at": created_at, "input": input,
            })
        };
        let history = (0..1100)
            .map(|index| {
                request(
                    format!("history-{index:04}"),
                    format!("2026-09-25T00:{:02}:{:02}Z", index / 60, index % 60),
                    Value::Null,
                )
            })
            .collect::<Vec<_>>();
        let steering = json!({"queue": {"source": "steering", "policy": "append", "key": null,
            "queued_after_request_id": "history-1099"}});
        for batch in [
            history,
            vec![request(
                "steer-b".into(),
                "2026-09-26T00:00:01Z".into(),
                steering.clone(),
            )],
            vec![request(
                "steer-a".into(),
                "2026-09-26T00:00:00Z".into(),
                steering,
            )],
        ] {
            gents::config_client::ConfigAccess::transact_local_write(
                &state.node,
                None,
                "codex.active.test_history",
                |txn| {
                    let batch = &batch;
                    Box::pin(async move {
                        txn.execute_with_variables(
                            "mutation($rows: [AgentRequestMutationInputArg!]) { create_AgentRequest(input: $rows) { _docID } }",
                            &json!({ "rows": batch }),
                        )
                        .await
                    })
                },
            )
            .await
            .expect("create requests");
        }

        let next = next_steering_request_after(&state, "thread", "history-1099")
            .await
            .expect("steering order beyond one arrival page")
            .expect("queued steering successor");
        assert_eq!(next.request_id, "steer-b", "arrival order, not created_at");
    }

    use super::*;
}
