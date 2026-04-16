use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{self, Context, Panel, RichText};
use egui_commonmark::CommonMarkCache;
use tokio::runtime::Runtime;
use tokio::sync::watch;

use crate::chat::controller as chat_controller;
use crate::client::{ClientCore, ClientStore};
use crate::state::{Activity, ShellState};
use crate::telemetry::{global_log_store, DesktopLogStore};
use crate::theme;
use crate::views;

pub struct DesktopApp {
    state: ShellState,
    client: Option<Arc<ClientCore>>,
    store_updates: Option<watch::Receiver<u64>>,
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
        let mut state = ShellState::default();
        let store_updates = client.as_ref().map(|client| {
            apply_bootstrap_state(&mut state, client.as_ref());
            apply_snapshot_state(&mut state, client.store().snapshot().as_ref());
            let snapshot = client.store().snapshot();
            let peer_statuses = client.peer_statuses();
            chat_controller::sync_from_snapshot(
                &mut state.chat,
                snapshot.as_ref(),
                &peer_statuses,
                true,
            );
            client.store_updates()
        });

        if client.is_none() {
            apply_bootstrap_failure_state(&mut state);
        }

        Self {
            state,
            client,
            store_updates,
            bootstrap_errors,
            log_store,
            markdown_cache: CommonMarkCache::default(),
            runtime,
        }
    }

    fn block_on_runtime<T>(&self, future: impl Future<Output = T>) -> T {
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| self.runtime.block_on(future))
        } else {
            self.runtime.block_on(future)
        }
    }

    fn shutdown_client(&mut self) {
        let Some(client) = self.client.take() else {
            self.store_updates = None;
            return;
        };

        self.store_updates = None;
        if let Err(error) = self.block_on_runtime(client.shutdown()) {
            tracing::error!(error = %error, "failed to shut down desktop client");
            self.bootstrap_errors
                .push(format!("desktop shutdown failed: {error}"));
        }
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

        if let (Some(client), Some(store_updates)) = (&self.client, &mut self.store_updates) {
            apply_client_transport_state(&mut self.state, client);
            let snapshot = client.store().snapshot();
            let peer_statuses = client.peer_statuses();
            chat_controller::sync_from_snapshot(
                &mut self.state.chat,
                snapshot.as_ref(),
                &peer_statuses,
                true,
            );
            if store_updates.has_changed().unwrap_or(false) {
                let _ = store_updates.borrow_and_update();
                let snapshot = client.store().snapshot();
                apply_snapshot_state(&mut self.state, snapshot.as_ref());
                let peer_statuses = client.peer_statuses();
                chat_controller::sync_from_snapshot(
                    &mut self.state.chat,
                    snapshot.as_ref(),
                    &peer_statuses,
                    true,
                );
                ctx.request_repaint();
            }

            self.state.status.error_count = live_error_count(client)
                + self.bootstrap_errors.len()
                + usize::from(client.last_mutation_error().is_some());
        }

        ctx.request_repaint_after(Duration::from_millis(33));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let store = self.client.as_ref().map(|client| client.store().snapshot());
        let store_ref = store.as_deref();

        if let (Some(client), Some(store_ref)) = (self.client.as_deref(), store_ref) {
            apply_first_launch_focus(&mut self.state, client, store_ref);
        }
        views::prepare_state(&mut self.state, self.client.as_deref(), store_ref);
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

fn live_error_count(client: &ClientCore) -> usize {
    client.bootstrap_errors().len() + client.peer_issue_count()
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
