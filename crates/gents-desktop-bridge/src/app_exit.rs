use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tauri::{AppHandle, Emitter, ExitRequestApi, Manager, Runtime};

use crate::config::ManagedServerPolicy;
use crate::contract::APP_QUIT_FAILED_EVENT;
use crate::error::BridgeError;
use crate::state::DesktopAppState;
use crate::tauri_commands::{lifecycle, managed_server};

/// Tauri emits another exit request when the prepared task calls `app.exit`.
/// That request may proceed only after the native service owner has settled.
#[derive(Default)]
pub(crate) struct AppExitState {
    pending: AtomicBool,
    ready: AtomicBool,
}

struct ExitPreparation(Arc<AppExitState>);

impl AppExitState {
    pub(crate) fn is_quitting(&self) -> bool {
        self.pending.load(Ordering::Acquire) || self.ready.load(Ordering::Acquire)
    }

    fn begin(self: &Arc<Self>) -> Option<ExitPreparation> {
        if self.ready.load(Ordering::Acquire) {
            return None;
        }
        self.pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| ExitPreparation(self.clone()))
    }
}

impl ExitPreparation {
    fn complete(&self) {
        self.0.ready.store(true, Ordering::Release);
    }

    async fn settle(
        &self,
        stop: impl std::future::Future<Output = Result<(), BridgeError>>,
    ) -> Result<(), BridgeError> {
        stop.await?;
        self.complete();
        Ok(())
    }
}

impl Drop for ExitPreparation {
    fn drop(&mut self) {
        self.0.pending.store(false, Ordering::Release);
    }
}

pub(crate) fn requested<R>(app: &AppHandle<R>, api: &ExitRequestApi, code: Option<i32>)
where
    R: Runtime,
{
    // Tauri cannot defer a restart request; its next process owns startup.
    if code == Some(tauri::RESTART_EXIT_CODE) {
        return;
    }
    let Some(state) = app.try_state::<DesktopAppState>() else {
        return;
    };
    if state.app_exit.ready.load(Ordering::Acquire) {
        return;
    }
    api.prevent_exit();
    let Some(preparation) = state.app_exit.begin() else {
        return;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<DesktopAppState>();
        match preparation
            .settle(async {
                if state.policy.managed_server == ManagedServerPolicy::Allowed
                    && state.policy.node_home.is_some()
                {
                    managed_server::prepare_app_exit(&app, &state).await?;
                }
                lifecycle::shutdown_client_for_app_exit(&app, &state).await
            })
            .await
        {
            Ok(()) => {
                app.exit(code.unwrap_or(0));
            }
            Err(error) => {
                tracing::error!(error = %error.message, "could not finish desktop shutdown before quitting");
                let _ = app.emit_to("main", APP_QUIT_FAILED_EVENT, error.message);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn native_stop_must_finish_before_an_exit_can_proceed() {
        let state = Arc::new(AppExitState::default());
        let preparation = state.begin().unwrap();
        let (release, held) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            preparation
                .settle(async move {
                    held.await.unwrap();
                    Ok(())
                })
                .await
        });
        tokio::task::yield_now().await;
        assert!(state.is_quitting());
        assert!(!state.ready.load(Ordering::Acquire));
        release.send(()).unwrap();
        task.await.unwrap().unwrap();
        assert!(state.ready.load(Ordering::Acquire));
        assert!(state.begin().is_none());
    }

    #[tokio::test]
    async fn failed_native_stop_keeps_the_app_open_and_allows_another_attempt() {
        let state = Arc::new(AppExitState::default());
        let preparation = state.begin().unwrap();
        let error = preparation
            .settle(async { Err(BridgeError::untyped("service stop refused")) })
            .await
            .unwrap_err();
        assert_eq!(error.message, "service stop refused");
        assert!(!state.ready.load(Ordering::Acquire));
        drop(preparation);
        assert!(!state.is_quitting());
        assert!(state.begin().is_some());
    }

    #[tokio::test]
    async fn client_start_claim_is_blocked_during_exit_and_restored_after_failed_exit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = crate::config::BridgeConfig {
            home: crate::config::HomePolicy::FixedRoot(temp.path().join("desktop")),
            bootstrap: crate::config::BootstrapPolicy::PairedRemoteOnly,
            store_key_custody: gents::store_key::StoreKeyCustodyChoice::File,
            ..Default::default()
        };
        let policy = crate::state::resolve_policy(&config, None).expect("policy");
        let state = DesktopAppState::new(policy);

        let preparation = state.app_exit.clone().begin().expect("begin exit");
        assert!(matches!(
            crate::tauri_commands::lifecycle::single_flight::register_client_start(&state),
            Err(error) if error.code == crate::error::BridgeErrorCode::ClientStartFailed
        ));
        assert!(state
            .bridge
            .lock()
            .expect("bridge lock")
            .start_inflight
            .is_none());

        assert!(preparation
            .settle(async { Err(BridgeError::untyped("managed stop failed")) })
            .await
            .is_err());
        drop(preparation);
        assert!(!state.app_exit.is_quitting());
        let registration =
            crate::tauri_commands::lifecycle::single_flight::register_client_start(&state)
                .expect("failed exit permits client start retry");
        let crate::tauri_commands::lifecycle::single_flight::ClientStartRegistration::Claimed {
            progress_tx,
            progress_rx: _,
        } = registration
        else {
            panic!("retry must claim an empty client-start slot")
        };
        state.bridge.lock().expect("bridge lock").start_inflight = None;
        drop(progress_tx);

        let preparation = state.app_exit.clone().begin().expect("begin second exit");
        preparation
            .settle(async { Ok::<(), BridgeError>(()) })
            .await
            .expect("successful exit preparation");
        drop(preparation);
        assert!(state.app_exit.ready.load(Ordering::Acquire));
        assert!(state.app_exit.is_quitting());
        assert!(matches!(
            crate::tauri_commands::lifecycle::single_flight::register_client_start(&state),
            Err(error) if error.code == crate::error::BridgeErrorCode::ClientStartFailed
        ));
    }
}
