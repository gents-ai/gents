use anyhow::{Context, Result};
use gents::{
    backend_registry::list_enabled_backends, load_agent_behavior, AgentBehaviorDocument,
    InferenceBackend,
};
use gents_codex_protocol as codex;
use serde_json::{json, Value};

use super::super::bound_behavior::{model_selection_id, parse_model_selection_id};
use super::super::protocol::{
    absolute_path, backend_model_summary, send_error, send_typed_json_result,
};
use super::super::{Outbound, ShimState, JSONRPC_INVALID_PARAMS};
use crate::config_writes::{write_agent_behavior_document, ConfigAccess};

pub(super) async fn apply_config_writes(
    outbound: &Outbound,
    state: &ShimState,
    request_id: codex::RequestId,
    writes: Vec<(String, Value)>,
) -> Result<()> {
    for (key_path, value) in writes {
        if key_path != "model" {
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
            "version": "gents-shim",
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

pub(super) async fn load_bound_behavior(state: &ShimState) -> Result<AgentBehaviorDocument> {
    let behavior_id = state.behavior_id.as_ref();
    load_agent_behavior(state.node.as_ref(), behavior_id)
        .await
        .context("loading bound AgentBehavior")?
        .ok_or_else(|| anyhow::anyhow!("bound AgentBehavior {behavior_id:?} disappeared"))
}

pub(super) async fn available_model_backends(state: &ShimState) -> Result<Vec<InferenceBackend>> {
    let mut backends = list_enabled_backends(state.node.as_ref()).await?;
    backends.retain(|backend| backend.is_available());
    backends.sort_by(|left, right| left.backend_id.cmp(&right.backend_id));
    Ok(backends)
}

pub(super) fn model_list_entries(
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
