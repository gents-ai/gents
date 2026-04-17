use defra_agent_protocol::client_protocol::ClientTurnState;
use eframe::egui::{self, Key, RichText, TextEdit, Ui};

use crate::audit;
use crate::chat::domain::submission::SendStatus;
use crate::state::{PendingChatAction, PendingShellAction, ShellState};
use crate::theme;

pub fn show(
    ui: &mut Ui,
    state: &mut ShellState,
    _store: &crate::client::ClientStore,
    selected_agent_did: Option<&str>,
    _turn_state: Option<ClientTurnState>,
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
        let text_edit = TextEdit::multiline(&mut state.chat.editor.composer_text)
            .id_source(audit::targets::CHAT_COMPOSER_TEXT)
            .desired_rows(3)
            .hint_text("Message the selected agent");
        audit::add_sized(
            ui,
            audit::targets::CHAT_COMPOSER_TEXT,
            [ui.available_width(), 84.0],
            text_edit,
        );
        ui.add_space(8.0);

        ui.columns(2, |columns| {
            columns[0].vertical_centered_justified(|ui| {
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

            columns[1].with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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

fn submit(state: &mut ShellState, selected_agent_did: Option<&str>) {
    let Some(agent_did) = selected_agent_did else {
        state.chat.editor.last_submission_error =
            Some("select an agent before sending".to_string());
        return;
    };
    let _ = agent_did;
    state.queue_shell_action(PendingShellAction::Chat(PendingChatAction::SubmitComposer));
}
