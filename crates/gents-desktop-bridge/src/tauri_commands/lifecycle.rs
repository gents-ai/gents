use std::sync::Arc;

use gents_desktop_core::client::ClientCore;
use gents_desktop_core::local_runtime::{
    dangerously_overwrite_desktop_home, init_standard_local_runtime, reset_desktop_runtime_state,
    DesktopInitOptions, DesktopInitSummary,
};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tokio::sync::watch;

use crate::config::BootstrapPolicy;
use crate::error::{BridgeError, BridgeErrorCode};
use crate::snapshot::{build_bootstrap_summary_for_policy, build_client_snapshot_with_grants};
use crate::state::{
    current_core, snapshot_grants, spawn_client_update_task, ClientStartProgress, DesktopAppState,
};
use crate::types::{
    ClientUpdateEvent, DesktopBootstrapSummary, DesktopClientSnapshot, DesktopInitRequest,
};

const CLIENT_START_STACK_SIZE: usize = 16 * 1024 * 1024;
/// A client start that completes no stage for this long fails, so callers
/// waiting on an in-flight start are released. The store it may still open
/// stays owned by the lifecycle lock until that start ends.
const CLIENT_START_STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const NO_STAGE_COMPLETED: &str = "none";

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
        super::managed_server::start_running_managed_pairing(&state, Arc::clone(&core)).await;
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

    super::managed_server::start_running_managed_pairing(&state, Arc::clone(&core)).await;

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
    let state = app.state::<DesktopAppState>();
    // Own the lifecycle before opening the peer directory and embedded node,
    // not only while installing the result. Managed-runtime restart refreshes
    // its ephemeral P2P endpoint under this same lock. Without covering the
    // open, that refresh can observe no installed core while the detached
    // starter already holds the peer-directory lease, then fail trying to use
    // the offline writer for the same directory.
    let start_result =
        match acquire_client_lifecycle(Arc::clone(&state.client_lifecycle), CLIENT_LIFECYCLE_WAIT)
            .await
        {
            Ok(lifecycle) => start_client_core_async(paths, lifecycle).await,
            Err(error) => Err((error, None)),
        };
    {
        let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
        bridge.incompatible_client_store = match &start_result {
            Ok(_) => None,
            Err((_, incompatible)) => incompatible.clone(),
        };
    }
    let start_result = start_result.map_err(|(error, _)| error);

    match start_result {
        Ok((core, _lifecycle_guard)) => {
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
            tracing::error!(
                error = %error.message,
                code = error.code.as_str(),
                "desktop client start: single-flight failed"
            );
            let _ = progress_tx.send(ClientStartProgress::Failed(error));
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
            ClientStartProgress::Failed(error) => return Err(error),
        }
    }
}

fn start_inflight_is_pending(state: &DesktopAppState) -> bool {
    state
        .bridge
        .lock()
        .expect("desktop bridge lock poisoned")
        .start_inflight
        .is_some()
}

/// Wait until any in-flight start finishes (ready or failed). Does **not** hold
/// `client_lifecycle` so the detached starter can acquire it to install.
async fn wait_for_start_inflight(state: &DesktopAppState) -> Result<(), BridgeError> {
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

/// Stops the live desktop client, if any, and releases its store. The caller
/// holds the managed-server lifecycle lock.
pub(super) async fn shutdown_client_under_managed_lifecycle<R: Runtime>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<(), BridgeError> {
    super::managed_server::drain_managed_runtime_pairing(state).await;
    drop(exclude_client(state).await?);
    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent::coarse("lifecycle"),
    );
    Ok(())
}

/// Stops the live desktop client and returns the owned client lifecycle lock
/// with no client running and no start in flight. The lock is the one a
/// start thread owns while it opens the store (and keeps until a late core
/// is closed), so while the guard is held no start, current or timed out,
/// has the store, peer directory or keys open, and the caller may move or
/// delete the client state.
pub(super) async fn exclude_client(
    state: &DesktopAppState,
) -> Result<ClientLifecycleGuard, BridgeError> {
    // Drain an in-flight start without the lifecycle lock so its starter can
    // finish installing; retry if another start is claimed in between.
    loop {
        wait_for_start_inflight(state).await?;
        let lifecycle_guard = Arc::clone(&state.client_lifecycle).lock_owned().await;
        if start_inflight_is_pending(state) {
            drop(lifecycle_guard);
            continue;
        }
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
                .map_err(|error| BridgeError::untyped(error.to_string()))?;
        }
        return Ok(lifecycle_guard);
    }
}

/// Quits the desktop application without touching any local state; the
/// "keep it and quit" answer to a home this version cannot open.
#[tauri::command]
pub fn desktop_app_quit<R: Runtime>(app: AppHandle<R>) {
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

/// How long a start waits for the store while an earlier start that stalled
/// is still closing it.
const CLIENT_LIFECYCLE_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

async fn acquire_client_lifecycle(
    lifecycle: Arc<tokio::sync::Mutex<()>>,
    wait: std::time::Duration,
) -> Result<ClientLifecycleGuard, BridgeError> {
    tokio::time::timeout(wait, lifecycle.lock_owned())
        .await
        .map_err(|_| {
            BridgeError::new(
                BridgeErrorCode::ClientStartFailed,
                "a previous desktop client start is still closing its store; try again shortly",
            )
        })
}
type ClientStartDelivery<T> = (anyhow::Result<T>, ClientLifecycleGuard);

type ClientStartFailure = (
    BridgeError,
    Option<gents::storage_backend::IncompatibleStore>,
);

/// Open the embedded node on a large-stack OS thread without blocking a Tokio
/// worker: the worker awaits a oneshot instead of `thread::join`. The thread
/// owns the lifecycle guard and hands it back with the core.
async fn start_client_core_async(
    paths: gents_desktop_core::client::DesktopPaths,
    lifecycle: ClientLifecycleGuard,
) -> Result<(ClientCore, ClientLifecycleGuard), ClientStartFailure> {
    let node_data_dir = paths.node_data_dir().to_path_buf();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let (stage_tx, stage_rx) = watch::channel(NO_STAGE_COMPLETED);
    std::thread::Builder::new()
        .name("desktop-client-start".to_string())
        .stack_size(CLIENT_START_STACK_SIZE)
        .spawn(move || {
            tauri::async_runtime::block_on(async move {
                let result = ClientCore::start_with_paths_reporting_stages(paths, stage_tx).await;
                deliver_client_start(tx, result, lifecycle, |core: ClientCore| async move {
                    core.shutdown().await
                })
                .await;
            })
        })
        .map_err(|error| {
            (
                BridgeError::new(
                    BridgeErrorCode::ClientStartFailed,
                    format!("spawning desktop client startup thread: {error}"),
                ),
                None,
            )
        })?;
    bounded_client_start(rx, stage_rx, CLIENT_START_STALL_TIMEOUT, &node_data_dir).await
}

/// Hands the started core and the lifecycle guard to the waiting starter. A
/// starter that gave up no longer owns them: the late core is closed first,
/// and only then is the guard released, so init and reset never remove a
/// store that is still open.
async fn deliver_client_start<T, S, SF, E>(
    tx: tokio::sync::oneshot::Sender<ClientStartDelivery<T>>,
    result: anyhow::Result<T>,
    lifecycle: ClientLifecycleGuard,
    shutdown: S,
) where
    S: FnOnce(T) -> SF,
    SF: std::future::Future<Output = Result<(), E>>,
    E: std::fmt::Display,
{
    if let Err((Ok(late), lifecycle)) = tx.send((result, lifecycle)) {
        tracing::warn!(
            "desktop client start: closing a core that opened after its start timed out"
        );
        if let Err(error) = shutdown(late).await {
            tracing::warn!(error = %error, "desktop client start: failed to close the late core");
        }
        drop(lifecycle);
    }
}

async fn bounded_client_start<T>(
    mut delivered: tokio::sync::oneshot::Receiver<ClientStartDelivery<T>>,
    mut completed_stage: watch::Receiver<&'static str>,
    stall: std::time::Duration,
    node_data_dir: &std::path::Path,
) -> Result<(T, ClientLifecycleGuard), ClientStartFailure> {
    let mut deadline = tokio::time::Instant::now() + stall;
    let mut reporting = true;
    loop {
        tokio::select! {
            delivered = &mut delivered => {
                return match delivered {
                    Ok((Ok(core), lifecycle)) => Ok((core, lifecycle)),
                    Ok((Err(error), _)) => Err(classify_client_start_error(&error, node_data_dir)),
                    Err(_) => Err((
                        BridgeError::new(
                            BridgeErrorCode::ClientStartFailed,
                            "desktop client startup thread panicked or dropped its result",
                        ),
                        None,
                    )),
                };
            }
            changed = completed_stage.changed(), if reporting => match changed {
                Ok(()) => deadline = tokio::time::Instant::now() + stall,
                Err(_) => reporting = false,
            },
            () = tokio::time::sleep_until(deadline) => {
                return Err((
                    BridgeError::new(
                        BridgeErrorCode::ClientStartFailed,
                        format!(
                            "the desktop client made no startup progress for {} seconds (last completed stage: {})",
                            stall.as_secs(),
                            *completed_stage.borrow()
                        ),
                    ),
                    None,
                ));
            }
        }
    }
}

/// A client store this version cannot open is `IncompatibleLocalStore`,
/// naming the store; every other start failure stays untyped.
fn classify_client_start_error(
    error: &anyhow::Error,
    node_data_dir: &std::path::Path,
) -> ClientStartFailure {
    match gents::storage_backend::incompatible_store(error, node_data_dir) {
        Some(store) => (
            BridgeError::new(BridgeErrorCode::IncompatibleLocalStore, store.to_string()),
            Some(store),
        ),
        None => (BridgeError::untyped(error.to_string()), None),
    }
}

#[tauri::command]
pub fn desktop_set_selected_agent<R: Runtime>(
    app: AppHandle<R>,
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
    // The backend is shared by all native views; only a single-view host may
    // narrow its observation/request lookup scope to the selected agent.
    core.set_selected_agent_did(if app.webview_windows().len() > 1 {
        None
    } else {
        did.clone()
    });

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

    #[tokio::test]
    async fn a_stalled_client_start_keeps_the_store_locked_until_its_core_closes() {
        let lifecycle = Arc::new(tokio::sync::Mutex::new(()));
        let (delivery_tx, delivery_rx) = tokio::sync::oneshot::channel::<ClientStartDelivery<u8>>();
        let (stage_tx, stage_rx) = watch::channel(NO_STAGE_COMPLETED);
        let starter_guard = Arc::clone(&lifecycle).lock_owned().await;
        stage_tx.send_replace("embedded_node");

        let error = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            bounded_client_start(
                delivery_rx,
                stage_rx,
                std::time::Duration::from_millis(20),
                std::path::Path::new("/tmp/node"),
            ),
        )
        .await
        .expect("a stalled start is bounded")
        .expect_err("a stalled start fails")
        .0;
        assert_eq!(error.code, BridgeErrorCode::ClientStartFailed);
        assert!(error.message.contains("embedded_node"), "{}", error.message);

        // Waiters are released, but init or a retry still waits for the lock
        // the stalled start holds.
        let init = tokio::spawn({
            let lifecycle = Arc::clone(&lifecycle);
            async move {
                let _guard = lifecycle.lock().await;
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !init.is_finished(),
            "the store is still owned by the late start"
        );

        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        deliver_client_start(delivery_tx, Ok(7), starter_guard, {
            let closed = Arc::clone(&closed);
            move |_late: u8| async move {
                assert!(
                    lifecycle.try_lock().is_err(),
                    "the lock is released only after the late core closes"
                );
                closed.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok::<(), std::convert::Infallible>(())
            }
        })
        .await;
        assert!(closed.load(std::sync::atomic::Ordering::SeqCst));
        tokio::time::timeout(std::time::Duration::from_secs(5), init)
            .await
            .expect("init proceeds once the late core is closed")
            .unwrap();
    }

    #[tokio::test]
    async fn a_retry_reports_a_previous_start_that_is_still_closing() {
        let lifecycle = Arc::new(tokio::sync::Mutex::new(()));
        let held = Arc::clone(&lifecycle).lock_owned().await;
        let error =
            acquire_client_lifecycle(Arc::clone(&lifecycle), std::time::Duration::from_millis(20))
                .await
                .expect_err("the store is still owned by the stalled start");
        assert_eq!(error.code, BridgeErrorCode::ClientStartFailed);
        assert!(error.message.contains("still closing"), "{}", error.message);
        drop(held);
        acquire_client_lifecycle(lifecycle, std::time::Duration::from_millis(20))
            .await
            .expect("a retry proceeds once the late core is closed");
    }

    #[tokio::test]
    async fn client_start_is_bounded_by_time_without_progress() {
        let lifecycle = Arc::new(tokio::sync::Mutex::new(()));
        let (delivery_tx, delivery_rx) = tokio::sync::oneshot::channel::<ClientStartDelivery<u8>>();
        let (stage_tx, stage_rx) = watch::channel(NO_STAGE_COMPLETED);
        let guard = Arc::clone(&lifecycle).lock_owned().await;
        let progress = tokio::spawn(async move {
            // Each stage lands within the stall bound; together they exceed it.
            for stage in [
                "paths_and_identity",
                "embedded_node",
                "saved_peer",
                "saved_peer",
            ] {
                tokio::time::sleep(std::time::Duration::from_millis(60)).await;
                stage_tx.send_replace(stage);
            }
            deliver_client_start(delivery_tx, Ok(7), guard, |_late: u8| async {
                Ok::<(), std::convert::Infallible>(())
            })
            .await;
        });
        let (core, _lifecycle) = bounded_client_start(
            delivery_rx,
            stage_rx,
            std::time::Duration::from_millis(150),
            std::path::Path::new("/tmp/node"),
        )
        .await
        .expect("a start that keeps progressing is not cut off");
        assert_eq!(core, 7);
        progress.await.unwrap();
    }

    #[test]
    fn an_older_client_store_fails_start_as_an_incompatible_local_store() {
        let node = std::path::Path::new("/tmp/desktop/node");
        let refused = anyhow::Error::new(gents::migration::MigrationError::UnknownLineage {
            collection: "AgentSession".into(),
            versions: "bafy-old".into(),
        })
        .context("ensure_migrations");
        let (error, store) = classify_client_start_error(&refused, node);
        assert_eq!(error.code, BridgeErrorCode::IncompatibleLocalStore);
        assert!(
            error.message.contains("/tmp/desktop/node"),
            "{}",
            error.message
        );
        assert_eq!(
            store.map(|store| store.kind),
            Some(gents::storage_backend::IncompatibleStoreKind::UnknownLineage)
        );

        let (error, store) =
            classify_client_start_error(&anyhow::anyhow!("peer directory is locked"), node);
        assert_eq!(error.code, BridgeErrorCode::Unknown);
        assert!(store.is_none());
    }

    fn test_state() -> (tempfile::TempDir, DesktopAppState) {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = crate::config::BridgeConfig {
            home: crate::config::HomePolicy::FixedRoot(temp.path().join("desktop")),
            bootstrap: crate::config::BootstrapPolicy::PairedRemoteOnly,
            ..Default::default()
        };
        let policy = crate::state::resolve_policy(&config, None).expect("policy");
        (temp, DesktopAppState::new(policy))
    }

    /// A reset holds the client exclusion across shutdown and retirement: an
    /// in-flight start finishes first, and a start claimed meanwhile cannot
    /// reach the client state until the exclusion is released.
    #[tokio::test]
    async fn a_client_start_cannot_open_the_store_while_a_reset_excludes_it() {
        use std::sync::Arc;
        use std::time::Duration;

        let (_temp, state) = test_state();
        let state = Arc::new(state);

        // A start is in flight: the exclusion waits for it.
        let (progress, _) = watch::channel(ClientStartProgress::Pending);
        state.bridge.lock().unwrap().start_inflight = Some(progress.clone());
        let excluding = {
            let state = Arc::clone(&state);
            tokio::spawn(async move {
                let guard = exclude_client(&state).await.expect("exclude");
                drop(guard);
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !excluding.is_finished(),
            "an in-flight start is drained first"
        );
        state.bridge.lock().unwrap().start_inflight = None;
        let _ = progress.send(ClientStartProgress::Ready);
        tokio::time::timeout(Duration::from_secs(5), excluding)
            .await
            .expect("exclusion follows the drained start")
            .unwrap();

        // While excluded, a newly claimed starter waits at the lifecycle lock
        // that guards opening the store.
        let guard = exclude_client(&state).await.expect("exclude");
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let opened = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let starter = {
            let state = Arc::clone(&state);
            let barrier = Arc::clone(&barrier);
            let opened = Arc::clone(&opened);
            tokio::spawn(async move {
                barrier.wait().await;
                let _lifecycle = state.client_lifecycle.lock().await;
                opened.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        };
        barrier.wait().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !opened.load(std::sync::atomic::Ordering::SeqCst),
            "no start opens client state during retirement"
        );
        drop(guard);
        tokio::time::timeout(Duration::from_secs(5), starter)
            .await
            .expect("the start proceeds after the reset")
            .unwrap();
        assert!(opened.load(std::sync::atomic::Ordering::SeqCst));
    }

    /// A desktop state whose principal key an older build wrote with ambient
    /// (0644) permissions fails start as an incompatible local store, before
    /// any node or peer directory is opened, and the key is left as it was.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_insecure_old_client_key_fails_start_as_an_incompatible_local_store() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = gents_desktop_core::client::DesktopPaths::from_root(temp.path());
        gents::identity::load_or_create_file_identity(paths.identity_key_path()).unwrap();
        std::fs::set_permissions(
            paths.identity_key_path(),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();

        let lifecycle = Arc::new(tokio::sync::Mutex::new(())).lock_owned().await;
        let Err((error, store)) = start_client_core_async(paths.clone(), lifecycle).await else {
            panic!("an insecure key must not start the client");
        };
        assert_eq!(error.code, BridgeErrorCode::IncompatibleLocalStore);
        assert!(
            error.message.contains("unsafe file permissions"),
            "{}",
            error.message
        );
        let store = store.expect("typed refusal");
        assert_eq!(
            store.kind,
            gents::storage_backend::IncompatibleStoreKind::InsecureKey
        );
        assert_eq!(store.data_path, paths.identity_key_path());
        assert_eq!(
            std::fs::metadata(paths.identity_key_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
        assert!(!paths.peer_directory_path().exists());
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
        let failed = ClientStartProgress::Failed(BridgeError::untyped("boom"));
        match failed {
            ClientStartProgress::Failed(error) => assert_eq!(error.message, "boom"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}
