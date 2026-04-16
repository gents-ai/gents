use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Context, Panel, RichText};
use egui_commonmark::CommonMarkCache;
use tokio::runtime::Runtime;
use tokio::sync::watch;
use tokio::time::sleep;

use crate::chat::controller as chat_controller;
use crate::client::{
    ClientCore, ClientCoreOptions, ClientStore, DesktopPaths, P2PHealth, P2PHealthStatus,
};
use crate::state::{Activity, PendingChatAction, PendingShellAction, ShellState};
use crate::telemetry::{global_log_store, DesktopLogStore};
use crate::theme;
use crate::views;

const P2P_AUTO_RESTART_COOLDOWN: Duration = Duration::from_secs(20);
const CLIENT_RESTART_MAX_ATTEMPTS: usize = 10;
const CLIENT_RESTART_BACKOFF: Duration = Duration::from_millis(250);

#[derive(Debug, Clone)]
struct ClientRestartPlan {
    paths: DesktopPaths,
    options: ClientCoreOptions,
}

impl ClientRestartPlan {
    fn from_client(client: &ClientCore) -> Self {
        Self {
            paths: client.paths().clone(),
            options: client.options().clone(),
        }
    }

    fn discovered_default() -> Option<Self> {
        DesktopPaths::discover().ok().map(|paths| Self {
            paths,
            options: ClientCoreOptions::default(),
        })
    }

    async fn start(&self) -> anyhow::Result<ClientCore> {
        ClientCore::start_with_paths_and_options(self.paths.clone(), self.options.clone()).await
    }
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

    fn from_parts(
        cc: &eframe::CreationContext<'_>,
        runtime: Arc<Runtime>,
        client: Option<Arc<ClientCore>>,
        bootstrap_errors: Vec<String>,
        log_store: Arc<DesktopLogStore>,
    ) -> Self {
        theme::apply_theme(&cc.egui_ctx);
        let mut app = Self {
            state: ShellState::default(),
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
            markdown_cache: CommonMarkCache::default(),
            runtime,
        };
        app.attach_client(client, bootstrap_errors);
        app
    }

    fn block_on_runtime<T>(&self, future: impl Future<Output = T>) -> T {
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| self.runtime.block_on(future))
        } else {
            self.runtime.block_on(future)
        }
    }

    fn attach_client(&mut self, client: Option<Arc<ClientCore>>, bootstrap_errors: Vec<String>) {
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

    fn process_pending_shell_actions(&mut self) {
        let pending_actions = self.state.drain_pending_shell_actions();
        for action in pending_actions {
            self.process_pending_shell_action(action);
        }
    }

    fn process_pending_shell_action(&mut self, action: PendingShellAction) {
        match action {
            PendingShellAction::Navigate(activity) => {
                self.state.activity = activity;
            }
            PendingShellAction::OpenPeersSetup => {
                self.state.activity = Activity::Peers;
                self.state.peers.show_add_form = true;
            }
            PendingShellAction::Chat(action) => self.process_pending_chat_action(action),
        }
    }

    fn process_pending_chat_action(&mut self, action: PendingChatAction) {
        match action {
            PendingChatAction::SelectDeployment { peer_id, agent_did } => {
                chat_controller::select_deployment(&mut self.state.chat, peer_id, agent_did);
            }
            PendingChatAction::SelectConversation { session_id } => {
                chat_controller::select_conversation(&mut self.state.chat, session_id);
            }
            PendingChatAction::CreateConversation => {
                if let Err(error) = chat_controller::create_conversation(
                    &mut self.state.chat,
                    self.client.as_deref(),
                    self.runtime.as_ref(),
                ) {
                    self.state.chat.editor.last_submission_error = Some(error.to_string());
                }
            }
            PendingChatAction::SubmitComposer => {
                if let Err(error) = chat_controller::submit_composer(
                    &mut self.state.chat,
                    self.client.as_deref(),
                    self.runtime.as_ref(),
                ) {
                    self.state.chat.editor.last_submission_error = Some(error.to_string());
                }
            }
            PendingChatAction::RetryLatestRequest => {
                let latest_request = self.client.as_ref().and_then(|client| {
                    let session_id = self.state.chat.shell.selected_session_id.as_deref()?;
                    let snapshot = client.store().snapshot();
                    snapshot
                        .requests_for_session(session_id)
                        .into_iter()
                        .last()
                        .cloned()
                });

                match chat_controller::retry_latest_request(
                    &mut self.state.chat,
                    self.client.as_deref(),
                    self.runtime.as_ref(),
                    latest_request.as_ref(),
                ) {
                    Ok(()) => {
                        self.state.chat.editor.last_action_message =
                            Some("Retried latest request.".to_string());
                    }
                    Err(error) => {
                        self.state.chat.editor.last_submission_error = Some(error.to_string());
                    }
                }
            }
        }
    }

    fn shutdown_client(&mut self) {
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

    fn restart_client(&mut self, reason: &str) -> bool {
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

    fn show_sidebar(&mut self, ui: &mut egui::Ui, store: Option<&ClientStore>) {
        Panel::left("activity_sidebar")
            .resizable(false)
            .exact_size(self.state.activity.sidebar_width())
            .show_inside(ui, |ui| {
                views::show_sidebar(
                    ui,
                    &mut self.state,
                    self.client.as_deref(),
                    store,
                    self.runtime.as_ref(),
                );
            });
    }

    fn show_rail(&mut self, ui: &mut egui::Ui, store: Option<&ClientStore>) {
        let Some(width) = self.state.activity.rail_width() else {
            return;
        };

        Panel::right("activity_rail")
            .resizable(false)
            .exact_size(width)
            .show_inside(ui, |ui| {
                views::show_rail(
                    ui,
                    &mut self.state,
                    self.client.as_deref(),
                    store,
                    self.log_store.as_ref(),
                    self.runtime.as_ref(),
                );
            });
    }

    fn show_status_bar(&self, ui: &mut egui::Ui) {
        let palette = theme::palette();
        let metrics = theme::metrics();

        Panel::bottom("status_bar")
            .resizable(false)
            .exact_size(metrics.status_bar_height)
            .show_inside(ui, |ui| {
                let rect = ui.max_rect();
                ui.painter().line_segment(
                    [
                        egui::pos2(rect.left(), rect.top()),
                        egui::pos2(rect.left() + rect.width() * 0.52, rect.top()),
                    ],
                    egui::Stroke::new(1.0, palette.accent_dim),
                );

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    ui.label(
                        RichText::new(format!(
                            "peered {}/{}",
                            self.state.status.peered_now, self.state.status.peered_target
                        ))
                        .monospace()
                        .size(10.5)
                        .color(palette.text_2),
                    );
                    ui.label(
                        RichText::new(format!(
                            "{} runtime: {}",
                            self.state.status.active_agent, self.state.status.runtime_state
                        ))
                        .monospace()
                        .size(10.5)
                        .color(palette.text_0),
                    );
                    ui.label(
                        RichText::new(format!("gossip lag {}ms", self.state.status.gossip_lag_ms))
                            .monospace()
                            .size(10.5)
                            .color(palette.text_2),
                    );
                    ui.label(
                        RichText::new(format!(
                            "replication: {}",
                            self.state.status.replication_state
                        ))
                        .monospace()
                        .size(10.5)
                        .color(palette.text_2),
                    );
                    ui.label(
                        RichText::new(format!("errors {}", self.state.status.error_count))
                            .monospace()
                            .size(10.5)
                            .color(if self.state.status.error_count == 0 {
                                palette.text_2
                            } else {
                                palette.warning
                            }),
                    );
                    ui.label(
                        RichText::new(format!("p2p {}", self.state.status.p2p_state))
                            .monospace()
                            .size(10.5)
                            .color(if self.state.status.p2p_warning {
                                palette.warning
                            } else {
                                palette.text_2
                            }),
                    );
                    ui.label(
                        RichText::new(format!("frm:{:04}", self.state.status.frame_counter))
                            .monospace()
                            .size(10.5)
                            .color(palette.text_3),
                    );
                    ui.label(
                        RichText::new(self.state.status.did_short.clone())
                            .monospace()
                            .size(10.5)
                            .color(palette.text_2),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(self.state.status.build_label.clone())
                                .monospace()
                                .size(10.5)
                                .color(palette.text_3),
                        );
                    });
                });
            });
    }

    fn show_main(&mut self, ui: &mut egui::Ui, store: Option<&ClientStore>) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            if !self.bootstrap_errors.is_empty() {
                self.show_bootstrap_banner(ui);
                ui.add_space(10.0);
            }
            if let Some(error) = self
                .client
                .as_ref()
                .and_then(|client| client.last_mutation_error())
            {
                self.show_mutation_banner(ui, &error);
                ui.add_space(10.0);
            }
            views::show_main(
                ui,
                &mut self.state,
                self.client.as_deref(),
                store,
                self.log_store.as_ref(),
                self.runtime.as_ref(),
                &mut self.markdown_cache,
            );
        });
    }

    fn show_bootstrap_banner(&self, ui: &mut egui::Ui) {
        let palette = theme::palette();

        ui.group(|ui| {
            ui.label(
                RichText::new("BOOTSTRAP")
                    .family(theme::stencil_family())
                    .size(13.0)
                    .color(palette.warning)
                    .strong(),
            );
            ui.add_space(6.0);
            for error in &self.bootstrap_errors {
                ui.label(
                    RichText::new(error)
                        .monospace()
                        .size(11.0)
                        .color(palette.text_1),
                );
            }
            if self.client.is_none() {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "The shell is still usable, but client-core startup needs to succeed before replication and submissions can be wired in.",
                    )
                    .size(12.5)
                    .color(palette.text_2),
                );
            }
        });
    }

    fn show_mutation_banner(&self, ui: &mut egui::Ui, error: &str) {
        let palette = theme::palette();

        ui.group(|ui| {
            ui.label(
                RichText::new("MUTATION")
                    .family(theme::stencil_family())
                    .size(13.0)
                    .color(palette.warning)
                    .strong(),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new(error)
                    .monospace()
                    .size(11.0)
                    .color(palette.text_1),
            );
        });
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
                    } else if should_auto_restart_p2p(
                        previous_health.as_ref(),
                        &health,
                        self.last_auto_p2p_restart_at,
                        Instant::now(),
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

fn apply_bootstrap_state(state: &mut ShellState, client: &ClientCore) {
    let error_count = live_error_count(client);

    apply_client_transport_state(state, client);
    apply_p2p_health_state(state, &client.p2p_health());
    state.status.active_agent = "desktop client".to_string();
    state.status.runtime_state = if !client.bootstrap_errors().is_empty() {
        "client core degraded".to_string()
    } else if client.peer_issue_count() > 0 {
        "peer repair active".to_string()
    } else {
        "client core online".to_string()
    };
    state.status.replication_state = "subscriptions armed".to_string();
    state.status.error_count = error_count;
}

fn apply_bootstrap_failure_state(state: &mut ShellState) {
    state.identity.did_short = "identity unavailable".to_string();
    state.status.peered_now = 0;
    state.status.peered_target = 0;
    state.status.p2p_state = "offline".to_string();
    state.status.p2p_warning = true;
    state.status.active_agent = "desktop client".to_string();
    state.status.runtime_state = "bootstrap failed".to_string();
    state.status.replication_state = "offline".to_string();
    state.status.error_count = 1;
    state.status.build_label = "bootstrap".to_string();
}

fn apply_snapshot_state(state: &mut ShellState, store: &ClientStore) {
    state.status.runtime_state = format!(
        "{} agents / {} conversations",
        store.agent_principals.len(),
        store.conversations.len()
    );
    state.status.replication_state = if store.requests.is_empty() {
        "subscriptions armed".to_string()
    } else {
        format!("{} requests observed", store.requests.len())
    };
}

fn apply_client_transport_state(state: &mut ShellState, client: &ClientCore) {
    state.identity.did_short = client.principal().short_did();
    state.status.peered_now = client.dialed_peer_count();
    state.status.peered_target = client.configured_peer_count();
    state.status.did_short = client.principal().short_did();
    state.status.build_label = format!("peer:{}", abbreviate_id(client.local_peer_id()));
}

fn apply_p2p_health_state(state: &mut ShellState, health: &P2PHealth) {
    state.status.p2p_state = if health.status == P2PHealthStatus::Healthy {
        format!(
            "{} · {} peers / {} reps",
            health.status.label(),
            health.connected_peer_count,
            health.replicator_count
        )
    } else {
        format!(
            "{} · {} fails",
            health.status.label(),
            health.consecutive_failures
        )
    };
    state.status.p2p_warning = health.status != P2PHealthStatus::Healthy;
}

fn sync_shell_state_from_client(state: &mut ShellState, client: &ClientCore) {
    let snapshot = client.store().snapshot();
    let store = snapshot.as_ref();
    apply_snapshot_state(state, store);

    chat_controller::sync_from_snapshot(&mut state.chat, store, true);
    apply_first_launch_focus(state, client, store);
    views::prepare_state(state, Some(client), Some(store));
}

fn live_error_count(client: &ClientCore) -> usize {
    client.bootstrap_errors().len()
        + client.peer_issue_count()
        + usize::from(client.p2p_health().status != P2PHealthStatus::Healthy)
}

fn should_auto_restart_p2p(
    previous: Option<&P2PHealth>,
    next: &P2PHealth,
    last_attempt: Option<Instant>,
    now: Instant,
) -> bool {
    if next.status != P2PHealthStatus::Wedged {
        return false;
    }

    if last_attempt.is_some_and(|attempted_at| {
        now.saturating_duration_since(attempted_at) < P2P_AUTO_RESTART_COOLDOWN
    }) {
        return false;
    }

    match previous {
        Some(previous) => {
            previous.status != P2PHealthStatus::Wedged
                || previous.consecutive_failures != next.consecutive_failures
                || previous.last_error != next.last_error
        }
        None => true,
    }
}

fn apply_first_launch_focus(state: &mut ShellState, client: &ClientCore, store: &ClientStore) {
    if !state.onboarding.first_launch_redirect_done
        && state.activity == Activity::Chat
        && should_focus_first_launch(client, store)
    {
        state.activity = Activity::Peers;
        state.peers.show_add_form = true;
        state.onboarding.first_launch_redirect_done = true;
    }
}

fn should_focus_first_launch(client: &ClientCore, store: &ClientStore) -> bool {
    client.configured_peer_count() == 0
        && store.agent_principals.is_empty()
        && store.conversations.is_empty()
        && store.requests.is_empty()
        && store.responses.is_empty()
}

fn abbreviate_id(value: &str) -> String {
    if value.len() <= 12 {
        return value.to_string();
    }

    format!("{}..{}", &value[..8], &value[value.len() - 2..])
}

#[cfg(test)]
mod tests;
