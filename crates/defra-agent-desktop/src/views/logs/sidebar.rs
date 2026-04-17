use eframe::egui::{self, RichText, Ui};

use crate::state::{LogsFilter, ShellState};
use crate::theme;
use crate::views;

use super::filtering::render_filter_chip;

pub(super) fn show_sidebar(ui: &mut Ui, state: &mut ShellState) {
    let palette = theme::palette();

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            views::sidebar_heading(ui, "Log Controls", Some("local"));
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.vertical(|ui| {
                views::card(
                    ui,
                    "Capture",
                    "The desktop mirrors local tracing into a ring buffer. Use filters here to focus on replication, peering, turn execution, and warnings while you debug the shell.",
                );
                ui.add_space(10.0);
                views::section_kicker(ui, "FILTER");
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    render_filter_chip(ui, state, LogsFilter::All);
                    for category in crate::telemetry::DesktopLogCategory::ALL {
                        render_filter_chip(ui, state, LogsFilter::Category(category));
                    }
                });
                ui.add_space(10.0);
                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new("status")
                            .family(theme::stencil_family())
                            .size(13.0)
                            .color(palette.text_1)
                            .strong(),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!(
                            "current filter   {}",
                            state.logs.filter.label().to_ascii_lowercase()
                        ))
                        .monospace()
                        .size(10.5)
                        .color(palette.text_1),
                    );
                    ui.label(
                        RichText::new(format!(
                            "replication      {}",
                            state.status.replication_state
                        ))
                        .monospace()
                        .size(10.5)
                        .color(palette.text_1),
                    );
                    ui.label(
                        RichText::new(format!("errors           {}", state.status.error_count))
                            .monospace()
                            .size(10.5)
                            .color(if state.status.error_count == 0 {
                                palette.text_2
                            } else {
                                palette.warning
                            }),
                    );
                });
            });
        });
    });
}
