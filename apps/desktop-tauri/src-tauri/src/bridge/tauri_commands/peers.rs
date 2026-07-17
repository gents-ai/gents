use std::time::Duration;

use defra_agent_desktop_core::local_runtime::fetch_runtime_connection_payload;
use tauri::{AppHandle, State};

use super::super::commands::{add_peer, remove_peer, rename_peer, repair_p2p};
use super::super::state::{current_core, DesktopAppState};
use super::super::types::{
    DesktopClientSnapshot, PeerAddRequest, PeerRemoveResponse, PeerStatusFetchRequest,
};
use super::emit_config_update_and_snapshot;

#[tauri::command]
pub(crate) fn desktop_peer_add(
    app: AppHandle,
    request: PeerAddRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        add_peer(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
pub(crate) fn desktop_peer_status_fetch(
    request: PeerStatusFetchRequest,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::block_on(async move {
        fetch_runtime_connection_payload(&request.server_address)
            .await
            .map_err(|error| error.to_string())
    })
}

#[tauri::command]
pub(crate) fn desktop_p2p_repair(
    app: AppHandle,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        repair_p2p(core.as_ref(), Duration::from_millis(250))
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
pub(crate) fn desktop_peer_remove(
    app: AppHandle,
    peer_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<PeerRemoveResponse, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        let mutation = remove_peer(core.as_ref(), peer_id)
            .await
            .map_err(|error| error.to_string())?;
        let snapshot = emit_config_update_and_snapshot(&app, &core).await?;
        Ok(PeerRemoveResponse::new(snapshot, mutation))
    })
}

#[tauri::command]
pub(crate) fn desktop_peer_rename(
    app: AppHandle,
    peer_id: String,
    label: String,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        rename_peer(core.as_ref(), peer_id, label)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}
