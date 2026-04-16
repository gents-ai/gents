use defra_agent_protocol::client_protocol::ClientTurnState;
use eframe::egui::{self, Key, RichText, TextEdit, Ui};
use tokio::runtime::Runtime;

use crate::audit;
use crate::chat::controller;
use crate::chat::domain::submission::SendStatus;
use crate::client::ClientCore;
use crate::client::ClientStore;
use crate::state::ShellState;
use crate::theme;

use super::turn_state_label;

pub fn show(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: &ClientStore,
    runtime: &Runtime,
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
        ui.label(
            RichText::new("Composer")
                .family(theme::stencil_family())
                .size(13.0)
                .color(palette.text_1)
                .strong(),
        );
        ui.add_space(6.0);

        let text_edit = TextEdit::multiline(&mut state.chat.composer_text)
            .id_source(audit::targets::CHAT_COMPOSER_TEXT)
            .desired_rows(4)
            .hint_text("Send an operational request to the selected agent");
        audit::add_sized(
            ui,
            audit::targets::CHAT_COMPOSER_TEXT,
            [ui.available_width(), 96.0],
            text_edit,
        );
        ui.add_space(8.0);

        ui.columns(2, |columns| {
            columns[0].vertical(|ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "[behavior: {}]",
                            state
                                .chat
                                .selected_behavior_override
                                .as_deref()
                                .or_else(|| {
                                    selected_agent_did.and_then(|agent_did| {
                                        store
                                            .agent_principals
                                            .iter()
                                            .find(|row| row.agent_did == agent_did)
                                            .and_then(|row| row.default_behavior_id.as_deref())
                                    })
                                })
                                .unwrap_or("inherited")
                        ))
                        .monospace()
                        .size(11.0)
                        .color(palette.text_2),
                    );
                    ui.label(
                        RichText::new("[tools: inherited]")
                            .monospace()
                            .size(11.0)
                            .color(palette.text_2),
                    );
                    ui.label(
                        RichText::new(format!("[turn: {}]", turn_state_label(turn_state)))
                            .monospace()
                            .size(11.0)
                            .color(palette.text_2),
                    );
                });
                ui.add_space(4.0);
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
                    submit(state, client, runtime, selected_agent_did);
                }
            });
        });
    });
}

fn submit(
    state: &mut ShellState,
    client: Option<&ClientCore>,
    runtime: &Runtime,
    selected_agent_did: Option<&str>,
) {
    let Some(agent_did) = selected_agent_did else {
        state.chat.last_submission_error = Some("select an agent before sending".to_string());
        return;
    };
    let _ = agent_did;
    if let Err(error) = controller::submit_composer(&mut state.chat, client, runtime) {
        state.chat.last_submission_error = Some(error.to_string());
    }
}
