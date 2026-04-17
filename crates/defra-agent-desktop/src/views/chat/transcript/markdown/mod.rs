mod parser;
mod table;

#[cfg(test)]
mod tests;

use eframe::egui::{self, Ui};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

use self::parser::{segment_markdown, MarkdownSegment};

const MARKDOWN_THEME_LIGHT: &str = "base16-ocean.light";
const MARKDOWN_THEME_DARK: &str = "base16-ocean.dark";

pub fn markdown_theme_names() -> (&'static str, &'static str) {
    (MARKDOWN_THEME_LIGHT, MARKDOWN_THEME_DARK)
}

pub(super) fn render_markdown(
    ui: &mut Ui,
    markdown_cache: &mut CommonMarkCache,
    id_salt: impl std::hash::Hash,
    text: &str,
) {
    ui.push_id(id_salt, |ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
        for (index, segment) in segment_markdown(text).into_iter().enumerate() {
            match segment {
                MarkdownSegment::Prose(body) => {
                    // Each prose segment gets its own id scope so egui_commonmark's
                    // internal per-ui state (code block collapsibles, etc.) does
                    // not collide with other prose segments in the same message.
                    ui.push_id(("prose_segment", index), |ui| {
                        CommonMarkViewer::new()
                            .syntax_theme_light(MARKDOWN_THEME_LIGHT)
                            .syntax_theme_dark(MARKDOWN_THEME_DARK)
                            .show(ui, markdown_cache, &body);
                    });
                }
                MarkdownSegment::Table(table) => {
                    ui.push_id(("table_segment", index), |ui| {
                        table::render_table(ui, index, &table);
                        ui.add_space(4.0);
                    });
                }
            }
        }
    });
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub(super) struct InlineStyle {
    pub(super) code: bool,
    pub(super) strong: bool,
    pub(super) emphasis: bool,
    pub(super) link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CellRun {
    pub(super) text: String,
    pub(super) style: InlineStyle,
}

pub(super) type Cell = Vec<CellRun>;

pub(super) struct ParsedTable {
    pub(super) headers: Vec<Cell>,
    pub(super) rows: Vec<Vec<Cell>>,
}

impl ParsedTable {
    pub(super) fn num_cols(&self) -> usize {
        self.headers
            .len()
            .max(self.rows.iter().map(|row| row.len()).max().unwrap_or(0))
    }
}
