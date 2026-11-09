//! The evaluator is the trial's User. Only a User grants an Agent reader on a
//! published WorkspaceRoot (`workspace_roots.reader_manager`), and an Agent
//! created during the trial with host tools stays demoted until one does. Before
//! each stage the evaluator therefore grants the trial root to the Agents the
//! runtime demoted for their tool surface, as a User does after a config receipt's
//! `host_root_access` names that step, and waits for them to become ready.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use gents_protocol::node_readiness::{AgentReadinessState, AgentReadinessUnavailableReason};

use super::POLL;
use crate::eval::runner::embedded::home::{fetch_node_readiness_snapshot, EmbeddedHome};

const READMISSION_WINDOW: Duration = Duration::from_secs(30);

/// Agents waiting for the trial root: demoted for their tool surface and not
/// yet granted by this trial.
fn awaiting_root(
    snapshot: &gents_protocol::node_readiness::NodeReadinessSnapshot,
    granted: &BTreeSet<String>,
) -> Vec<String> {
    snapshot
        .agents
        .iter()
        .filter(|agent| {
            agent.state == AgentReadinessState::Unavailable
                && agent.reason == Some(AgentReadinessUnavailableReason::ToolSurfaceUnavailable)
                && !granted.contains(&agent.agent_did)
        })
        .map(|agent| agent.agent_did.clone())
        .collect()
}

pub(super) async fn grant_pending_root_readers(
    home: &EmbeddedHome,
    evaluator_did: &str,
    workspace: &Path,
    granted: &mut BTreeSet<String>,
) -> Result<()> {
    let Some(snapshot) = fetch_node_readiness_snapshot(&home.node, home.did()).await? else {
        return Ok(());
    };
    let pending = awaiting_root(&snapshot, granted);
    if pending.is_empty() {
        return Ok(());
    }
    let access = home.client_access(evaluator_did)?;
    let root = workspace
        .to_str()
        .with_context(|| format!("{} is not UTF-8", workspace.display()))?;
    for agent_did in &pending {
        crate::tools::surface::grant_workspace_root_reader(&access, root, agent_did)
            .await
            .with_context(|| format!("the trial User granting {agent_did} the trial root"))?;
        granted.insert(agent_did.clone());
    }
    let started = Instant::now();
    while started.elapsed() < READMISSION_WINDOW {
        let ready = fetch_node_readiness_snapshot(&home.node, home.did())
            .await?
            .is_some_and(|snapshot| {
                pending.iter().all(|agent_did| {
                    snapshot.agents.iter().any(|agent| {
                        &agent.agent_did == agent_did && agent.state == AgentReadinessState::Ready
                    })
                })
            });
        if ready {
            return Ok(());
        }
        tokio::time::sleep(POLL).await;
    }
    tracing::warn!(agents = ?pending, "granted Agents stayed unavailable; their tool surface fails for another reason");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::node_readiness::{AgentReadinessEntry, NodeReadinessSnapshot};

    #[test]
    fn only_tool_surface_demotions_not_yet_granted_await_the_root() {
        let entry = |agent_did: &str, state, reason| AgentReadinessEntry {
            agent_did: agent_did.to_owned(),
            state,
            reason,
        };
        let snapshot = NodeReadinessSnapshot {
            format_version: 1,
            process_state: gents_protocol::node_readiness::NodeReadinessProcessState::Ready,
            active_generation: 2,
            router_generation: 2,
            default_agent_did: "engineer".to_owned(),
            agents: vec![
                entry("engineer", AgentReadinessState::Ready, None),
                entry(
                    "scout",
                    AgentReadinessState::Unavailable,
                    Some(AgentReadinessUnavailableReason::ToolSurfaceUnavailable),
                ),
                entry(
                    "granted",
                    AgentReadinessState::Unavailable,
                    Some(AgentReadinessUnavailableReason::ToolSurfaceUnavailable),
                ),
                entry(
                    "disabled",
                    AgentReadinessState::Unavailable,
                    Some(AgentReadinessUnavailableReason::AgentDisabled),
                ),
            ],
        };
        let granted = BTreeSet::from(["granted".to_owned()]);
        assert_eq!(awaiting_root(&snapshot, &granted), vec!["scout".to_owned()]);
    }
}
