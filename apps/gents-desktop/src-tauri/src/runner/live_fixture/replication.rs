use std::time::{Duration, Instant};

use anyhow::Result;
use gents_desktop_core::client::{ClientCore, DesktopPaths, PeerRecord};

use super::agent::LiveAgentDocs;

pub(super) async fn wait_for_connectable_iroh_addr(
    core: &ClientCore,
    label: &str,
) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let addrs = core.p2p().listen_addresses().await?;
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

pub(super) async fn configure_live_replicators(
    desktop_core: &ClientCore,
    remote_core: &ClientCore,
    label: &str,
) -> Result<()> {
    let desktop_addr = wait_for_connectable_iroh_addr(desktop_core, "desktop").await?;
    let remote_addr = wait_for_connectable_iroh_addr(remote_core, label).await?;
    let desktop_peer_id = desktop_core.local_peer_id().to_string();
    let remote_peer_id = remote_core.local_peer_id().to_string();

    connect_peer_with_retry(
        desktop_core,
        &remote_addr,
        &remote_peer_id,
        &format!("desktop -> {label}"),
    )
    .await?;
    connect_peer_with_retry(
        remote_core,
        &desktop_addr,
        &desktop_peer_id,
        &format!("{label} -> desktop"),
    )
    .await?;
    set_replicator_with_retry(
        remote_core,
        &desktop_addr,
        &format!("{label} -> desktop replicator"),
        subscribed_collection_names_for_runner(),
    )
    .await?;
    set_replicator_with_retry(
        desktop_core,
        &remote_addr,
        &format!("desktop -> {label} replicator"),
        subscribed_collection_names_for_runner(),
    )
    .await?;
    Ok(())
}

async fn connect_peer_with_retry(
    core: &ClientCore,
    addr: &str,
    peer_id: &str,
    label: &str,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if is_connected_peer(core, peer_id).await? {
            return Ok(());
        }

        match core.p2p().connect_peer(addr).await {
            Ok(()) => {
                wait_for_connected_peer(core, peer_id, label).await?;
                return Ok(());
            }
            Err(error) => {
                if is_connected_peer(core, peer_id).await? {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    anyhow::bail!("timed out connecting {label} to {peer_id}: {error}");
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
}

async fn is_connected_peer(core: &ClientCore, peer_id: &str) -> Result<bool> {
    let peers = core.p2p().connected_peers().await?;
    Ok(peers.iter().any(|peer| peer.contains(peer_id)))
}

pub(super) async fn wait_for_connected_peer(
    core: &ClientCore,
    peer_id: &str,
    label: &str,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if is_connected_peer(core, peer_id).await? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for connected peer {peer_id} on {label}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn set_replicator_with_retry(
    core: &ClientCore,
    addr: &str,
    label: &str,
    collections: Vec<String>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match core
            .p2p()
            .add_replicator(
                collections.clone(),
                Some(addr),
                Default::default(),
                Vec::new(),
                None,
            )
            .await
        {
            Ok(()) => return Ok(()),
            Err(error) => {
                if Instant::now() >= deadline {
                    anyhow::bail!("timed out configuring {label}: {error}");
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
}

/// Collections the live fixture replicates between its two nodes.
///
/// `RenderedRequest` is deliberately excluded. Its `request_json` and
/// `provenance_json` are the whole conversation, the system prompt, and the
/// tool surface in plaintext — `RenderedRequest` carries no `@policy` and no
/// field encryption, because both are blocked on defradb.rs#1318 — and this
/// list is built from `ALL_COLLECTION_NAMES` verbatim, so a new collection
/// joins the P2P subscription set with no decision being taken about it.
/// Capture is on by default, so shipping those bodies to a fixture peer would
/// be exactly that unmade decision.
fn subscribed_collection_names_for_runner() -> Vec<String> {
    gents_protocol::schemas::RUNTIME_COLLECTION_NAMES
        .iter()
        .chain(gents_protocol::schemas::ALL_COLLECTION_NAMES.iter())
        .filter(|name| !gents_protocol::schemas::is_local_audit_collection(name))
        .map(|name| (*name).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::subscribed_collection_names_for_runner;

    /// The runner's subscription set is derived from a list that grows
    /// whenever a collection is added, so the exclusion has to be asserted
    /// rather than assumed.
    #[test]
    fn the_runner_does_not_replicate_plaintext_provider_bodies() {
        let names = subscribed_collection_names_for_runner();
        for sensitive in gents_protocol::schemas::LOCAL_AUDIT_COLLECTION_NAMES {
            assert!(
                !names.iter().any(|name| name == sensitive),
                "{sensitive} must stay out of the fixture replication set: {names:?}"
            );
        }
        assert!(
            names.iter().any(|name| name == "AgentRequest"),
            "the exclusion must not have emptied the set: {names:?}"
        );
    }
}

pub(super) fn write_peer_directory_records(
    paths: &DesktopPaths,
    records: &[PeerRecord],
) -> Result<()> {
    std::fs::create_dir_all(paths.root())?;
    let payload = serde_json::json!({ "peers": records });
    std::fs::write(
        paths.peer_directory_path(),
        serde_json::to_vec_pretty(&payload)?,
    )?;
    Ok(())
}

pub(super) async fn wait_for_live_documents(
    desktop_core: &ClientCore,
    agent_did: &str,
    docs: &LiveAgentDocs,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        desktop_core.refresh_store().await?;
        let snapshot = desktop_core.store().snapshot();
        let has_principal = snapshot
            .agent_principals
            .iter()
            .any(|row| row.agent_did == agent_did);
        let has_behavior = snapshot
            .behaviors
            .iter()
            .any(|row| row.behavior_id == docs.behavior_id);
        let has_subagent_behavior = snapshot
            .behaviors
            .iter()
            .any(|row| row.behavior_id == docs.subagent_behavior_id);
        let has_backend = snapshot
            .inference_backends
            .iter()
            .any(|row| row.backend_id == docs.backend_id);
        let has_subagent_backend = snapshot
            .inference_backends
            .iter()
            .any(|row| row.backend_id == docs.subagent_backend_id);
        let has_tools = snapshot
            .tool_selections
            .iter()
            .any(|row| row.selection_id == docs.tool_selection_id);
        let has_subagent_tools = snapshot
            .tool_selections
            .iter()
            .any(|row| row.selection_id == docs.subagent_tool_selection_id);
        let has_profile = snapshot
            .inference_profiles
            .iter()
            .any(|row| row.profile_id == docs.inference_profile_id);

        if has_principal
            && has_behavior
            && has_subagent_behavior
            && has_backend
            && has_subagent_backend
            && has_tools
            && has_subagent_tools
            && has_profile
        {
            return Ok(());
        }

        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for live documents to replicate to desktop");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
