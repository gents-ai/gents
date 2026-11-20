use super::emit_config_update_and_snapshot;
use crate::state::current_core;
use crate::state::DesktopAppState;
use crate::types::ClientAccessRequestView;
use crate::types::DesktopClientSnapshot;
use crate::types::RuntimeAccessConnectRequest;
use crate::types::RuntimeRemoveResponse;
use gents_store::client_error::BridgeError;
use tauri::{AppHandle, Runtime, State};

#[tauri::command]
pub async fn desktop_runtime_access_request(
    request: RuntimeAccessConnectRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ClientAccessRequestView, BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
    crate::commands::request_runtime_access(&core, request).await
}

#[tauri::command]
pub async fn desktop_runtime_remove<R>(
    app: AppHandle<R>,
    node_did: String,
    state: State<'_, DesktopAppState>,
) -> Result<RuntimeRemoveResponse, BridgeError>
where
    R: Runtime,
{
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
    let removed = crate::commands::forget_runtime(&core, node_did)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let snapshot = emit_config_update_and_snapshot(&app, &core, &state).await?;
    Ok(RuntimeRemoveResponse { snapshot, removed })
}

#[tauri::command]
pub async fn desktop_runtime_rename<R>(
    app: AppHandle<R>,
    node_did: String,
    label: String,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError>
where
    R: Runtime,
{
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
    crate::commands::rename_runtime(&core, node_did, label)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_runtime_access_list(
    node_did: String,
    state: State<'_, DesktopAppState>,
) -> Result<Vec<ClientAccessRequestView>, BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
    core.list_runtime_access_requests(&node_did)
        .await
        .map(|requests| requests.into_iter().map(Into::into).collect())
        .map_err(|error| BridgeError::untyped(format!("{error:#}")))
}

#[tauri::command]
pub async fn desktop_runtime_access_decide(
    request: crate::types::RuntimeAccessDecisionRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ClientAccessRequestView, BridgeError> {
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
    core.decide_runtime_access(&request.node_did, &request.request_id, request.decision)
        .await
        .map(Into::into)
        .map_err(|error| BridgeError::untyped(format!("{error:#}")))
}
