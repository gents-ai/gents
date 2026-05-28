use anyhow::{Context, Result};
use codex_app_server_protocol as codex;
use defra_agent::{
    list_inference_profile_records, load_agent_behavior, load_inference_profile,
    AgentBehaviorDocument,
};
use serde_json::{json, Value};

use super::super::bound_behavior::load_bound_inference_profile_id_for_state;
use super::super::protocol::{
    absolute_path, empty_rate_limits, initialize_result, model_summary, send_error, send_result,
    send_typed_json_result,
};
use super::super::{Outbound, ShimState, JSONRPC_INVALID_PARAMS};
use crate::config_writes::{write_agent_behavior_document, ConfigAccess};

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
            let profiles = list_inference_profile_records(state.node.as_ref())
                .await
                .context("listing InferenceProfile documents for Codex ModelList")?;
            let current_profile_id =
                load_bound_inference_profile_id_for_state(state.node.as_ref(), &state.behavior_id)
                    .await
                    .context("resolving current inference profile for ModelList")?;
            let entries: Vec<Value> = profiles
                .into_iter()
                .map(|(_doc_id, profile)| {
                    let is_default = profile.profile_id == current_profile_id;
                    model_summary(&profile, is_default)
                })
                .collect();
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
            let profile_id =
                load_bound_inference_profile_id_for_state(state.node.as_ref(), &state.behavior_id)
                    .await
                    .context("resolving current inference profile for ConfigRead")?;
            send_typed_json_result::<codex::ConfigReadResponse>(
                outbound,
                request_id,
                json!({
                    "config": {
                        "model": profile_id,
                        "model_provider": "defra",
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
        other => unreachable!(
            "non-basic Codex request routed to basic handler: {}",
            other.method()
        ),
    }
}

async fn apply_config_writes(
    outbound: &Outbound,
    state: &ShimState,
    request_id: codex::RequestId,
    writes: Vec<(String, Value)>,
) -> Result<()> {
    for (key_path, value) in writes {
        if key_path != "model" {
            // Other keys keep the existing no-op ack semantics.
            continue;
        }
        let new_profile_id = match value.as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => {
                return send_error(
                    outbound,
                    request_id,
                    JSONRPC_INVALID_PARAMS,
                    "ConfigValueWrite for `model` requires a non-empty string".to_string(),
                )
                .await;
            }
        };
        if load_inference_profile(state.node.as_ref(), &new_profile_id)
            .await
            .context("looking up target InferenceProfile for ConfigValueWrite")?
            .is_none()
        {
            return send_error(
                outbound,
                request_id,
                JSONRPC_INVALID_PARAMS,
                format!(
                    "InferenceProfile {new_profile_id:?} not found; available ids \
                     are visible via ModelList"
                ),
            )
            .await;
        }
        apply_profile_to_bound_behavior(state, &new_profile_id).await?;
    }
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

async fn apply_profile_to_bound_behavior(state: &ShimState, profile_id: &str) -> Result<()> {
    let behavior_id = state.behavior_id.as_ref();
    let mut behavior: AgentBehaviorDocument = load_agent_behavior(state.node.as_ref(), behavior_id)
        .await
        .context("loading bound AgentBehavior for profile mutation")?
        .ok_or_else(|| anyhow::anyhow!("bound AgentBehavior {behavior_id:?} disappeared"))?;
    behavior.inference_profile_id = Some(profile_id.to_string());
    let access = ConfigAccess::Graphql(state.graphql.as_ref().to_string());
    write_agent_behavior_document(&access, &behavior)
        .await
        .context("writing AgentBehavior with new inference_profile_id")?;
    Ok(())
}
