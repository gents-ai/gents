use std::sync::Arc;

use defra_agent_desktop_core::client::{ClientCore, DesktopPaths};
use defra_agent_desktop_core::local_runtime::{
    dangerously_overwrite_desktop_home, default_agent_home, init_standard_local_runtime,
    reset_desktop_runtime_state, DesktopInitOptions, DesktopInitSummary,
};
use tauri::{AppHandle, Emitter, State};

use super::super::snapshot::{build_bootstrap_summary, build_client_snapshot};
use super::super::state::{current_core, spawn_client_update_task, DesktopAppState};
use super::super::types::{
    ClientUpdateEvent, DesktopBootstrapSummary, DesktopClientSnapshot, DesktopInitRequest,
};

#[tauri::command]
pub(crate) fn desktop_bootstrap_summary() -> Result<DesktopBootstrapSummary, String> {
    tauri::async_runtime::block_on(build_bootstrap_summary())
}

#[tauri::command]
pub(crate) fn desktop_init_local_standard(
    request: DesktopInitRequest,
) -> Result<DesktopInitSummary, String> {
    tauri::async_runtime::block_on(async move {
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
            let _ =
                reset_desktop_runtime_state(&desktop_paths).map_err(|error| error.to_string())?;
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
    })
}

#[tauri::command]
pub(crate) fn desktop_client_start(
    app: AppHandle,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    if let Some(core) = current_core(&state) {
        return tauri::async_runtime::block_on(build_client_snapshot(Some(&core)));
    }

    let core = Arc::new(
        tauri::async_runtime::block_on(ClientCore::start()).map_err(|error| error.to_string())?,
    );
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

    tauri::async_runtime::block_on(build_client_snapshot(Some(&core)))
}

#[tauri::command]
pub(crate) fn desktop_client_shutdown(
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
        tauri::async_runtime::block_on(core.shutdown()).map_err(|error| error.to_string())?;
    }

    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent {
            reason: "lifecycle",
        },
    );

    tauri::async_runtime::block_on(build_client_snapshot(None))
}

#[tauri::command]
pub(crate) fn desktop_client_snapshot(
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let core = current_core(&state);
    tauri::async_runtime::block_on(build_client_snapshot(core.as_ref()))
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

    // Lazy-load the new scope's rows. Best-effort: log a warning on failure
    // but don't fail the selection update — the next refresh or event will
    // converge.
    if let Some(did_str) = did {
        let core_arc = Arc::clone(&core);
        tauri::async_runtime::spawn(async move {
            if let Err(err) = core_arc.ensure_agent_loaded(&did_str).await {
                tracing::warn!(error = %err, agent_did = %did_str, "ensure_agent_loaded failed");
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
