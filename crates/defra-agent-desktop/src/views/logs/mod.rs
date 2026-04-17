mod entry;
mod filtering;
mod rail;
mod sidebar;

#[cfg(test)]
mod tests;

use eframe::egui::{self, RichText, Ui};

use crate::state::{LogsFilter, ShellState};
use crate::telemetry::DesktopLogStore;
use crate::theme;
use crate::views;

use self::entry::render_entry;
use self::filtering::{filtered_entries, render_filter_chip};

pub fn show_main(ui: &mut Ui, state: &mut ShellState, log_store: &DesktopLogStore) {
    let palette = theme::palette();
    let snapshot = log_store.snapshot();
    let entries = filtered_entries(&snapshot, state.logs.filter);
    let filter_badge = format!("filter: {}", state.logs.filter.label().to_ascii_lowercase());

    ui.vertical(|ui| {
        views::toolbar(ui, "Live Logs", "stream / local node", &filter_badge);
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            render_filter_chip(ui, state, LogsFilter::All);
            for category in crate::telemetry::DesktopLogCategory::ALL {
                render_filter_chip(ui, state, LogsFilter::Category(category));
            }
        });
        ui.add_space(10.0);
        ui.label(
            RichText::new(format!(
                "{} shown · {} captured",
                snapshot.entries.len(),
                snapshot.total_events
            ))
            .monospace()
            .size(11.0)
            .color(palette.text_2),
        );
        ui.add_space(10.0);

        if entries.is_empty() {
            views::card(
                ui,
                "No Matching Events",
                "Nothing in the current filter has been observed yet.",
            );
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for entry in entries {
                    render_entry(ui, &entry);
                    ui.add_space(6.0);
                }
            });
    });
}

pub fn show_sidebar(ui: &mut Ui, state: &mut ShellState) {
    sidebar::show_sidebar(ui, state);
}

pub fn show_rail(
    ui: &mut Ui,
    client: Option<&crate::client::ClientCore>,
    store: Option<&crate::client::ClientStore>,
    log_store: &DesktopLogStore,
) {
    rail::show_rail(ui, client, store, log_store);
}
