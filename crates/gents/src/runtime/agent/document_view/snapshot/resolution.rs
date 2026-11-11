use super::{
    append_inference_selection, ensure_inference_available, resolve_inference,
    DocumentResolveContext, DocumentRuntimeView,
};
use anyhow::{anyhow, Result};
use defra_node::Node as EmbeddedNode;
use gents_documents::Agent as AgentDocument;
use gents_protocol::node_readiness::AgentReadinessUnavailableReason;
use rapidhash::{HashMapExt, RapidHashMap, RapidHashSet};
use std::sync::Arc;

use crate::runtime::agent::tool_selection_from_document;
use gents_automation::triggers::runtime_snapshot::UnavailableAgent;
use gents_documents::AgentToolConfig;
use gents_tools::surface::{AgentWithTools, ResolvedToolSelection, ToolSurface};

type AgentContextDocuments = (
    Option<gents_documents::AgentContext>,
    Option<gents_documents::CompactionConfig>,
    Option<gents_inference::resolved::ResolvedInference>,
    ResolvedToolSelection,
    AgentToolConfig,
);

pub(super) async fn backend_admission_configs(
    context: &DocumentResolveContext,
    view: &DocumentRuntimeView,
) -> Result<(
    RapidHashMap<String, gents_inference::admission::BackendAdmissionConfig>,
    RapidHashMap<String, gents_inference::backends::health::BackendHealthSnapshot>,
)> {
    let measured_health = context.backend_health.snapshot().await;
    let mut backend_admission_configs = RapidHashMap::new();
    for record in view.backends.values() {
        let backend = &record.value;
        if let Some(observation) = view.backend_observations.get(&backend.backend_id) {
            backend_admission_configs.insert(
                backend.backend_id.clone(),
                gents_inference::admission::BackendAdmissionConfig::from_backend(
                    backend,
                    observation,
                )?
                .with_measured_unhealthy(
                    measured_health
                        .get(&backend.backend_id)
                        .and_then(|health| health.state_for(backend))
                        .is_some_and(|state| state.blocks_routing()),
                ),
            );
        }
    }
    Ok((backend_admission_configs, measured_health))
}

pub(super) fn agent_context_documents(
    view: &DocumentRuntimeView,
    backend_admission_configs: &RapidHashMap<
        String,
        gents_inference::admission::BackendAdmissionConfig,
    >,
    measured_health: &RapidHashMap<
        String,
        gents_inference::backends::health::BackendHealthSnapshot,
    >,
    agent_config: &AgentDocument,
) -> Result<AgentContextDocuments> {
    let scope = view.node.value.node_did.as_str();
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
        ensure_inference_available(view, summary, backend_admission_configs, measured_health)
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
                    owned_doc!(&view.tool_services, service.mcp_service_id.as_str(), scope)?;
                }
            }

            let mut selected = tool_selection_from_document(tools)?;
            let merged = super::super::merge_surface_tools(tools, view)?;
            selected.query_tools = merged.query_tools;
            selected.write_tools = merged.write_tools;
            selected.surface_of_tool = merged.surface_of_tool;
            let agents = AgentToolConfig::from_document_with_targets(
                tools,
                view.agent_targets.values().map(|record| &record.value),
            )?;
            for target in &agents.targets {
                if target.target_node_did == scope {
                    owned_doc!(&view.agents, target.agent_did.as_str(), scope)?;
                }
            }
            (selected, agents)
        }
    };
    Ok((context, compaction, summary, tool_selection, agents))
}

pub(super) fn agent_configuration_documents(
    view: &DocumentRuntimeView,
    behavior_record: &super::super::DocumentRecord<AgentDocument>,
    inference: &gents_inference::resolved::ResolvedInference,
    summary: Option<&gents_inference::resolved::ResolvedInference>,
    resolved_context: Option<&gents_documents::AgentContext>,
) -> Result<Vec<gents_inference::resolved::SelectedConfigurationDocument>> {
    let mut configuration_documents =
        vec![gents_inference::resolved::SelectedConfigurationDocument {
            collection: gents_store::collection::Collection::Agent,
            doc_id: behavior_record.doc_id.clone(),
            logical_id: None,
        }];
    append_inference_selection(view, inference, &mut configuration_documents)?;
    if let Some(summary) = summary {
        append_inference_selection(view, summary, &mut configuration_documents)?;
    }
    if let Some(context) = resolved_context {
        let context_record = view
            .contexts
            .get(&context.context_id)
            .ok_or_else(|| anyhow!("resolved AgentContext disappeared"))?;
        configuration_documents.push(gents_inference::resolved::SelectedConfigurationDocument {
            collection: gents_store::collection::Collection::AgentContext,
            doc_id: context_record.doc_id.clone(),
            logical_id: None,
        });
        for id in &context.skill_ids {
            let record = view
                .skills
                .get(id)
                .ok_or_else(|| anyhow!("resolved Skill disappeared"))?;
            configuration_documents.push(
                gents_inference::resolved::SelectedConfigurationDocument {
                    collection: gents_store::collection::Collection::Skill,
                    doc_id: record.doc_id.clone(),
                    logical_id: Some(id.clone()),
                },
            );
        }
        if let Some(id) = &context.compaction_id {
            let record = view
                .compactions
                .get(id)
                .ok_or_else(|| anyhow!("resolved CompactionConfig disappeared"))?;
            configuration_documents.push(
                gents_inference::resolved::SelectedConfigurationDocument {
                    collection: gents_store::collection::Collection::Compaction,
                    doc_id: record.doc_id.clone(),
                    logical_id: None,
                },
            );
        }
        if let Some(tools_id) = &context.tools_id {
            let tools_record = view
                .tools
                .get(tools_id)
                .ok_or_else(|| anyhow!("resolved Tools disappeared"))?;
            configuration_documents.push(
                gents_inference::resolved::SelectedConfigurationDocument {
                    collection: gents_store::collection::Collection::Tools,
                    doc_id: tools_record.doc_id.clone(),
                    logical_id: None,
                },
            );
        }
    }
    Ok(configuration_documents)
}

pub(super) async fn resolve_behavior_surfaces(
    node: &Arc<EmbeddedNode>,
    context: &DocumentResolveContext,
    own_node_did: &str,
    agents: Vec<AgentWithTools>,
    unavailable_agents: &mut RapidHashMap<String, UnavailableAgent>,
) -> Vec<(AgentWithTools, ToolSurface)> {
    let candidate_agent_dids = agents
        .iter()
        .map(|agent_config| agent_config.agent.agent_did.clone())
        .collect::<RapidHashSet<_>>();
    let mut behavior_surfaces = Vec::with_capacity(agents.len());
    for agent_config in agents {
        match agent_config
            .tools
            .resolve_with_available_agent_targets(
                node,
                own_node_did,
                &candidate_agent_dids,
                &*context.plugins,
            )
            .await
        {
            Ok(tool_surface) => behavior_surfaces.push((agent_config, tool_surface)),
            Err(error) => {
                unavailable_agents.insert(
                    agent_config.agent.agent_did.clone(),
                    UnavailableAgent::new(
                        AgentReadinessUnavailableReason::ToolSurfaceUnavailable,
                        error.to_string(),
                    ),
                );
            }
        }
    }

    behavior_surfaces
}

pub(super) fn retain_active_agent_targets(
    own_node_did: &str,
    behavior_surfaces: Vec<(AgentWithTools, ToolSurface)>,
) -> (
    Vec<AgentWithTools>,
    RapidHashMap<String, Arc<ToolSurface>>,
) {
    let active_agent_dids = behavior_surfaces
        .iter()
        .map(|(agent_config, _)| agent_config.agent.agent_did.clone())
        .collect::<RapidHashSet<_>>();
    let mut agents = Vec::with_capacity(behavior_surfaces.len());
    let mut tool_surfaces = RapidHashMap::with_capacity(behavior_surfaces.len());
    for (agent_config, mut tool_surface) in behavior_surfaces {
        for target in tool_surface.agent_targets() {
            if target.target_node_did == own_node_did
                && !active_agent_dids.contains(&target.agent_did)
            {
                tracing::warn!(
                    agent_did = %agent_config.agent.agent_did,
                    target_name = %target.name,
                    target_agent_did = %target.agent_did,
                    "dropping LOCAL agent target: target agent is not active \
                     (agent may be disabled or its backend/MCP resolution failed)"
                );
            }
        }
        tool_surface.retain_agent_targets(own_node_did, &active_agent_dids);
        tool_surfaces.insert(agent_config.agent.agent_did.clone(), Arc::new(tool_surface));
        agents.push(agent_config);
    }
    (agents, tool_surfaces)
}

pub(super) fn attach_watcher_identities(
    context: &DocumentResolveContext,
    view: &DocumentRuntimeView,
    unavailable_agents: &mut RapidHashMap<String, UnavailableAgent>,
) {
    // Availability and locally verifiable identity are independent: the watcher
    // must still observe work for a disabled or otherwise unrunnable agent.
    for (agent_did, reason) in unavailable_agents {
        let Some(record) = view.agents.get(agent_did) else {
            continue;
        };
        if record.value.node_did != context.identity.did() {
            continue;
        }
        if let Ok(identity) = gents_store::identity::resolve_agent_identity(
            context.identity.as_ref(),
            &record.value.identity_binding(),
        ) {
            if identity.did() != context.identity.did() {
                reason.watcher_identity = Some(
                    gents_automation::triggers::runtime_snapshot::ResolvedWatcherIdentity(identity),
                );
            }
        }
    }
}
