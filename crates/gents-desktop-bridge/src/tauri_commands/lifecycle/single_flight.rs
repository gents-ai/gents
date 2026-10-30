use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use tokio::sync::watch;

use super::core_start::{acquire_client_lifecycle, start_client_core_async, CLIENT_LIFECYCLE_WAIT};
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::{spawn_client_update_task, ClientStartProgress, DesktopAppState};
use crate::types::ClientUpdateEvent;

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

pub(super) fn claim_or_join_client_start<R>(
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

pub(super) async fn run_detached_client_start<R>(
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

pub(super) async fn wait_for_client_start_progress(
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

pub(super) fn start_inflight_is_pending(state: &DesktopAppState) -> bool {
    state
        .bridge
        .lock()
        .expect("desktop bridge lock poisoned")
        .start_inflight
        .is_some()
}

pub(super) async fn wait_for_start_inflight(state: &DesktopAppState) -> Result<(), BridgeError> {
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
