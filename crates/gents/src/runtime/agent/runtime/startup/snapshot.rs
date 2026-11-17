use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tracing::Instrument;

use crate::runtime::agent::{DocumentResolveContext, Gents};
use gents_automation::triggers::runtime_snapshot::{ResolvedRuntimeSnapshot, UnavailableAgent};
use gents_inference::admission::BackendAdmissionConfig;
use gents_inference::backends::registry;
use gents_tools::surface::{ToolRuntimeContext, ToolSurface};

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

pub(in crate::runtime::agent::runtime) fn preserve_generation_for_surface_error(
    error: &anyhow::Error,
) -> bool {
    gents_pack::plugin::store::PublishedPluginReadError::error_chain_is_busy(error.as_ref())
}

/// Admission shares the caller's bounded publication epoch; exhaustion
/// demotes only the plugin surfaces still busy, never the whole node.
pub(in crate::runtime::agent::runtime) async fn demote_unbuildable_agents_until(
    tool_runtime: &ToolRuntimeContext,
    snapshot: &mut ResolvedRuntimeSnapshot,
    deadline: tokio::time::Instant,
    defer_busy: bool,
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
            .build_tools_until(&tool_runtime, deadline)
            .await
        {
            if defer_busy && preserve_generation_for_surface_error(&error) {
                return Err(error)
                    .context("plugin publication deferred; keeping the active runtime generation");
            }
            // Preserve B5's Lean-owned root-denial classification, and B12's
            // typed publication cause. Raw error context stays diagnostic-only.
            let refusal = gents_tools::surface::tool_root_refusal(error.as_ref());
            let (publishing, denial) = refusal.map_or(
                (
                    gents_tools::surface::RootPublishing::NoPolicy,
                    gents_tools::surface::RootDenialKind::Admitted,
                ),
                |refusal| (refusal.publishing(), refusal.denial()),
            );
            let cause = gents_tools::surface::tool_surface_reason(publishing, denial);
            let reason = UnavailableAgent::new(cause, format!("building tool surface: {error:#}"))
                .with_error_cause(&error);
            tracing::error!(
                agent_did = %agent_did,
                public_reason = ?reason.public_reason,
                diagnostic = %reason.diagnostic,
                "agent_config tool surface unavailable; running without it"
            );
            crate::runtime::native_logging::mirror_service_diagnostic(&format!(
                "gents runtime: agent_config {agent_did} unavailable: {}",
                reason.diagnostic
            ));
            snapshot.demote_agent(&agent_did, reason);
        }
    }
    Ok(())
}

#[cfg(test)]
pub(in crate::runtime::agent::runtime) async fn validate_startup_snapshot(
    agent: &Gents,
    tool_runtime: &ToolRuntimeContext,
    snapshot: &mut ResolvedRuntimeSnapshot,
) -> Result<()> {
    validate_startup_snapshot_until(
        agent,
        tool_runtime,
        snapshot,
        tokio::time::Instant::now() + std::time::Duration::from_secs(60),
    )
    .await
}

pub(in crate::runtime::agent::runtime) async fn validate_startup_snapshot_until(
    agent: &Gents,
    tool_runtime: &ToolRuntimeContext,
    snapshot: &mut ResolvedRuntimeSnapshot,
    deadline: tokio::time::Instant,
) -> Result<()> {
    snapshot.validate_node_readiness_source().with_context(|| {
        format!(
            "validate agent_config readiness source for {}",
            agent.node_did()
        )
    })?;

    demote_unbuildable_agents_until(tool_runtime, snapshot, deadline, false).await?;

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

        let blocking = unavailable
            .iter()
            .filter(|(_, reason)| !is_degraded_startup_unavailable_reason(reason.public_reason)
                && !degraded_publication_failure(
                    reason.public_reason == gents_protocol::node_readiness::AgentReadinessUnavailableReason::ToolSurfaceUnavailable,
                    reason.public_cause == Some(gents_automation::triggers::runtime_snapshot::PublicUnavailableCause::PluginPublicationBusy),
                ))
            .collect::<Vec<_>>();
        if !blocking.is_empty() {
            // Only the presentation-safe reason leaves this boundary.
            for (agent_did, reason) in &blocking {
                tracing::error!(
                    agent_did = %agent_did,
                    public_reason = ?reason.public_reason,
                    diagnostic = %reason.diagnostic,
                    "agent_config unavailable at startup blocks the agent from starting"
                );
                crate::runtime::native_logging::mirror_service_diagnostic(&format!(
                    "gents runtime: agent_config {agent_did} unavailable at startup: {}",
                    reason.diagnostic
                ));
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

#[cfg(test)]
mod publication_tests {
    use super::*;

    #[test]
    fn surface_failure_disposition_replays_the_lean_owner() {
        gents_lean_vocab::families::runtime_reconcile_surface_publication_keep_previous::replay(
            |i| {
                let error = if i.busy {
                    anyhow::Error::new(gents_pack::plugin::store::PublishedPluginReadError::Busy)
                } else {
                    anyhow::anyhow!("plugin is missing")
                }
                .context("constructing surface");
                preserve_generation_for_surface_error(&error)
            },
        );
    }
}

fn degraded_publication_failure(surface_unavailable: bool, busy_cause: bool) -> bool {
    surface_unavailable && busy_cause
}

#[cfg(test)]
#[test]
fn busy_only_all_unavailable_startup_replays_the_lean_owner() {
    gents_lean_vocab::families::runtime_reconcile_surface_publication_degraded_publication_failure::replay(|i| degraded_publication_failure(i.surface_unavailable, i.busy_cause));
}
