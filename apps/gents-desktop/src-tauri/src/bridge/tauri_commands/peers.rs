use std::time::Duration;

use gents_desktop_core::local_runtime::fetch_runtime_connection_payload;
use tauri::{AppHandle, State};

use super::super::commands::{add_peer, pair_bearer, remove_peer, rename_peer, repair_p2p};
use super::super::state::{current_core, DesktopAppState};
use super::super::types::{
    BearerPairingRequest, BearerPairingResponse, DesktopClientSnapshot, NetworkReplicatorView,
    NetworkSavedPeerView, NetworkStatusView, PeerAddRequest, PeerRemoveResponse,
    PeerStatusFetchRequest,
};
use super::emit_config_update_and_snapshot;

#[tauri::command]
pub(crate) async fn desktop_peer_add(
    app: AppHandle,
    request: PeerAddRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    add_peer(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub(crate) async fn desktop_peer_pair_bearer(
    app: AppHandle,
    request: BearerPairingRequest,
    state: State<'_, DesktopAppState>,
) -> Result<BearerPairingResponse, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let pairing = pair_bearer(core.as_ref(), request)
        .await
        .map_err(|error| format!("{error:#}"))?;
    let snapshot = emit_config_update_and_snapshot(&app, &core).await?;
    Ok(BearerPairingResponse::new(snapshot, pairing))
}

#[tauri::command]
pub(crate) async fn desktop_peer_status_fetch(
    request: PeerStatusFetchRequest,
) -> Result<serde_json::Value, String> {
    fetch_runtime_connection_payload(&request.server_address)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn desktop_p2p_repair(
    app: AppHandle,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    repair_p2p(core.as_ref(), Duration::from_millis(250))
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub(crate) async fn desktop_peer_remove(
    app: AppHandle,
    peer_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<PeerRemoveResponse, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let mutation = remove_peer(core.as_ref(), peer_id)
        .await
        .map_err(|error| error.to_string())?;
    let snapshot = emit_config_update_and_snapshot(&app, &core).await?;
    Ok(PeerRemoveResponse::new(snapshot, mutation))
}

#[tauri::command]
pub(crate) async fn desktop_peer_rename(
    app: AppHandle,
    peer_id: String,
    label: String,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    rename_peer(core.as_ref(), peer_id, label)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub(crate) async fn desktop_network_status(
    state: State<'_, DesktopAppState>,
) -> Result<NetworkStatusView, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
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
