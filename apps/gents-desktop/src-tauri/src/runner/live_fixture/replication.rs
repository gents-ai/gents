use std::time::{Duration, Instant};

use anyhow::Result;
use gents_desktop_core::client::{
    subscribed_collection_names, ClientCore, DesktopPaths, PeerRecord,
};

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
        subscribed_collection_names(),
    )
    .await?;
    set_replicator_with_retry(
        desktop_core,
        &remote_addr,
        &format!("desktop -> {label} replicator"),
        subscribed_collection_names(),
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
    tokio::time::timeout(Duration::from_secs(10), core.request_p2p_repair())
        .await
        .map_err(|_| anyhow::anyhow!("timed out requesting peer repair on {label}"))??;
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

pub(super) async fn wait_for_ready_peer_status(
    core: &ClientCore,
    peer_id: &str,
    label: &str,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = core.sync_state();
        let dial_ready = snapshot
            .peers
            .iter()
            .any(|peer| peer.peer_id == peer_id && peer.dial_succeeded);
        let durable_ready = snapshot
            .directory
            .iter()
            .any(|peer| peer.peer_id == peer_id && peer.is_chat_ready_at(chrono::Utc::now()));
        if dial_ready && durable_ready {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let observed = snapshot.peers.iter().find(|peer| peer.peer_id == peer_id);
            anyhow::bail!(
                "timed out waiting for ready peer status {peer_id} on {label}; observed={observed:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn set_replicator_with_retry(
    core: &ClientCore,
    addr: &str,
    label: &str,
    collections: Vec<&'static str>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match core
            .p2p()
            .add_replicator(
                collections.iter().map(|name| (*name).to_owned()).collect(),
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
    node_did: &str,
    docs: &LiveAgentDocs,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        desktop_core.refresh_store().await?;
        let snapshot = desktop_core.store().snapshot();
        let has_node = snapshot.nodes.iter().any(|row| row.node_did == node_did);
        let has_agent = snapshot
            .agents
            .iter()
            .any(|row| row.agent_id == docs.agent_id);
        let has_target_agent = snapshot
            .agents
            .iter()
            .any(|row| row.agent_id == docs.target_agent_id);
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
            anyhow::bail!("timed out waiting for live documents to replicate to desktop");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
