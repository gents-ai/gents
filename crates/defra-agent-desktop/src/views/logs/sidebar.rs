use eframe::egui::{self, RichText, Ui};

use crate::state::ShellState;
use crate::views;

pub(super) fn show_sidebar(ui: &mut Ui, state: &mut ShellState) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            views::sidebar_heading(ui, "Capture", Some("local"));
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.vertical(|ui| {
                views::card(
                    ui,
                    "Local Trace Buffer",
                    "Logs are buffered in memory for this session. Use the main-panel filters to focus on replication, peering, turns, writes, or warnings.",
                );
                ui.add_space(10.0);
                ui.label(
                    RichText::new(format!(
                        "current filter   {}",
                        state.logs.filter.label().to_ascii_lowercase()
                    ))
                    .monospace()
                    .size(10.5)
                    .color(crate::theme::palette().text_2),
                );
            });
        });
    });
}
