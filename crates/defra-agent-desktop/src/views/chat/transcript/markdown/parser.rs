use super::{Cell, CellRun, InlineStyle, ParsedTable};

pub(super) enum MarkdownSegment {
    Prose(String),
    Table(ParsedTable),
}

pub(super) fn segment_markdown(text: &str) -> Vec<MarkdownSegment> {
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
