use defra_agent_protocol::client_protocol::ClientTurnState;
use eframe::egui::{self, Key, RichText, TextEdit, Ui};
use tokio::runtime::Runtime;

use crate::audit;
use crate::client::ClientCore;
use crate::client::ClientStore;
use crate::state::ShellState;
use crate::theme;

use super::{send_disabled, turn_state_label};

pub fn show(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: &ClientStore,
    runtime: &Runtime,
    selected_agent_did: Option<&str>,
    turn_state: Option<ClientTurnState>,
) {
    let palette = theme::palette();
    let client_available = client.is_some();
    let disabled = send_disabled(
        client_available,
        selected_agent_did,
        &state.chat.composer_text,
        turn_state,
    );

    ui.group(|ui| {
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
            .desired_rows(5)
            .hint_text("Send an operational request to the selected agent");
        audit::add_sized(
            ui,
            audit::targets::CHAT_COMPOSER_TEXT,
            [ui.available_width(), 110.0],
            text_edit,
        );
        ui.add_space(8.0);

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
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let clicked = audit::add_enabled(
                    ui,
                    audit::targets::CHAT_SEND,
                    !disabled,
                    egui::Button::new("Send"),
                )
                .clicked();
                let shortcut = !disabled
                    && ui.input(|input| input.modifiers.command && input.key_pressed(Key::Enter));
                if clicked || shortcut {
                    submit(state, client, runtime, selected_agent_did);
                }
            });
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new("Cmd+Enter to send")
                .monospace()
                .size(10.5)
                .color(if disabled {
                    palette.text_3
                } else {
                    palette.text_2
                }),
        );
    });
}

fn submit(
    state: &mut ShellState,
    client: Option<&ClientCore>,
    runtime: &Runtime,
    selected_agent_did: Option<&str>,
) {
    let Some(client) = client else {
        state.chat.last_submission_error = Some("client core is offline".to_string());
        return;
    };
    let Some(agent_did) = selected_agent_did else {
        state.chat.last_submission_error = Some("select an agent before sending".to_string());
        return;
    };

    let behavior_override = state.chat.selected_behavior_override.as_deref();
    let submission = if let Some(session_id) = state.chat.selected_session_id.clone() {
        runtime.block_on(client.submit_request(
            &session_id,
            agent_did,
            &state.chat.composer_text,
            behavior_override,
        ))
    } else {
        runtime.block_on(async {
            let created = client
                .create_conversation(agent_did, behavior_override)
                .await?;
            client
                .submit_request(
                    &created.session_id,
                    agent_did,
                    &state.chat.composer_text,
                    behavior_override,
                )
                .await
        })
    };

    match submission {
        Ok(result) => {
            state.chat.selected_session_id = Some(result.session_id);
            state.chat.last_submission_error = None;
            state.chat.last_action_message = None;
            state.chat.last_export_payload = None;
            state.chat.composer_text.clear();
            state.chat.transcript_stick_to_bottom = true;
        }
        Err(error) => {
            state.chat.last_submission_error = Some(error.to_string());
        }
    }
}
