mod bootstrap;
mod client_binding;
mod panels;
mod shell_actions;

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::client::{ClientCore, P2PHealth, P2PHealthStatus};
use crate::state::ShellState;
use crate::telemetry::{global_log_store, DesktopLogStore};
use eframe::egui::{self, Context};
use egui_commonmark::CommonMarkCache;
use tokio::runtime::Runtime;
use tokio::sync::watch;

use self::bootstrap::{
    apply_bootstrap_failure_state, apply_bootstrap_state, apply_client_transport_state,
    apply_p2p_health_state, live_error_count, sync_shell_state_from_client, ClientRestartPlan,
};

const P2P_AUTO_RESTART_COOLDOWN: Duration = Duration::from_secs(20);
const CLIENT_RESTART_MAX_ATTEMPTS: usize = 10;
const CLIENT_RESTART_BACKOFF: Duration = Duration::from_millis(250);

#[cfg(test)]
pub(crate) fn should_auto_restart_p2p(
    previous: Option<&P2PHealth>,
    next: &P2PHealth,
    last_attempt: Option<Instant>,
    now: Instant,
    cooldown: Duration,
) -> bool {
    bootstrap::should_auto_restart_p2p(previous, next, last_attempt, now, cooldown)
}

pub struct DesktopApp {
    state: ShellState,
    client: Option<Arc<ClientCore>>,
    client_generation: u64,
    store_updates: Option<watch::Receiver<u64>>,
    p2p_health_updates: Option<watch::Receiver<P2PHealth>>,
    client_restart_plan: Option<ClientRestartPlan>,
    last_p2p_health: Option<P2PHealth>,
    last_auto_p2p_restart_at: Option<Instant>,
    bootstrap_errors: Vec<String>,
    log_store: Arc<DesktopLogStore>,
    markdown_cache: CommonMarkCache,
    runtime: Arc<Runtime>,
}

impl DesktopApp {
    pub fn new(cc: &eframe::CreationContext<'_>, runtime: Arc<Runtime>) -> Self {
        let (client, bootstrap_errors) = match runtime.block_on(ClientCore::start()) {
            Ok(core) => {
                let client = Arc::new(core);
                (Some(client.clone()), Vec::new())
            }
            Err(error) => (None, vec![error.to_string()]),
        };

        Self::from_parts(cc, runtime, client, bootstrap_errors, global_log_store())
    }
}

impl eframe::App for DesktopApp {
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.state.status.advance_frame();

        if let Some(reason) = self.state.pending_client_restart_reason.take() {
            self.restart_client(&reason);
            ctx.request_repaint();
            return;
        }

        let mut auto_restart_reason: Option<&'static str> = None;
        self.process_pending_shell_actions();

        if let Some(client) = self.client.as_ref() {
            apply_client_transport_state(&mut self.state, client);
            let current_health = client.p2p_health();
            apply_p2p_health_state(&mut self.state, &current_health);
            if self.last_p2p_health.is_none() {
                self.last_p2p_health = Some(current_health);
            }
            sync_shell_state_from_client(&mut self.state, client);

            if let Some(store_updates) = &mut self.store_updates {
                if store_updates.has_changed().unwrap_or(false) {
                    let _ = store_updates.borrow_and_update();
                    ctx.request_repaint();
                }
            }

            if let Some(p2p_health_updates) = &mut self.p2p_health_updates {
                if p2p_health_updates.has_changed().unwrap_or(false) {
                    let previous_health = self.last_p2p_health.clone();
                    let health = p2p_health_updates.borrow_and_update().clone();
                    apply_p2p_health_state(&mut self.state, &health);
                    self.last_p2p_health = Some(health.clone());
                    if health.status == P2PHealthStatus::Healthy {
                        self.last_auto_p2p_restart_at = None;
                    } else if bootstrap::should_auto_restart_p2p(
                        previous_health.as_ref(),
                        &health,
                        self.last_auto_p2p_restart_at,
                        Instant::now(),
                        P2P_AUTO_RESTART_COOLDOWN,
                    ) {
                        self.last_auto_p2p_restart_at = Some(Instant::now());
                        auto_restart_reason = Some("P2P transport wedged");
                    }
                    ctx.request_repaint();
                }
            }

            self.state.status.error_count = live_error_count(client)
                + self.bootstrap_errors.len()
                + usize::from(client.last_mutation_error().is_some());
        }

        if let Some(reason) = auto_restart_reason {
            self.restart_client(reason);
            ctx.request_repaint();
            return;
        }

        ctx.request_repaint_after(Duration::from_millis(33));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let store = self.client.as_ref().map(|client| client.store().snapshot());
        let store_ref = store.as_deref();

        self.show_status_bar(ui);
        self.show_sidebar(ui, store_ref);
        self.show_rail(ui, store_ref);
        self.show_main(ui, store_ref);
    }

    fn on_exit(&mut self) {
        self.shutdown_client();
    }
}

impl Drop for DesktopApp {
    fn drop(&mut self) {
        self.shutdown_client();
    }
}

#[cfg(test)]
mod tests;
