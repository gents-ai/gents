use std::sync::Arc;

use gents_desktop_core::local_runtime::{
    dangerously_overwrite_desktop_home, init_standard_local_runtime, reset_desktop_runtime_state,
    DesktopInitOptions, DesktopInitSummary,
};
use tauri::{AppHandle, Emitter, Runtime, State, WebviewWindow};

use self::exclusion::exclude_client;
use self::single_flight::{
    claim_or_join_client_start, start_inflight_is_pending, wait_for_client_start_progress,
    wait_for_start_inflight,
};
use crate::config::BootstrapPolicy;
use crate::error::{BridgeError, BridgeErrorCode};
use crate::snapshot::{build_bootstrap_summary_for_policy, build_client_snapshot_with_grants};
use crate::state::{current_core, snapshot_grants, DesktopAppState};
use crate::types::{
    ClientUpdateEvent, DesktopBootstrapSummary, DesktopClientSnapshot, DesktopInitRequest,
};

mod core_start;
pub(super) mod exclusion;
pub(crate) mod single_flight;

#[tauri::command]
pub async fn desktop_bootstrap_summary(
    state: State<'_, DesktopAppState>,
) -> Result<DesktopBootstrapSummary, BridgeError> {
    build_bootstrap_summary_for_policy(&state.policy)
        .await
        .map_err(BridgeError::untyped)
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

    // Drain in-flight start before locking lifecycle so the starter can finish
    // installing (it needs the lifecycle mutex).
    wait_for_start_inflight(&state).await?;

    let _lifecycle_guard = state.client_lifecycle.lock().await;
    if start_inflight_is_pending(&state) {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "desktop client is starting; retry local runtime init after start completes",
        ));
    }
    ensure_client_stopped_for_init(current_core(&state).is_some())?;

    let node_home = state.policy.node_home.clone().ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::Unsupported,
            "node home is not configured for this host",
        )
    })?;
    let desktop_paths = state.policy.desktop_paths.clone();

    if request.dangerously_overwrite {
        dangerously_overwrite_desktop_home(desktop_paths.root())
            .map_err(|error| BridgeError::untyped(error.to_string()))?;
    } else if request.reset {
        let _ = reset_desktop_runtime_state(&desktop_paths)
            .map_err(|error| BridgeError::untyped(error.to_string()))?;
    }

    init_standard_local_runtime(DesktopInitOptions {
        node_home,
        desktop_paths,
        label: request
            .label
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| "Forge".to_string()),
    })
    .await
    .map_err(BridgeError::classify_transport_error)
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
pub async fn desktop_client_start<R>(
    app: AppHandle<R>,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError>
where
    R: Runtime,
{
    if state.app_exit.is_quitting() {
        return Err(BridgeError::new(
            BridgeErrorCode::ClientStartFailed,
            "desktop client cannot start while the desktop is shutting down",
        ));
    }
    let grants = snapshot_grants(&state);

    // NodeBuilder runs on a detached task so cancelling this command cannot
    // leave the store open while a second start races it. Never hold the
    // std::sync::Mutex across an await (future must be Send).
    if let Some(core) = current_core(&state) {
        super::managed_server::pairing::start_running_managed_pairing(&state, Arc::clone(&core)).await;
        return build_client_snapshot_with_grants(Some(&core), Some(&state.policy), grants)
            .await
            .map_err(BridgeError::untyped);
    }

    // Opening the stale store would resume the pairing this retires.
    if let Err(error) = super::managed_server::reset::retire_orphaned_client_state(&state).await {
        tracing::warn!(
            error = %error.message,
            "could not archive client state paired with a node that is no longer here"
        );
        return Err(error);
    }

    let progress_rx = claim_or_join_client_start(&app, &state);

    wait_for_client_start_progress(progress_rx).await?;

    let core = current_core(&state).ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::ClientStartFailed,
            "desktop client start completed without installing a live client",
        )
    })?;

    super::managed_server::pairing::start_running_managed_pairing(&state, Arc::clone(&core)).await;

    build_client_snapshot_with_grants(Some(&core), Some(&state.policy), grants)
        .await
        .map_err(BridgeError::untyped)
}

#[tauri::command]
pub async fn desktop_client_shutdown<R>(
    app: AppHandle<R>,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError>
where
    R: Runtime,
{
    // Enrollment authoring temporarily installs bootstrap replication state.
    // Let that bounded operation unwind before dropping its core instead of
    // aborting the future between install and cleanup.
    let _managed_lifecycle = state.managed_server_lifecycle.lock().await;
    shutdown_client_under_managed_lifecycle(&app, &state).await?;

    let grants = snapshot_grants(&state);
    build_client_snapshot_with_grants(None, Some(&state.policy), grants)
        .await
        .map_err(BridgeError::untyped)
}

pub(crate) async fn shutdown_client_under_managed_lifecycle<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<(), BridgeError>
where
    R: Runtime,
{
    super::managed_server::pairing::drain_managed_runtime_pairing(state).await;
    drop(exclude_client(state).await?);
    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent::coarse("lifecycle"),
    );
    Ok(())
}

pub(crate) async fn shutdown_client_for_app_exit<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<(), BridgeError>
where
    R: Runtime,
{
    let _managed_lifecycle = state.managed_server_lifecycle.lock().await;
    shutdown_client_under_managed_lifecycle(app, state).await
}

#[tauri::command]
pub fn desktop_app_quit<R>(app: AppHandle<R>)
where
    R: Runtime,
{
    app.exit(0);
}

#[tauri::command]
pub async fn desktop_client_snapshot(
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let core = current_core(&state);
    let grants = snapshot_grants(&state);
    build_client_snapshot_with_grants(core.as_ref(), Some(&state.policy), grants)
        .await
        .map_err(BridgeError::untyped)
}

pub(super) type ClientLifecycleGuard = tokio::sync::OwnedMutexGuard<()>;

#[tauri::command]
pub fn desktop_set_selected_node<R>(
    window: WebviewWindow<R>,
    state: State<'_, DesktopAppState>,
    node_did: Option<String>,
) -> Result<(), BridgeError>
where
    R: Runtime,
{
    let did = node_did
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let core = {
        let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
        if !bridge.has_window(window.label()) {
            return Ok(());
        }
        let core = bridge.core.clone().ok_or_else(|| {
            BridgeError::new(
                BridgeErrorCode::ClientNotRunning,
                "desktop client not initialized",
            )
        })?;
        bridge.set_window_selected_node(window.label(), did.clone());
        core
    };

    if let Some(did_str) = did {
        let core_arc = Arc::clone(&core);
        tauri::async_runtime::spawn(async move {
            if let Err(err) = core_arc.refresh_node(&did_str).await {
                tracing::warn!(
                    error = %err,
                    node_did = %did_str,
                    "selected node refresh failed; observer recovery was scheduled"
                );
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
    pub transcript_invalidations: u64,
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
        transcript_invalidations: snap.transcript_invalidations,
    }))
}

#[cfg(all(test, unix))]
#[path = "lifecycle/host_tests.rs"]
mod host_tests;

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;
