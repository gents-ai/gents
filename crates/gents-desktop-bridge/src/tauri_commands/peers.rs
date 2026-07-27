use std::time::Duration;

use gents_desktop_core::local_runtime::fetch_runtime_connection_payload;
use tauri::{AppHandle, Runtime, State};

use crate::error::BridgeError;

use super::emit_config_update_and_snapshot;
use crate::commands::{add_peer, pair_bearer, remove_peer, rename_peer, repair_p2p};
use crate::state::{current_core, DesktopAppState};
use crate::types::{
    BearerPairingRequest, BearerPairingResponse, DesktopClientSnapshot, NetworkReplicatorView,
    NetworkSavedPeerView, NetworkStatusView, PeerAddRequest, PeerProbeRequest, PeerRemoveResponse,
    PeerStatusFetchRequest,
};

#[tauri::command]
pub async fn desktop_peer_add<R: Runtime>(
    app: AppHandle<R>,
    request: PeerAddRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    add_peer(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_peer_pair_bearer<R: Runtime>(
    app: AppHandle<R>,
    request: BearerPairingRequest,
    state: State<'_, DesktopAppState>,
) -> Result<BearerPairingResponse, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    let pairing = pair_bearer(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(format!("{error:#}")))?;
    let snapshot = emit_config_update_and_snapshot(&app, &core, &state).await?;
    Ok(BearerPairingResponse::new(snapshot, pairing))
}

#[tauri::command]
pub async fn desktop_peer_status_fetch(
    request: PeerStatusFetchRequest,
    state: State<'_, DesktopAppState>,
) -> Result<serde_json::Value, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };
    let peer_id = request.peer_id.trim();
    if peer_id.is_empty() {
        return Err(BridgeError::from_legacy_message("peer_id is required"));
    }
    let addr = core
        .peer_records()
        .await
        .into_iter()
        .find(|peer| peer.peer_id == peer_id)
        .map(|peer| peer.addr)
        .ok_or_else(|| format!("saved peer {peer_id} was not found"))?;
    fetch_runtime_connection_payload(&addr)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))
}

/// Admin-only: probe an arbitrary address before the peer is saved.
#[tauri::command]
pub async fn desktop_peer_probe_address(
    request: PeerProbeRequest,
) -> Result<serde_json::Value, BridgeError> {
    let address = request.server_address.trim();
    if address.is_empty() {
        return Err(BridgeError::from_legacy_message(
            "server_address is required",
        ));
    }
    fetch_runtime_connection_payload(address)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))
}

#[tauri::command]
pub async fn desktop_p2p_repair<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    repair_p2p(core.as_ref(), Duration::from_millis(250))
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_peer_remove<R: Runtime>(
    app: AppHandle<R>,
    peer_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<PeerRemoveResponse, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    let mutation = remove_peer(core.as_ref(), peer_id)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    let snapshot = emit_config_update_and_snapshot(&app, &core, &state).await?;
    Ok(PeerRemoveResponse::new(snapshot, mutation))
}

#[tauri::command]
pub async fn desktop_peer_rename<R: Runtime>(
    app: AppHandle<R>,
    peer_id: String,
    label: String,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    rename_peer(core.as_ref(), peer_id, label)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_network_status(
    state: State<'_, DesktopAppState>,
) -> Result<NetworkStatusView, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    let status = core.network_status().await;
    fn split<T>(probe: Result<T, String>, empty: T) -> (T, Option<String>) {
        match probe {
            Ok(value) => (value, None),
            Err(error) => (empty, Some(error)),
        }
    }
    let (local_peer_id, local_peer_id_error) = match status.local_peer_id {
        Ok(value) => (Some(value), None),
        Err(error) => (None, Some(error)),
    };
    let (listen_addresses, listen_addresses_error) = split(status.listen_addresses, Vec::new());
    let (connected_peers, connected_peers_error) = split(status.connected_peers, Vec::new());
    let (replicators, replicators_error) = split(status.replicators, Vec::new());
    Ok(NetworkStatusView {
        local_peer_id,
        local_peer_id_error,
        listen_addresses,
        listen_addresses_error,
        connected_peers,
        connected_peers_error,
        replicators: replicators
            .into_iter()
            .map(|replicator| NetworkReplicatorView {
                peer_id: replicator.peer_id,
                address: replicator.address,
                collections: replicator.collections,
                status: replicator.status,
                last_status_change: replicator.last_status_change,
            })
            .collect(),
        replicators_error,
        saved_peers: status
            .saved_peers
            .into_iter()
            .map(|record| NetworkSavedPeerView {
                peer_id: record.peer_id,
                label: record.label,
                addr: record.addr,
                agent_did: record.agent_did,
                source: record.source,
            })
            .collect(),
    })
}
