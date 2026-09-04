use std::sync::Arc;

use gents_desktop_core::client::ClientCore;
use gents_desktop_core::local_runtime::{
    dangerously_overwrite_desktop_home, init_standard_local_runtime, reset_desktop_runtime_state,
    DesktopInitOptions, DesktopInitSummary,
};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tokio::sync::watch;

use crate::config::BootstrapPolicy;
use crate::contract::{current_contract, BridgeContract};
use crate::error::{BridgeError, BridgeErrorCode};
use crate::snapshot::{build_bootstrap_summary_for_policy, build_client_snapshot_with_grants};
use crate::state::{
    current_core, snapshot_grants, spawn_client_update_task, ClientStartProgress, DesktopAppState,
};
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
    // Re-check under the lock: a new start must not begin mid-init.
    if start_inflight_is_pending(&state) {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "desktop client is starting; retry local runtime init after start completes",
        ));
    }
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
            .map_err(|error| BridgeError::untyped(error.to_string()))?;
    } else if request.reset {
        let _ = reset_desktop_runtime_state(&desktop_paths)
            .map_err(|error| BridgeError::untyped(error.to_string()))?;
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
    .map_err(|error| BridgeError::classify_transport_error(error.to_string()))
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
    let grants = snapshot_grants(&state);

    // Single-flight: return the live core, wait on an in-flight start, or become
    // the starter. NodeBuilder runs on a detached task so cancelling this
    // command cannot leave the store open while a second start races it.
    // Never hold the std::sync::Mutex across an await (future must be Send).
    if let Some(core) = current_core(&state) {
        return build_client_snapshot_with_grants(Some(&core), Some(&state.policy), grants)
            .await
            .map_err(BridgeError::untyped);
    }

    let progress_rx = claim_or_join_client_start(&app, &state);

    wait_for_client_start_progress(progress_rx).await?;

    let core = current_core(&state).ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::ClientStartFailed,
            "desktop client start completed without installing a live client",
        )
    })?;

    build_client_snapshot_with_grants(Some(&core), Some(&state.policy), grants)
        .await
        .map_err(BridgeError::untyped)
}

/// Register as the single-flight starter or subscribe to the in-flight one.
/// Synchronous so the bridge mutex is never held across an await.
fn claim_or_join_client_start<R: Runtime>(
    app: &AppHandle<R>,
    state: &State<'_, DesktopAppState>,
) -> watch::Receiver<ClientStartProgress> {
    let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");

    // Another caller may have installed the core between our check and this lock.
    // Waiters still use the progress channel; Ready is sent after install.
    if let Some(sender) = bridge.start_inflight.as_ref() {
        tracing::debug!("desktop client start: joining in-flight single-flight start");
        return sender.subscribe();
    }

    let (tx, rx) = watch::channel(ClientStartProgress::Pending);
    // If a core is already live, mark ready immediately so waiters short-circuit.
    if bridge.core.is_some() {
        let _ = tx.send(ClientStartProgress::Ready);
        return rx;
    }

    bridge.start_inflight = Some(tx.clone());
    let paths = state.policy.desktop_paths.clone();
    let app_for_start = app.clone();
    drop(bridge);

    tracing::info!("desktop client start: single-flight starter claimed");
    tauri::async_runtime::spawn(async move {
        run_detached_client_start(app_for_start, paths, tx).await;
    });
    rx
}

async fn run_detached_client_start<R: Runtime>(
    app: AppHandle<R>,
    paths: gents_desktop_core::client::DesktopPaths,
    progress_tx: watch::Sender<ClientStartProgress>,
) {
    let start_result = start_client_core_async(paths).await;

    let state = app.state::<DesktopAppState>();
    // Serialize install against shutdown so we never leave an untracked open DB
    // or install over a concurrent tear-down without coordination.
    let _lifecycle_guard = state.client_lifecycle.lock().await;

    match start_result {
        Ok(core) => {
            let core = Arc::new(core);
            let orphan = {
                let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
                let orphan = if bridge.core.is_none() {
                    let updates_task = spawn_client_update_task(app.clone(), Arc::clone(&core));
                    bridge.core = Some(Arc::clone(&core));
                    bridge.updates_task = Some(updates_task);
                    None
                } else {
                    // Shutdown or another install won the slot. Drop our open
                    // node so the store is not held by an untracked Arc.
                    Some(core)
                };
                bridge.start_inflight = None;
                orphan
            };

            if let Some(orphan_core) = orphan {
                tracing::warn!(
                    "desktop client start: core already installed after open; shutting down orphan"
                );
                if let Err(error) = orphan_core.shutdown().await {
                    tracing::warn!(
                        error = %error,
                        "desktop client start: failed to shut down orphan core"
                    );
                }
            } else {
                let _ = app.emit(
                    "desktop://client-updated",
                    ClientUpdateEvent::coarse("lifecycle"),
                );
                tracing::info!("desktop client start: single-flight ready");
            }

            let _ = progress_tx.send(ClientStartProgress::Ready);
        }
        Err(error) => {
            {
                let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
                bridge.start_inflight = None;
            }
            let message = error.message.clone();
            let _ = progress_tx.send(ClientStartProgress::Failed(message));
            tracing::error!(
                error = %error.message,
                "desktop client start: single-flight failed"
            );
        }
    }
}

async fn wait_for_client_start_progress(
    mut progress_rx: watch::Receiver<ClientStartProgress>,
) -> Result<(), BridgeError> {
    loop {
        let current = progress_rx.borrow().clone();
        match current {
            ClientStartProgress::Pending => {
                if progress_rx.changed().await.is_err() {
                    return Err(BridgeError::new(
                        BridgeErrorCode::ClientStartFailed,
                        "desktop client start was abandoned",
                    ));
                }
            }
            ClientStartProgress::Ready => return Ok(()),
            ClientStartProgress::Failed(message) => {
                return Err(BridgeError::untyped(message));
            }
        }
    }
}

fn start_inflight_is_pending(state: &State<'_, DesktopAppState>) -> bool {
    state
        .bridge
        .lock()
        .expect("desktop bridge lock poisoned")
        .start_inflight
        .is_some()
}

/// Wait until any in-flight start finishes (ready or failed). Does **not** hold
/// `client_lifecycle` so the detached starter can acquire it to install.
async fn wait_for_start_inflight(state: &State<'_, DesktopAppState>) -> Result<(), BridgeError> {
    loop {
        let progress_rx = {
            let bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
            bridge
                .start_inflight
                .as_ref()
                .map(|sender| sender.subscribe())
        };
        let Some(progress_rx) = progress_rx else {
            return Ok(());
        };
        // Failed starts still clear inflight and wake waiters; treat failure as
        // "not running" for shutdown/init drain purposes.
        match wait_for_client_start_progress(progress_rx).await {
            Ok(()) => {}
            Err(_) => {
                // Starter failed; inflight should be cleared. Loop to confirm.
            }
        }
    }
}

#[tauri::command]
pub async fn desktop_client_shutdown<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    // Drain in-flight start first (without lifecycle) so the starter can install
    // and we can then take the core cleanly. Retry if a start sneaks in between
    // drain and the lifecycle lock.
    let (core, updates_task) = loop {
        wait_for_start_inflight(&state).await?;
        let lifecycle_guard = state.client_lifecycle.lock().await;
        if start_inflight_is_pending(&state) {
            drop(lifecycle_guard);
            continue;
        }
        let taken = {
            let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
            (bridge.core.take(), bridge.updates_task.take())
        };
        drop(lifecycle_guard);
        break taken;
    };

    if let Some(task) = updates_task {
        task.abort();
    }

    if let Some(core) = core {
        core.shutdown()
            .await
            .map_err(|error| BridgeError::untyped(error.to_string()))?;
    }

    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent::coarse("lifecycle"),
    );

    let grants = snapshot_grants(&state);
    build_client_snapshot_with_grants(None, Some(&state.policy), grants)
        .await
        .map_err(BridgeError::untyped)
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

/// Open the embedded node on a large-stack OS thread without blocking a Tokio
/// worker: the worker awaits a oneshot instead of `thread::join`.
async fn start_client_core_async(
    paths: gents_desktop_core::client::DesktopPaths,
) -> Result<ClientCore, BridgeError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("desktop-client-start".to_string())
        .stack_size(CLIENT_START_STACK_SIZE)
        .spawn(move || {
            let result = tauri::async_runtime::block_on(ClientCore::start_with_paths(paths));
            let _ = tx.send(result);
        })
        .map_err(|error| {
            BridgeError::new(
                BridgeErrorCode::ClientStartFailed,
                format!("spawning desktop client startup thread: {error}"),
            )
        })?;

    match rx.await {
        Ok(Ok(core)) => Ok(core),
        Ok(Err(error)) => Err(BridgeError::classify_transport_error(error.to_string())),
        Err(_) => Err(BridgeError::new(
            BridgeErrorCode::ClientStartFailed,
            "desktop client startup thread panicked or dropped its result",
        )),
    }
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
            match core_arc.refresh_agent(&did_str).await {
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
    pub response_in_place_merges: u64,
    pub response_copy_on_write_merges: u64,
    /// Transcript-content database changes that invalidated bounded session
    /// projections without copying their rows into the global observer.
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
        response_in_place_merges: snap.response_in_place_merges,
        response_copy_on_write_merges: snap.response_copy_on_write_merges,
        transcript_invalidations: snap.transcript_invalidations,
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

    #[test]
    fn start_progress_ready_is_distinct_from_pending() {
        assert!(matches!(
            ClientStartProgress::Pending,
            ClientStartProgress::Pending
        ));
        assert!(matches!(
            ClientStartProgress::Ready,
            ClientStartProgress::Ready
        ));
        let failed = ClientStartProgress::Failed("boom".into());
        match failed {
            ClientStartProgress::Failed(message) => assert_eq!(message, "boom"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}
