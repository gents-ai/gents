use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::{Context, Result};
use gents::graphql::escape_graphql_string;
use gents::UpdateSubscriptionSource;
use gents_codex_protocol as codex;
use gents_protocol::client_protocol::RequestLifecycleState;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::watch;

use super::progress::{
    decode_gents_tool_call_progress, gents_tool_progress_query, tool_completed_at_ms,
};
use super::projection_state::ChildStatus;
use super::protocol::{
    now_millis, send_notification, send_thread_status_changed, timestamp_seconds,
    turn_value_with_timing,
};
use super::store::query_node_json;
use super::subagent_projection::{
    attach_subagent_link, collab_agent_status, collab_projection, collab_tool_item,
    load_authorized_subagent_threads_for_root, LinkedSubagentThread,
    SubagentProjectionUpdateFilter,
};
use super::thread_projection::CodexThreadRecord;
use super::turn::{
    codex_turn_id_for_request, install_stream_control, stream_gents_turn, TurnStreamOptions,
};
use super::turn_projection::TurnProjection;
use super::{ConnectionState, ShimState};
use crate::SubmittedRequest;

#[derive(Clone, Debug, Deserialize)]
struct BackgroundContinuationRequest {
    request_id: String,
    session_id: String,
    agent_did: String,
    #[serde(default)]
    behavior_id: Option<String>,
    #[serde(default)]
    metadata: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ChildLifecycleSignature {
    status: ChildStatus,
    failure_reason: Option<String>,
}

pub(super) async fn ensure_loaded_root_continuation_stream(
    connection: &ConnectionState,
    state: &ShimState,
    record: &CodexThreadRecord,
    baseline_turns: Option<Vec<codex::Turn>>,
) {
    if record.is_subagent() {
        return;
    }

    let watcher_id = state.next_id("gents-root-continuation-stream");
    let task_connection = connection.clone();
    let task_state = state.clone();
    let task_watcher_id = watcher_id.clone();
    let thread_id = record.session_id.clone();
    let task_thread_id = thread_id.clone();
    let task = tokio::spawn(async move {
        let result = watch_loaded_root_continuations(
            &task_connection,
            &task_state,
            &task_thread_id,
            &task_watcher_id,
            baseline_turns,
        )
        .await;
        if let Err(error) = result {
            tracing::warn!(
                %error,
                thread_id = task_thread_id,
                "Codex shim root continuation projection stopped"
            );
        }
        task_connection
            .clear_turn_streams_owned_by(&task_watcher_id)
            .await;
        task_connection
            .clear_root_continuation_stream_if_current(&task_thread_id, &task_watcher_id)
            .await;
    });
    connection
        .replace_root_continuation_stream(thread_id, watcher_id, task.abort_handle())
        .await;
}

async fn watch_loaded_root_continuations(
    connection: &ConnectionState,
    state: &ShimState,
    thread_id: &str,
    watcher_id: &str,
    baseline_turns: Option<Vec<codex::Turn>>,
) -> Result<()> {
    let suppress_existing_terminal = baseline_turns.is_none();
    let mut baseline_turns = baseline_turns
        .unwrap_or_default()
        .into_iter()
        .map(|turn| (turn.id.clone(), turn))
        .collect::<BTreeMap<_, _>>();
    let mut observed = BTreeSet::<String>::new();
    let mut observed_children = BTreeMap::<String, ChildLifecycleSignature>::new();
    let mut initialized = false;
    let mut updates = state.node.subscribe_updates();
    let mut updates_closed = false;
    let update_filter = SubagentProjectionUpdateFilter::from_state(state);

    loop {
        if !state.is_thread_loaded(thread_id).await || connection.outbound.is_closed() {
            return Ok(());
        }

        let links = load_authorized_subagent_threads_for_root(state, thread_id).await?;
        for link in &links {
            let signature = ChildLifecycleSignature {
                status: collab_agent_status(link.client_projection),
                failure_reason: link.failure_reason.clone(),
            };
            if signature.status == ChildStatus::NotFound {
                continue;
            }
            let previous = observed_children.insert(link.session_id.clone(), signature.clone());
            if initialized && previous.as_ref() != Some(&signature) {
                project_child_lifecycle_update(connection, state, link, &links).await?;
            }
        }

        let requests = load_background_continuation_requests(state, thread_id).await?;
        let mut projected_request = false;
        for request in requests {
            let lifecycle_state = request.lifecycle_state.as_deref().unwrap_or("");
            if observed.contains(&request.request_id) {
                continue;
            }

            if !continuation_request_has_started(lifecycle_state) {
                continue;
            }
            if RequestLifecycleState::parse_opt(Some(lifecycle_state))
                == Some(RequestLifecycleState::Superseded)
            {
                observed.insert(request.request_id);
                continue;
            }

            let baseline_turn = baseline_turns.remove(&request.request_id);
            if baseline_turn_is_terminal(baseline_turn.as_ref())
                && RequestLifecycleState::is_terminal_str(Some(lifecycle_state))
            {
                observed.insert(request.request_id);
                continue;
            }
            if !initialized
                && suppress_existing_terminal
                && RequestLifecycleState::is_terminal_str(Some(lifecycle_state))
            {
                observed.insert(request.request_id);
                continue;
            }

            observed.insert(request.request_id.clone());
            project_background_continuation(connection, state, watcher_id, request, baseline_turn)
                .await?;
            projected_request = true;
        }
        initialized = true;

        if projected_request {
            continue;
        }

        if updates_closed {
            tokio::time::sleep(Duration::from_millis(
                state.poll_interval.as_millis().max(250) as u64,
            ))
            .await;
            continue;
        }

        loop {
            let Some(message) = updates.recv().await else {
                updates_closed = true;
                tracing::warn!(
                    thread_id,
                    "Codex shim root continuation update subscription closed; polling"
                );
                break;
            };
            let dropped = updates.check_and_reset_dropped();
            if dropped > 0 {
                tracing::warn!(
                    thread_id,
                    dropped,
                    "Codex shim root continuation update subscription dropped messages"
                );
                break;
            }
            if message
                .as_update()
                .is_some_and(|update| update_filter.affects_collection_id(&update.collection_id))
            {
                break;
            }
        }
    }
}

async fn project_child_lifecycle_update(
    connection: &ConnectionState,
    state: &ShimState,
    link: &LinkedSubagentThread,
    links: &[LinkedSubagentThread],
) -> Result<()> {
    let turn_id =
        codex_turn_id_for_request(state, &link.parent_session_id, &link.parent_request_id).await?;
    if connection
        .has_turn_stream(&link.parent_session_id, &turn_id)
        .await
    {
        return Ok(());
    }

    let response = query_node_json(
        state.node.as_ref(),
        &gents_tool_progress_query(&link.parent_request_id, &link.parent_session_id),
    )
    .await?;
    let Some(mut tool) = response
        .pointer("/data/AgentToolCall")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(decode_gents_tool_call_progress)
        .find(|tool| {
            tool.tool_name == "spawn_subagent"
                && tool.child_request_id.as_deref() == Some(link.request_id.as_str())
        })
    else {
        return Ok(());
    };
    attach_subagent_link(&mut tool, links);
    let Some(projection) = collab_projection(&tool) else {
        return Ok(());
    };
    send_notification(
        &connection.outbound,
        state,
        codex::ServerNotification::ItemCompleted(codex::ItemCompletedNotification {
            item: collab_tool_item(&link.parent_session_id, &tool, &projection),
            thread_id: link.parent_session_id.clone(),
            turn_id,
            completed_at_ms: tool_completed_at_ms(&tool).unwrap_or_else(now_millis),
        }),
    )
    .await
}

async fn project_background_continuation(
    connection: &ConnectionState,
    state: &ShimState,
    watcher_id: &str,
    request: BackgroundContinuationRequest,
    baseline_turn: Option<codex::Turn>,
) -> Result<()> {
    let turn_id = request.request_id.clone();
    let started_at = request.created_at.as_deref().and_then(timestamp_seconds);
    if baseline_turn.is_none() {
        send_notification(
            &connection.outbound,
            state,
            codex::ServerNotification::TurnStarted(codex::TurnStartedNotification {
                thread_id: request.session_id.clone(),
                turn: turn_value_with_timing(
                    &turn_id,
                    codex::TurnStatus::InProgress,
                    Vec::new(),
                    None,
                    started_at,
                    None,
                ),
            }),
        )
        .await?;
        send_thread_status_changed(
            &connection.outbound,
            state,
            &request.session_id,
            codex::ThreadStatus::Active {
                active_flags: Vec::new(),
            },
        )
        .await?;
    }

    let submitted = SubmittedRequest {
        request_id: request.request_id,
        session_id: request.session_id.clone(),
        agent_did: request.agent_did,
        behavior_id: request.behavior_id,
        temperature: None,
        top_p: None,
        top_k: None,
        seed: None,
        max_tokens: None,
        max_total_tokens: None,
        metadata: request.metadata,
        created_at: request.created_at,
    };
    let cwd = state.thread_cwd(&request.session_id).await;
    let mut projection = TurnProjection::new(state, &request.session_id, &turn_id, cwd, started_at);
    let options = baseline_turn.map_or_else(
        || TurnStreamOptions::fresh_background_completion(request.session_id.clone()),
        |turn| TurnStreamOptions::resumed_background_completion(request.session_id.clone(), turn),
    );
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let stream_registration = install_stream_control(
        connection,
        request.session_id.clone(),
        turn_id.clone(),
        Some(watcher_id),
        cancel_tx,
    )
    .await;
    let result = stream_gents_turn(
        connection,
        state,
        &submitted,
        &mut projection,
        cancel_rx,
        options,
    )
    .await
    .with_context(|| {
        format!(
            "projecting background completion request {} for root thread {}",
            submitted.request_id, submitted.session_id
        )
    });
    stream_registration.clear().await;
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            let message = format!("GENTS background continuation failed: {error}");
            projection
                .append_agent_delta(&connection.outbound, &format!("[agent error] {message}\n"))
                .await?;
            projection
                .finish_turn(
                    &connection.outbound,
                    codex::TurnStatus::Failed,
                    Some(message),
                )
                .await?;
            send_thread_status_changed(
                &connection.outbound,
                state,
                &request.session_id,
                codex::ThreadStatus::SystemError,
            )
            .await
        }
    }
}

async fn load_background_continuation_requests(
    state: &ShimState,
    thread_id: &str,
) -> Result<Vec<BackgroundContinuationRequest>> {
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{thread_id}" }},
                    agent_did: {{ _eq: "{agent_did}" }},
                    behavior_id: {{ _eq: "{behavior_id}" }}
                }},
                order: [{{ created_at: ASC }}, {{ request_id: ASC }}]
            ) {{
                request_id
                session_id
                agent_did
                behavior_id
                metadata
                lifecycle_state
                created_at
            }}
        }}"#,
        thread_id = escape_graphql_string(thread_id),
        agent_did = escape_graphql_string(state.agent_did.as_ref()),
        behavior_id = escape_graphql_string(state.behavior_id.as_ref()),
    );
    let response = query_node_json(state.node.as_ref(), &query).await?;
    response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(serde_json::from_value::<BackgroundContinuationRequest>)
        .collect::<serde_json::Result<Vec<_>>>()
        .context("decoding background completion AgentRequest rows")
        .map(|rows| {
            rows.into_iter()
                .filter(|row| is_background_completion_metadata(row.metadata.as_deref()))
                .collect()
        })
}

pub(super) fn is_background_completion_metadata(metadata: Option<&str>) -> bool {
    let Some(metadata) = metadata.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    serde_json::from_str::<Value>(metadata)
        .ok()
        .and_then(|value| value.get("queue").cloned())
        .is_some_and(|queue| {
            queue
                .get("source")
                .and_then(Value::as_str)
                .is_some_and(|source| source == "background_completion")
                && queue.get("policy").and_then(Value::as_str) == Some("coalesce")
                && queue
                    .get("key")
                    .and_then(Value::as_str)
                    .is_some_and(|key| !key.trim().is_empty())
        })
}

fn continuation_request_has_started(lifecycle_state: &str) -> bool {
    !matches!(
        RequestLifecycleState::parse_opt(Some(lifecycle_state.trim())),
        None | Some(
            RequestLifecycleState::Pending | RequestLifecycleState::WorkspaceBindingPending
        )
    )
}

fn baseline_turn_is_terminal(turn: Option<&codex::Turn>) -> bool {
    turn.is_some_and(|turn| turn.status != codex::TurnStatus::InProgress)
}

#[cfg(test)]
mod tests {
    use super::{
        baseline_turn_is_terminal, continuation_request_has_started,
        is_background_completion_metadata, turn_value_with_timing,
    };
    use gents_codex_protocol as codex;
    use gents_protocol::client_protocol::RequestLifecycleState;

    #[test]
    fn recognizes_only_the_canonical_background_completion_source() {
        assert!(is_background_completion_metadata(Some(
            r#"{"queue":{"source":"background_completion","policy":"coalesce","key":"background_completion:thread-1"}}"#
        )));
        assert!(!is_background_completion_metadata(Some(
            r#"{"queue":{"source":"subagent_completion","policy":"coalesce","key":"background_completion:thread-1"}}"#
        )));
        assert!(!is_background_completion_metadata(Some(
            r#"{"queue":{"source":"steering","policy":"coalesce","key":"background_completion:thread-1"}}"#
        )));
        assert!(!is_background_completion_metadata(Some(
            r#"{"queue":{"source":"background_completion","policy":"append","key":"background_completion:thread-1"}}"#
        )));
        assert!(!is_background_completion_metadata(Some("{}")));
    }

    #[test]
    fn pending_wakes_are_not_announced_before_the_runtime_claims_them() {
        assert!(!continuation_request_has_started("pending"));
        assert!(!continuation_request_has_started("workspaceBindingPending"));
        assert!(continuation_request_has_started("claimed"));
        assert!(continuation_request_has_started("processing"));
        assert!(RequestLifecycleState::is_terminal_str(Some("completed")));
        assert!(RequestLifecycleState::is_terminal_str(Some("interrupted")));
    }

    #[test]
    fn in_progress_resume_baseline_still_requires_terminal_projection() {
        let mut turn = turn_value_with_timing(
            "wake-1",
            codex::TurnStatus::InProgress,
            Vec::new(),
            None,
            None,
            None,
        );
        assert!(!baseline_turn_is_terminal(Some(&turn)));
        turn.status = codex::TurnStatus::Completed;
        assert!(baseline_turn_is_terminal(Some(&turn)));
        assert!(!baseline_turn_is_terminal(None));
    }
}
