use gents_desktop_core::local_runtime::fetch_runtime_connection_payload;
use tauri::{AppHandle, Runtime, State};

use crate::error::BridgeError;

use super::emit_config_update_and_snapshot;
use crate::commands::{remove_peer, rename_peer};
use crate::state::{current_core, DesktopAppState};
use crate::types::{
    DesktopClientSnapshot, EnrollmentRequestView, EnrollmentStatusRequest, PeerRemoveResponse,
    PeerStatusFetchRequest,
};

#[tauri::command]
pub async fn desktop_peer_status_fetch(
    request: PeerStatusFetchRequest,
    state: State<'_, DesktopAppState>,
) -> Result<serde_json::Value, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    let peer_id = request.peer_id.trim();
    if peer_id.is_empty() {
        return Err(BridgeError::untyped("peer_id is required"));
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
        .map_err(BridgeError::classify_transport_error)
}

#[tauri::command]
pub async fn desktop_peer_enroll_status(
    request: EnrollmentStatusRequest,
    state: State<'_, DesktopAppState>,
) -> Result<EnrollmentRequestView, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    crate::commands::request_status_enrollment(&core, request).await
}

#[tauri::command]
pub async fn desktop_peer_remove<R>(
    app: AppHandle<R>,
    peer_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<PeerRemoveResponse, BridgeError>
where
    R: Runtime,
{
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    let mutation = remove_peer(core.as_ref(), peer_id)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let snapshot = emit_config_update_and_snapshot(&app, &core, &state).await?;
    Ok(PeerRemoveResponse::new(snapshot, mutation))
}

#[tauri::command]
pub async fn desktop_peer_rename<R>(
    app: AppHandle<R>,
    peer_id: String,
    label: String,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError>
where
    R: Runtime,
{
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    rename_peer(core.as_ref(), peer_id, label)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}
