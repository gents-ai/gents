use std::sync::Arc;

use gents_desktop_core::client::{ClientCore, DesktopPaths};
use gents_desktop_core::local_runtime::{
    dangerously_overwrite_desktop_home, default_agent_home, init_standard_local_runtime,
    reset_desktop_runtime_state, DesktopInitOptions, DesktopInitSummary,
};
use tauri::{AppHandle, Emitter, State};

use super::super::snapshot::{build_bootstrap_summary, build_client_snapshot};
use super::super::state::{current_core, spawn_client_update_task, DesktopAppState};
use super::super::types::{
    ClientUpdateEvent, DesktopBootstrapSummary, DesktopClientSnapshot, DesktopInitRequest,
};

const CLIENT_START_STACK_SIZE: usize = 16 * 1024 * 1024;

#[tauri::command]
pub(crate) async fn desktop_bootstrap_summary() -> Result<DesktopBootstrapSummary, String> {
    build_bootstrap_summary().await
}

#[tauri::command]
pub(crate) async fn desktop_init_local_standard(
    request: DesktopInitRequest,
) -> Result<DesktopInitSummary, String> {
    let agent_home = match request.agent_home {
        Some(path) => path,
        None => default_agent_home().map_err(|error| error.to_string())?,
    };
    let desktop_paths = match request.desktop_home {
        Some(root) => DesktopPaths::from_root(root),
        None => DesktopPaths::discover().map_err(|error| error.to_string())?,
    };

    if request.dangerously_overwrite {
        dangerously_overwrite_desktop_home(desktop_paths.root())
            .map_err(|error| error.to_string())?;
    } else if request.reset {
        let _ = reset_desktop_runtime_state(&desktop_paths).map_err(|error| error.to_string())?;
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
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn desktop_client_start(
    app: AppHandle,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    if let Some(core) = current_core(&state) {
        return build_client_snapshot(Some(&core)).await;
    }

    let core = Arc::new(start_client_core_with_large_stack()?);
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

    build_client_snapshot(Some(&core)).await
}

#[tauri::command]
pub(crate) async fn desktop_client_shutdown(
    app: AppHandle,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let (core, updates_task) = {
        let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
        (bridge.core.take(), bridge.updates_task.take())
    };

    if let Some(task) = updates_task {
        task.abort();
    }

    if let Some(core) = core {
        core.shutdown().await.map_err(|error| error.to_string())?;
    }

    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent {
            reason: "lifecycle",
        },
    );

    build_client_snapshot(None).await
}

#[tauri::command]
pub(crate) async fn desktop_client_snapshot(
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let core = current_core(&state);
    build_client_snapshot(core.as_ref()).await
}

fn start_client_core_with_large_stack() -> Result<ClientCore, String> {
    // iOS gives its WebKit/main thread a much smaller stack than macOS. DefraDB
    // schema migration can legitimately exceed it during first client startup,
    // so keep the command asynchronous and poll startup on a temporary thread
    // with explicit headroom.
    std::thread::Builder::new()
        .name("desktop-client-start".to_string())
        .stack_size(CLIENT_START_STACK_SIZE)
        .spawn(|| tauri::async_runtime::block_on(ClientCore::start()))
        .map_err(|error| format!("spawning desktop client startup thread: {error}"))?
        .join()
        .map_err(|_| "desktop client startup thread panicked".to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) fn desktop_set_selected_agent(
    state: State<'_, DesktopAppState>,
    agent_did: Option<String>,
) -> Result<(), String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client not initialized".to_string());
    };
    let did = agent_did
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    core.set_selected_agent_did(did.clone());

    // Lazy-load the new scope's rows. GraphQL-managed demo peers need a remote
    // refresh; otherwise the scoped reload hits only the desktop's embedded
    // node and can miss fresh worker conversations.
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

#[derive(serde::Serialize)]
pub(crate) struct DesktopObserverMetrics {
    pub events_received: u64,
    pub docs_fetched: u64,
    pub debounce_flushes: u64,
    pub scope_reloads: u64,
    pub drop_recoveries: u64,
    pub local_write_redundant_fetches: u64,
    pub fetch_failures: u64,
}

#[tauri::command]
pub(crate) async fn desktop_observer_metrics(
    state: State<'_, DesktopAppState>,
) -> Result<Option<DesktopObserverMetrics>, String> {
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
