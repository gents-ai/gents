use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tracing::Instrument;

use crate::runtime::agent::{DocumentResolveContext, Gents};
use gents_automation::triggers::runtime_snapshot::{ResolvedRuntimeSnapshot, UnavailableAgent};
use gents_inference::admission::BackendAdmissionConfig;
use gents_inference::backends::registry;
use gents_tools::surface::{ToolRuntimeContext, ToolSurface};

use super::super::surface_publication::{startup_cause, startup_proceeds, StartupCause};

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
/// runnable while its tools still fail to build. A build that meets an
/// installer waits for it until `publication_deadline`, then demotes the
/// agent with the transient publication cause.
pub(in crate::runtime::agent::runtime) async fn demote_unbuildable_agents(
    system_log: bool,
    tool_runtime: &ToolRuntimeContext,
    snapshot: &mut ResolvedRuntimeSnapshot,
    publication_deadline: tokio::time::Instant,
) -> Result<()> {
    let mut agent_dids = snapshot.agents.keys().cloned().collect::<Vec<_>>();
    agent_dids.sort();

    for agent_did in agent_dids {
        let tool_surface = snapshot
            .tool_surfaces
            .get(&agent_did)
            .ok_or_else(|| anyhow!("missing tool surface for agent_config {agent_did}"))?;
        let agent = snapshot
            .agents
            .get(&agent_did)
            .ok_or_else(|| anyhow!("missing resolved agent {agent_did} during tool admission"))?;
        let tool_runtime = tool_runtime.for_agent(agent);
        if let Err(error) = tool_surface
            .build_tools_until(&tool_runtime, publication_deadline)
            .await
        {
            // Lean `Runtime.StartupDegraded.toolSurfaceReason` decides the cause
            // from the facts the refusal carries: an enabled published root over
            // the tools plus a denial is the missing reader, whose grant the
            // text states; every other refusal is the generic unbuilt surface.
            let refusal = gents_tools::surface::tool_root_refusal(error.as_ref());
            let (publishing, denial) = refusal.map_or(
                (
                    gents_tools::surface::RootPublishing::NoPolicy,
                    gents_tools::surface::RootDenialKind::Admitted,
                ),
                |refusal| (refusal.publishing(), refusal.denial()),
            );
            let cause = gents_tools::surface::tool_surface_reason(publishing, denial);
            let reason = match refusal {
                Some(gents_tools::surface::ToolRootRefusal::ReaderMissing { .. })
                    if cause
                        == gents_protocol::node_readiness::AgentReadinessUnavailableReason::WorkspaceRootReaderMissing =>
                {
                    let grant = refusal.map(ToString::to_string).unwrap_or_default();
                    UnavailableAgent::new(cause, format!("building tool surface: {grant}"))
                        .with_public_message(grant)
                }
                _ => UnavailableAgent::new(cause, format!("building tool surface: {error:#}")),
            }
            .with_publication_busy(
                gents_pack::plugin::store::PublishedPluginReadError::error_chain_is_busy(
                    error.as_ref(),
                ),
            );
            tracing::error!(
                agent_did = %agent_did,
                public_reason = ?reason.public_reason,
                diagnostic = %reason.diagnostic,
                "agent_config tool surface unavailable; running without it"
            );
            crate::runtime::native_logging::mirror_service_diagnostic(
                system_log,
                &format!(
                    "gents runtime: agent_config {agent_did} unavailable: {}",
                    reason.diagnostic
                ),
            );
            snapshot.demote_agent(&agent_did, reason);
        }
    }
    Ok(())
}

/// Whether startup must read the snapshot under the plugin store's
/// publication lease: an agent selects plugin tools, or a held store already
/// left one unavailable. A plugin-free node never depends on the store.
pub(in crate::runtime::agent::runtime) fn depends_on_publication(
    snapshot: &ResolvedRuntimeSnapshot,
) -> bool {
    snapshot
        .tool_surfaces
        .values()
        .any(|surface| surface.has_plugin_tools())
        || snapshot
            .unavailable_agents
            .values()
            .any(|unavailable| unavailable.publication_busy)
}

pub(in crate::runtime::agent::runtime) async fn validate_startup_snapshot(
    agent: &Gents,
    tool_runtime: &ToolRuntimeContext,
    snapshot: &mut ResolvedRuntimeSnapshot,
    publication_deadline: tokio::time::Instant,
) -> Result<()> {
    snapshot.validate_node_readiness_source().with_context(|| {
        format!(
            "validate agent_config readiness source for {}",
            agent.node_did()
        )
    })?;

    demote_unbuildable_agents(
        agent.system_log,
        tool_runtime,
        snapshot,
        publication_deadline,
    )
    .await?;

    if snapshot.agents.is_empty() {
        let mut unavailable = snapshot
            .unavailable_agents
            .iter()
            .map(|(agent_did, reason)| (agent_did.clone(), reason.clone()))
            .collect::<Vec<_>>();
        unavailable.sort_by(|left, right| left.0.cmp(&right.0));

        if unavailable.is_empty() {
            anyhow::bail!(
                "agent {} has no runnable agents at startup",
                agent.node_did()
            );
        }

        let causes = unavailable
            .iter()
            .map(|(_, reason)| startup_cause_of(reason))
            .collect::<Vec<_>>();
        let blocking = unavailable
            .iter()
            .filter(|(_, reason)| startup_cause_of(reason) == StartupCause::Invalid)
            .collect::<Vec<_>>();
        if !startup_proceeds(snapshot.agents.len(), &causes) {
            // Only the presentation-safe reason leaves this boundary.
            for (agent_did, reason) in &blocking {
                tracing::error!(
                    agent_did = %agent_did,
                    public_reason = ?reason.public_reason,
                    diagnostic = %reason.diagnostic,
                    "agent_config unavailable at startup blocks the agent from starting"
                );
                crate::runtime::native_logging::mirror_service_diagnostic(
                    agent.system_log,
                    &format!(
                        "gents runtime: agent_config {agent_did} unavailable at startup: {}",
                        reason.diagnostic
                    ),
                );
            }
            let blocking = blocking
                .iter()
                .map(|(agent_did, reason)| format!("{agent_did}: {}", reason.public_message()))
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
    node: &std::sync::Arc<defra_node::Node>,
    agents: &[gents_tools::surface::AgentWithTools],
    plugins: &gents_toolcalls::plugin::executor::PluginExecutor,
) -> Result<RapidHashMap<String, Arc<ToolSurface>>> {
    let mut tool_surfaces = RapidHashMap::with_capacity(agents.len());
    for agent in agents {
        let tool_surface = agent
            .tools
            .resolve(node, agent.agent.node_did(), plugins)
            .await?;
        tool_surfaces.insert(agent.agent.agent_did.clone(), Arc::new(tool_surface));
    }
    Ok(tool_surfaces)
}

pub(in crate::runtime::agent::runtime) async fn resolve_startup_snapshot(
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
                agent.default_agent_did().to_string(),
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
    node: &std::sync::Arc<defra_node::Node>,
    agents: &[gents_tools::surface::AgentWithTools],
) -> Result<RapidHashMap<String, BackendAdmissionConfig>> {
    let mut configs = RapidHashMap::new();
    for agent in agents {
        let agent = &agent.agent;
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
            let backend = registry::lookup_backend(node, agent.node_did(), backend_id)
                .await?
                .ok_or_else(|| {
                    anyhow!(
                        "agent_config {} references missing backend {}",
                        agent.agent_did,
                        backend_id
                    )
                })?;
            let observation =
                registry::lookup_backend_observation(node, agent.node_did(), backend_id)
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
            agent_did = %agent.agent_did,
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
    node: &std::sync::Arc<defra_node::Node>,
    resolve_context: &DocumentResolveContext,
) -> Result<ResolvedRuntimeSnapshot> {
    crate::runtime::agent::resolve_document_runtime_snapshot(node, resolve_context).await
}

fn startup_cause_of(reason: &UnavailableAgent) -> StartupCause {
    startup_cause(
        is_degraded_startup_unavailable_reason(reason.public_reason),
        reason.public_reason
            == gents_protocol::node_readiness::AgentReadinessUnavailableReason::ToolSurfaceUnavailable,
        reason.publication_busy,
    )
}
