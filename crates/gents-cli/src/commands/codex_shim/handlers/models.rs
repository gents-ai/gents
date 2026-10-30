use anyhow::{Context, Result};
use gents::config_client::{
    apply_desired_state_plan, load_inference_backend_in_txn, read_desired_state_record_in_txn,
    DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::document_config::{BackendAuth, BackendModelCatalog};
use gents::inference::backends::registry::{
    list_enabled_backends_for_agent, lookup_backend_observation, lookup_backend_observation_in_txn,
};
use gents::{
    list_agents, list_inference_profile_records, load_inference_profile, Collection,
    InferenceBackend, InferenceProfile, ReasoningEffort,
};
use gents_codex_protocol as codex;
use gents_protocol::row::{
    project_node_readiness_summary, AgentReadinessUnavailableReason, ProjectedNodeReadinessSummary,
};
use serde_json::{json, Value};

use super::super::bound_behavior::model_selection_id;
use super::super::protocol::{
    absolute_path, backend_model_summary, send_error, send_typed_json_result,
};
use super::super::{Outbound, ShimState, JSONRPC_INVALID_PARAMS};
use crate::config_writes::ConfigAccess;

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
        apply_model_to_bound_agent(state, &selection).await?;
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

pub(super) struct BoundAgent {
    pub(super) inference_profile: InferenceProfile,
}

pub(super) struct AvailableBackend {
    pub(super) backend: InferenceBackend,
    pub(super) catalog: Option<BackendModelCatalog>,
}

async fn load_bound_agent_document(state: &ShimState) -> Result<gents::AgentDocument> {
    let node_did = state.node_did.as_ref();
    let agent_id = state.agent_id.as_ref();
    let raw = ConfigAccess::transact_local_write(&state.node, None, "codex.bound_agent", |txn| {
        Box::pin(async move {
            read_desired_state_record_in_txn(txn, Collection::Agent, node_did, agent_id)
                .await?
                .map(|(_, value)| value)
                .ok_or_else(|| anyhow::anyhow!("bound Agent {agent_id:?} missing for {node_did:?}"))
        })
    })
    .await?;
    let agent: gents::AgentDocument = serde_json::from_value(raw)
        .with_context(|| format!("decoding bound Agent {agent_id:?}"))?;
    Ok(agent)
}

pub(super) async fn load_bound_agent(state: &ShimState) -> Result<BoundAgent> {
    let agent = load_bound_agent_document(state).await?;
    let node_did = state.node_did.as_ref();
    let inference_profile =
        load_inference_profile(&state.node, node_did, &agent.inference_profile_id)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "bound inference profile {:?} missing for {node_did:?}",
                    agent.inference_profile_id
                )
            })?;
    inference_profile
        .validate()
        .context("bound inference profile is invalid")?;
    Ok(BoundAgent { inference_profile })
}

/// Drops a backend only on an explicit readiness veto; fail-closed admission stays
/// in `fleet_slots.rs`/`healthz`, and another node's backend is never selectable.
pub(super) async fn available_model_backends(state: &ShimState) -> Result<Vec<AvailableBackend>> {
    let node_did = state.node_did.as_ref();
    let mut backends = list_enabled_backends_for_agent(state.node.as_ref(), node_did)
        .await
        .context("listing enabled inference backends")?;

    let agents = list_agents(state.node.as_ref(), node_did)
        .await
        .context("listing agents for model selection")?;
    let profiles = list_inference_profile_records(state.node.as_ref(), node_did)
        .await
        .context("listing inference profiles for model selection")?;
    let profile_backend: std::collections::BTreeMap<&str, &str> = profiles
        .iter()
        .map(|(_, profile)| (profile.profile_id.as_str(), profile.backend_id.as_str()))
        .collect();
    let bindings = agents
        .iter()
        .filter_map(|agent| {
            profile_backend
                .get(agent.inference_profile_id.as_str())
                .map(|backend_id| (agent.agent_id.clone(), (*backend_id).to_string()))
        })
        .collect::<Vec<_>>();

    let readiness_row = crate::commands::status::load_node_readiness(
        &ConfigAccess::local(state.node.clone()),
        node_did,
    )
    .await
    .context("loading agent readiness for model selection")?;
    backends.retain(|backend| {
        !backend_vetoed_by_readiness(
            &backend.backend_id,
            &bindings,
            readiness_row.as_ref(),
            node_did,
        )
    });
    backends.sort_by(|left, right| left.backend_id.cmp(&right.backend_id));

    let mut available = Vec::with_capacity(backends.len());
    for backend in backends {
        let credential_scope =
            matches!(backend.auth, BackendAuth::NodeOAuth { .. }).then_some(node_did);
        let catalog = lookup_backend_observation(&state.node, node_did, &backend.backend_id)
            .await
            .with_context(|| {
                format!(
                    "loading catalog observation for backend {:?}",
                    backend.backend_id
                )
            })?
            .map(|observation| {
                observation
                    .catalog_for(credential_scope)
                    .map(|catalog| catalog.cloned())
            })
            .transpose()?
            .flatten();
        available.push(AvailableBackend { backend, catalog });
    }
    Ok(available)
}

fn backend_vetoed_by_readiness(
    backend_id: &str,
    bindings: &[(String, String)],
    readiness_row: Option<&gents_protocol::row::NodeReadinessRow>,
    node_did: &str,
) -> bool {
    let Some(readiness_row) = readiness_row else {
        return false;
    };
    let summary = match project_node_readiness_summary(Some(readiness_row), node_did) {
        ProjectedNodeReadinessSummary::Observed(summary) => summary,
        ProjectedNodeReadinessSummary::Unknown(_) => return false,
    };

    let bound_agent_ids = bindings
        .iter()
        .filter(|(_, binding_backend_id)| binding_backend_id == backend_id)
        .map(|(agent_id, _)| agent_id.as_str())
        .collect::<Vec<_>>();

    if bound_agent_ids.is_empty() {
        return false;
    }

    bound_agent_ids.into_iter().all(|agent_id| {
        matches!(
            summary.unavailable_agents.get(agent_id),
            Some(
                AgentReadinessUnavailableReason::BackendDisabled
                    | AgentReadinessUnavailableReason::BackendTemporarilyUnavailable
            )
        )
    })
}

pub(super) fn model_list_entries(backends: &[AvailableBackend], bound: &BoundAgent) -> Vec<Value> {
    let mut entries = backends
        .iter()
        .filter_map(|available| {
            available
                .catalog
                .as_ref()
                .map(|catalog| (available, catalog))
        })
        .flat_map(|(available, catalog)| {
            catalog
                .models
                .iter()
                .filter(|model| !model.model_name.trim().is_empty())
                .map(move |model| (available, model))
        })
        .map(|(available, model)| {
            let backend = &available.backend;
            let model_name = model.model_name.as_str();
            let selection_id = model_selection_id(&backend.backend_id, model_name);
            let is_default = bound.inference_profile.backend_id == backend.backend_id
                && bound.inference_profile.model_name == model_name;
            let mut entry = backend_model_summary(backend, model_name, &selection_id, is_default);
            entry["supportedReasoningEfforts"] =
                advertised_effort_options(model.reasoning_efforts.as_deref());
            entry
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

fn advertised_effort_options(efforts: Option<&[ReasoningEffort]>) -> Value {
    let Some(efforts) = efforts else {
        return json!([]);
    };
    let options = efforts
        .iter()
        .filter_map(|effort| {
            let name = effort.as_str();
            matches!(
                name,
                "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
            )
            .then(|| {
                json!({
                    "reasoningEffort": name,
                    "description": format!("{name} reasoning effort"),
                })
            })
        })
        .collect::<Vec<_>>();
    json!(options)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelSelection {
    backend_id: String,
    model_name: String,
}

async fn resolve_model_selection(
    state: &ShimState,
    requested_model: &str,
) -> Result<ModelSelection> {
    let bound = load_bound_agent(state).await?;
    select_advertised_model(
        &available_model_backends(state).await?,
        &bound.inference_profile.backend_id,
        requested_model,
    )
}

fn select_advertised_model(
    backends: &[AvailableBackend],
    current_backend: &str,
    requested_model: &str,
) -> Result<ModelSelection> {
    anyhow::ensure!(
        !requested_model.trim().is_empty(),
        "model selection must be nonempty"
    );
    let explicit =
        requested_model.contains(super::super::bound_behavior::MODEL_SELECTION_SEPARATOR);
    let mut matches = backends
        .iter()
        .flat_map(|available| {
            available
                .catalog
                .iter()
                .flat_map(move |catalog| catalog.models.iter().map(move |model| (available, model)))
        })
        .filter(|(available, model)| {
            if explicit {
                model_selection_id(&available.backend.backend_id, &model.model_name)
                    == requested_model
            } else {
                model.model_name == requested_model
            }
        })
        .collect::<Vec<_>>();
    if !explicit
        && matches
            .iter()
            .any(|(backend, _)| backend.backend.backend_id == current_backend)
    {
        matches.retain(|(backend, _)| backend.backend.backend_id == current_backend);
    }
    anyhow::ensure!(
        !matches.is_empty(),
        "no backend advertises model {requested_model:?} in this node's credential scope"
    );
    anyhow::ensure!(
        matches.len() == 1,
        "ambiguous advertised model {requested_model:?}; specify backend::model"
    );
    let (backend, model) = matches[0];
    Ok(ModelSelection {
        backend_id: backend.backend.backend_id.clone(),
        model_name: model.model_name.clone(),
    })
}

async fn apply_model_to_bound_agent(state: &ShimState, selection: &ModelSelection) -> Result<()> {
    let access = ConfigAccess::Graphql(state.graphql.clone());
    apply_model_to_bound_agent_with_access(state, selection, &access).await
}

async fn apply_model_to_bound_agent_with_access(
    state: &ShimState,
    selection: &ModelSelection,
    access: &ConfigAccess,
) -> Result<()> {
    apply_model_selection(
        access,
        state.node_did.as_ref(),
        state.agent_id.as_ref(),
        selection,
    )
    .await
}

async fn apply_model_selection(
    access: &ConfigAccess,
    owner: &str,
    agent_id: &str,
    selection: &ModelSelection,
) -> Result<()> {
    let new_profile_id = uuid::Uuid::new_v4().to_string();
    access
        .transact_write("codex.model_selection", |txn| {
            let new_profile_id = &new_profile_id;
            Box::pin(async move {
                let (_, value) =
                    read_desired_state_record_in_txn(txn, Collection::Agent, owner, agent_id)
                        .await?
                        .context("bound agent does not exist for this node")?;
                let mut agent: gents::AgentDocument = serde_json::from_value(value)?;
                anyhow::ensure!(agent.enabled, "bound agent is disabled");
                let (_, value) = read_desired_state_record_in_txn(
                    txn,
                    Collection::InferenceProfile,
                    owner,
                    &agent.inference_profile_id,
                )
                .await?
                .context("bound inference profile does not exist for this node")?;
                let mut profile: InferenceProfile = serde_json::from_value(value)?;
                let backend = load_inference_backend_in_txn(txn, owner, &selection.backend_id)
                    .await?
                    .context("selected backend does not exist for this node")?;
                backend.validate()?;
                anyhow::ensure!(backend.enabled, "selected backend is disabled");
                let observation =
                    lookup_backend_observation_in_txn(txn, owner, &selection.backend_id)
                        .await?
                        .context("selected backend has no discovery observation")?;
                let scope = matches!(backend.auth, BackendAuth::NodeOAuth { .. }).then_some(owner);
                let catalog = observation
                    .catalog_for(scope)?
                    .context("selected backend has no catalog in this credential scope")?;
                let mut models = catalog
                    .models
                    .iter()
                    .filter(|model| model.model_name == selection.model_name);
                let model = models
                    .next()
                    .context("selected backend does not advertise requested model")?;
                anyhow::ensure!(models.next().is_none(), "ambiguous advertised model");
                if let (Some(effort), Some(supported)) =
                    (profile.reasoning_effort, model.reasoning_efforts.as_ref())
                {
                    anyhow::ensure!(
                        gents::config::admits_reasoning_effort(&backend, effort, supported),
                        "selected model does not advertise configured reasoning effort {effort:?}"
                    );
                }
                if profile.backend_id == selection.backend_id
                    && profile.model_name == selection.model_name
                {
                    return Ok(());
                }
                profile.profile_id = new_profile_id.clone();
                profile.backend_id = selection.backend_id.clone();
                profile.model_name = selection.model_name.clone();
                agent.inference_profile_id = profile.profile_id.clone();
                let plan = DesiredStateApplyPlan::new(
                    [
                        (Collection::InferenceProfile, serde_json::to_value(profile)?),
                        (Collection::Agent, serde_json::to_value(agent)?),
                    ]
                    .into_iter()
                    .map(|(collection, value)| DesiredStateApplyDocument {
                        collection,
                        add: value.clone(),
                        update: value,
                    })
                    .collect(),
                )?;
                apply_desired_state_plan(txn, &plan).await?;
                Ok(())
            })
        })
        .await
}

#[cfg(test)]
mod tests;
