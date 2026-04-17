use eframe::egui::{self, Panel, RichText};

use crate::client::ClientStore;
use crate::theme;
use crate::views;

use super::DesktopApp;

impl DesktopApp {
    pub(super) fn show_sidebar(&mut self, ui: &mut egui::Ui, store: Option<&ClientStore>) {
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

    pub(super) fn show_rail(&mut self, ui: &mut egui::Ui, store: Option<&ClientStore>) {
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

    pub(super) fn show_status_bar(&self, ui: &mut egui::Ui) {
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

    pub(super) fn show_main(&mut self, ui: &mut egui::Ui, store: Option<&ClientStore>) {
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
