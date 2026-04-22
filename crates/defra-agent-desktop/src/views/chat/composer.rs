use defra_agent_protocol::client_protocol::ClientTurnState;
use eframe::egui::{self, Key, RichText, TextEdit, Ui};

use super::turn_state_label;
use crate::audit;
use crate::chat::domain::submission::SendStatus;
use crate::state::{PendingChatAction, PendingShellAction, ShellState};
use crate::theme;

pub fn show(
    ui: &mut Ui,
    state: &mut ShellState,
    _store: &crate::client::ClientStore,
    selected_agent_did: Option<&str>,
    turn_state: Option<ClientTurnState>,
    send_status: SendStatus,
) {
    let palette = theme::palette();
    let disabled = send_status.is_disabled();
    let shortcut =
        !disabled && ui.input(|input| input.modifiers.command && input.key_pressed(Key::Enter));
    let helper_text = send_status
        .blocked_reason()
        .map(|reason| reason.hint())
        .unwrap_or("Cmd+Enter to send".to_string());

    ui.group(|ui| {
        ui.set_width(ui.available_width());
        let footer_reserve = 52.0;
        let min_text_height = if state.chat.editor.composer_expanded {
            144.0
        } else {
            76.0
        };
        let text_height = (ui.available_height() - footer_reserve).max(min_text_height);
        let desired_rows = state
            .chat
            .editor
            .composer_text
            .lines()
            .count()
            .max(if state.chat.editor.composer_expanded {
                7
            } else {
                2
            })
            .min(24);
        let edit_width = ui.available_width();
        let response = egui::ScrollArea::vertical()
            .max_height(text_height)
            .show(ui, |ui| {
                ui.add_sized(
                    [
                        edit_width - 6.0,
                        text_height.max(desired_rows as f32 * 20.0),
                    ],
                    TextEdit::multiline(&mut state.chat.editor.composer_text)
                        .id_source(audit::targets::CHAT_COMPOSER_TEXT)
                        .desired_rows(desired_rows)
                        .desired_width(ui.available_width())
                        .hint_text("Message the selected agent"),
                )
            })
            .inner;
        audit::record(ui, audit::targets::CHAT_COMPOSER_TEXT, &response);
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                let (turn_label, turn_color) = turn_status(turn_state, &palette);
                ui.label(
                    RichText::new(turn_label)
                        .monospace()
                        .size(10.5)
                        .color(turn_color),
                );
                ui.label(
                    RichText::new(helper_text)
                        .monospace()
                        .size(10.5)
                        .color(if disabled {
                            palette.warning
                        } else {
                            palette.text_2
                        }),
                );
            });
            ui.add_space(8.0);
            let composer_label = if state.chat.editor.composer_expanded {
                "Compact"
            } else {
                "Expand"
            };
            if audit::add(
                ui,
                "chat.composer.toggle_size",
                egui::Button::new(
                    RichText::new(composer_label)
                        .monospace()
                        .size(10.5)
                        .color(palette.text_1),
                )
                .min_size(egui::vec2(64.0, 24.0)),
            )
            .clicked()
            {
                state.chat.editor.composer_expanded = !state.chat.editor.composer_expanded;
                state.chat.editor.composer_panel_height =
                    Some(if state.chat.editor.composer_expanded {
                        236.0
                    } else {
                        148.0
                    });
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().button_padding = egui::vec2(12.0, 8.0);
                let clicked = audit::add_enabled(
                    ui,
                    audit::targets::CHAT_SEND,
                    !disabled,
                    egui::Button::new("Send").min_size(egui::vec2(92.0, 32.0)),
                )
                .clicked();
                if clicked || shortcut {
                    submit(state, selected_agent_did);
                }
            });
        });
    });
}

fn turn_status(
    turn_state: Option<ClientTurnState>,
    palette: &theme::Palette,
) -> (String, egui::Color32) {
    match turn_state {
        Some(ClientTurnState::WaitingForClaim) => ("turn waiting...".to_string(), palette.warning),
        Some(ClientTurnState::Streaming) => ("turn streaming...".to_string(), palette.accent),
        // TODO(ui-polish): distinguish interrupted from failed in display text/icons.
        // For now treat identically — both mean "no complete response."
        Some(ClientTurnState::Failed) | Some(ClientTurnState::Interrupted) => {
            ("turn failed".to_string(), palette.warning)
        }
        Some(ClientTurnState::Superseded) => ("turn superseded".to_string(), palette.text_2),
        Some(ClientTurnState::Completed) => ("turn completed".to_string(), palette.text_2),
        None => (format!("turn {}", turn_state_label(None)), palette.text_3),
    }
}

fn submit(state: &mut ShellState, selected_agent_did: Option<&str>) {
    let Some(agent_did) = selected_agent_did else {
        state.chat.editor.last_submission_error =
            Some("select an agent before sending".to_string());
        return;
    };
    let _ = agent_did;
    state.queue_shell_action(PendingShellAction::Chat(PendingChatAction::SubmitComposer));
}
