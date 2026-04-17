use eframe::egui::{self, Ui};
use egui::text::{LayoutJob, TextFormat};
use egui::{FontFamily, FontId};

use crate::theme;

use super::{Cell, InlineStyle, ParsedTable};

pub(super) fn render_table(ui: &mut Ui, index: usize, table: &ParsedTable) {
    let num_cols = table.num_cols();
    if num_cols == 0 {
        return;
    }
    let palette = theme::palette();
    let body_font = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Body)
        .cloned()
        .unwrap_or_else(|| FontId::proportional(13.0));
    let mono_font = FontId::new(body_font.size, FontFamily::Monospace);
    let available_width = ui.available_width();
    let col_width = (available_width / num_cols as f32).max(80.0);

    let format_for = |style: &InlineStyle, base_color: egui::Color32| -> TextFormat {
        let font_id = if style.code {
            mono_font.clone()
        } else {
            FontId::new(body_font.size, body_font.family.clone())
        };
        let color = if style.link.is_some() {
            palette.accent
        } else if style.strong {
            palette.text_0
        } else {
            base_color
        };
        let background = if style.code {
            palette.background_2
        } else {
            egui::Color32::TRANSPARENT
        };
        let underline = if style.link.is_some() {
            egui::Stroke::new(1.0, color)
        } else {
            egui::Stroke::NONE
        };
        TextFormat {
            font_id,
            color,
            background,
            italics: style.emphasis,
            underline,
            ..Default::default()
        }
    };

    egui::Grid::new(("markdown_table", index))
        .num_columns(num_cols)
        .min_col_width(col_width)
        .max_col_width(col_width)
        .striped(true)
        .show(ui, |ui| {
            for idx in 0..num_cols {
                let empty: Cell = Vec::new();
                let cell = table.headers.get(idx).unwrap_or(&empty);
                ui.label(cell_to_layout_job(cell, palette.text_0, &format_for));
            }
            ui.end_row();
            for row in &table.rows {
                for idx in 0..num_cols {
                    let empty: Cell = Vec::new();
                    let cell = row.get(idx).unwrap_or(&empty);
                    ui.label(cell_to_layout_job(cell, palette.text_1, &format_for));
                }
                ui.end_row();
            }
        });
}

fn cell_to_layout_job(
    cell: &Cell,
    base_color: egui::Color32,
    format_for: &dyn Fn(&InlineStyle, egui::Color32) -> TextFormat,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    for run in cell {
        let format = format_for(&run.style, base_color);
        job.append(&run.text, 0.0, format);
    }
    job
}
