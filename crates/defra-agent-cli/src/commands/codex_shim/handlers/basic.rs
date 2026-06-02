use anyhow::{Context, Result};
use codex_app_server_protocol as codex;
use defra_agent::{
    backend_registry::list_enabled_backends, load_agent_behavior, AgentBehaviorDocument,
    InferenceBackend,
};
use serde_json::{json, Value};

use super::super::bound_behavior::{
    load_bound_model_selection_id_for_state, model_selection_id, parse_model_selection_id,
};
use super::super::protocol::{
    absolute_path, backend_model_summary, empty_rate_limits, initialize_result, send_error,
    send_result, send_typed_json_result,
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
        let new_model_id = match value.as_str() {
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
        let selection = match resolve_model_selection(state, &new_model_id).await {
            Ok(selection) => selection,
            Err(err) => {
                return send_error(
                    outbound,
                    request_id,
                    JSONRPC_INVALID_PARAMS,
                    err.to_string(),
                )
                .await;
            }
        };
        apply_model_to_bound_behavior(state, &selection).await?;
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

struct ModelSelection {
    backend_id: String,
    model_name: String,
}

async fn load_bound_behavior(state: &ShimState) -> Result<AgentBehaviorDocument> {
    let behavior_id = state.behavior_id.as_ref();
    load_agent_behavior(state.node.as_ref(), behavior_id)
        .await
        .context("loading bound AgentBehavior")?
        .ok_or_else(|| anyhow::anyhow!("bound AgentBehavior {behavior_id:?} disappeared"))
}

async fn available_model_backends(state: &ShimState) -> Result<Vec<InferenceBackend>> {
    let mut backends = list_enabled_backends(state.node.as_ref()).await?;
    backends.retain(|backend| backend.is_available());
    backends.sort_by(|left, right| left.backend_id.cmp(&right.backend_id));
    Ok(backends)
}

fn model_list_entries(
    backends: &[InferenceBackend],
    behavior: &AgentBehaviorDocument,
) -> Vec<Value> {
    let current_backend_id = behavior
        .backend_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let current_model_name = behavior
        .model_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mut entries = backends
        .iter()
        .flat_map(|backend| {
            backend
                .models
                .iter()
                .map(move |model_name| (backend, model_name.trim()))
        })
        .filter(|(_, model_name)| !model_name.is_empty())
        .map(|(backend, model_name)| {
            let selection_id = model_selection_id(&backend.backend_id, model_name);
            let is_default = current_backend_id == Some(backend.backend_id.as_str())
                && current_model_name == Some(model_name);
            backend_model_summary(backend, model_name, &selection_id, is_default)
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        left.get("displayName")
            .and_then(Value::as_str)
            .cmp(&right.get("displayName").and_then(Value::as_str))
            .then_with(|| {
                left.get("id")
                    .and_then(Value::as_str)
                    .cmp(&right.get("id").and_then(Value::as_str))
            })
    });
    entries
}

async fn resolve_model_selection(
    state: &ShimState,
    requested_model: &str,
) -> Result<ModelSelection> {
    let requested_model = requested_model.trim();
    if requested_model.is_empty() {
        anyhow::bail!("ConfigValueWrite for `model` requires a non-empty string");
    }

    let behavior = load_bound_behavior(state).await?;
    let current_backend_id = behavior
        .backend_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let backends = available_model_backends(state).await?;
    let target = if let Some((backend_id, model_name)) = parse_model_selection_id(requested_model) {
        backends
            .iter()
            .find(|backend| {
                backend.backend_id == backend_id && backend_has_model(backend, model_name)
            })
            .map(|backend| (backend, model_name))
    } else {
        backends
            .iter()
            .find(|backend| {
                current_backend_id == Some(backend.backend_id.as_str())
                    && backend_has_model(backend, requested_model)
            })
            .or_else(|| {
                backends
                    .iter()
                    .find(|backend| backend_has_model(backend, requested_model))
            })
            .map(|backend| (backend, requested_model))
    };
    let Some((backend, model_name)) = target else {
        let available = backends
            .iter()
            .flat_map(|backend| backend.models.iter())
            .map(|model| model.trim())
            .filter(|model| !model.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "model {requested_model:?} not found in any available InferenceBackend; available models: [{available}]"
        );
    };

    Ok(ModelSelection {
        backend_id: backend.backend_id.clone(),
        model_name: model_name.to_string(),
    })
}

fn backend_has_model(backend: &InferenceBackend, model_name: &str) -> bool {
    backend
        .models
        .iter()
        .any(|model| model.trim() == model_name)
}

async fn apply_model_to_bound_behavior(
    state: &ShimState,
    selection: &ModelSelection,
) -> Result<()> {
    let mut behavior = load_bound_behavior(state).await?;
    behavior.backend_id = Some(selection.backend_id.clone());
    behavior.model_name = Some(selection.model_name.clone());
    let access = ConfigAccess::Graphql(state.graphql.as_ref().to_string());
    write_agent_behavior_document(&access, &behavior)
        .await
        .context("writing AgentBehavior with selected backend model")?;
    Ok(())
}
