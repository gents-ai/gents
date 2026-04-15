use defra_agent_protocol::client_protocol::ClientTurnState;
use defra_agent_protocol::row::AgentResponseRow;
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
        crate::views::card(
            ui,
            "No Conversation Selected",
            "Pick a conversation from the sidebar or submit a new message to create one.",
        );
        return;
    };

    let transcript = store.transcript(session_id);
    let requests = store.requests_for_session(session_id);
    let latest_reasoning = latest_reasoning_response(store, session_id);

    egui::ScrollArea::vertical()
        .stick_to_bottom(state.chat.transcript_stick_to_bottom)
        .show(ui, |ui| {
            if transcript.messages.is_empty() && requests.is_empty() {
                crate::views::card(
                    ui,
                    "Transcript Empty",
                    "This conversation has not produced messages yet. Submitted requests will appear here as soon as the local replica observes them.",
                );
            }

            if transcript.messages.is_empty() {
                for request in requests {
                    if let Some(content) = request.content.as_deref() {
                        message_block(ui, markdown_cache, "USER", palette.text_1, content);
                        ui.add_space(10.0);
                    }
                    if let Some(response) = store.latest_response_for_request(&request.request_id) {
                        if let Some(content) = response_fallback_content(response) {
                            message_block(ui, markdown_cache, "ASSISTANT", palette.accent, content);
                            ui.add_space(10.0);
                        }
                    }
                }
            } else {
                for message in &transcript.messages {
                    let label = match message.role.as_deref() {
                        Some("assistant") => "ASSISTANT",
                        Some("tool") => "TOOL",
                        _ => "USER",
                    };
                    let color = if label == "ASSISTANT" {
                        palette.accent
                    } else {
                        palette.text_1
                    };
                    message_block(
                        ui,
                        markdown_cache,
                        label,
                        color,
                        message.content.as_deref().unwrap_or_default(),
                    );
                    ui.add_space(6.0);

                    for tool_call in transcript
                        .tool_calls
                        .iter()
                        .filter(|tool_call| tool_call.message_sequence == message.sequence)
                    {
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
                            .fill(palette.background_1)
                            .stroke(egui::Stroke::new(1.0, stroke_color))
                            .corner_radius(4)
                            .inner_margin(10)
                            .show(ui, |ui| {
                                let response = ui
                                    .selectable_label(expanded, label)
                                    .on_hover_text("toggle tool card");
                                audit::record(
                                    ui,
                                    &audit::targets::chat_tool_card(&card_id),
                                    &response,
                                );
                                if response.clicked()
                                    && !state.chat.expanded_tool_cards.insert(card_id.clone())
                                {
                                    state.chat.expanded_tool_cards.remove(&card_id);
                                }

                                if expanded {
                                    if let Some(args) = tool_call.args.as_deref() {
                                        ui.add_space(6.0);
                                        ui.label(
                                            RichText::new("ARGS")
                                                .monospace()
                                                .size(10.5)
                                                .color(palette.text_2),
                                        );
                                        render_markdown(
                                            ui,
                                            markdown_cache,
                                            &fenced_code_block(args, None),
                                        );
                                    }
                                    let output = transcript
                                        .tool_results
                                        .iter()
                                        .find(|result| result.tool_name == tool_call.tool_name)
                                        .and_then(|result| result.output_text.as_deref())
                                        .or(tool_call.result.as_deref())
                                        .unwrap_or("");
                                    if !output.trim().is_empty() {
                                        ui.add_space(6.0);
                                        ui.label(
                                            RichText::new("OUTPUT")
                                                .monospace()
                                                .size(10.5)
                                                .color(palette.text_2),
                                        );
                                        render_markdown(
                                            ui,
                                            markdown_cache,
                                            &fenced_code_block(output, None),
                                        );
                                    }
                                }
                            });
                        ui.add_space(8.0);
                    }
                }
            }

            if let Some(response) = latest_reasoning {
                ui.add_space(6.0);
                reasoning_block(ui, state, markdown_cache, response);
            }
        });
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
                    response.reasoning.as_deref().unwrap_or_default(),
                );
            }
        });
}

fn message_block(
    ui: &mut Ui,
    markdown_cache: &mut CommonMarkCache,
    label: &str,
    label_color: egui::Color32,
    body: &str,
) {
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
                render_markdown(ui, markdown_cache, body);
            });
    });
}

fn render_markdown(ui: &mut Ui, markdown_cache: &mut CommonMarkCache, text: &str) {
    CommonMarkViewer::new()
        .syntax_theme_light(MARKDOWN_THEME_LIGHT)
        .syntax_theme_dark(MARKDOWN_THEME_DARK)
        .show(ui, markdown_cache, text);
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
