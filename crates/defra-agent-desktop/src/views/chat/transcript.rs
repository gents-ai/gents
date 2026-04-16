use defra_agent_protocol::client_protocol::ClientTurnState;
use defra_agent_protocol::row::{AgentResponseRow, AgentToolCallRow, AgentToolResultRow};
use defra_agent_protocol::transcript::{present_persisted_message, PresentedMessageRole};
use eframe::egui::{self, RichText, Ui};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

use crate::audit;
use crate::client::ClientStore;
use crate::state::ShellState;
use crate::theme;

use super::turn_state_label;

const MARKDOWN_THEME_LIGHT: &str = "base16-ocean.light";
const MARKDOWN_THEME_DARK: &str = "base16-ocean.dark";

pub fn show(
    ui: &mut Ui,
    state: &mut ShellState,
    store: &ClientStore,
    selected_session_id: Option<&str>,
    turn_state: Option<ClientTurnState>,
    markdown_cache: &mut CommonMarkCache,
) {
    let palette = theme::palette();

    ui.group(|ui| {
        ui.label(
            RichText::new(format!(
                "TURN STATE  {}",
                turn_state_label(turn_state).to_uppercase()
            ))
            .monospace()
            .size(10.5)
            .color(match turn_state {
                Some(ClientTurnState::Streaming) => palette.accent,
                Some(ClientTurnState::Failed) => palette.danger,
                Some(ClientTurnState::Completed) => palette.text_1,
                Some(ClientTurnState::Superseded) => palette.warning,
                _ => palette.text_2,
            }),
        );
    });
    ui.add_space(10.0);

    let Some(session_id) = selected_session_id else {
        transcript_surface(ui, |ui| {
            centered_status_card(
                ui,
                "No Conversation Selected",
                "Pick a conversation from the sidebar or submit a new message to create one.",
            );
        });
        return;
    };

    let transcript = store.transcript(session_id);
    let requests = store.requests_for_session(session_id);
    let latest_reasoning = latest_reasoning_response(store, session_id);

    if transcript.messages.is_empty() && requests.is_empty() {
        transcript_surface(ui, |ui| {
            centered_status_card(
                ui,
                "Transcript Empty",
                "This conversation has not produced messages yet. Submitted requests will appear here as soon as the local replica observes them.",
            );
        });
        show_tool_detail_modal(ui.ctx(), state, markdown_cache);
        return;
    }

    transcript_surface(ui, |ui| {
        let scroll_output = egui::ScrollArea::vertical()
            .stick_to_bottom(state.chat.editor.transcript_stick_to_bottom)
            .show(ui, |ui| {
                if transcript.messages.is_empty() {
                    for request in requests {
                        if let Some(content) = request.content.as_deref() {
                            message_block(
                                ui,
                                markdown_cache,
                                format!("request:{}:content", request.request_id),
                                "USER",
                                palette.text_1,
                                content,
                            );
                            ui.add_space(10.0);
                        }
                        if let Some(response) =
                            store.latest_response_for_request(&request.request_id)
                        {
                            if let Some(content) = response_fallback_content(response) {
                                message_block(
                                    ui,
                                    markdown_cache,
                                    format!("response:{}:fallback", response.response_key.as_str()),
                                    "ASSISTANT",
                                    palette.accent,
                                    content,
                                );
                                ui.add_space(10.0);
                            }
                        }
                    }
                } else {
                    for message in &transcript.messages {
                        let presentation = present_persisted_message(
                            message.role.as_deref().unwrap_or("user"),
                            message.content.as_deref().unwrap_or_default(),
                        );
                        let related_tool_calls: Vec<_> = transcript
                            .tool_calls
                            .iter()
                            .copied()
                            .filter(|tool_call| tool_call.message_sequence == message.sequence)
                            .collect();
                        let suppress_tool_message = presentation.role == PresentedMessageRole::Tool
                            && (!transcript.tool_calls.is_empty()
                                || !transcript.tool_results.is_empty());

                        if presentation.has_visible_body() && !suppress_tool_message {
                            message_block(
                                ui,
                                markdown_cache,
                                format!(
                                    "message:{}:{}",
                                    message.sequence.unwrap_or_default(),
                                    presentation.role.label()
                                ),
                                presentation.role.label(),
                                message_label_color(presentation.role),
                                &presentation.body_markdown,
                            );
                            ui.add_space(6.0);
                        }

                        if !related_tool_calls.is_empty() {
                            tool_turn_block(
                                ui,
                                state,
                                &related_tool_calls,
                                &transcript.tool_results,
                            );
                            ui.add_space(8.0);
                        }
                    }
                }

                if let Some(response) = latest_reasoning {
                    ui.add_space(6.0);
                    reasoning_block(ui, state, markdown_cache, response);
                }
            });

        // Keep the stick-to-bottom flag in sync with where the user ended up
        // so manual scrolling up stops the auto-jump, and returning to the
        // bottom resumes it.
        let viewport_bottom = scroll_output.state.offset.y + scroll_output.inner_rect.height();
        let at_bottom = viewport_bottom + 2.0 >= scroll_output.content_size.y;
        state.chat.editor.transcript_stick_to_bottom = at_bottom;
    });

    show_tool_detail_modal(ui.ctx(), state, markdown_cache);
}

fn message_label_color(role: PresentedMessageRole) -> egui::Color32 {
    match role {
        PresentedMessageRole::Assistant => theme::palette().accent,
        PresentedMessageRole::Tool => theme::palette().warning,
        PresentedMessageRole::User => theme::palette().text_1,
    }
}

pub fn markdown_theme_names() -> (&'static str, &'static str) {
    (MARKDOWN_THEME_LIGHT, MARKDOWN_THEME_DARK)
}

fn latest_reasoning_response<'a>(
    store: &'a ClientStore,
    session_id: &str,
) -> Option<&'a AgentResponseRow> {
    store
        .responses
        .iter()
        .rev()
        .find(|response| response.session_id.as_deref() == Some(session_id))
        .filter(|response| {
            response
                .reasoning
                .as_deref()
                .is_some_and(|reasoning| !reasoning.trim().is_empty())
        })
}

fn response_fallback_content(response: &AgentResponseRow) -> Option<&str> {
    response
        .content
        .as_deref()
        .filter(|content| !content.trim().is_empty())
        .or_else(|| {
            response
                .error_message
                .as_deref()
                .filter(|content| !content.trim().is_empty())
        })
}

fn reasoning_block(
    ui: &mut Ui,
    state: &mut ShellState,
    markdown_cache: &mut CommonMarkCache,
    response: &AgentResponseRow,
) {
    let palette = theme::palette();
    let card_id = format!("reasoning:{}", response.response_key);
    let expanded = state
        .chat
        .editor
        .expanded_reasoning_cards
        .contains(&card_id);

    egui::Frame::new()
        .fill(palette.background_1)
        .stroke(egui::Stroke::new(1.0, palette.stroke))
        .corner_radius(4)
        .inner_margin(10)
        .show(ui, |ui| {
            let label = format!(
                "REASONING DISCLOSURE  {}",
                response.status.as_deref().unwrap_or("observed")
            );
            let toggle = ui
                .selectable_label(expanded, label)
                .on_hover_text("toggle reasoning disclosure");
            audit::record(
                ui,
                &audit::targets::chat_reasoning(&response.response_key),
                &toggle,
            );
            if toggle.clicked()
                && !state
                    .chat
                    .editor
                    .expanded_reasoning_cards
                    .insert(card_id.clone())
            {
                state.chat.editor.expanded_reasoning_cards.remove(&card_id);
            }

            if expanded {
                ui.add_space(6.0);
                render_markdown(
                    ui,
                    markdown_cache,
                    format!("reasoning:{}", response.response_key),
                    response.reasoning.as_deref().unwrap_or_default(),
                );
            }
        });
}

fn message_block(
    ui: &mut Ui,
    markdown_cache: &mut CommonMarkCache,
    markdown_id: impl std::hash::Hash,
    label: &str,
    label_color: egui::Color32,
    body: &str,
) {
    turn_block(ui, label, label_color, |ui| {
        render_markdown(ui, markdown_cache, markdown_id, body);
    });
}

fn tool_turn_block(
    ui: &mut Ui,
    state: &mut ShellState,
    tool_calls: &[&AgentToolCallRow],
    tool_results: &[&AgentToolResultRow],
) {
    let palette = theme::palette();

    supporting_block(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("TOOLS")
                    .monospace()
                    .size(9.5)
                    .color(palette.warning),
            );
            ui.label(
                RichText::new(format!(
                    "{} call{}",
                    tool_calls.len(),
                    if tool_calls.len() == 1 { "" } else { "s" }
                ))
                .monospace()
                .size(9.5)
                .color(palette.text_3),
            );
        });
        ui.add_space(4.0);
        for (index, tool_call) in tool_calls.iter().enumerate() {
            let card_id = tool_call
                .tool_call_id
                .clone()
                .or_else(|| Some(tool_call.tool_call_key.clone()))
                .unwrap_or_else(|| tool_call.tool_name.clone().unwrap_or_default());
            let expanded = state.chat.editor.expanded_tool_cards.contains(&card_id);
            let label = format!(
                "{}  {}",
                tool_call.tool_name.as_deref().unwrap_or("tool"),
                tool_call.status.as_deref().unwrap_or("pending")
            );
            let stroke_color = tool_status_color(tool_call.status.as_deref());

            egui::Frame::new()
                .fill(if expanded {
                    palette.background_1
                } else {
                    palette.background_0
                })
                .stroke(egui::Stroke::new(
                    1.0,
                    if expanded {
                        stroke_color
                    } else {
                        palette.stroke_subtle
                    },
                ))
                .corner_radius(4)
                .inner_margin(6)
                .show(ui, |ui| {
                    let output = tool_results
                        .iter()
                        .find(|result| result.tool_name == tool_call.tool_name)
                        .and_then(|result| result.output_text.as_deref())
                        .or(tool_call.result.as_deref())
                        .unwrap_or("");
                    ui.horizontal(|ui| {
                        ui.spacing_mut().button_padding = egui::vec2(6.0, 2.0);
                        ui.label(RichText::new("●").size(10.0).color(stroke_color));
                        let response = ui
                            .selectable_label(
                                expanded,
                                RichText::new(label).size(11.5).color(palette.text_1),
                            )
                            .on_hover_text("toggle tool summary");
                        audit::record(ui, &audit::targets::chat_tool_card(&card_id), &response);
                        if response.clicked()
                            && !state
                                .chat
                                .editor
                                .expanded_tool_cards
                                .insert(card_id.clone())
                        {
                            state.chat.editor.expanded_tool_cards.remove(&card_id);
                        }

                        ui.label(
                            RichText::new(tool_call.status.as_deref().unwrap_or("pending"))
                                .monospace()
                                .size(9.5)
                                .color(palette.text_2),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let output_button = audit::add_enabled(
                                ui,
                                audit::targets::chat_tool_output(&card_id),
                                !output.trim().is_empty(),
                                egui::Button::new(
                                    RichText::new("Output")
                                        .monospace()
                                        .size(9.0)
                                        .color(palette.text_1),
                                )
                                .min_size(egui::vec2(58.0, 20.0)),
                            );
                            if output_button.clicked() {
                                open_tool_detail_modal(
                                    state,
                                    &card_id,
                                    &format!(
                                        "TOOL OUTPUT · {}",
                                        tool_call.tool_name.as_deref().unwrap_or("tool")
                                    ),
                                    output,
                                    None,
                                );
                            }
                            let args_button = audit::add_enabled(
                                ui,
                                audit::targets::chat_tool_args(&card_id),
                                tool_call
                                    .args
                                    .as_deref()
                                    .is_some_and(|args| !args.trim().is_empty()),
                                egui::Button::new(
                                    RichText::new("Args")
                                        .monospace()
                                        .size(9.0)
                                        .color(palette.text_1),
                                )
                                .min_size(egui::vec2(46.0, 20.0)),
                            );
                            if args_button.clicked() {
                                open_tool_detail_modal(
                                    state,
                                    &card_id,
                                    &format!(
                                        "TOOL ARGUMENTS · {}",
                                        tool_call.tool_name.as_deref().unwrap_or("tool")
                                    ),
                                    tool_call.args.as_deref().unwrap_or_default(),
                                    Some("json"),
                                );
                            }
                        });
                    });

                    if expanded {
                        ui.add_space(4.0);
                        compact_tool_metadata(ui, tool_call);
                    }
                });

            if index + 1 < tool_calls.len() {
                ui.add_space(4.0);
            }
        }
    });
}

fn turn_block(ui: &mut Ui, label: &str, label_color: egui::Color32, body: impl FnOnce(&mut Ui)) {
    let palette = theme::palette();

    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width(66.0);
            ui.with_layout(egui::Layout::top_down_justified(egui::Align::Max), |ui| {
                ui.label(
                    RichText::new(label)
                        .monospace()
                        .size(10.5)
                        .color(label_color),
                );
            });
        });
        egui::Frame::new()
            .fill(palette.background_1)
            .stroke(egui::Stroke::new(1.0, palette.stroke_subtle))
            .corner_radius(4)
            .inner_margin(10)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.vertical(|ui| {
                    body(ui);
                });
            });
    });
}

fn render_markdown(
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

fn compact_tool_metadata(ui: &mut Ui, tool_call: &AgentToolCallRow) {
    let palette = theme::palette();
    for (label, value) in [
        ("tool", tool_call.tool_name.as_deref().unwrap_or("unknown")),
        ("status", tool_call.status.as_deref().unwrap_or("pending")),
        (
            "call id",
            tool_call
                .tool_call_id
                .as_deref()
                .unwrap_or(tool_call.tool_call_key.as_str()),
        ),
        ("started", tool_call.started_at.as_deref().unwrap_or("n/a")),
        (
            "completed",
            tool_call.completed_at.as_deref().unwrap_or("n/a"),
        ),
    ] {
        ui.horizontal(|ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(label.to_ascii_uppercase())
                    .monospace()
                    .size(10.0)
                    .color(palette.text_2),
            );
            ui.label(
                RichText::new(value)
                    .monospace()
                    .size(10.5)
                    .color(palette.text_1),
            );
        });
    }
}

fn transcript_surface(ui: &mut Ui, body: impl FnOnce(&mut Ui)) {
    let palette = theme::palette();
    let available_height = ui.available_height();
    let expected_bounds = egui::Rect::from_min_size(ui.cursor().min, ui.available_size());
    let prev_clip = ui.clip_rect();
    ui.set_clip_rect(prev_clip.intersect(expected_bounds));
    egui::Frame::new()
        .fill(palette.background_0)
        .stroke(egui::Stroke::new(1.0, palette.stroke_subtle))
        .corner_radius(6)
        .inner_margin(14)
        .show(ui, |ui| {
            ui.set_min_height((available_height - 28.0).max(0.0));
            body(ui);
        });
    ui.set_clip_rect(prev_clip);
}

fn centered_status_card(ui: &mut Ui, title: &str, body: &str) {
    let available = ui.available_size();
    ui.allocate_ui_with_layout(
        available,
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            ui.add_space((ui.available_height() * 0.22).max(24.0));
            ui.set_max_width(520.0);
            crate::views::card(ui, title, body);
        },
    );
}

fn supporting_block(ui: &mut Ui, body: impl FnOnce(&mut Ui)) {
    let palette = theme::palette();

    ui.horizontal(|ui| {
        ui.add_space(66.0);
        egui::Frame::new()
            .fill(palette.background_0)
            .stroke(egui::Stroke::new(1.0, palette.stroke_subtle))
            .corner_radius(4)
            .inner_margin(8)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.vertical(|ui| {
                    body(ui);
                });
            });
    });
}

fn open_tool_detail_modal(
    state: &mut ShellState,
    card_id: &str,
    title: &str,
    body: &str,
    language: Option<&str>,
) {
    state.chat.editor.tool_detail_modal = Some(crate::state::ToolDetailModalState {
        card_id: card_id.to_string(),
        title: title.to_string(),
        body: body.to_string(),
        language: language.map(str::to_string),
    });
}

fn show_tool_detail_modal(
    ctx: &egui::Context,
    state: &mut ShellState,
    markdown_cache: &mut CommonMarkCache,
) {
    let Some(modal) = state.chat.editor.tool_detail_modal.clone() else {
        return;
    };

    let mut open = true;
    egui::Window::new(modal.title.clone())
        .id(egui::Id::new(("tool_detail_modal", modal.card_id.as_str())))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(760.0)
        .default_height(520.0)
        .show(ctx, |ui| {
            ui.label(
                RichText::new(modal.card_id.as_str())
                    .monospace()
                    .size(10.5)
                    .color(theme::palette().text_2),
            );
            ui.add_space(8.0);
            egui::ScrollArea::vertical().show(ui, |ui| {
                let content = match modal.language.as_deref() {
                    Some(language) => fenced_code_block(&modal.body, Some(language)),
                    None => fenced_code_block(&modal.body, None),
                };
                render_markdown(
                    ui,
                    markdown_cache,
                    format!("tool-detail:{}", modal.card_id),
                    &content,
                );
            });
        });

    if !open {
        state.chat.editor.tool_detail_modal = None;
    }
}

fn fenced_code_block(content: &str, language: Option<&str>) -> String {
    let language = language.unwrap_or_default();
    format!("```{language}\n{content}\n```")
}

fn tool_status_color(status: Option<&str>) -> egui::Color32 {
    match status.unwrap_or_default() {
        "completed" | "complete" | "success" => theme::palette().accent,
        "failed" | "error" => theme::palette().danger,
        "running" | "streaming" | "processing" => theme::palette().warning,
        _ => theme::palette().stroke,
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
        let text = include_str!("fixtures/keyfiles_response.md");
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
