use eframe::egui::{self, Ui};

use crate::audit;
use crate::state::{LogsFilter, ShellState};
use crate::telemetry::{DesktopLogEntry, DesktopLogSnapshot};
use crate::theme;

pub(super) fn render_filter_chip(ui: &mut Ui, state: &mut ShellState, filter: LogsFilter) {
    let palette = theme::palette();
    let selected = state.logs.filter == filter;
    let response = ui.selectable_label(selected, filter.label());
    audit::record(ui, audit::targets::logs_filter(filter), &response);

    if response.clicked() {
        state.logs.filter = filter;
    }

    if selected {
        let rect = response.rect.expand2(egui::vec2(1.0, 1.0));
        ui.painter().rect_stroke(
            rect,
            4.0,
            egui::Stroke::new(1.0, palette.accent),
            egui::StrokeKind::Outside,
        );
    }
}

pub(super) fn filtered_entries(
    snapshot: &DesktopLogSnapshot,
    filter: LogsFilter,
) -> Vec<DesktopLogEntry> {
    snapshot
        .entries
        .iter()
        .filter(|entry| matches_filter(entry, filter))
        .cloned()
        .collect()
}

pub(super) fn matches_filter(entry: &DesktopLogEntry, filter: LogsFilter) -> bool {
    match filter {
        LogsFilter::All => true,
        LogsFilter::Category(category) => entry.category == category,
    }
}
