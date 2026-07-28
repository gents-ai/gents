use std::sync::Arc;

use gents_desktop_core::client::ClientCore;
use gents_desktop_core::local_runtime::{
    dangerously_overwrite_desktop_home, init_standard_local_runtime, reset_desktop_runtime_state,
    DesktopInitOptions, DesktopInitSummary,
};
use tauri::{AppHandle, Emitter, Runtime, State};

use crate::config::BootstrapPolicy;
use crate::contract::{current_contract, BridgeContract};
use crate::error::{BridgeError, BridgeErrorCode};
use crate::snapshot::{build_bootstrap_summary_for_policy, build_client_snapshot_with_grants};
use crate::state::{current_core, snapshot_grants, spawn_client_update_task, DesktopAppState};
use crate::types::{
    ClientUpdateEvent, DesktopBootstrapSummary, DesktopClientSnapshot, DesktopInitRequest,
};

const CLIENT_START_STACK_SIZE: usize = 16 * 1024 * 1024;

#[tauri::command]
pub async fn desktop_bridge_contract() -> Result<BridgeContract, BridgeError> {
    Ok(current_contract())
}

#[tauri::command]
pub async fn desktop_bootstrap_summary(
    state: State<'_, DesktopAppState>,
) -> Result<DesktopBootstrapSummary, BridgeError> {
    build_bootstrap_summary_for_policy(&state.policy)
        .await
        .map_err(BridgeError::from_legacy_message)
}

#[tauri::command]
pub async fn desktop_init_local_standard(
    request: DesktopInitRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopInitSummary, BridgeError> {
    match &state.policy.bootstrap {
        BootstrapPolicy::PairedRemoteOnly => {
            return Err(BridgeError::new(
                BridgeErrorCode::Unsupported,
                "local runtime provisioning is disabled (PairedRemoteOnly)",
            ));
        }
        BootstrapPolicy::LocalRuntimeAllowed { .. } => {}
    }

    let _lifecycle_guard = state.client_lifecycle.lock().await;
    ensure_client_stopped_for_init(current_core(&state).is_some())?;

    let agent_home = state.policy.agent_home.clone().ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::Unsupported,
            "agent home is not configured for this host",
        )
    })?;
    let desktop_paths = state.policy.desktop_paths.clone();

    if request.dangerously_overwrite {
        dangerously_overwrite_desktop_home(desktop_paths.root())
            .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    } else if request.reset {
        let _ = reset_desktop_runtime_state(&desktop_paths)
            .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    }

    init_standard_local_runtime(DesktopInitOptions {
        agent_home,
        desktop_paths,
        label: request
            .label
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| "Local Agent".to_string()),
    })
    .await
    .map_err(|error| BridgeError::from_legacy_message(error.to_string()))
}

fn ensure_client_stopped_for_init(client_is_running: bool) -> Result<(), BridgeError> {
    if client_is_running {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "shut down the desktop client before initializing or resetting its storage",
        ));
    }
    Ok(())
}

#[tauri::command]
pub async fn desktop_client_start<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let _lifecycle_guard = state.client_lifecycle.lock().await;
    let grants = snapshot_grants(&state);
    if let Some(core) = current_core(&state) {
        return build_client_snapshot_with_grants(Some(&core), Some(&state.policy), grants)
            .await
            .map_err(BridgeError::from_legacy_message);
    }

    let paths = state.policy.desktop_paths.clone();
    let core = Arc::new(start_client_core_with_large_stack(paths)?);
    let updates_task = spawn_client_update_task(app.clone(), Arc::clone(&core));

    {
        let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
        bridge.core = Some(Arc::clone(&core));
        bridge.updates_task = Some(updates_task);
    }

    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent {
            reason: "lifecycle",
        },
    );

    build_client_snapshot_with_grants(Some(&core), Some(&state.policy), grants)
        .await
        .map_err(BridgeError::from_legacy_message)
}

#[tauri::command]
pub async fn desktop_client_shutdown<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let _lifecycle_guard = state.client_lifecycle.lock().await;
    let (core, updates_task) = {
        let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
        (bridge.core.take(), bridge.updates_task.take())
    };

    if let Some(task) = updates_task {
        task.abort();
    }

    if let Some(core) = core {
        core.shutdown()
            .await
            .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    }

    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent {
            reason: "lifecycle",
        },
    );

    let grants = snapshot_grants(&state);
    build_client_snapshot_with_grants(None, Some(&state.policy), grants)
        .await
        .map_err(BridgeError::from_legacy_message)
}

#[tauri::command]
pub async fn desktop_client_snapshot(
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let core = current_core(&state);
    let grants = snapshot_grants(&state);
    build_client_snapshot_with_grants(core.as_ref(), Some(&state.policy), grants)
        .await
        .map_err(BridgeError::from_legacy_message)
}

fn start_client_core_with_large_stack(
    paths: gents_desktop_core::client::DesktopPaths,
) -> Result<ClientCore, BridgeError> {
    std::thread::Builder::new()
        .name("desktop-client-start".to_string())
        .stack_size(CLIENT_START_STACK_SIZE)
        .spawn(move || tauri::async_runtime::block_on(ClientCore::start_with_paths(paths)))
        .map_err(|error| {
            BridgeError::new(
                BridgeErrorCode::ClientStartFailed,
                format!("spawning desktop client startup thread: {error}"),
            )
        })?
        .join()
        .map_err(|_| {
            BridgeError::new(
                BridgeErrorCode::ClientStartFailed,
                "desktop client startup thread panicked",
            )
        })?
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))
}

#[tauri::command]
pub fn desktop_set_selected_agent(
    state: State<'_, DesktopAppState>,
    agent_did: Option<String>,
) -> Result<(), BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::new(
            BridgeErrorCode::ClientNotRunning,
            "desktop client not initialized",
        ));
    };
    let did = agent_did
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    core.set_selected_agent_did(did.clone());

    if let Some(did_str) = did {
        let core_arc = Arc::clone(&core);
        tauri::async_runtime::spawn(async move {
            match core_arc.refresh_remote_agent(&did_str).await {
                Ok(Some(_version)) => {}
                Ok(None) => {
                    if let Err(err) = core_arc.ensure_agent_loaded(&did_str).await {
                        tracing::warn!(
                            error = %err,
                            agent_did = %did_str,
                            "ensure_agent_loaded failed"
                        );
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        agent_did = %did_str,
                        "remote selection refresh failed"
                    );
                    if let Err(err) = core_arc.ensure_agent_loaded(&did_str).await {
                        tracing::warn!(
                            error = %err,
                            agent_did = %did_str,
                            "ensure_agent_loaded failed after remote refresh failure"
                        );
                    }
                }
            }
        });
    }
    Ok(())
}

#[derive(serde::Serialize, ts_rs::TS)]
pub struct DesktopObserverMetrics {
    pub events_received: u64,
    pub docs_fetched: u64,
    pub debounce_flushes: u64,
    pub scope_reloads: u64,
    pub drop_recoveries: u64,
    pub local_write_redundant_fetches: u64,
    pub fetch_failures: u64,
}

#[tauri::command]
pub async fn desktop_observer_metrics(
    state: State<'_, DesktopAppState>,
) -> Result<Option<DesktopObserverMetrics>, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Ok(None);
    };
    let Some(snap) = core.observer_metrics().await else {
        return Ok(None);
    };
    Ok(Some(DesktopObserverMetrics {
        events_received: snap.events_received,
        docs_fetched: snap.docs_fetched,
        debounce_flushes: snap.debounce_flushes,
        scope_reloads: snap.scope_reloads,
        drop_recoveries: snap.drop_recoveries,
        local_write_redundant_fetches: snap.local_write_redundant_fetches,
        fetch_failures: snap.fetch_failures,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_init_rejects_a_live_client_before_touching_storage() {
        let error = ensure_client_stopped_for_init(true).expect_err("live client must reject init");
        assert_eq!(error.code, BridgeErrorCode::InvalidArgument);
        assert!(error.message.contains("shut down"));
        assert!(ensure_client_stopped_for_init(false).is_ok());
    }
}
