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
        egui::ScrollArea::vertical()
            .stick_to_bottom(state.chat.transcript_stick_to_bottom)
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
    let expanded = state.chat.expanded_reasoning_cards.contains(&card_id);

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
            if toggle.clicked() && !state.chat.expanded_reasoning_cards.insert(card_id.clone()) {
                state.chat.expanded_reasoning_cards.remove(&card_id);
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
            let expanded = state.chat.expanded_tool_cards.contains(&card_id);
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
                            && !state.chat.expanded_tool_cards.insert(card_id.clone())
                        {
                            state.chat.expanded_tool_cards.remove(&card_id);
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
                body(ui);
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
        CommonMarkViewer::new()
            .syntax_theme_light(MARKDOWN_THEME_LIGHT)
            .syntax_theme_dark(MARKDOWN_THEME_DARK)
            .show(ui, markdown_cache, text);
    });
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
    let available = ui.available_size();
    let (outer_rect, _) =
        ui.allocate_exact_size(available, egui::Sense::hover());
    ui.painter().rect(
        outer_rect,
        6.0,
        palette.background_0,
        egui::Stroke::new(1.0, palette.stroke_subtle),
        egui::StrokeKind::Inside,
    );
    let inner_rect = outer_rect.shrink(14.0);
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(inner_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.set_clip_rect(inner_rect);
            body(ui);
        },
    );
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
                body(ui);
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
    state.chat.tool_detail_modal = Some(crate::state::ToolDetailModalState {
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
    let Some(modal) = state.chat.tool_detail_modal.clone() else {
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
        state.chat.tool_detail_modal = None;
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

