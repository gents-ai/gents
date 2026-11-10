use std::sync::Arc;

use super::single_flight::{start_inflight_is_pending, wait_for_start_inflight};
use super::ClientLifecycleGuard;
use crate::error::BridgeError;
use crate::state::DesktopAppState;

pub(in crate::tauri_commands) async fn exclude_client(
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

pub(super) async fn shutdown_excluded_client<Fut>(
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
