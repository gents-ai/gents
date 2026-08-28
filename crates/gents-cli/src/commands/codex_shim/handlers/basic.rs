use anyhow::{Context, Result};
use gents_codex_protocol as codex;
use serde_json::json;

use super::super::bound_behavior::load_bound_model_selection_id_for_state;
use super::super::protocol::{
    empty_rate_limits, initialize_result, send_result, send_typed_json_result,
};
use super::super::{Outbound, ShimState};
use super::models::{
    apply_config_writes, available_model_backends, load_bound_behavior, model_list_entries,
};
use super::skills::load_skill_metadata;

pub(super) async fn handle_basic_request(
    outbound: &Outbound,
    state: &ShimState,
    request: codex::ClientRequest,
) -> Result<()> {
    match request {
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
            let behavior = load_bound_behavior(state)
                .await
                .context("loading bound AgentBehavior for ModelList")?;
            let backends = available_model_backends(state)
                .await
                .context("listing available backend models for ModelList")?;
            let entries = model_list_entries(&backends, &behavior);
            send_typed_json_result::<codex::ModelListResponse>(
                outbound,
                request_id,
                json!({
                    "data": entries,
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
            let model_id =
                load_bound_model_selection_id_for_state(state.node.as_ref(), &state.behavior_id)
                    .await
                    .context("resolving current model selection for ConfigRead")?;
            send_typed_json_result::<codex::ConfigReadResponse>(
                outbound,
                request_id,
                json!({
                    "config": {
                        "model": model_id,
                        "model_provider": "gents",
                        "approval_policy": "never",
                        "sandbox_mode": "danger-full-access"
                    },
                    "origins": {}
                }),
            )
            .await
        }
        codex::ClientRequest::ConfigValueWrite {
            request_id, params, ..
        } => {
            apply_config_writes(
                outbound,
                state,
                request_id,
                vec![(params.key_path, params.value)],
            )
            .await
        }
        codex::ClientRequest::ConfigBatchWrite {
            request_id, params, ..
        } => {
            let writes = params
                .edits
                .into_iter()
                .map(|edit| (edit.key_path, edit.value))
                .collect::<Vec<_>>();
            apply_config_writes(outbound, state, request_id, writes).await
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
            let entry = match load_skill_metadata(state).await {
                Ok(skills) => codex::SkillsListEntry {
                    cwd: state.cwd.clone(),
                    skills,
                    errors: Vec::new(),
                },
                Err(error) => codex::SkillsListEntry {
                    cwd: state.cwd.clone(),
                    skills: Vec::new(),
                    errors: vec![codex::SkillErrorInfo {
                        path: state.cwd.clone(),
                        message: format!("failed to load skills: {error}"),
                    }],
                },
            };
            send_result(
                outbound,
                request_id,
                codex::SkillsListResponse { data: vec![entry] },
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
        other => unreachable!(
            "non-basic Codex request routed to basic handler: {}",
            other.method()
        ),
    }
}
