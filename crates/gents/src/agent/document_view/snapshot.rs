use super::automation::resolve_automation;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use defra_node::EmbeddedNode;
use gents_protocol::node_readiness::AgentReadinessUnavailableReason;

use crate::admission::BackendAvailability;
use crate::config::ResolvedAgent;
use crate::document_config::Agent as AgentDocument;
use crate::runtime_snapshot::{ResolvedRuntimeSnapshot, UnavailableAgent};
use crate::tool_surface::ResolvedToolSelection;

use super::DocumentRuntimeView;

use crate::agent::{
    agent_config_from_documents, assemble_node_and_agents, tool_selection_from_document,
    AgentBuildError, DocumentResolveContext,
};
use crate::identity::RuntimeNode;
use crate::tool_surface::AgentToolConfig;

// The view is already scoped; check again at reference resolution so a foreign
// record cannot satisfy a reference even in an independently constructed view.
macro_rules! owned_doc {
    ($map:expr, $id:expr, $owner:expr) => {{
        (|| -> anyhow::Result<_> {
            let id = $id;
            anyhow::ensure!(
                !id.trim().is_empty(),
                "explicit configuration reference is blank"
            );
            let record = $map
                .get(id)
                .ok_or_else(|| anyhow!("missing {} reference {id:?}", stringify!($map)))?;
            anyhow::ensure!(
                record.value.node_did == $owner,
                "foreign configuration reference {id:?}"
            );
            Ok(&record.value)
        })()
    }};
}

/// A agent_config on a backend this build cannot run is unavailable, not pending:
/// no arriving document repairs it.
#[derive(Debug, thiserror::Error)]
#[error("backend {backend_id} has provider kind {kind} this build does not know")]
struct UnknownKindBackend {
    backend_id: String,
    kind: String,
}

struct AgentResolutionError {
    code: AgentReadinessUnavailableReason,
    detail: anyhow::Error,
}

impl AgentResolutionError {
    fn new(code: AgentReadinessUnavailableReason, detail: anyhow::Error) -> Self {
        Self { code, detail }
    }
}

pub(crate) async fn resolve_document_runtime_snapshot_from_view(
    node: &EmbeddedNode,
    context: &DocumentResolveContext,
    view: &DocumentRuntimeView,
) -> Result<ResolvedRuntimeSnapshot> {
    if !view.node.value.enabled {
        anyhow::bail!("node {} is disabled", view.node.value.node_did);
    }

    let default_agent_id = view
        .node
        .value
        .default_agent_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_default();

    let node_data = RuntimeNode {
        node_did: view.node.value.node_did.clone(),
        identity: context.identity.clone(),
        default_agent_id: default_agent_id.clone(),
        display_name: view.node.value.display_name.clone(),
        enabled: view.node.value.enabled,
    };

    let measured_vetoed = context.backend_health.vetoed_backend_ids().await;
    let mut backend_admission_configs = HashMap::new();
    for record in view.backends.values() {
        let backend = &record.value;
        if let Some(observation) = view.backend_observations.get(&backend.backend_id) {
            backend_admission_configs.insert(
                backend.backend_id.clone(),
                crate::admission::BackendAdmissionConfig::from_backend(backend, observation)?
                    .with_measured_unhealthy(measured_vetoed.contains(&backend.backend_id)),
            );
        }
    }

    let mut unavailable_agents = HashMap::new();
    let mut agent_factories: Vec<
        Box<
            dyn FnOnce(Arc<RuntimeNode>) -> std::result::Result<ResolvedAgent, AgentBuildError>
                + Send,
        >,
    > = Vec::new();

    let all_skills = sorted_skills(view);

    for behavior_record in view.agents.values() {
        let agent_config = &behavior_record.value;
        if !agent_config.enabled {
            unavailable_agents.insert(
                agent_config.agent_id.clone(),
                UnavailableAgent::new(
                    AgentReadinessUnavailableReason::AgentDisabled,
                    format!("agent {} is disabled", agent_config.agent_id),
                ),
            );
            continue;
        }

        let resolved_result: std::result::Result<_, AgentResolutionError> = (|| {
            let scope = view.node.value.node_did.as_str();
            let inference =
                resolve_inference(view, &agent_config.inference_profile_id).map_err(|error| {
                    AgentResolutionError::new(
                        AgentReadinessUnavailableReason::InferenceProfileInvalid,
                        error,
                    )
                })?;
            ensure_inference_available(view, &inference, &backend_admission_configs)?;
            let context_result: Result<_> = (|| {
                anyhow::ensure!(agent_config.node_did == scope, "agent owner mismatch");
                let context = agent_config
                    .context_id
                    .as_deref()
                    .map(|id| owned_doc!(&view.contexts, id, scope))
                    .transpose()?
                    .cloned();
                if let Some(context) = &context {
                    for id in &context.skill_ids {
                        owned_doc!(&view.skills, id.as_str(), scope)?;
                    }
                }
                let compaction = context
                    .as_ref()
                    .and_then(|context| context.compaction_id.as_deref())
                    .map(|id| owned_doc!(&view.compactions, id, scope))
                    .transpose()?
                    .cloned();
                let summary = compaction
                    .as_ref()
                    .and_then(|config| config.inference_profile_id.as_deref())
                    .map(|id| resolve_inference(view, id))
                    .transpose()?;
                if let Some(summary) = &summary {
                    ensure_inference_available(view, summary, &backend_admission_configs)
                        .map_err(|error| error.detail)?;
                }
                let tools = context
                    .as_ref()
                    .and_then(|context| context.tools_id.as_deref())
                    .map(|id| owned_doc!(&view.tools, id, scope))
                    .transpose()?;
                let (tool_selection, agents) = match tools {
                    None => (ResolvedToolSelection::default(), AgentToolConfig::default()),
                    Some(tools) => {
                        tools.validate()?;
                        if let Some(remote) = &tools.remote {
                            for service in &remote.services {
                                owned_doc!(
                                    &view.tool_services,
                                    service.mcp_service_id.as_str(),
                                    scope
                                )?;
                            }
                        }

                        let mut selected = tool_selection_from_document(tools)?;
                        let merged = super::merge_surface_tools(tools, view)?;
                        selected.query_tools = merged.query_tools;
                        // Canonical surfaces own all datastore write declarations.
                        selected.write_tools = merged.write_tools;
                        selected.surface_of_tool = merged.surface_of_tool;
                        let eth = super::expand_eth_tools(tools, view)?;
                        selected.eth_queries = eth.queries;
                        selected.eth_calls = eth.calls;
                        let agents =
                            crate::tool_surface::AgentToolConfig::from_document_with_targets(
                                tools,
                                view.agent_targets.values().map(|record| &record.value),
                            )?;
                        for target in &agents.targets {
                            if target.target_node_did == scope {
                                owned_doc!(&view.agents, target.agent_id.as_str(), scope)?;
                            }
                        }
                        (selected, agents)
                    }
                };
                Ok((context, compaction, summary, tool_selection, agents))
            })();
            let (context, compaction, summary, tools, agents) =
                context_result.map_err(|error| {
                    AgentResolutionError::new(
                        AgentReadinessUnavailableReason::ToolConfigurationInvalid,
                        error,
                    )
                })?;
            Ok((inference, context, compaction, summary, tools, agents))
        })();

        match resolved_result {
            Ok((inference, resolved_context, compaction, summary, tool_selection, agent_tools)) => {
                let agent_id = agent_config.agent_id.clone();
                let behavior_value = agent_config.clone();
                let tool_ceiling = context.tool_ceiling.clone();
                let skill_ids = resolved_context
                    .as_ref()
                    .map(|context| context.skill_ids.as_slice())
                    .unwrap_or(&[]);
                let behavior_skills =
                    crate::skills::effective_skills(&all_skills, &agent_config.node_did, skill_ids)
                        .into_iter()
                        .cloned()
                        .collect::<Vec<_>>();
                let factory: Box<
                    dyn FnOnce(
                            Arc<RuntimeNode>,
                        )
                            -> std::result::Result<ResolvedAgent, AgentBuildError>
                        + Send,
                > = Box::new(move |principal| {
                    agent_config_from_documents(
                        principal,
                        &behavior_value,
                        resolved_context.as_ref(),
                        compaction,
                        summary,
                        &inference,
                        tool_selection,
                        agent_tools,
                        &tool_ceiling,
                        behavior_skills,
                    )
                    .map_err(|error| AgentBuildError {
                        agent_id: agent_id.clone(),
                        error,
                    })
                });
                agent_factories.push(factory);
            }
            Err(error) => {
                unavailable_agents.insert(
                    agent_config.agent_id.clone(),
                    UnavailableAgent::new(error.code, error.detail.to_string()),
                );
            }
        }
    }

    let (principal, agent_results) = assemble_node_and_agents(node_data, agent_factories);

    let mut agents = Vec::<Arc<ResolvedAgent>>::new();
    for result in agent_results {
        match result {
            Ok(agent_arc) => agents.push(agent_arc),
            Err(AgentBuildError { agent_id, error }) => {
                unavailable_agents.insert(
                    agent_id,
                    UnavailableAgent::new(
                        AgentReadinessUnavailableReason::RuntimeConfigurationInvalid,
                        error.to_string(),
                    ),
                );
            }
        }
    }

    let own_node_did = context.identity.did().to_string();
    let candidate_agent_ids = agents
        .iter()
        .map(|agent_config| agent_config.agent_id.clone())
        .collect::<HashSet<_>>();
    let mut behavior_surfaces = Vec::with_capacity(agents.len());
    for agent_config in agents {
        match agent_config
            .tools
            .resolve_with_available_agent_targets(
                node,
                &own_node_did,
                &candidate_agent_ids,
                &context.plugins,
            )
            .await
        {
            Ok(tool_surface) => behavior_surfaces.push((agent_config, tool_surface)),
            Err(error) => {
                unavailable_agents.insert(
                    agent_config.agent_id.clone(),
                    UnavailableAgent::new(
                        AgentReadinessUnavailableReason::ToolSurfaceUnavailable,
                        error.to_string(),
                    ),
                );
            }
        }
    }

    let active_agent_ids = behavior_surfaces
        .iter()
        .map(|(agent_config, _)| agent_config.agent_id.clone())
        .collect::<HashSet<_>>();
    let mut agents = Vec::with_capacity(behavior_surfaces.len());
    let mut tool_surfaces = HashMap::with_capacity(behavior_surfaces.len());
    for (agent_config, mut tool_surface) in behavior_surfaces {
        for target in tool_surface.agent_targets() {
            if target.target_node_did == own_node_did
                && !active_agent_ids.contains(&target.agent_id)
            {
                tracing::warn!(
                    agent_id = %agent_config.agent_id,
                    target_name = %target.name,
                    target_agent_id = %target.agent_id,
                    "dropping LOCAL agent target: target agent is not active \
                     (agent may be disabled or its backend/MCP resolution failed)"
                );
            }
        }
        tool_surface.retain_agent_targets(&own_node_did, &active_agent_ids);
        tool_surfaces.insert(agent_config.agent_id.clone(), Arc::new(tool_surface));
        agents.push(agent_config);
    }

    let automation = resolve_automation(view, &unavailable_agents);
    Ok(ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
        default_agent_id,
        agents,
        tool_surfaces,
        backend_admission_configs,
        unavailable_agents,
    )
    .with_node(principal)
    .with_local_did(context.identity.did().to_string())
    .with_automation(automation))
}

fn ensure_inference_available(
    view: &DocumentRuntimeView,
    inference: &crate::config::ResolvedInference,
    admission_configs: &HashMap<String, crate::admission::BackendAdmissionConfig>,
) -> std::result::Result<(), AgentResolutionError> {
    let scope = view.node.value.node_did.as_str();
    let backend = &inference.backend;
    let admission = admission_configs.get(&backend.backend_id).ok_or_else(|| {
        AgentResolutionError::new(
            AgentReadinessUnavailableReason::BackendNotConfigured,
            anyhow!("backend {} has no matching observation", backend.backend_id),
        )
    })?;
    let unavailable = match admission.availability() {
        BackendAvailability::Available => None,
        BackendAvailability::Disabled => Some(AgentReadinessUnavailableReason::BackendDisabled),
        BackendAvailability::ProbeNotHealthy | BackendAvailability::MeasuredUnhealthy => {
            Some(AgentReadinessUnavailableReason::BackendTemporarilyUnavailable)
        }
    };
    if let Some(code) = unavailable {
        return Err(AgentResolutionError::new(
            code,
            anyhow!(
                "backend {} is unavailable: {:?}",
                backend.backend_id,
                admission.availability()
            ),
        ));
    }
    if matches!(
        backend.auth,
        crate::document_config::BackendAuth::NodeOAuth { .. }
    ) {
        let provider = match backend.provider_kind {
            crate::backend_provider::BackendProviderKind::ChatGptCodex => {
                crate::chatgpt_codex::CHATGPT_CODEX_PROVIDER
            }
            crate::backend_provider::BackendProviderKind::XaiGrokOAuth => {
                crate::xai_grok_oauth::XAI_OAUTH_PROVIDER
            }
            crate::backend_provider::BackendProviderKind::ClaudeCliSubscription => {
                crate::claude_oauth::CLAUDE_OAUTH_PROVIDER
            }
            _ => {
                return Err(AgentResolutionError::new(
                    AgentReadinessUnavailableReason::CredentialsRequired,
                    anyhow!("provider has no node OAuth adapter"),
                ));
            }
        };
        if !view.resolves_oauth_credential(
            provider,
            crate::oauth_credential::AccountPick::Reference(backend.auth.oauth_account_ref()),
        ) {
            return Err(AgentResolutionError::new(
                AgentReadinessUnavailableReason::CredentialsRequired,
                anyhow!(
                    "backend {} requires enabled OAuthCredential for {scope}",
                    backend.backend_id
                ),
            ));
        }
    }
    Ok(())
}

pub(super) fn collect_unresolved_behavior_references(
    view: &DocumentRuntimeView,
    agent_config: &AgentDocument,
    details: &mut Vec<String>,
) {
    let scope = view.node.value.node_did.as_str();
    let result: Result<()> = (|| {
        anyhow::ensure!(agent_config.node_did == scope, "agent owner mismatch");
        select_inference_documents(view, &agent_config.inference_profile_id)?;
        if let Some(id) = &agent_config.context_id {
            let context = owned_doc!(&view.contexts, id.as_str(), scope)?;
            if let Some(id) = &context.compaction_id {
                let compaction = owned_doc!(&view.compactions, id.as_str(), scope)?;
                if let Some(id) = &compaction.inference_profile_id {
                    select_inference_documents(view, id)?;
                }
            }
            if let Some(id) = &context.tools_id {
                owned_doc!(&view.tools, id.as_str(), scope)?;
            }
            for id in &context.skill_ids {
                owned_doc!(&view.skills, id.as_str(), scope)?;
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        if error.downcast_ref::<UnknownKindBackend>().is_none() {
            details.push(format!("agent {}: {error:#}", agent_config.agent_id));
        }
    }
}

struct SelectedInferenceDocuments<'a> {
    profile: &'a crate::document_config::InferenceProfile,
    backend: &'a crate::document_config::InferenceBackend,
    sampling: Option<&'a crate::document_config::InferenceSampling>,
    execution: Option<&'a crate::document_config::InferenceExecution>,
    retry_policy: Option<&'a crate::document_config::InferenceRetryPolicy>,
}

/// Resolve the documents one inference selection names, without judging them.
/// Only a failure here can be repaired by a document arriving, so this is what
/// the control watcher's visibility gate may wait on; `resolve_inference`'s
/// advertised-model, credential and structural validation can be permanently
/// false for an agent the router never selects, and the snapshot reports
/// that per agent as an `UnavailableAgent` instead of blocking.
fn select_inference_documents<'a>(
    view: &'a DocumentRuntimeView,
    id: &str,
) -> Result<SelectedInferenceDocuments<'a>> {
    let scope = view.node.value.node_did.as_str();
    let profile = owned_doc!(&view.inference_profiles, id, scope)?;
    if let Some(kind) = view.unknown_kind_backends.get(&profile.backend_id) {
        return Err(UnknownKindBackend {
            backend_id: profile.backend_id.clone(),
            kind: kind.clone(),
        }
        .into());
    }
    let backend = owned_doc!(&view.backends, profile.backend_id.as_str(), scope)?;
    let sampling = profile
        .sampling_id
        .as_deref()
        .map(|id| owned_doc!(&view.inference_sampling, id, scope))
        .transpose()?;
    let execution = profile
        .execution_id
        .as_deref()
        .map(|id| owned_doc!(&view.inference_execution, id, scope))
        .transpose()?;
    let retry_policy = execution
        .and_then(|execution| execution.retry_policy_id.as_deref())
        .map(|id| owned_doc!(&view.inference_retry_policies, id, scope))
        .transpose()?;
    Ok(SelectedInferenceDocuments {
        profile,
        backend,
        sampling,
        execution,
        retry_policy,
    })
}

fn resolve_inference(
    view: &DocumentRuntimeView,
    id: &str,
) -> Result<crate::config::ResolvedInference> {
    let selected = select_inference_documents(view, id)?;
    let profile = selected.profile.clone();
    let backend = selected.backend.clone();
    anyhow::ensure!(
        !profile.model_name.trim().is_empty(),
        "profile {} has no model selection",
        profile.profile_id
    );
    let sampling = selected.sampling.cloned();
    let execution = selected.execution.cloned();
    let retry_policy = selected.retry_policy.cloned();
    let advertised_model = crate::config::advertised_model_for_profile(
        &backend,
        &profile,
        view.backend_observations.get(&backend.backend_id),
    )?;
    let resolved = crate::config::ResolvedInference {
        backend,
        profile,
        sampling,
        execution,
        retry_policy,
        advertised_model,
    };
    resolved.backend.validate()?;
    resolved.profile.validate()?;
    if let Some(sampling) = &resolved.sampling {
        sampling.validate()?;
    }
    if let Some(execution) = &resolved.execution {
        execution.validate()?;
    }
    if let Some(retry) = &resolved.retry_policy {
        retry.validate()?;
    }
    resolved.context_window()?;
    resolved.max_turns()?;
    resolved.sampling_config()?;
    Ok(resolved)
}

#[cfg(test)]
mod advertised_context_override_tests {
    use crate::config::validate_advertised_context_override;

    #[test]
    fn context_override_admission_matches_lean() {
        let snapshot = crate::lean_vocab_test::lean_contract_snapshot();
        let cases = snapshot.configuration_scope_cases["context_bounds"]
            .as_array()
            .unwrap();
        assert_eq!(cases.len(), 8);
        for case in cases {
            let mut selected = profile(1);
            selected.context_window = case["selected"].as_i64();
            let model = advertised(case["default"].as_i64(), case["maximum"].as_i64());
            assert_eq!(
                validate_advertised_context_override(&selected, &model).is_ok(),
                case["allowed"].as_bool().unwrap(),
                "{case}"
            );
        }
    }

    fn profile(context_window: i64) -> crate::document_config::InferenceProfile {
        serde_json::from_value(serde_json::json!({
            "node_did": "did:test:owner",
            "profile_id": "profile",
            "backend_id": "backend",
            "model_name": "gpt-5.6-sol",
            "context_window": context_window
        }))
        .unwrap()
    }

    fn advertised(
        default: Option<i64>,
        maximum: Option<i64>,
    ) -> crate::document_config::AdvertisedModel {
        crate::document_config::AdvertisedModel {
            model_name: "gpt-5.6-sol".into(),
            display_name: None,
            context_window: default,
            max_context_window: maximum,
            max_output_tokens: None,
            reasoning_efforts: None,
        }
    }

    #[test]
    fn explicit_advertised_context_maximum_accepts_boundary_and_rejects_above() {
        let model = advertised(Some(272_000), Some(872_000));
        assert!(validate_advertised_context_override(&profile(872_000), &model).is_ok());
        assert!(validate_advertised_context_override(&profile(872_001), &model).is_err());
    }

    #[test]
    fn absent_or_invalid_advertised_maximum_does_not_invent_a_runtime_cap() {
        for model in [
            advertised(Some(272_000), None),
            advertised(Some(272_000), Some(0)),
            advertised(Some(272_000), Some(128_000)),
        ] {
            assert!(validate_advertised_context_override(&profile(872_001), &model).is_ok());
        }
    }
}

// The runtime configuration fingerprint is compared across independently
// resolved views. Every collection map is keyed/sorted by the projector;
// skills are the one value vector embedded in ResolvedAgent's Debug value, so
// canonicalize it before both prompt construction and fingerprinting.
pub(super) fn sorted_skills(view: &DocumentRuntimeView) -> Vec<crate::skills::Skill> {
    let mut skills = view
        .skills
        .values()
        .map(|record| skill_from_document(&record.value))
        .collect::<Vec<_>>();
    skills.sort_by(|left, right| left.skill_id.cmp(&right.skill_id));
    skills
}

fn skill_from_document(doc: &crate::document_config::SkillDocument) -> crate::skills::Skill {
    crate::skills::Skill {
        skill_id: doc.skill_id.clone(),
        node_did: doc.node_did.clone(),
        name: doc.name.clone().unwrap_or_default(),
        description: doc.description.clone().unwrap_or_default(),
        instructions: doc.instructions.clone().unwrap_or_default(),
        source_directory: doc.source_directory.clone(),
        tool_refs: doc.tool_refs.clone(),
        display_name: doc.display_name.clone(),
        enabled: doc.enabled,
    }
}
