use std::sync::Arc;

use gents_desktop_core::client::ClientCore;
use tokio::sync::watch;

use super::ClientLifecycleGuard;
use crate::error::{BridgeError, BridgeErrorCode};

const CLIENT_START_STACK_SIZE: usize = 16 * 1024 * 1024;
const CLIENT_START_STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
pub(super) const NO_STAGE_COMPLETED: &str = "none";

pub(super) const CLIENT_LIFECYCLE_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

pub(super) async fn acquire_client_lifecycle(
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
pub(super) type ClientStartDelivery<T> = (anyhow::Result<T>, ClientLifecycleGuard);

pub(super) type ClientStartFailure = (
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
pub(super) async fn start_client_core_async(
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
pub(super) async fn deliver_client_start<T, S, SF, E>(
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

pub(super) async fn bounded_client_start<T>(
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

pub(super) fn classify_client_start_error(
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
