use eframe::egui::{self, Ui};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

use crate::theme;

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
                        render_table(ui, index, &table);
                        ui.add_space(4.0);
                    });
                }
            }
        }
    });
}

enum MarkdownSegment {
    Prose(String),
    Table(ParsedTable),
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
struct InlineStyle {
    code: bool,
    strong: bool,
    emphasis: bool,
    link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CellRun {
    text: String,
    style: InlineStyle,
}

type Cell = Vec<CellRun>;

struct ParsedTable {
    headers: Vec<Cell>,
    rows: Vec<Vec<Cell>>,
}

impl ParsedTable {
    fn num_cols(&self) -> usize {
        self.headers
            .len()
            .max(self.rows.iter().map(|row| row.len()).max().unwrap_or(0))
    }
}

fn segment_markdown(text: &str) -> Vec<MarkdownSegment> {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let parser = Parser::new_ext(text, options).into_offset_iter();

    let mut segments: Vec<MarkdownSegment> = Vec::new();
    let mut cursor = 0usize;
    let mut depth: u32 = 0;
    let mut table_start: Option<usize> = None;
    let mut in_table = false;
    let mut current_table: Option<ParsedTable> = None;
    let mut current_row: Vec<Cell> = Vec::new();
    let mut current_cell: Cell = Vec::new();
    let mut current_style: InlineStyle = InlineStyle::default();
    let mut strong_depth: u32 = 0;
    let mut emphasis_depth: u32 = 0;
    let mut in_head = false;

    let flush_prose = |segments: &mut Vec<MarkdownSegment>, slice: &str| {
        if slice.trim().is_empty() {
            return;
        }
        if let Some(MarkdownSegment::Prose(existing)) = segments.last_mut() {
            existing.push_str(slice);
        } else {
            segments.push(MarkdownSegment::Prose(slice.to_string()));
        }
    };

    for (event, range) in parser {
        match event {
            Event::Start(Tag::Table(_)) => {
                if depth == 0 {
                    if range.start > cursor {
                        flush_prose(&mut segments, &text[cursor..range.start]);
                    }
                    table_start = Some(range.start);
                    in_table = true;
                    current_table = Some(ParsedTable {
                        headers: Vec::new(),
                        rows: Vec::new(),
                    });
                }
                depth += 1;
            }
            Event::Start(Tag::TableHead) => {
                in_head = true;
                depth += 1;
            }
            Event::Start(Tag::TableRow) => {
                current_row.clear();
                depth += 1;
            }
            Event::Start(Tag::TableCell) => {
                current_cell.clear();
                current_style = InlineStyle::default();
                strong_depth = 0;
                emphasis_depth = 0;
                depth += 1;
            }
            Event::Start(Tag::Strong) => {
                if in_table {
                    strong_depth += 1;
                    current_style.strong = strong_depth > 0;
                }
                depth += 1;
            }
            Event::Start(Tag::Emphasis) => {
                if in_table {
                    emphasis_depth += 1;
                    current_style.emphasis = emphasis_depth > 0;
                }
                depth += 1;
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                if in_table && current_style.link.is_none() {
                    current_style.link = Some(dest_url.to_string());
                }
                depth += 1;
            }
            Event::Start(_) => {
                depth += 1;
            }
            Event::End(TagEnd::Table) => {
                depth = depth.saturating_sub(1);
                if depth == 0 && in_table {
                    if let Some(table) = current_table.take() {
                        segments.push(MarkdownSegment::Table(table));
                    }
                    cursor = range.end;
                    in_table = false;
                    table_start = None;
                }
            }
            Event::End(TagEnd::TableHead) => {
                depth = depth.saturating_sub(1);
                if in_table {
                    if let Some(table) = current_table.as_mut() {
                        table.headers = std::mem::take(&mut current_row);
                    }
                }
                in_head = false;
            }
            Event::End(TagEnd::TableRow) => {
                depth = depth.saturating_sub(1);
                if in_table && !in_head {
                    if let Some(table) = current_table.as_mut() {
                        table.rows.push(std::mem::take(&mut current_row));
                    }
                }
            }
            Event::End(TagEnd::TableCell) => {
                depth = depth.saturating_sub(1);
                if in_table {
                    let cell = std::mem::take(&mut current_cell);
                    let trimmed = trim_cell(cell);
                    current_row.push(trimmed);
                    current_style = InlineStyle::default();
                    strong_depth = 0;
                    emphasis_depth = 0;
                }
            }
            Event::End(TagEnd::Strong) => {
                depth = depth.saturating_sub(1);
                if in_table {
                    strong_depth = strong_depth.saturating_sub(1);
                    current_style.strong = strong_depth > 0;
                }
            }
            Event::End(TagEnd::Emphasis) => {
                depth = depth.saturating_sub(1);
                if in_table {
                    emphasis_depth = emphasis_depth.saturating_sub(1);
                    current_style.emphasis = emphasis_depth > 0;
                }
            }
            Event::End(TagEnd::Link) => {
                depth = depth.saturating_sub(1);
                if in_table {
                    current_style.link = None;
                }
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
            }
            Event::Text(chunk) if in_table => {
                let mut style = current_style.clone();
                style.code = false;
                push_cell_run(&mut current_cell, chunk.to_string(), style);
            }
            Event::Code(chunk) if in_table => {
                let mut style = current_style.clone();
                style.code = true;
                push_cell_run(&mut current_cell, chunk.to_string(), style);
            }
            _ => {}
        }
    }

    if cursor < text.len() {
        flush_prose(&mut segments, &text[cursor..]);
    }

    if segments.is_empty() && !text.is_empty() {
        segments.push(MarkdownSegment::Prose(text.to_string()));
    }

    let _ = table_start;
    segments
}

fn push_cell_run(cell: &mut Cell, chunk: String, style: InlineStyle) {
    if let Some(last) = cell.last_mut() {
        if last.style == style {
            last.text.push_str(&chunk);
            return;
        }
    }
    cell.push(CellRun { text: chunk, style });
}

fn trim_cell(cell: Cell) -> Cell {
    let mut runs: Vec<CellRun> = cell
        .into_iter()
        .filter(|run| !run.text.is_empty())
        .collect();
    if let Some(first) = runs.first_mut() {
        let trimmed = first.text.trim_start().to_string();
        first.text = trimmed;
    }
    if let Some(last) = runs.last_mut() {
        let trimmed = last.text.trim_end().to_string();
        last.text = trimmed;
    }
    runs.retain(|run| !run.text.is_empty());
    runs
}

fn render_table(ui: &mut Ui, index: usize, table: &ParsedTable) {
    use egui::text::{LayoutJob, TextFormat};
    use egui::{FontFamily, FontId};

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
}

#[cfg(test)]
mod table_parser_tests {
    use super::{segment_markdown, CellRun, MarkdownSegment};

    #[test]
    fn pure_prose_returns_single_prose_segment() {
        let segments = segment_markdown("hello `world` and **bold**");
        assert_eq!(segments.len(), 1);
        assert!(matches!(segments[0], MarkdownSegment::Prose(_)));
    }

    #[test]
    fn prose_with_six_fenced_code_blocks_renders_as_single_prose() {
        let body = r#"
## Repo Overview

**defra-agent** is a Rust agent runtime.

```rust
fn main() {}
```

```toml
name = "x"
```

```bash
echo hi
```

```json
{"k":1}
```

```lean
theorem t : True := trivial
```

```txt
plain
```

tail prose here.
"#;
        let segments = segment_markdown(body);
        assert_eq!(
            segments.len(),
            1,
            "no tables, so the whole body should be one prose segment"
        );
        assert!(matches!(segments[0], MarkdownSegment::Prose(_)));
        let MarkdownSegment::Prose(ref prose) = segments[0] else {
            unreachable!()
        };
        assert!(prose.contains("tail prose here"));
    }

    #[test]
    fn keyfiles_response_segments_keep_prose_after_table() {
        let text = include_str!("../fixtures/keyfiles_response.md");
        let segments = segment_markdown(text);
        let kinds: Vec<&str> = segments
            .iter()
            .map(|segment| match segment {
                MarkdownSegment::Prose(_) => "prose",
                MarkdownSegment::Table(_) => "table",
            })
            .collect();
        assert!(
            kinds.iter().any(|k| *k == "table"),
            "expected at least one table segment, got {:?}",
            kinds
        );
        assert!(
            kinds.last().map(|k| *k == "prose").unwrap_or(false),
            "expected trailing prose segment after the table, got {:?}",
            kinds
        );
        let MarkdownSegment::Prose(tail) = segments.last().unwrap() else {
            panic!("last segment must be prose");
        };
        assert!(
            tail.contains("Important Constraints"),
            "trailing prose should contain the post-table section; got:\n{}",
            tail
        );
    }

    #[test]
    fn pipe_table_is_parsed_into_headers_and_rows() {
        let text = "intro\n\n| Crate | Purpose |\n| --- | --- |\n| `defra-agent` | Runtime library |\n| `defra-agent-cli` | Compiled CLI (`defra-agent`) |\n\nafter";
        let segments = segment_markdown(text);
        let kinds: Vec<&str> = segments
            .iter()
            .map(|segment| match segment {
                MarkdownSegment::Prose(_) => "prose",
                MarkdownSegment::Table(_) => "table",
            })
            .collect();
        assert_eq!(kinds, ["prose", "table", "prose"]);

        let MarkdownSegment::Table(table) = &segments[1] else {
            panic!("middle segment must be a table");
        };
        let flatten = |cell: &[CellRun]| -> String {
            cell.iter().map(|run| run.text.as_str()).collect::<String>()
        };
        assert_eq!(flatten(&table.headers[0]), "Crate");
        assert_eq!(flatten(&table.headers[1]), "Purpose");
        assert_eq!(table.rows.len(), 2);
        assert_eq!(flatten(&table.rows[0][0]), "defra-agent");
        assert_eq!(flatten(&table.rows[0][1]), "Runtime library");
        assert_eq!(flatten(&table.rows[1][0]), "defra-agent-cli");
        assert_eq!(flatten(&table.rows[1][1]), "Compiled CLI (defra-agent)");
        assert!(
            table.rows[0][0][0].style.code,
            "first cell's inline code run must be flagged as code"
        );
    }

    #[test]
    fn table_cells_preserve_strong_emphasis_and_link_styling() {
        let text = concat!(
            "| Case | Cell |\n",
            "| --- | --- |\n",
            "| bold | **load-bearing** |\n",
            "| italic | *load-bearing* |\n",
            "| link | [load-bearing](https://example.invalid/docs) |\n",
        );
        let segments = segment_markdown(text);
        let MarkdownSegment::Table(table) = segments
            .iter()
            .find(|s| matches!(s, MarkdownSegment::Table(_)))
            .expect("a table segment")
        else {
            unreachable!();
        };
        let bold = &table.rows[0][1];
        assert_eq!(bold.len(), 1);
        assert!(bold[0].style.strong, "strong run must be flagged");
        assert_eq!(bold[0].text, "load-bearing");

        let italic = &table.rows[1][1];
        assert_eq!(italic.len(), 1);
        assert!(italic[0].style.emphasis, "emphasis run must be flagged");

        let link = &table.rows[2][1];
        assert_eq!(link.len(), 1);
        assert_eq!(
            link[0].style.link.as_deref(),
            Some("https://example.invalid/docs")
        );
        assert_eq!(link[0].text, "load-bearing");
    }
}
