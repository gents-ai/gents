use std::sync::Arc;

use gents_desktop_core::client::ClientCore;
use gents_desktop_core::local_runtime::{
    dangerously_overwrite_desktop_home, init_standard_local_runtime, reset_desktop_runtime_state,
    DesktopInitOptions, DesktopInitSummary,
};
use tauri::{AppHandle, Emitter, Manager, Runtime, State, WebviewWindow};
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
        super::managed_server::pairing::start_running_managed_pairing(&state, Arc::clone(&core))
            .await;
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

pub(crate) enum ClientStartRegistration {
    Joined(watch::Receiver<ClientStartProgress>),
    Ready(watch::Receiver<ClientStartProgress>),
    Claimed {
        progress_tx: watch::Sender<ClientStartProgress>,
        progress_rx: watch::Receiver<ClientStartProgress>,
    },
}

pub(crate) fn register_client_start(
    state: &DesktopAppState,
) -> Result<ClientStartRegistration, BridgeError> {
    let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");

    if state.app_exit.is_quitting() {
        return Err(BridgeError::new(
            BridgeErrorCode::ClientStartFailed,
            "desktop client cannot start while the desktop is shutting down",
        ));
    }

    if let Some(sender) = bridge.start_inflight.as_ref() {
        tracing::debug!("desktop client start: joining in-flight single-flight start");
        return Ok(ClientStartRegistration::Joined(sender.subscribe()));
    }

    let (tx, rx) = watch::channel(ClientStartProgress::Pending);
    if bridge.core.is_some() {
        let _ = tx.send(ClientStartProgress::Ready);
        return Ok(ClientStartRegistration::Ready(rx));
    }

    bridge.start_inflight = Some(tx.clone());
    Ok(ClientStartRegistration::Claimed {
        progress_tx: tx,
        progress_rx: rx,
    })
}

fn claim_or_join_client_start<R>(
    app: &AppHandle<R>,
    state: &State<'_, DesktopAppState>,
) -> watch::Receiver<ClientStartProgress>
where
    R: Runtime,
{
    let registration = match register_client_start(state.inner()) {
        Ok(registration) => registration,
        Err(error) => {
            let (sender, receiver) = watch::channel(ClientStartProgress::Failed(error));
            drop(sender);
            return receiver;
        }
    };
    match registration {
        ClientStartRegistration::Joined(receiver) | ClientStartRegistration::Ready(receiver) => {
            receiver
        }
        ClientStartRegistration::Claimed {
            progress_tx,
            progress_rx,
        } => {
            let paths = state.policy.desktop_paths.clone();
            let app_for_start = app.clone();

            tracing::info!("desktop client start: single-flight starter claimed");
            tauri::async_runtime::spawn(async move {
                run_detached_client_start(app_for_start, paths, progress_tx).await;
            });
            progress_rx
        }
    }
}

async fn run_detached_client_start<R>(
    app: AppHandle<R>,
    paths: gents_desktop_core::client::DesktopPaths,
    progress_tx: watch::Sender<ClientStartProgress>,
) where
    R: Runtime,
{
    let state = app.state::<DesktopAppState>();
    #[cfg(any(unix, windows))]
    let mut client_listener = None;
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
            Ok(lifecycle) => {
                #[cfg(any(unix, windows))]
                {
                    match reserve_client_listener(&state.policy).await {
                        Ok((startup, listener)) => {
                            client_listener = listener;
                            let result = start_client_core_async(
                                paths,
                                state.policy.store_key_custody,
                                lifecycle,
                            )
                            .await;
                            drop(startup);
                            result
                        }
                        Err(error) => Err((error, None)),
                    }
                }
                #[cfg(not(any(unix, windows)))]
                start_client_core_async(paths, state.policy.store_key_custody, lifecycle).await
            }
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
            #[cfg(any(unix, windows))]
            let mut client_host = client_listener.map(|listener| {
                crate::host::ClientHostService::with_accounts(
                    core.clone(),
                    listener,
                    state.pending_oauth_credentials.clone(),
                )
            });
            let orphan = {
                let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
                let orphan = if bridge.core.is_none() {
                    let updates_task = spawn_client_update_task(app.clone(), Arc::clone(&core));
                    bridge.install_core(Arc::clone(&core));
                    bridge.updates_task = Some(updates_task);
                    #[cfg(any(unix, windows))]
                    {
                        bridge.client_host = client_host.take();
                    }
                    None
                } else {
                    // Drop our open node so the store is not held by an
                    // untracked Arc.
                    Some(core)
                };
                bridge.start_inflight = None;
                orphan
            };

            if let Some(orphan_core) = orphan {
                #[cfg(any(unix, windows))]
                if let Some(host) = client_host {
                    if let Err(error) = host.shutdown().await {
                        tracing::warn!(%error, "closing orphan client attachments failed");
                    }
                }
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

#[cfg(any(unix, windows))]
async fn reserve_client_listener(
    policy: &crate::config::ResolvedBridgePolicy,
) -> Result<
    (
        Option<gents::file_lock::FileLock>,
        Option<crate::host::local::LocalListener>,
    ),
    BridgeError,
> {
    use crate::host::local::{startup_lock, LocalListener};

    if !policy.client_host {
        return Ok((None, None));
    }
    let startup = match tokio::time::timeout(
        CLIENT_LIFECYCLE_WAIT,
        startup_lock(&policy.desktop_paths),
    )
    .await
    {
        Ok(Ok(startup)) => startup,
        Ok(Err(error)) => {
            tracing::warn!(%error, "local client attachments are unavailable");
            return Ok((None, None));
        }
        Err(_) => {
            return Err(BridgeError::new(
                BridgeErrorCode::ClientStartFailed,
                "another frontend is starting the client installation; try again shortly",
            ))
        }
    };
    let listener = match LocalListener::bind(&policy.desktop_paths).await {
        Ok(listener) => Some(listener),
        Err(error)
            if matches!(
                error.downcast_ref::<std::fs::TryLockError>(),
                Some(std::fs::TryLockError::WouldBlock)
            ) =>
        {
            return Err(BridgeError::new(BridgeErrorCode::ClientStartFailed,
                "the client installation is already in use; close its editor attachments before starting the desktop"));
        }
        Err(error) => {
            tracing::warn!(%error, "local client attachments are unavailable");
            None
        }
    };
    Ok((Some(startup), listener))
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
        if let Ok(()) = wait_for_client_start_progress(progress_rx).await {}
    }
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
        #[cfg(any(unix, windows))]
        let client_host;
        let (core, updates_task) = {
            let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
            #[cfg(any(unix, windows))]
            {
                client_host = bridge.client_host.take();
            }
            (bridge.core.take(), bridge.updates_task.take())
        };
        if let Some(task) = updates_task {
            task.abort();
        }
        return shutdown_excluded_client(lifecycle_guard, async move {
            #[cfg(any(unix, windows))]
            let attachments_result = match client_host {
                Some(host) => host
                    .shutdown()
                    .await
                    .map_err(|error| BridgeError::untyped(error.to_string())),
                None => Ok(()),
            };
            let core_result = match core {
                Some(core) => core
                    .shutdown()
                    .await
                    .map_err(|error| BridgeError::untyped(error.to_string())),
                None => Ok(()),
            };
            #[cfg(any(unix, windows))]
            attachments_result?;
            core_result
        })
        .await;
    }
}

async fn shutdown_excluded_client<Fut>(
    lifecycle_guard: ClientLifecycleGuard,
    shutdown: Fut,
) -> Result<ClientLifecycleGuard, BridgeError>
where
    Fut: std::future::Future<Output = Result<(), BridgeError>> + Send + 'static,
{
    let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let result = shutdown.await;
        let _ = finished_tx.send((lifecycle_guard, result));
    });
    let (lifecycle_guard, result) = finished_rx
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    result?;
    Ok(lifecycle_guard)
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
#[expect(
    clippy::disallowed_methods,
    reason = "dedicated large-stack OS thread outside the runtime workers; the caller awaits a oneshot"
)]
async fn start_client_core_async(
    paths: gents_desktop_core::client::DesktopPaths,
    store_key_custody: gents::store_key::StoreKeyCustodyChoice,
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
                let options = gents_desktop_core::client::ClientCoreOptions {
                    store_key_custody,
                    ..Default::default()
                };
                let result =
                    ClientCore::start_with_paths_reporting_stages(paths, options, stage_tx).await;
                deliver_client_start(tx, result, lifecycle, |core: ClientCore| async move {
                    Arc::new(core).shutdown().await
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

fn classify_client_start_error(
    error: &anyhow::Error,
    node_data_dir: &std::path::Path,
) -> ClientStartFailure {
    match gents::storage_backend::incompatible_store(error.as_ref(), node_data_dir) {
        Some(store) => (
            BridgeError::new(BridgeErrorCode::IncompatibleLocalStore, store.to_string()),
            Some(store),
        ),
        None => (BridgeError::untyped(error.to_string()), None),
    }
}

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
