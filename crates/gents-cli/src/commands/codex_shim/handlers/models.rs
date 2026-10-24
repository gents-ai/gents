use anyhow::{Context, Result};
use gents::backend_registry::{
    list_enabled_backends_for_agent, lookup_backend_observation, lookup_backend_observation_in_txn,
};
use gents::config_client::{
    apply_desired_state_plan, load_inference_backend_in_txn, read_desired_state_record_in_txn,
    DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::document_config::{BackendAuth, BackendModelCatalog};
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
use super::super::CodexShimError;
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
                return Ok(send_error(
                    outbound,
                    request_id,
                    JSONRPC_INVALID_PARAMS,
                    "ConfigValueWrite for `model` requires a non-empty string".to_string(),
                )
                .await?);
            }
        };
        let selection = match resolve_model_selection(state, &new_model_id).await {
            Ok(selection) => selection,
            Err(err) => {
                return Ok(send_error(
                    outbound,
                    request_id,
                    JSONRPC_INVALID_PARAMS,
                    err.to_string(),
                )
                .await?);
            }
        };
        apply_model_to_bound_agent(state, &selection).await?;
    }
    Ok(send_typed_json_result::<codex::ConfigWriteResponse>(
        outbound,
        request_id,
        json!({
            "status": "ok",
            "version": "gents-shim",
            "filePath": absolute_path(&state.codex_home.join("config.toml")),
            "overriddenMetadata": null
        }),
    )
    .await?)
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
            let value =
                read_desired_state_record_in_txn(txn, Collection::Agent, node_did, agent_id)
                    .await?
                    .map(|(_, value)| value)
                    .ok_or_else(|| CodexShimError::BoundAgentMissing {
                        agent_id: agent_id.to_owned(),
                        node_did: node_did.to_owned(),
                    })?;
            Ok(value)
        })
    })
    .await?;
    let agent: gents::AgentDocument =
        serde_json::from_value(raw).map_err(|source| CodexShimError::DecodeBoundAgent {
            agent_id: agent_id.to_owned(),
            source,
        })?;
    Ok(agent)
}

pub(super) async fn load_bound_agent(state: &ShimState) -> Result<BoundAgent> {
    let agent = load_bound_agent_document(state).await?;
    let node_did = state.node_did.as_ref();
    let inference_profile =
        load_inference_profile(&state.node, node_did, &agent.inference_profile_id)
            .await?
            .ok_or_else(|| CodexShimError::BoundInferenceProfileMissing {
                profile_id: agent.inference_profile_id.clone(),
                node_did: node_did.to_owned(),
            })?;
    inference_profile
        .validate()
        .map_err(|error| CodexShimError::BoundInferenceProfileInvalid(Box::new(error)))?;
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
    Ok(select_advertised_model(
        &available_model_backends(state).await?,
        &bound.inference_profile.backend_id,
        requested_model,
    )?)
}

fn select_advertised_model(
    backends: &[AvailableBackend],
    current_backend: &str,
    requested_model: &str,
) -> Result<ModelSelection, CodexShimError> {
    if requested_model.trim().is_empty() {
        return Err(CodexShimError::ModelSelectionEmpty);
    }
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
    if matches.is_empty() {
        return Err(CodexShimError::ModelNotAdvertised {
            model: requested_model.to_owned(),
        });
    }
    if matches.len() != 1 {
        return Err(CodexShimError::ModelAmbiguous {
            model: requested_model.to_owned(),
        });
    }
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
                        .ok_or(CodexShimError::BoundAgentNotFoundForNode)?;
                let mut agent: gents::AgentDocument = serde_json::from_value(value)?;
                if !agent.enabled {
                    return Err(CodexShimError::BoundAgentDisabled.into());
                }
                let (_, value) = read_desired_state_record_in_txn(
                    txn,
                    Collection::InferenceProfile,
                    owner,
                    &agent.inference_profile_id,
                )
                .await?
                .ok_or(CodexShimError::BoundInferenceProfileNotFoundForNode)?;
                let mut profile: InferenceProfile = serde_json::from_value(value)?;
                let backend = load_inference_backend_in_txn(txn, owner, &selection.backend_id)
                    .await?
                    .ok_or(CodexShimError::SelectedBackendNotFound)?;
                backend.validate()?;
                if !backend.enabled {
                    return Err(CodexShimError::SelectedBackendDisabled.into());
                }
                let observation =
                    lookup_backend_observation_in_txn(txn, owner, &selection.backend_id)
                        .await?
                        .ok_or(CodexShimError::SelectedBackendNoObservation)?;
                let scope = matches!(backend.auth, BackendAuth::NodeOAuth { .. }).then_some(owner);
                let catalog = observation
                    .catalog_for(scope)?
                    .ok_or(CodexShimError::SelectedBackendNoCatalog)?;
                let mut models = catalog
                    .models
                    .iter()
                    .filter(|model| model.model_name == selection.model_name);
                let model = models
                    .next()
                    .ok_or(CodexShimError::SelectedModelNotAdvertised)?;
                if models.next().is_some() {
                    return Err(CodexShimError::SelectedModelAmbiguous.into());
                }
                if let (Some(effort), Some(supported)) =
                    (profile.reasoning_effort, model.reasoning_efforts.as_ref())
                {
                    if !gents::config::admits_reasoning_effort(&backend, effort, supported) {
                        return Err(CodexShimError::ReasoningEffortUnsupported {
                            effort: format!("{effort:?}"),
                        }
                        .into());
                    }
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
mod tests {
    use super::*;
    use crate::config_writes::write_agent_document;
    use gents::config_client::{
        write_inference_backend_document, write_inference_profile_document,
    };
    use gents::defra_node::EmbeddedNode;
    use gents::document_config::AdvertisedModel;
    use gents::{record_model_catalog_in_txn, BackendProviderKind};
    use gents_protocol::error_chain::ErrorChain;
    use gents_protocol::row::{
        AgentReadinessEntry, AgentReadinessState, AgentReadinessUnavailableReason,
        NodeReadinessProcessState, NodeReadinessRow, NodeReadinessSnapshot,
        NODE_READINESS_FORMAT_VERSION,
    };
    use std::sync::Arc;

    fn agent(agent_id: &str, profile_id: &str) -> gents::AgentDocument {
        gents::AgentDocument {
            agent_id: agent_id.to_string(),
            node_did: "did:test:codex-shim".to_string(),
            display_name: None,
            description: None,
            context_id: None,
            inference_profile_id: profile_id.to_string(),
            enabled: true,
            tags: Vec::new(),
            created_at: None,
        }
    }

    fn backend(backend_id: &str) -> InferenceBackend {
        InferenceBackend {
            node_did: "did:test:codex-shim".to_string(),
            backend_id: backend_id.to_string(),
            name: "Backend A".to_string(),
            provider_kind: BackendProviderKind::OpenAiCompatible,
            openai_wire_api: None,
            endpoint: "http://127.0.0.1:11434/v1".to_string(),
            auth: BackendAuth::Unauthenticated,
            connect_timeout_secs: None,
            discovery_timeout_secs: None,
            max_concurrent: None,
            max_queue_depth: None,
            enabled: true,
            tags: Vec::new(),
        }
    }

    fn profile(profile_id: &str, model_name: &str) -> InferenceProfile {
        InferenceProfile {
            node_did: "did:test:codex-shim".to_string(),
            profile_id: profile_id.to_string(),
            display_name: None,
            description: None,
            backend_id: "backend-a".to_string(),
            model_name: model_name.to_string(),
            reasoning_effort: None,
            service_tier: None,
            context_window: None,
            max_output_tokens: None,
            sampling_id: None,
            execution_id: None,
            tags: Vec::new(),
        }
    }

    fn advertised(model_name: &str) -> AdvertisedModel {
        AdvertisedModel {
            model_name: model_name.to_string(),
            display_name: None,
            context_window: None,
            max_context_window: None,
            max_output_tokens: None,
            reasoning_efforts: None,
        }
    }

    fn readiness_row(
        node_did: &str,
        default_agent_id: &str,
        entries: Vec<(&str, bool)>,
        updated_at: &str,
    ) -> NodeReadinessRow {
        NodeReadinessRow {
            node_did: node_did.to_string(),
            snapshot_json: serde_json::to_string(&NodeReadinessSnapshot {
                format_version: NODE_READINESS_FORMAT_VERSION,
                process_state: NodeReadinessProcessState::Ready,
                active_generation: 1,
                router_generation: 1,
                default_agent_id: default_agent_id.to_string(),
                agents: entries
                    .into_iter()
                    .map(|(agent_id, ready)| AgentReadinessEntry {
                        agent_id: agent_id.to_string(),
                        state: if ready {
                            AgentReadinessState::Ready
                        } else {
                            AgentReadinessState::Unavailable
                        },
                        reason: if ready {
                            None
                        } else {
                            Some(AgentReadinessUnavailableReason::BackendTemporarilyUnavailable)
                        },
                    })
                    .collect(),
            })
            .unwrap(),
            updated_at: updated_at.to_string(),
        }
    }

    #[test]
    fn backend_vetoed_when_every_bound_agent_is_explicitly_vetoed() {
        let node_did = "did:test:codex-shim";
        let bindings = vec![("default".to_string(), "backend-a".to_string())];
        let row = readiness_row(
            node_did,
            "default",
            vec![("default", false)],
            "2026-09-03T11:59:50Z",
        );

        assert!(
            backend_vetoed_by_readiness("backend-a", &bindings, Some(&row), node_did),
            "a backend the local prober vetoed via readiness must not be offered for \
             selection even though the InferenceBackend document itself would read healthy"
        );
    }

    #[test]
    fn backend_not_vetoed_when_bound_agent_is_ready() {
        let node_did = "did:test:codex-shim";
        let bindings = vec![("default".to_string(), "backend-a".to_string())];
        let row = readiness_row(
            node_did,
            "default",
            vec![("default", true)],
            "2026-09-03T11:59:50Z",
        );

        assert!(!backend_vetoed_by_readiness(
            "backend-a",
            &bindings,
            Some(&row),
            node_did,
        ));
    }

    #[test]
    fn missing_readiness_row_leaves_configured_backend_offered() {
        let node_did = "did:test:codex-shim";
        let bindings = vec![("default".to_string(), "backend-a".to_string())];

        assert!(!backend_vetoed_by_readiness(
            "backend-a",
            &bindings,
            None,
            node_did,
        ));
    }

    #[test]
    fn backend_with_no_bound_agents_is_not_vetoed() {
        let node_did = "did:test:codex-shim";
        let bindings = vec![("default".to_string(), "backend-a".to_string())];
        let row = readiness_row(
            node_did,
            "default",
            vec![("default", true)],
            "2026-09-03T11:59:50Z",
        );

        assert!(!backend_vetoed_by_readiness(
            "backend-b",
            &bindings,
            Some(&row),
            node_did,
        ));
    }

    #[test]
    fn model_list_entries_advertise_discovered_models_and_efforts() {
        let catalog = BackendModelCatalog {
            node_did: None,
            observed_at: "2026-09-03T12:00:00Z".to_string(),
            models: vec![
                AdvertisedModel {
                    model_name: "model-y".to_string(),
                    display_name: None,
                    context_window: None,
                    max_context_window: None,
                    max_output_tokens: None,
                    reasoning_efforts: None,
                },
                AdvertisedModel {
                    model_name: "model-x".to_string(),
                    display_name: None,
                    context_window: Some(32_000),
                    max_context_window: None,
                    max_output_tokens: None,
                    reasoning_efforts: Some(vec![ReasoningEffort::High, ReasoningEffort::Max]),
                },
            ],
        };
        let available = AvailableBackend {
            backend: backend("backend-a"),
            catalog: Some(catalog),
        };
        let bound = BoundAgent {
            inference_profile: profile("profile-x", "model-x"),
        };

        let entries = model_list_entries(&[available], &bound);
        assert_eq!(
            entries.len(),
            2,
            "each advertised model is listed once; efforts never become synthetic model variants"
        );
        let x = entries
            .iter()
            .find(|entry| entry["id"] == json!("backend-a::model-x"))
            .expect("advertised model-x listed");
        assert_eq!(
            x["isDefault"],
            json!(true),
            "the bound profile's model is default"
        );
        let efforts = x["supportedReasoningEfforts"].as_array().unwrap();
        assert_eq!(
            efforts.len(),
            1,
            "efforts outside the Codex protocol vocabulary are omitted, not invented: {efforts:?}"
        );
        assert_eq!(efforts[0]["reasoningEffort"], json!("high"));
        let y = entries
            .iter()
            .find(|entry| entry["id"] == json!("backend-a::model-y"))
            .expect("advertised model-y listed");
        assert_eq!(y["isDefault"], json!(false));
        assert_eq!(
            y["supportedReasoningEfforts"],
            json!([]),
            "unknown effort sets advertise no selectable efforts"
        );
    }

    async fn seed_backend_with_catalog(
        access: &ConfigAccess,
        node: &Arc<EmbeddedNode>,
        models: Vec<AdvertisedModel>,
    ) -> InferenceBackend {
        let backend = backend("backend-a");
        write_inference_backend_document(access, &backend)
            .await
            .expect("seed backend");
        ConfigAccess::transact_local_write(node, None, "codex.models.test.catalog", |txn| {
            let backend = &backend;
            let models = models.clone();
            Box::pin(async move {
                record_model_catalog_in_txn(
                    txn,
                    backend,
                    BackendModelCatalog {
                        node_did: None,
                        observed_at: chrono::Utc::now().to_rfc3339(),
                        models,
                    },
                )
                .await
            })
        })
        .await
        .expect("seed catalog");
        backend
    }

    #[tokio::test]
    async fn apply_model_to_bound_agent_rejects_an_unadvertised_model() {
        let node = Arc::new(
            crate::shared::test_support::node_builder()
                .build()
                .await
                .unwrap(),
        );
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let node_did = "did:test:codex-shim";
        let access = ConfigAccess::local(node.clone());
        seed_backend_with_catalog(
            &access,
            &node,
            vec![advertised("model-x"), advertised("not-advertised")],
        )
        .await;

        let mut unadvertised = profile("profile-unadvertised", "not-advertised");
        unadvertised.backend_id = "backend-a".to_string();
        write_inference_profile_document(&access, &unadvertised)
            .await
            .expect("seed profile while its model is advertised");
        write_inference_profile_document(&access, &profile("profile-x", "model-x"))
            .await
            .expect("seed advertised profile");
        write_agent_document(&access, &agent("default", "profile-x"))
            .await
            .expect("seed agent");
        seed_backend_with_catalog(&access, &node, vec![advertised("model-x")]).await;

        let selection = ModelSelection {
            backend_id: "backend-a".into(),
            model_name: "not-advertised".into(),
        };

        let error = apply_model_selection(&access, node_did, "default", &selection)
            .await
            .expect_err("an unadvertised model must be rejected");
        assert!(
            format!("{}", ErrorChain(error.as_ref())).contains("does not advertise"),
            "{}",
            ErrorChain(error.as_ref())
        );
    }

    #[tokio::test]
    async fn apply_model_to_bound_agent_accepts_an_advertised_model() {
        let node = Arc::new(
            crate::shared::test_support::node_builder()
                .build()
                .await
                .unwrap(),
        );
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let node_did = "did:test:codex-shim";
        let access = ConfigAccess::local(node.clone());
        seed_backend_with_catalog(
            &access,
            &node,
            vec![advertised("model-x"), advertised("model-y")],
        )
        .await;

        write_inference_profile_document(&access, &profile("profile-x", "model-x"))
            .await
            .expect("seed advertised profile-x");
        write_inference_profile_document(&access, &profile("profile-y", "model-y"))
            .await
            .expect("seed advertised profile-y");
        write_agent_document(&access, &agent("default", "profile-x"))
            .await
            .expect("seed agent");

        let selection = ModelSelection {
            backend_id: "backend-a".into(),
            model_name: "model-y".into(),
        };

        apply_model_selection(&access, node_did, "default", &selection)
            .await
            .expect("an advertised profile must pass validation and commit");
        let updated = gents::load_agent(node.as_ref(), "default")
            .await
            .expect("reload agent")
            .expect("agent remains present");
        assert_ne!(updated.inference_profile_id, "profile-y");
        assert_ne!(updated.inference_profile_id, "profile-x");
        let selected =
            gents::load_inference_profile(&node, node_did, &updated.inference_profile_id)
                .await
                .expect("reload profile")
                .expect("selected profile remains present");
        assert_eq!(selected.backend_id, "backend-a");
        assert_eq!(selected.model_name, "model-y");
        assert_eq!(
            model_selection_id(&selected.backend_id, &selected.model_name),
            "backend-a::model-y"
        );
    }
    #[test]
    fn selection_requires_exact_unambiguous_advertisement() {
        let make = |id: &str, names: &[&str]| AvailableBackend {
            backend: backend(id),
            catalog: Some(BackendModelCatalog {
                node_did: None,
                observed_at: "2026-09-03T12:00:00Z".into(),
                models: names.iter().map(|name| advertised(name)).collect(),
            }),
        };
        let backends = vec![make("a", &["model"]), make("b", &["model"])];
        assert!(select_advertised_model(&backends, "other", "model").is_err());
        assert_eq!(
            select_advertised_model(&backends, "a", "model")
                .unwrap()
                .backend_id,
            "a"
        );
        assert_eq!(
            select_advertised_model(&backends, "a", "b::model")
                .unwrap()
                .backend_id,
            "b"
        );
        assert!(select_advertised_model(&backends, "a", " b::model").is_err());
        assert!(select_advertised_model(&backends, "a", "missing").is_err());
        assert!(select_advertised_model(
            &[AvailableBackend {
                backend: backend("a"),
                catalog: None
            }],
            "a",
            "a::model"
        )
        .is_err());
        let collision = vec![make("a", &["b::c"]), make("a::b", &["c"])];
        assert!(select_advertised_model(&collision, "a", "a::b::c").is_err());
    }

    #[tokio::test]
    async fn model_change_copies_controls_and_preserves_shared_profiles_and_noop() {
        let node = Arc::new(
            crate::shared::test_support::node_builder()
                .build()
                .await
                .unwrap(),
        );
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::local(node.clone());
        let owner = "did:test:codex-shim";
        seed_backend_with_catalog(&access, &node, vec![advertised("x"), advertised("y")]).await;
        let plan = DesiredStateApplyPlan::new(
            [
                (
                    Collection::InferenceSampling,
                    json!({"node_did":owner,"sampling_id":"sample","temperature":0.4}),
                ),
                (
                    Collection::InferenceExecution,
                    json!({"node_did":owner,"execution_id":"exec","max_turns":1000}),
                ),
            ]
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
        )
        .unwrap();
        access
            .transact_write("test.controls", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await })
            })
            .await
            .unwrap();
        let mut original = profile("shared", "x");
        original.sampling_id = Some("sample".into());
        original.execution_id = Some("exec".into());
        original.context_window = Some(32000);
        original.max_output_tokens = Some(8192);
        original.reasoning_effort = Some(ReasoningEffort::High);
        original.tags = vec!["keep".into()];
        write_inference_profile_document(&access, &original)
            .await
            .unwrap();
        write_inference_profile_document(&access, &profile("other-policy", "y"))
            .await
            .unwrap();
        for name in ["selected", "neighbor"] {
            write_agent_document(&access, &agent(name, "shared"))
                .await
                .unwrap();
        }
        let selection = ModelSelection {
            backend_id: "backend-a".into(),
            model_name: "y".into(),
        };
        apply_model_selection(&access, owner, "selected", &selection)
            .await
            .unwrap();
        let selected = gents::load_agent(&node, "selected").await.unwrap().unwrap();
        assert_ne!(selected.inference_profile_id, "shared");
        assert_ne!(selected.inference_profile_id, "other-policy");
        let new_profile =
            gents::load_inference_profile(&node, owner, &selected.inference_profile_id)
                .await
                .unwrap()
                .unwrap();
        let mut expected = original.clone();
        expected.profile_id = selected.inference_profile_id.clone();
        expected.model_name = "y".into();
        assert_eq!(new_profile, expected);
        assert_eq!(
            gents::load_inference_profile(&node, owner, "shared")
                .await
                .unwrap()
                .unwrap(),
            original
        );
        assert_eq!(
            gents::load_agent(&node, "neighbor")
                .await
                .unwrap()
                .unwrap()
                .inference_profile_id,
            "shared"
        );
        let before = gents::list_inference_profile_records(&node, owner)
            .await
            .unwrap()
            .len();
        apply_model_selection(&access, owner, "selected", &selection)
            .await
            .unwrap();
        assert_eq!(
            gents::list_inference_profile_records(&node, owner)
                .await
                .unwrap()
                .len(),
            before
        );
        assert_eq!(
            gents::load_agent(&node, "selected").await.unwrap().unwrap(),
            selected
        );
        let escaped = gents::graphql::escape_graphql_string(&selected.inference_profile_id);
        let response = node.execute(&format!(r#"mutation {{ update_InferenceProfile(filter: {{node_did: {{_eq: "did:test:codex-shim"}}, profile_id: {{_eq: "{escaped}"}}}}, input: {{sampling_id:"missing"}}) {{_docID}} }}"#)).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        assert!(apply_model_selection(
            &access,
            owner,
            "selected",
            &ModelSelection {
                backend_id: "backend-a".into(),
                model_name: "x".into()
            }
        )
        .await
        .is_err());
        assert_eq!(
            gents::list_inference_profile_records(&node, owner)
                .await
                .unwrap()
                .len(),
            before
        );
        assert_eq!(
            gents::load_agent(&node, "selected").await.unwrap().unwrap(),
            selected
        );
        write_inference_profile_document(&access, &new_profile)
            .await
            .unwrap();
        let mut unsupported = advertised("y");
        unsupported.reasoning_efforts = Some(vec![ReasoningEffort::Low]);
        seed_backend_with_catalog(&access, &node, vec![unsupported]).await;
        assert!(
            apply_model_selection(&access, owner, "selected", &selection)
                .await
                .is_err()
        );
        assert_eq!(
            gents::list_inference_profile_records(&node, owner)
                .await
                .unwrap()
                .len(),
            before
        );
        assert_eq!(
            gents::load_agent(&node, "selected").await.unwrap().unwrap(),
            selected
        );
    }
    #[tokio::test]
    async fn model_list_scope_excludes_foreign_malformed_backend_before_decode() {
        let node = Arc::new(
            crate::shared::test_support::node_builder()
                .build()
                .await
                .unwrap(),
        );
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::local(node.clone());
        write_inference_backend_document(&access, &backend("same"))
            .await
            .unwrap();
        let response = node.execute(r#"mutation { create_InferenceBackend(input: {node_did:"did:test:foreign",backend_id:"same",name:"Foreign",provider_kind:"OpenAiCompatible",endpoint:"http://127.0.0.1:1",enabled:true,auth:{kind:"invalid"}}) {_docID} }"#).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let own = list_enabled_backends_for_agent(&node, "did:test:codex-shim")
            .await
            .unwrap();
        assert_eq!(own.len(), 1);
        assert_eq!(own[0].node_did, "did:test:codex-shim");
        assert!(list_enabled_backends_for_agent(&node, "did:test:foreign")
            .await
            .is_err());
    }
}
