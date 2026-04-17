use defra_agent_protocol::row::{AgentToolCallRow, AgentToolResultRow};
use eframe::egui::{self, RichText, Ui};

use crate::audit;
use crate::state::ShellState;
use crate::theme;

use super::messages::supporting_block;
use super::modal::open_tool_detail_modal;

pub(super) fn tool_turn_block(
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

fn tool_status_color(status: Option<&str>) -> egui::Color32 {
    match status.unwrap_or_default() {
        "completed" | "complete" | "success" => theme::palette().accent,
        "failed" | "error" => theme::palette().danger,
        "running" | "streaming" | "processing" => theme::palette().warning,
        _ => theme::palette().stroke,
    }
}
