use anyhow::Result;
use gents_codex_protocol as codex;
use serde_json::json;

use super::super::protocol::{absolute_path, send_result, send_typed_json_result};
use super::super::{trace, Outbound, ShimState};

pub(super) async fn send_empty_local_stub(
    outbound: &Outbound,
    state: &ShimState,
    request: &codex::ClientRequest,
) -> Result<bool> {
    let request_id = request.id().clone();
    let method = request.method();
    match request {
        codex::ClientRequest::ThreadIncrementElicitation { request_id, .. } => {
            send_result(
                outbound,
                request_id.clone(),
                codex::ThreadIncrementElicitationResponse {
                    count: 0,
                    paused: false,
                },
            )
            .await?
        }
        codex::ClientRequest::ThreadDecrementElicitation { request_id, .. } => {
            send_result(
                outbound,
                request_id.clone(),
                codex::ThreadDecrementElicitationResponse {
                    count: 0,
                    paused: false,
                },
            )
            .await?
        }
        codex::ClientRequest::MemoryReset { request_id, .. } => {
            send_result(outbound, request_id.clone(), codex::MemoryResetResponse {}).await?
        }
        codex::ClientRequest::ThreadApproveGuardianDeniedAction { request_id, .. } => {
            send_result(
                outbound,
                request_id.clone(),
                codex::ThreadApproveGuardianDeniedActionResponse {},
            )
            .await?
        }
        codex::ClientRequest::MarketplaceAdd {
            request_id, params, ..
        } => {
            let marketplace_name = params
                .ref_name
                .as_deref()
                .unwrap_or(params.source.as_str())
                .to_string();
            send_typed_json_result::<codex::MarketplaceAddResponse>(
                outbound,
                request_id.clone(),
                json!({
                    "marketplaceName": marketplace_name,
                    "installedRoot": absolute_path(
                        &state.codex_home.join("marketplaces").join("gents-shim")
                    ),
                    "alreadyAdded": true
                }),
            )
            .await?
        }
        codex::ClientRequest::MarketplaceRemove {
            request_id, params, ..
        } => {
            send_typed_json_result::<codex::MarketplaceRemoveResponse>(
                outbound,
                request_id.clone(),
                json!({
                    "marketplaceName": params.marketplace_name,
                    "installedRoot": null
                }),
            )
            .await?
        }
        codex::ClientRequest::MarketplaceUpgrade {
            request_id, params, ..
        } => {
            let selected_marketplaces = params
                .marketplace_name
                .as_ref()
                .map(|name| vec![name.clone()])
                .unwrap_or_default();
            send_typed_json_result::<codex::MarketplaceUpgradeResponse>(
                outbound,
                request_id.clone(),
                json!({
                    "selectedMarketplaces": selected_marketplaces,
                    "upgradedRoots": [],
                    "errors": []
                }),
            )
            .await?
        }
        codex::ClientRequest::PluginInstalled { request_id, .. } => {
            send_typed_json_result::<codex::PluginInstalledResponse>(
                outbound,
                request_id.clone(),
                json!({
                    "marketplaces": [],
                    "marketplaceLoadErrors": []
                }),
            )
            .await?
        }
        codex::ClientRequest::PluginShareSave {
            request_id, params, ..
        } => {
            let remote_plugin_id = params
                .remote_plugin_id
                .as_deref()
                .unwrap_or("gents-shim-plugin-share")
                .to_string();
            send_result(
                outbound,
                request_id.clone(),
                codex::PluginShareSaveResponse {
                    remote_plugin_id,
                    share_url: "gents-shim://plugin-share/unavailable".to_string(),
                },
            )
            .await?
        }
        codex::ClientRequest::PluginShareUpdateTargets { request_id, .. } => {
            send_typed_json_result::<codex::PluginShareUpdateTargetsResponse>(
                outbound,
                request_id.clone(),
                json!({
                    "principals": [],
                    "discoverability": "PRIVATE"
                }),
            )
            .await?
        }
        codex::ClientRequest::PluginShareList { request_id, .. } => {
            send_typed_json_result::<codex::PluginShareListResponse>(
                outbound,
                request_id.clone(),
                json!({ "data": [] }),
            )
            .await?
        }
        codex::ClientRequest::PluginShareCheckout {
            request_id, params, ..
        } => {
            send_typed_json_result::<codex::PluginShareCheckoutResponse>(
                outbound,
                request_id.clone(),
                json!({
                    "remotePluginId": params.remote_plugin_id,
                    "pluginId": params.remote_plugin_id,
                    "pluginName": params.remote_plugin_id,
                    "pluginPath": absolute_path(
                        &state.codex_home
                            .join("plugin-share")
                            .join(params.remote_plugin_id.as_str())
                    ),
                    "marketplaceName": "gents-shim",
                    "marketplacePath": absolute_path(
                        &state.codex_home.join("marketplaces").join("gents-shim")
                    ),
                    "remoteVersion": null
                }),
            )
            .await?
        }
        codex::ClientRequest::PluginShareDelete { request_id, .. } => {
            send_result(
                outbound,
                request_id.clone(),
                codex::PluginShareDeleteResponse {},
            )
            .await?
        }
        codex::ClientRequest::AppsList { request_id, .. } => {
            send_typed_json_result::<codex::AppsListResponse>(
                outbound,
                request_id.clone(),
                json!({
                    "data": [],
                    "nextCursor": null
                }),
            )
            .await?
        }
        codex::ClientRequest::SkillsConfigWrite {
            request_id, params, ..
        } => {
            send_result(
                outbound,
                request_id.clone(),
                codex::SkillsConfigWriteResponse {
                    effective_enabled: params.enabled,
                },
            )
            .await?
        }
        codex::ClientRequest::ExperimentalFeatureEnablementSet {
            request_id, params, ..
        } => {
            send_result(
                outbound,
                request_id.clone(),
                codex::ExperimentalFeatureEnablementSetResponse {
                    enablement: params.enablement.clone(),
                },
            )
            .await?
        }
        codex::ClientRequest::RemoteControlStatusRead { request_id, .. } => {
            send_typed_json_result::<codex::RemoteControlStatusReadResponse>(
                outbound,
                request_id.clone(),
                json!({
                    "status": "disabled",
                    "serverName": "gents-shim",
                    "installationId": "gents-shim",
                    "environmentId": null
                }),
            )
            .await?
        }
        codex::ClientRequest::MockExperimentalMethod {
            request_id, params, ..
        } => {
            send_result(
                outbound,
                request_id.clone(),
                codex::MockExperimentalMethodResponse {
                    echoed: params.value.clone(),
                },
            )
            .await?
        }
        codex::ClientRequest::WindowsSandboxReadiness { request_id, .. } => {
            send_typed_json_result::<codex::WindowsSandboxReadinessResponse>(
                outbound,
                request_id.clone(),
                json!({ "status": "notConfigured" }),
            )
            .await?
        }
        codex::ClientRequest::LoginAccount { request_id, .. } => {
            send_result(
                outbound,
                request_id.clone(),
                codex::LoginAccountResponse::ApiKey {},
            )
            .await?
        }
        codex::ClientRequest::CancelLoginAccount { request_id, .. } => {
            send_result(
                outbound,
                request_id.clone(),
                codex::CancelLoginAccountResponse {
                    status: codex::CancelLoginAccountStatus::NotFound,
                },
            )
            .await?
        }
        codex::ClientRequest::LogoutAccount { request_id, .. } => {
            send_result(
                outbound,
                request_id.clone(),
                codex::LogoutAccountResponse {},
            )
            .await?
        }
        codex::ClientRequest::SendAddCreditsNudgeEmail { request_id, .. } => {
            send_result(
                outbound,
                request_id.clone(),
                codex::SendAddCreditsNudgeEmailResponse {
                    status: codex::AddCreditsNudgeEmailStatus::CooldownActive,
                },
            )
            .await?
        }
        codex::ClientRequest::FeedbackUpload {
            request_id, params, ..
        } => {
            send_result(
                outbound,
                request_id.clone(),
                codex::FeedbackUploadResponse {
                    thread_id: params.thread_id.clone().unwrap_or_default(),
                },
            )
            .await?
        }
        codex::ClientRequest::GetAuthStatus { request_id, .. } => {
            send_result(
                outbound,
                request_id.clone(),
                codex::GetAuthStatusResponse {
                    auth_method: None,
                    auth_token: None,
                    requires_openai_auth: Some(false),
                },
            )
            .await?
        }
        _ => return Ok(false),
    };

    trace::shim_event(
        &state.trace_path,
        format!("compat_stub {request_id} {method} empty_local"),
    );
    tracing::debug!(%request_id, %method, "Codex shim empty local compatibility stub");
    Ok(true)
}
