use std::sync::Arc;

use anyhow::{Context, Result};
use defra_p2p_adapter::{P2POperations as P2POps, ReplicatorInfo};
use tokio::time::timeout;

use super::P2P_OPERATION_TIMEOUT;

pub(super) async fn p2p_local_peer_id(p2p: &Arc<dyn P2POps>) -> Result<String> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.local_peer_id()).await {
        Ok(result) => result
            .map_err(anyhow::Error::msg)
            .context("reading desktop P2P peer id"),
        Err(_) => anyhow::bail!("timed out reading desktop P2P peer id"),
    }
}

pub(super) async fn p2p_listen_addresses(p2p: &Arc<dyn P2POps>) -> Result<Vec<String>> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.listen_addresses()).await {
        Ok(result) => result
            .map_err(anyhow::Error::msg)
            .context("reading desktop P2P listen addresses"),
        Err(_) => anyhow::bail!("timed out reading desktop P2P listen addresses"),
    }
}

pub(super) async fn p2p_connected_peers(p2p: &Arc<dyn P2POps>) -> Result<Vec<String>> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.connected_peers()).await {
        Ok(result) => result
            .map_err(anyhow::Error::msg)
            .context("reading desktop P2P connected peers"),
        Err(_) => anyhow::bail!("timed out reading desktop P2P connected peers"),
    }
}

pub(super) async fn p2p_connect_peer(p2p: &Arc<dyn P2POps>, addr: &str) -> Result<()> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.connect_peer(addr)).await {
        Ok(result) => result
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("connecting desktop P2P peer {addr}")),
        Err(_) => anyhow::bail!("timed out connecting desktop P2P peer {addr}"),
    }
}

pub(super) async fn p2p_disconnect_peer(p2p: &Arc<dyn P2POps>, addr: &str) -> Result<()> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.disconnect_peer(addr)).await {
        Ok(result) => result
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("disconnecting desktop P2P peer {addr}")),
        Err(_) => anyhow::bail!("timed out disconnecting desktop P2P peer {addr}"),
    }
}

pub(super) async fn p2p_notify_network_change(p2p: &Arc<dyn P2POps>) -> Result<()> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.notify_network_change()).await {
        Ok(result) => result
            .map_err(anyhow::Error::msg)
            .context("refreshing desktop P2P network state"),
        Err(_) => anyhow::bail!("timed out refreshing desktop P2P network state"),
    }
}

pub(super) async fn p2p_get_replicators(p2p: &Arc<dyn P2POps>) -> Result<Vec<ReplicatorInfo>> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.get_replicators()).await {
        Ok(result) => result
            .map_err(anyhow::Error::msg)
            .context("reading desktop P2P replicators"),
        Err(_) => anyhow::bail!("timed out reading desktop P2P replicators"),
    }
}

pub(super) async fn p2p_sync_status(p2p: &Arc<dyn P2POps>) -> Result<serde_json::Value> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.sync_status()).await {
        Ok(result) => result
            .map_err(anyhow::Error::msg)
            .context("reading desktop database sync status"),
        Err(_) => anyhow::bail!("timed out reading desktop database sync status"),
    }
}

pub(super) async fn p2p_remove_replicator(
    p2p: &Arc<dyn P2POps>,
    collections: Vec<String>,
    addr: &str,
) -> Result<()> {
    match timeout(
        P2P_OPERATION_TIMEOUT,
        p2p.remove_replicator(collections, Some(addr)),
    )
    .await
    {
        Ok(result) => result
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("removing desktop P2P replicator for {addr}")),
        Err(_) => anyhow::bail!("timed out removing desktop P2P replicator for {addr}"),
    }
}

pub(super) async fn p2p_sync_branchable_collection(
    p2p: &Arc<dyn P2POps>,
    collection_id: &str,
) -> Result<()> {
    match timeout(
        P2P_OPERATION_TIMEOUT,
        p2p.sync_branchable_collection(collection_id),
    )
    .await
    {
        Ok(result) => result
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("syncing desktop P2P branchable collection {collection_id}")),
        Err(_) => {
            anyhow::bail!("timed out syncing desktop P2P branchable collection {collection_id}")
        }
    }
}
