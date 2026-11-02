use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;
use std::future::Future;
use std::sync::Arc;

pub(super) type LifecycleGuard<'a> = tokio::sync::MutexGuard<'a, ()>;

pub(super) const START_CANCELLED: &str =
    "Starting the local Node was cancelled because the Node was stopped, restarted, or reconfigured.";
pub(super) const START_SUPERSEDED: &str =
    "This start of the local Node was replaced by a newer start.";

#[derive(Clone, Default)]
pub struct StartWait {
    token: tokio_util::sync::CancellationToken,
    reason: Arc<std::sync::OnceLock<&'static str>>,
}

impl StartWait {
    fn cancel(&self, reason: &'static str) {
        let _ = self.reason.set(reason);
        self.token.cancel();
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    async fn cancelled(&self) {
        self.token.cancelled().await
    }

    pub(super) fn cancel_message(&self) -> &'static str {
        self.reason.get().copied().unwrap_or(START_CANCELLED)
    }
}

pub(super) async fn begin_start_wait(state: &DesktopAppState) -> StartWait {
    let token = StartWait::default();
    let mut managed = state.managed_server.lock().await;
    if let Some(previous) = managed.start_wait.replace(token.clone()) {
        previous.cancel(START_SUPERSEDED);
    }
    managed.starting = true;
    managed.last_error = None;
    managed.approval_refused = false;
    token
}

pub(in crate::tauri_commands) async fn cancel_start_wait(state: &DesktopAppState) {
    let mut managed = state.managed_server.lock().await;
    if let Some(token) = managed.start_wait.take() {
        token.cancel(START_CANCELLED);
        managed.starting = false;
    }
}

pub(in crate::tauri_commands) async fn lock_lifecycle_superseding_start(
    state: &DesktopAppState,
) -> LifecycleGuard<'_> {
    cancel_start_wait(state).await;
    let lifecycle = state.managed_server_lifecycle.lock().await;
    cancel_start_wait(state).await;
    lifecycle
}

pub(super) async fn finish_start_wait(state: &DesktopAppState, token: &StartWait) {
    let mut managed = state.managed_server.lock().await;
    if !token.is_cancelled() {
        managed.start_wait = None;
        managed.starting = false;
    }
}

pub(super) async fn wait_unlocked<T>(
    token: &StartWait,
    wait: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    tokio::select! {
        biased;
        _ = token.cancelled() => anyhow::bail!(token.cancel_message()),
        result = wait => result,
    }
}

pub(super) async fn relock_start<'a>(
    state: &'a DesktopAppState,
    token: &StartWait,
) -> anyhow::Result<LifecycleGuard<'a>> {
    let lifecycle = state.managed_server_lifecycle.lock().await;
    if token.is_cancelled() {
        anyhow::bail!(token.cancel_message());
    }
    Ok(lifecycle)
}

pub(super) async fn ensure_no_start_waiting(state: &DesktopAppState) -> Result<(), BridgeError> {
    if state.managed_server.lock().await.start_wait.is_some() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "The local Node is still starting. Change start at login after it finishes.",
        ));
    }
    Ok(())
}

pub(super) fn ensure_not_cancelled(token: &StartWait) -> anyhow::Result<()> {
    if token.is_cancelled() {
        anyhow::bail!(token.cancel_message());
    }
    Ok(())
}
