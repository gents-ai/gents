use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use defra_p2p_adapter::P2POperations;
use gents_desktop_bridge::client_runtime::ClientRuntime;

use super::agent::LiveAgentDocs;

pub(super) async fn wait_for_runtime_iroh_addr(
    p2p: &Arc<dyn P2POperations>,
    label: &str,
) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let addrs = p2p.listen_addresses().await?;
        if let Some(addr) = addrs
            .iter()
            .find(|addr| addr.contains("/p2p/") || addr.starts_with("endpoint"))
        {
            return Ok(addr.clone());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for {label} listen address");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub(super) async fn wait_for_live_documents(
    desktop_core: &ClientRuntime,
    node_did: &str,
    docs: &LiveAgentDocs,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        desktop_core.refresh_node(node_did).await?;
        let snapshot = desktop_core.stores().snapshot(node_did);
        let has_node = snapshot.nodes.iter().any(|row| row.node_did == node_did);
        let has_agent = snapshot
            .agents
            .iter()
            .any(|row| row.agent_did == docs.agent_did);
        let has_target_agent = snapshot
            .agents
            .iter()
            .any(|row| row.agent_did == docs.target_agent_did);
        let has_tools = snapshot
            .tools
            .iter()
            .any(|row| row.tools_id == docs.tools_id);
        let has_target_tools = snapshot
            .tools
            .iter()
            .any(|row| row.tools_id == docs.target_tools_id);
        let has_target = snapshot
            .agent_targets
            .iter()
            .any(|row| row.target_id == docs.target_id);
        let has_profile = snapshot
            .inference_profiles
            .iter()
            .any(|row| row.profile_id == docs.inference_profile_id);

        if has_node
            && has_agent
            && has_target_agent
            && has_tools
            && has_target_tools
            && has_target
            && has_profile
        {
            return Ok(());
        }

        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for live documents from runtime");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
