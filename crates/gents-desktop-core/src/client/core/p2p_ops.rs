use std::sync::Arc;

use defra_p2p_adapter::{P2POperations as P2POps, ReplicatorInfo};
use tokio::time::timeout;

use super::{ClientCoreError, P2P_OPERATION_TIMEOUT};

type Result<T, E = ClientCoreError> = std::result::Result<T, E>;

fn operation_failed(operation: &str, source: defra_p2p_adapter::P2PError) -> ClientCoreError {
    ClientCoreError::P2pOperation {
        operation: operation.to_owned(),
        source,
    }
}

fn operation_timed_out(operation: impl Into<String>) -> ClientCoreError {
    ClientCoreError::P2pOperationTimedOut {
        operation: operation.into(),
    }
}

pub(super) async fn p2p_local_peer_id(p2p: &Arc<dyn P2POps>) -> Result<String> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.local_peer_id()).await {
        Ok(result) => {
            result.map_err(|error| operation_failed("reading desktop P2P peer id", error))
        }
        Err(_) => Err(operation_timed_out("reading desktop P2P peer id")),
    }
}

pub(super) async fn p2p_listen_addresses(p2p: &Arc<dyn P2POps>) -> Result<Vec<String>> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.listen_addresses()).await {
        Ok(result) => {
            result.map_err(|error| operation_failed("reading desktop P2P listen addresses", error))
        }
        Err(_) => Err(operation_timed_out("reading desktop P2P listen addresses")),
    }
}

pub(super) async fn p2p_connected_peers(p2p: &Arc<dyn P2POps>) -> Result<Vec<String>> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.connected_peers()).await {
        Ok(result) => {
            result.map_err(|error| operation_failed("reading desktop P2P connected peers", error))
        }
        Err(_) => Err(operation_timed_out("reading desktop P2P connected peers")),
    }
}

pub(super) async fn p2p_connect_peer(p2p: &Arc<dyn P2POps>, addr: &str) -> Result<()> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.connect_peer(addr)).await {
        Ok(result) => result.map_err(|error| {
            operation_failed(&format!("connecting desktop P2P peer {addr}"), error)
        }),
        Err(_) => Err(operation_timed_out(format!(
            "connecting desktop P2P peer {addr}"
        ))),
    }
}

pub(super) async fn p2p_disconnect_peer(p2p: &Arc<dyn P2POps>, addr: &str) -> Result<()> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.disconnect_peer(addr)).await {
        Ok(result) => result.map_err(|error| {
            operation_failed(&format!("disconnecting desktop P2P peer {addr}"), error)
        }),
        Err(_) => Err(operation_timed_out(format!(
            "disconnecting desktop P2P peer {addr}"
        ))),
    }
}

pub(super) async fn p2p_notify_network_change(p2p: &Arc<dyn P2POps>) -> Result<()> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.notify_network_change()).await {
        Ok(result) => {
            result.map_err(|error| operation_failed("refreshing desktop P2P network state", error))
        }
        Err(_) => Err(operation_timed_out("refreshing desktop P2P network state")),
    }
}

pub(super) async fn p2p_get_replicators(p2p: &Arc<dyn P2POps>) -> Result<Vec<ReplicatorInfo>> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.get_replicators()).await {
        Ok(result) => {
            result.map_err(|error| operation_failed("reading desktop P2P replicators", error))
        }
        Err(_) => Err(operation_timed_out("reading desktop P2P replicators")),
    }
}

pub(super) async fn p2p_sync_status(p2p: &Arc<dyn P2POps>) -> Result<serde_json::Value> {
    match timeout(P2P_OPERATION_TIMEOUT, p2p.sync_status()).await {
        Ok(result) => {
            result.map_err(|error| operation_failed("reading desktop database sync status", error))
        }
        Err(_) => Err(operation_timed_out("reading desktop database sync status")),
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
        Ok(result) => result.map_err(|error| {
            operation_failed(
                &format!("removing desktop P2P replicator for {addr}"),
                error,
            )
        }),
        Err(_) => Err(operation_timed_out(format!(
            "removing desktop P2P replicator for {addr}"
        ))),
    }
}
