use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tracing::Instrument;

use crate::admission::BackendAdmissionConfig;
use crate::agent::{DocumentResolveContext, Gents};
use crate::backend_registry;
use crate::runtime_snapshot::{ResolvedRuntimeSnapshot, UnavailableAgent};
use crate::tool_surface::{ToolRuntimeContext, ToolSurface};

pub(super) fn is_degraded_startup_unavailable_reason(
    reason: gents_protocol::node_readiness::AgentReadinessUnavailableReason,
) -> bool {
    use gents_protocol::node_readiness::AgentReadinessUnavailableReason as Reason;

    matches!(
        reason,
        Reason::AgentDisabled
            | Reason::BackendNotConfigured
            | Reason::BackendDisabled
            | Reason::BackendTemporarilyUnavailable
            | Reason::CredentialsRequired
    )
}

/// Demotes each agent whose tool surface fails to build to
/// `tool_surface_unavailable`. Startup and the control watcher both run it
/// on every resolved snapshot, so a reconcile cannot bring such an agent back
/// runnable while its tools still fail to build.
pub(in crate::agent::runtime) async fn demote_unbuildable_agents(
    tool_runtime: &ToolRuntimeContext,
    snapshot: &mut ResolvedRuntimeSnapshot,
) -> Result<()> {
    let mut agent_ids = snapshot.agents.keys().cloned().collect::<Vec<_>>();
    agent_ids.sort();

    for agent_id in agent_ids {
        let tool_surface = snapshot
            .tool_surfaces
            .get(&agent_id)
            .ok_or_else(|| anyhow!("missing tool surface for agent_config {agent_id}"))?;
        if let Err(error) = tool_surface.build_tools(tool_runtime).await {
            let reason = UnavailableAgent::new(
                gents_protocol::node_readiness::AgentReadinessUnavailableReason::ToolSurfaceUnavailable,
                format!("building tool surface: {error:#}"),
            );
            tracing::error!(
                agent_id = %agent_id,
                public_reason = ?reason.public_reason,
                diagnostic = %reason.diagnostic,
                "agent_config tool surface unavailable; running without it"
            );
            crate::native_logging::mirror_service_diagnostic(&format!(
                "gents runtime: agent_config {agent_id} unavailable: {}",
                reason.diagnostic
            ));
            snapshot.demote_agent(&agent_id, reason);
        }
    }
    Ok(())
}

pub(in crate::agent::runtime) async fn validate_startup_snapshot(
    agent: &Gents,
    tool_runtime: &ToolRuntimeContext,
    snapshot: &mut ResolvedRuntimeSnapshot,
) -> Result<()> {
    snapshot.validate_node_readiness_source().with_context(|| {
        format!(
            "validate agent_config readiness source for {}",
            agent.node_did()
        )
    })?;

    demote_unbuildable_agents(tool_runtime, snapshot).await?;

    if snapshot.agents.is_empty() {
        let mut unavailable = snapshot
            .unavailable_agents
            .iter()
            .map(|(agent_id, reason)| (agent_id.clone(), reason.clone()))
            .collect::<Vec<_>>();
        unavailable.sort_by(|left, right| left.0.cmp(&right.0));

        if unavailable.is_empty() {
            anyhow::bail!(
                "agent {} has no runnable agents at startup",
                agent.node_did()
            );
        }

        let blocking = unavailable
            .iter()
            .filter(|(_, reason)| !is_degraded_startup_unavailable_reason(reason.public_reason))
            .collect::<Vec<_>>();
        if !blocking.is_empty() {
            // Only the presentation-safe reason leaves this boundary.
            for (agent_id, reason) in &blocking {
                tracing::error!(
                    agent_id = %agent_id,
                    public_reason = ?reason.public_reason,
                    diagnostic = %reason.diagnostic,
                    "agent_config unavailable at startup blocks the agent from starting"
                );
                crate::native_logging::mirror_service_diagnostic(&format!(
                    "gents runtime: agent_config {agent_id} unavailable at startup: {}",
                    reason.diagnostic
                ));
            }
            let blocking = blocking
                .iter()
                .map(|(agent_id, reason)| format!("{agent_id}: {}", reason.public_message()))
                .collect::<Vec<_>>();
            anyhow::bail!(
                "agent {} has no runnable agents at startup due to invalid configuration ({})",
                agent.node_did(),
                blocking.join("; ")
            );
        }
    }

    Ok(())
}

async fn resolve_tool_surfaces(
    node: &std::sync::Arc<defra_node::EmbeddedNode>,
    agents: &[Arc<crate::config::ResolvedAgent>],
    plugins: &crate::plugin::executor::PluginExecutor,
) -> Result<RapidHashMap<String, Arc<ToolSurface>>> {
    let mut tool_surfaces = RapidHashMap::with_capacity(agents.len());
    for agent in agents {
        let tool_surface = agent.tools.resolve(node, agent.node_did(), plugins).await?;
        tool_surfaces.insert(agent.agent_id.clone(), Arc::new(tool_surface));
    }
    Ok(tool_surfaces)
}

pub(in crate::agent::runtime) async fn resolve_startup_snapshot(
    agent: &Gents,
) -> Result<ResolvedRuntimeSnapshot> {
    match agent.document_runtime_context() {
        Some(resolve_context) => {
            resolve_document_snapshot_with_tools(&agent.node, resolve_context).await
        }
        None => {
            let tool_surfaces =
                resolve_tool_surfaces(&agent.node, &agent.agents, agent.plugins()).await?;
            let backend_admission_configs =
                resolve_backend_admission_configs(&agent.node, &agent.agents).await?;
            Ok(ResolvedRuntimeSnapshot::from_parts_with_admission_configs(
                agent.default_agent_id().to_string(),
                agent.agents.clone(),
                tool_surfaces,
                backend_admission_configs,
                agent.unavailable_agents.clone(),
            ))
            .map(|snapshot| {
                snapshot
                    .with_node(agent.runtime_node_arc())
                    .with_local_did(agent.node_did().to_string())
            })
        }
    }
}

async fn resolve_backend_admission_configs(
    node: &std::sync::Arc<defra_node::EmbeddedNode>,
    agents: &[Arc<crate::config::ResolvedAgent>],
) -> Result<RapidHashMap<String, BackendAdmissionConfig>> {
    let mut configs = RapidHashMap::new();
    for agent in agents {
        let Some(backend_id) = agent
            .backend_id
            .as_deref()
            .map(str::trim)
            .filter(|backend_id| !backend_id.is_empty())
        else {
            continue;
        };
        if configs.contains_key(backend_id) {
            continue;
        }
        let (resolved_backend_id, config) = async {
            let backend = backend_registry::lookup_backend(node, agent.node_did(), backend_id)
                .await?
                .ok_or_else(|| {
                    anyhow!(
                        "agent_config {} references missing backend {}",
                        agent.agent_id,
                        backend_id
                    )
                })?;
            let observation =
                backend_registry::lookup_backend_observation(node, agent.node_did(), backend_id)
                    .await?
                    .context("backend observation disappeared during admission resolution")?;
            tracing::Span::current().record("backend_enabled", backend.enabled);
            tracing::Span::current().record(
                "probe_status",
                observation.probe_status.as_deref().unwrap_or("unknown"),
            );
            tracing::Span::current().record("max_concurrent", backend.max_concurrent);
            tracing::Span::current().record("max_queue_depth", backend.max_queue_depth);
            Ok::<_, anyhow::Error>((
                backend.backend_id.clone(),
                BackendAdmissionConfig::from_backend(&backend, &observation)?,
            ))
        }
        .instrument(tracing::info_span!(
            "backend.admission_resolve",
            agent_id = %agent.agent_id,
            backend_id = %backend_id,
            backend_enabled = tracing::field::Empty,
            probe_status = tracing::field::Empty,
            max_concurrent = tracing::field::Empty,
            max_queue_depth = tracing::field::Empty,
        ))
        .await?;
        configs.insert(resolved_backend_id, config);
    }
    Ok(configs)
}

async fn resolve_document_snapshot_with_tools(
    node: &std::sync::Arc<defra_node::EmbeddedNode>,
    resolve_context: &DocumentResolveContext,
) -> Result<ResolvedRuntimeSnapshot> {
    crate::agent::resolve_document_runtime_snapshot(node, resolve_context).await
}
