use std::future::Future;
use std::sync::Arc;

use tokio::time::sleep;

use crate::client::ClientCore;
use crate::telemetry::DesktopLogStore;
use crate::theme;

use super::{
    apply_bootstrap_failure_state, apply_bootstrap_state, apply_p2p_health_state,
    sync_shell_state_from_client, ClientRestartPlan, DesktopApp, CLIENT_RESTART_BACKOFF,
    CLIENT_RESTART_MAX_ATTEMPTS,
};

impl DesktopApp {
    pub(super) fn from_parts(
        cc: &eframe::CreationContext<'_>,
        runtime: Arc<tokio::runtime::Runtime>,
        client: Option<Arc<ClientCore>>,
        bootstrap_errors: Vec<String>,
        log_store: Arc<DesktopLogStore>,
    ) -> Self {
        theme::apply_theme(&cc.egui_ctx);
        let mut app = Self {
            state: crate::state::ShellState::default(),
            client: None,
            client_generation: 0,
            store_updates: None,
            p2p_health_updates: None,
            client_restart_plan: client
                .as_ref()
                .map(|client| ClientRestartPlan::from_client(client.as_ref()))
                .or_else(ClientRestartPlan::discovered_default),
            last_p2p_health: None,
            last_auto_p2p_restart_at: None,
            bootstrap_errors: Vec::new(),
            log_store,
            markdown_cache: egui_commonmark::CommonMarkCache::default(),
            runtime,
        };
        app.attach_client(client, bootstrap_errors);
        app
    }

    pub(super) fn block_on_runtime<T>(&self, future: impl Future<Output = T>) -> T {
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| self.runtime.block_on(future))
        } else {
            self.runtime.block_on(future)
        }
    }

    pub(super) fn attach_client(
        &mut self,
        client: Option<Arc<ClientCore>>,
        bootstrap_errors: Vec<String>,
    ) {
        if client.is_some() {
            self.client_generation = self.client_generation.saturating_add(1);
        }
        self.client = client;
        self.bootstrap_errors = bootstrap_errors;
        self.store_updates = None;
        self.p2p_health_updates = None;
        self.last_p2p_health = None;

        if let Some(client) = self.client.as_ref() {
            self.client_restart_plan = Some(ClientRestartPlan::from_client(client.as_ref()));
            self.store_updates = Some(client.store_updates());
            self.p2p_health_updates = Some(client.p2p_health_updates());

            apply_bootstrap_state(&mut self.state, client.as_ref());
            let health = client.p2p_health();
            apply_p2p_health_state(&mut self.state, &health);
            self.last_p2p_health = Some(health);
            sync_shell_state_from_client(&mut self.state, client.as_ref());
        } else {
            apply_bootstrap_failure_state(&mut self.state);
        }
    }

    pub(super) fn shutdown_client(&mut self) {
        let Some(client) = self.client.take() else {
            self.store_updates = None;
            self.p2p_health_updates = None;
            self.last_p2p_health = None;
            return;
        };

        self.store_updates = None;
        self.p2p_health_updates = None;
        self.last_p2p_health = None;
        if let Err(error) = self.block_on_runtime(client.shutdown()) {
            tracing::error!(error = %error, "failed to shut down desktop client");
            self.bootstrap_errors
                .push(format!("desktop shutdown failed: {error}"));
        }
    }

    pub(super) fn restart_client(&mut self, reason: &str) -> bool {
        let Some(plan) = self.client_restart_plan.clone() else {
            let message = format!(
                "Desktop client restart requested, but no restart plan is available ({reason})."
            );
            tracing::warn!(reason, "desktop client restart skipped");
            self.bootstrap_errors.push(message.clone());
            self.state.peers.last_action_message = Some(message);
            return false;
        };

        tracing::warn!(reason, "restarting desktop client core");
        self.shutdown_client();

        match self.start_client_with_retry(&plan) {
            Ok(core) => {
                self.attach_client(Some(Arc::new(core)), Vec::new());
                self.state.peers.last_action_message =
                    Some(format!("Restarted desktop client core after {reason}."));
                true
            }
            Err(error) => {
                let message = format!("desktop client restart failed after {reason}: {error:#}");
                tracing::error!(reason, error = %error, "desktop client restart failed");
                self.attach_client(None, vec![message.clone()]);
                self.state.peers.last_action_message =
                    Some(format!("Restart failed after {reason}: {error:#}"));
                false
            }
        }
    }

    fn start_client_with_retry(&self, plan: &ClientRestartPlan) -> anyhow::Result<ClientCore> {
        self.block_on_runtime(async {
            let mut last_error = None;
            for attempt in 1..=CLIENT_RESTART_MAX_ATTEMPTS {
                match plan.start().await {
                    Ok(core) => return Ok(core),
                    Err(error) if attempt < CLIENT_RESTART_MAX_ATTEMPTS => {
                        tracing::warn!(
                            attempt,
                            max_attempts = CLIENT_RESTART_MAX_ATTEMPTS,
                            error = %error,
                            "desktop client restart attempt failed; retrying"
                        );
                        last_error = Some(error);
                        sleep(CLIENT_RESTART_BACKOFF).await;
                    }
                    Err(error) => return Err(error),
                }
            }

            Err(last_error.expect("restart loop should retain the last start error"))
        })
    }
}
