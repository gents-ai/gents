use anyhow::{Context, Result};
use codex_app_server_protocol as codex;
use serde_json::json;

use super::background::clean_background_terminals;
use super::compat::send_planned_stub;
use super::protocol::{
    absolute_path, client_request_from_jsonrpc, effective_cwd, empty_rate_limits,
    initialize_result, model_summary, send_error, send_result, send_typed_json_result,
};
use super::thread_projection::{
    clear_codex_thread_goal, codex_thread_json, create_codex_thread, get_codex_thread_goal,
    list_codex_threads, load_codex_thread, loaded_codex_thread_ids, resume_codex_thread,
    set_codex_thread_archived, set_codex_thread_git_info, set_codex_thread_goal,
    set_codex_thread_loaded, set_codex_thread_memory_mode, set_codex_thread_name,
    set_codex_thread_settings, thread_resume_response_json, thread_start_response_json,
};
use super::turn::{interrupt_active_turn, start_defra_turn, steer_defra_turn};
use super::{trace, ConnectionState, ShimState, JSONRPC_INVALID_PARAMS, JSONRPC_INVALID_REQUEST};

pub(super) async fn handle_request(
    connection: &ConnectionState,
    state: &ShimState,
    request: codex::JSONRPCRequest,
) -> Result<()> {
    let request_id = request.id.clone();
    let method = request.method.clone();
    tracing::info!(%method, %request_id, "Codex shim request");
    trace::shim_event(&state.trace_path, format!("request {request_id} {method}"));
    let outbound = &connection.outbound;
    let codex_request = match client_request_from_jsonrpc(request) {
        Ok(request) => request,
        Err(err) => {
            return send_error(
                outbound,
                request_id,
                JSONRPC_INVALID_REQUEST,
                format!("invalid Codex shim request `{method}`: {err}"),
            )
            .await;
        }
    };

    match codex_request {
        codex::ClientRequest::Initialize { request_id, .. } => {
            send_typed_json_result::<codex::InitializeResponse>(
                outbound,
                request_id,
                initialize_result(state),
            )
            .await
        }
        codex::ClientRequest::GetAccount { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::GetAccountResponse {
                    account: Some(codex::Account::ApiKey {}),
                    requires_openai_auth: false,
                },
            )
            .await
        }
        codex::ClientRequest::GetAccountRateLimits { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::GetAccountRateLimitsResponse {
                    rate_limits: empty_rate_limits(),
                    rate_limits_by_limit_id: None,
                },
            )
            .await
        }
        codex::ClientRequest::ModelList { request_id, .. } => {
            send_typed_json_result::<codex::ModelListResponse>(
                outbound,
                request_id,
                json!({
                    "data": [model_summary(state)],
                    "nextCursor": null
                }),
            )
            .await
        }
        codex::ClientRequest::ModelProviderCapabilitiesRead { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::ModelProviderCapabilitiesReadResponse {
                    namespace_tools: false,
                    image_generation: false,
                    web_search: false,
                },
            )
            .await
        }
        codex::ClientRequest::ConfigRead { request_id, .. } => {
            send_typed_json_result::<codex::ConfigReadResponse>(
                outbound,
                request_id,
                json!({
                    "config": {
                        "model": state.model.as_ref(),
                        "model_provider": "defra",
                        "approval_policy": "never",
                        "sandbox_mode": "danger-full-access"
                    },
                    "origins": {}
                }),
            )
            .await
        }
        codex::ClientRequest::ConfigBatchWrite { request_id, .. }
        | codex::ClientRequest::ConfigValueWrite { request_id, .. } => {
            send_typed_json_result::<codex::ConfigWriteResponse>(
                outbound,
                request_id,
                json!({
                    "status": "ok",
                    "version": "defra-shim",
                    "filePath": absolute_path(&state.codex_home.join("config.toml")),
                    "overriddenMetadata": null
                }),
            )
            .await
        }
        codex::ClientRequest::ConfigRequirementsRead { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::ConfigRequirementsReadResponse { requirements: None },
            )
            .await
        }
        codex::ClientRequest::ExternalAgentConfigDetect { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::ExternalAgentConfigDetectResponse { items: Vec::new() },
            )
            .await
        }
        codex::ClientRequest::ExternalAgentConfigImport { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::ExternalAgentConfigImportResponse {},
            )
            .await
        }
        codex::ClientRequest::ExperimentalFeatureList { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::ExperimentalFeatureListResponse {
                    data: Vec::new(),
                    next_cursor: None,
                },
            )
            .await
        }
        codex::ClientRequest::PermissionProfileList { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::PermissionProfileListResponse {
                    data: Vec::new(),
                    next_cursor: None,
                },
            )
            .await
        }
        codex::ClientRequest::CollaborationModeList { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::CollaborationModeListResponse { data: Vec::new() },
            )
            .await
        }
        codex::ClientRequest::SkillsList { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::SkillsListResponse { data: Vec::new() },
            )
            .await
        }
        codex::ClientRequest::HooksList { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::HooksListResponse { data: Vec::new() },
            )
            .await
        }
        codex::ClientRequest::PluginList { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::PluginListResponse {
                    marketplaces: Vec::new(),
                    marketplace_load_errors: Vec::new(),
                    featured_plugin_ids: Vec::new(),
                },
            )
            .await
        }
        codex::ClientRequest::McpServerStatusList { request_id, .. } => {
            send_result(
                outbound,
                request_id,
                codex::ListMcpServerStatusResponse {
                    data: Vec::new(),
                    next_cursor: None,
                },
            )
            .await
        }
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
            send_typed_json_result::<codex::ThreadStartResponse>(
                outbound,
                request_id,
                thread_start_response_json(state, &record),
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
            send_typed_json_result::<codex::ThreadResumeResponse>(
                outbound,
                request_id,
                thread_resume_response_json(state, &record),
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
            send_typed_json_result::<codex::ThreadReadResponse>(
                outbound,
                request_id,
                json!({
                    "thread": codex_thread_json(&record, params.include_turns)
                }),
            )
            .await
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
        codex::ClientRequest::TurnStart {
            request_id, params, ..
        } => {
            let connection = connection.clone();
            let state = state.clone();
            tokio::spawn(async move {
                if let Err(err) = start_defra_turn(
                    &connection,
                    &state,
                    request_id,
                    params.thread_id,
                    params.input,
                )
                .await
                {
                    tracing::warn!(%err, "Codex shim DEFRA turn task failed");
                }
            });
            Ok(())
        }
        codex::ClientRequest::TurnSteer {
            request_id, params, ..
        } => steer_defra_turn(connection, state, request_id, params).await,
        codex::ClientRequest::TurnInterrupt {
            request_id, params, ..
        } => {
            interrupt_active_turn(connection, state, &params.thread_id, &params.turn_id).await?;
            send_result(outbound, request_id, codex::TurnInterruptResponse {}).await
        }
        codex::ClientRequest::ThreadBackgroundTerminalsClean {
            request_id, params, ..
        } => {
            clean_background_terminals(state, &params.thread_id).await?;
            send_result(
                outbound,
                request_id,
                codex::ThreadBackgroundTerminalsCleanResponse {},
            )
            .await
        }
        unsupported => send_planned_stub(outbound, state, unsupported).await,
    }
}
