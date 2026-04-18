use eframe::egui::{self, Panel, RichText};

use crate::client::ClientStore;
use crate::theme;
use crate::views;

use super::DesktopApp;

impl DesktopApp {
    pub(super) fn show_sidebar(&mut self, ui: &mut egui::Ui, store: Option<&ClientStore>) {
        let width = responsive_sidebar_width(self.state.activity, ui.max_rect().width());
        Panel::left("activity_sidebar")
            .resizable(false)
            .exact_size(width)
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
        let Some(base_width) = self
            .should_show_rail()
            .then(|| self.state.activity.rail_width())
            .flatten()
        else {
            return;
        };
        let width = responsive_rail_width(self.state.activity, ui.max_rect().width(), base_width);
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

    fn should_show_rail(&self) -> bool {
        match self.state.activity {
            crate::state::Activity::Chat => false,
            crate::state::Activity::Manage => {
                use crate::state::ManageSection;

                matches!(
                    self.state.manage.selected_section,
                    ManageSection::Runtime
                        | ManageSection::RequestTimeline
                        | ManageSection::RecentFailures
                ) || self.state.manage.selected_entity_id.is_some()
                    || self.state.manage.draft.is_some()
            }
        }
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
                            "deployments {}/{}",
                            self.state.status.peered_now, self.state.status.peered_target
                        ))
                        .monospace()
                        .size(10.5)
                        .color(palette.text_2),
                    );
                    ui.label(
                        RichText::new(format!(
                            "{} / {}",
                            self.state.status.active_agent, self.state.status.runtime_state
                        ))
                        .monospace()
                        .size(10.5)
                        .color(palette.text_0),
                    );
                    ui.label(
                        RichText::new(format!(
                            "replication {}",
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

fn responsive_sidebar_width(activity: crate::state::Activity, total_width: f32) -> f32 {
    let desired = match activity {
        crate::state::Activity::Chat => total_width * 0.22,
        crate::state::Activity::Manage => total_width * 0.20,
    };

    match activity {
        crate::state::Activity::Chat => desired.clamp(272.0, 340.0),
        crate::state::Activity::Manage => desired.clamp(252.0, 320.0),
    }
}

fn responsive_rail_width(
    activity: crate::state::Activity,
    total_width: f32,
    base_width: f32,
) -> f32 {
    let desired = match activity {
        crate::state::Activity::Manage => total_width * 0.27,
        crate::state::Activity::Chat => base_width,
    };

    match activity {
        crate::state::Activity::Manage => desired.clamp(320.0, 440.0),
        crate::state::Activity::Chat => base_width,
    }
}
