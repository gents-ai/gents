use chrono::{Local, Utc};
use eframe::egui::{self, RichText, Ui};
use tracing::Level;

use crate::telemetry::{DesktopLogCategory, DesktopLogEntry, DesktopLogField};
use crate::theme;

pub(super) fn render_entry(ui: &mut Ui, entry: &DesktopLogEntry) {
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

pub(super) fn render_fields(fields: &[DesktopLogField]) -> String {
    fields
        .iter()
        .map(|field| format!("{}={}", field.name, field.value))
        .collect::<Vec<_>>()
        .join("  ")
}

pub(super) fn format_timestamp(timestamp: chrono::DateTime<Utc>) -> String {
    timestamp
        .with_timezone(&Local)
        .format("%H:%M:%S%.3f")
        .to_string()
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
