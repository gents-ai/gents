use chrono::{Local, Utc};
use eframe::egui::{self, RichText, Ui};
use tracing::Level;

use crate::audit;
use crate::client::{ClientCore, ClientStore};
use crate::state::{LogsFilter, ShellState};
use crate::telemetry::{
    DesktopLogCategory, DesktopLogEntry, DesktopLogField, DesktopLogSnapshot, DesktopLogStore,
};
use crate::theme;
use crate::views;

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
            for category in DesktopLogCategory::ALL {
                render_filter_chip(ui, state, LogsFilter::Category(category));
            }
        });
        ui.add_space(10.0);
        ui.label(
            RichText::new(format!(
                "{} events buffered · {} total captured · newest first",
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
                "The log capture layer is live, but nothing in the current filter has been observed yet.",
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
                    for category in DesktopLogCategory::ALL {
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

pub fn show_rail(
    ui: &mut Ui,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    log_store: &DesktopLogStore,
) {
    let palette = theme::palette();
    let snapshot = log_store.snapshot();
    let latest_warning = snapshot
        .entries
        .iter()
        .find(|entry| matches!(entry.level, Level::WARN | Level::ERROR))
        .cloned();
    let connected_peers = client
        .map(ClientCore::dialed_peer_count)
        .unwrap_or_default();
    let configured_peers = client
        .map(ClientCore::configured_peer_count)
        .unwrap_or_default();

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(14.0);
        views::sidebar_heading(ui, "Diagnostics", Some("live only"));
        ui.add_space(10.0);
        views::card(
            ui,
            "Capture",
            "Tracing is mirrored into a local ring buffer for the Logs activity. History is memory-only in MVP and resets on restart.",
        );
        ui.add_space(10.0);
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("rolling metrics")
                    .family(theme::stencil_family())
                    .size(13.0)
                    .color(palette.text_1)
                    .strong(),
            );
            ui.add_space(6.0);
            for row in [
                format!(
                    "approx store        {} / {} rows",
                    store
                        .map(|store| format_bytes(store.approx_serialized_bytes()))
                        .unwrap_or_else(|| "offline".to_string()),
                    store.map(ClientStore::row_count).unwrap_or_default()
                ),
                "replication lag     n/a (not instrumented yet)".to_string(),
                format!("peers               {connected_peers}/{configured_peers} connected"),
                format!("events              {:.1}/s", snapshot.events_per_second),
                format!(
                    "buffer              {}/{} live ({} dropped)",
                    snapshot.entries.len(),
                    snapshot.capacity,
                    snapshot.dropped_events
                ),
            ] {
                ui.label(
                    RichText::new(row)
                        .monospace()
                        .size(11.0)
                        .color(palette.text_1),
                );
            }
        });

        if let Some(entry) = latest_warning {
            ui.add_space(10.0);
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new("latest warning")
                        .family(theme::stencil_family())
                        .size(13.0)
                        .color(palette.warning)
                        .strong(),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "{} · {}",
                        format_timestamp(entry.timestamp),
                        entry.target
                    ))
                    .monospace()
                    .size(10.5)
                    .color(palette.text_2),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(entry.message)
                        .size(12.5)
                        .color(palette.text_1)
                        .line_height(Some(17.0)),
                );
                if !entry.fields.is_empty() {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(render_fields(&entry.fields))
                            .monospace()
                            .size(10.5)
                            .color(palette.text_2),
                    );
                }
            });
        }

    });
}

fn render_filter_chip(ui: &mut Ui, state: &mut ShellState, filter: LogsFilter) {
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

fn render_entry(ui: &mut Ui, entry: &DesktopLogEntry) {
    let palette = theme::palette();
    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format_timestamp(entry.timestamp))
                    .monospace()
                    .size(10.5)
                    .color(palette.text_2),
            );
            ui.label(
                RichText::new(entry.category.label().to_ascii_lowercase())
                    .monospace()
                    .size(10.5)
                    .color(category_color(entry.category)),
            );
            ui.label(
                RichText::new(entry.level.as_str())
                    .monospace()
                    .size(10.5)
                    .color(level_color(entry.level)),
            );
            ui.label(
                RichText::new(entry.target.as_str())
                    .monospace()
                    .size(10.5)
                    .color(palette.text_3),
            );
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new(entry.message.as_str())
                .size(13.0)
                .color(palette.text_0)
                .line_height(Some(18.0)),
        );
        if !entry.fields.is_empty() {
            ui.add_space(6.0);
            ui.label(
                RichText::new(render_fields(&entry.fields))
                    .monospace()
                    .size(10.5)
                    .color(palette.text_2),
            );
        }
    });
}

fn filtered_entries(snapshot: &DesktopLogSnapshot, filter: LogsFilter) -> Vec<DesktopLogEntry> {
    snapshot
        .entries
        .iter()
        .filter(|entry| matches_filter(entry, filter))
        .cloned()
        .collect()
}

fn matches_filter(entry: &DesktopLogEntry, filter: LogsFilter) -> bool {
    match filter {
        LogsFilter::All => true,
        LogsFilter::Category(category) => entry.category == category,
    }
}

fn category_color(category: DesktopLogCategory) -> egui::Color32 {
    let palette = theme::palette();
    match category {
        DesktopLogCategory::Replication => palette.info,
        DesktopLogCategory::Peering => palette.accent,
        DesktopLogCategory::Turns => palette.text_0,
        DesktopLogCategory::Writes => palette.text_1,
        DesktopLogCategory::Warnings => palette.warning,
    }
}

fn level_color(level: Level) -> egui::Color32 {
    let palette = theme::palette();
    match level {
        Level::ERROR => palette.danger,
        Level::WARN => palette.warning,
        Level::INFO => palette.text_1,
        Level::DEBUG => palette.info,
        Level::TRACE => palette.text_3,
    }
}

fn render_fields(fields: &[DesktopLogField]) -> String {
    fields
        .iter()
        .map(|field| format!("{}={}", field.name, field.value))
        .collect::<Vec<_>>()
        .join("  ")
}

fn format_timestamp(timestamp: chrono::DateTime<Utc>) -> String {
    timestamp
        .with_timezone(&Local)
        .format("%H:%M:%S%.3f")
        .to_string()
}

fn format_bytes(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;

    let bytes = bytes as f64;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes / KIB)
    } else {
        format!("{bytes:.0} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::telemetry::DesktopLogEntry;

    #[test]
    fn filter_matches_selected_category() {
        let entry = DesktopLogEntry {
            id: 1,
            timestamp: Utc::now(),
            level: Level::INFO,
            target: "desktop::observe".to_string(),
            category: DesktopLogCategory::Replication,
            message: "snapshot refreshed".to_string(),
            fields: Vec::new(),
        };

        assert!(matches_filter(&entry, LogsFilter::All));
        assert!(matches_filter(
            &entry,
            LogsFilter::Category(DesktopLogCategory::Replication)
        ));
        assert!(!matches_filter(
            &entry,
            LogsFilter::Category(DesktopLogCategory::Warnings)
        ));
    }

    #[test]
    fn format_bytes_uses_human_units() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KiB");
    }
}
