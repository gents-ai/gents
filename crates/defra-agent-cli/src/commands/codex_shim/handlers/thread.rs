use anyhow::{Context, Result};
use codex_app_server_protocol as codex;
use serde_json::json;

use super::super::history_projection::{
    conversation_summary_json, load_thread_turns, thread_turn_items_list_response,
    thread_turns_list_response,
};
use super::super::protocol::{
    effective_cwd, send_error, send_notification, send_result, send_typed_json_result,
};
use super::super::bound_behavior::load_bound_inference_profile_id_for_state;
use super::super::thread_projection::{
    clear_codex_thread_goal, codex_thread_json, codex_thread_json_with_turns, create_codex_thread,
    get_codex_thread_goal, list_codex_threads, load_codex_thread, loaded_codex_thread_ids,
    resume_codex_thread, set_codex_thread_archived, set_codex_thread_git_info,
    set_codex_thread_goal, set_codex_thread_loaded, set_codex_thread_memory_mode,
    set_codex_thread_name, set_codex_thread_settings, thread_resume_response_json,
    thread_start_response_json,
};
use super::super::thread_routes;
use super::super::{ConnectionState, JSONRPC_INVALID_PARAMS, Outbound, ShimState};

pub(super) async fn handle_thread_request(
    connection: &ConnectionState,
    state: &ShimState,
    outbound: &Outbound,
    request: codex::ClientRequest,
) -> Result<()> {
    match request {
        codex::ClientRequest::ThreadStart {
            request_id, params, ..
        } => {
            let cwd = effective_cwd(state, params.cwd.as_deref());
            let thread_id = state.next_thread_id();
            let record = create_codex_thread(state, &thread_id, &cwd).await?;
            connection
                .thread_cwds
                .lock()
                .await
                .insert(thread_id.clone(), cwd.clone());
            let bound_profile_id = load_bound_inference_profile_id_for_state(
                state.node.as_ref(),
                &state.behavior_id,
            )
            .await
            .context("resolving bound inference profile for ThreadStart")?;
            send_typed_json_result::<codex::ThreadStartResponse>(
                outbound,
                request_id,
                thread_start_response_json(&record, &bound_profile_id),
            )
            .await
        }
        codex::ClientRequest::ThreadResume {
            request_id, params, ..
        } => {
            let record =
                resume_codex_thread(state, &params.thread_id, params.cwd.as_deref()).await?;
            connection
                .thread_cwds
                .lock()
                .await
                .insert(record.session_id.clone(), record.cwd.clone());
            let bound_profile_id = load_bound_inference_profile_id_for_state(
                state.node.as_ref(),
                &state.behavior_id,
            )
            .await
            .context("resolving bound inference profile for ThreadResume")?;
            send_typed_json_result::<codex::ThreadResumeResponse>(
                outbound,
                request_id,
                thread_resume_response_json(&record, &bound_profile_id),
            )
            .await
        }
        codex::ClientRequest::ThreadList { request_id, .. } => {
            let threads: Vec<_> = list_codex_threads(state)
                .await?
                .into_iter()
                .map(|record| codex_thread_json(&record, false))
                .collect();
            send_typed_json_result::<codex::ThreadListResponse>(
                outbound,
                request_id,
                json!({
                    "data": threads,
                    "nextCursor": null,
                    "backwardsCursor": null
                }),
            )
            .await
        }
        codex::ClientRequest::ThreadFork {
            request_id, params, ..
        } => match thread_routes::fork_thread_response(state, params).await {
            Ok((record, response)) => {
                let thread_for_notification: codex::Thread = serde_json::from_value(
                    response
                        .get("thread")
                        .cloned()
                        .unwrap_or_else(|| codex_thread_json(&record, false)),
                )
                .context("validating forked thread notification")?;
                connection
                    .thread_cwds
                    .lock()
                    .await
                    .insert(record.session_id.clone(), record.cwd.clone());
                send_typed_json_result::<codex::ThreadForkResponse>(outbound, request_id, response)
                    .await?;
                send_notification(
                    outbound,
                    state,
                    codex::ServerNotification::ThreadStarted(codex::ThreadStartedNotification {
                        thread: thread_for_notification,
                    }),
                )
                .await
            }
            Err(err) => send_error(outbound, request_id, err.code, err.message).await,
        },
        codex::ClientRequest::ThreadSearch {
            request_id, params, ..
        } => match thread_routes::search_threads_response(state, params).await {
            Ok(response) => {
                send_typed_json_result::<codex::ThreadSearchResponse>(
                    outbound, request_id, response,
                )
                .await
            }
            Err(err) => send_error(outbound, request_id, err.code, err.message).await,
        },
        codex::ClientRequest::ThreadLoadedList { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::ThreadLoadedListResponse {
                    data: loaded_codex_thread_ids(state).await?,
                    next_cursor: None,
                },
            )
            .await
        }
        codex::ClientRequest::ThreadRead {
            request_id, params, ..
        } => {
            let Some(record) = load_codex_thread(state, &params.thread_id).await? else {
                return send_error(
                    outbound,
                    request_id,
                    JSONRPC_INVALID_PARAMS,
                    format!("unknown Codex thread `{}`", params.thread_id),
                )
                .await;
            };
            let turns = if params.include_turns {
                load_thread_turns(state, &record).await?
            } else {
                Vec::new()
            };
            send_typed_json_result::<codex::ThreadReadResponse>(
                outbound,
                request_id,
                json!({
                    "thread": codex_thread_json_with_turns(&record, turns)
                }),
            )
            .await
        }
        codex::ClientRequest::ThreadTurnsList {
            request_id, params, ..
        } => {
            let Some(record) = load_codex_thread(state, &params.thread_id).await? else {
                return send_error(
                    outbound,
                    request_id,
                    JSONRPC_INVALID_PARAMS,
                    format!("unknown Codex thread `{}`", params.thread_id),
                )
                .await;
            };
            let turns = load_thread_turns(state, &record).await?;
            let response = thread_turns_list_response(
                turns,
                params.cursor,
                params.limit,
                params.sort_direction,
                params.items_view,
            );
            send_result(outbound, request_id, response).await
        }
        codex::ClientRequest::ThreadTurnsItemsList {
            request_id, params, ..
        } => {
            let Some(record) = load_codex_thread(state, &params.thread_id).await? else {
                return send_error(
                    outbound,
                    request_id,
                    JSONRPC_INVALID_PARAMS,
                    format!("unknown Codex thread `{}`", params.thread_id),
                )
                .await;
            };
            let turns = load_thread_turns(state, &record).await?;
            let Some(response) = thread_turn_items_list_response(
                turns,
                &params.turn_id,
                params.cursor,
                params.limit,
                params.sort_direction,
            ) else {
                return send_error(
                    outbound,
                    request_id,
                    JSONRPC_INVALID_PARAMS,
                    format!(
                        "unknown Codex turn `{}` for thread `{}`",
                        params.turn_id, params.thread_id
                    ),
                )
                .await;
            };
            send_result(outbound, request_id, response).await
        }
        codex::ClientRequest::ThreadUnsubscribe {
            request_id, params, ..
        } => {
            set_codex_thread_loaded(state, &params.thread_id, false).await?;
            send_result(
                outbound,
                request_id,
                codex::ThreadUnsubscribeResponse {
                    status: codex::ThreadUnsubscribeStatus::Unsubscribed,
                },
            )
            .await
        }
        codex::ClientRequest::ThreadArchive {
            request_id, params, ..
        } => {
            set_codex_thread_archived(state, &params.thread_id, true).await?;
            send_result(outbound, request_id, codex::ThreadArchiveResponse {}).await
        }
        codex::ClientRequest::ThreadUnarchive {
            request_id, params, ..
        } => {
            set_codex_thread_archived(state, &params.thread_id, false).await?;
            let record = resume_codex_thread(state, &params.thread_id, None).await?;
            send_typed_json_result::<codex::ThreadUnarchiveResponse>(
                outbound,
                request_id,
                json!({ "thread": codex_thread_json(&record, false) }),
            )
            .await
        }
        codex::ClientRequest::ThreadSetName {
            request_id, params, ..
        } => {
            set_codex_thread_name(state, &params.thread_id, &params.name).await?;
            send_result(outbound, request_id, codex::ThreadSetNameResponse {}).await
        }
        codex::ClientRequest::ThreadMemoryModeSet {
            request_id, params, ..
        } => {
            set_codex_thread_memory_mode(state, &params.thread_id, params.mode).await?;
            send_result(outbound, request_id, codex::ThreadMemoryModeSetResponse {}).await
        }
        codex::ClientRequest::ThreadSettingsUpdate {
            request_id, params, ..
        } => {
            set_codex_thread_settings(state, &params.thread_id, &params).await?;
            if let Some(cwd) = params.cwd.as_deref() {
                let cwd = effective_cwd(state, cwd.to_str());
                connection
                    .thread_cwds
                    .lock()
                    .await
                    .insert(params.thread_id.clone(), cwd);
            }
            send_result(outbound, request_id, codex::ThreadSettingsUpdateResponse {}).await
        }
        codex::ClientRequest::ThreadMetadataUpdate {
            request_id, params, ..
        } => {
            let record = set_codex_thread_git_info(state, &params.thread_id, &params.git_info)
                .await?
                .with_context(|| format!("unknown Codex thread `{}`", params.thread_id))?;
            send_typed_json_result::<codex::ThreadMetadataUpdateResponse>(
                outbound,
                request_id,
                json!({ "thread": codex_thread_json(&record, false) }),
            )
            .await
        }
        codex::ClientRequest::ThreadGoalSet {
            request_id, params, ..
        } => {
            let goal = set_codex_thread_goal(state, &params).await?;
            send_result(outbound, request_id, codex::ThreadGoalSetResponse { goal }).await
        }
        codex::ClientRequest::ThreadGoalGet {
            request_id, params, ..
        } => {
            let goal = get_codex_thread_goal(state, &params.thread_id).await?;
            send_result(outbound, request_id, codex::ThreadGoalGetResponse { goal }).await
        }
        codex::ClientRequest::ThreadGoalClear {
            request_id, params, ..
        } => {
            let cleared = clear_codex_thread_goal(state, &params.thread_id).await?;
            send_result(
                outbound,
                request_id,
                codex::ThreadGoalClearResponse { cleared },
            )
            .await
        }
        codex::ClientRequest::GetConversationSummary {
            request_id, params, ..
        } => {
            match params {
                codex::GetConversationSummaryParams::ThreadId { conversation_id } => {
                    let thread_id = conversation_id.to_string();
                    let Some(record) = load_codex_thread(state, &thread_id).await? else {
                        return send_error(
                            outbound,
                            request_id,
                            JSONRPC_INVALID_PARAMS,
                            format!("unknown Codex thread `{thread_id}`"),
                        )
                        .await;
                    };
                    send_typed_json_result::<codex::GetConversationSummaryResponse>(
                        outbound,
                        request_id,
                        conversation_summary_json(state, &record),
                    )
                    .await
                }
                codex::GetConversationSummaryParams::RolloutPath { rollout_path } => send_error(
                    outbound,
                    request_id,
                    JSONRPC_INVALID_PARAMS,
                    format!(
                        "rollout path summaries are unavailable for DEFRA-backed Codex threads: {}",
                        rollout_path.display()
                    ),
                )
                .await,
            }
        }
        other => unreachable!(
            "non-thread Codex request routed to thread handler: {}",
            other.method()
        ),
    }
}
