use defra_agent_protocol::client_protocol::ClientTurnState;
use eframe::egui::{self, ComboBox, Key, RichText, TextEdit, Ui};

use crate::audit;
use crate::chat::domain::submission::SendStatus;
use crate::client::ClientStore;
use crate::state::{PendingChatAction, PendingShellAction, ShellState};
use crate::theme;

use super::turn_state_label;
use super::view_model::{behavior_selection_entries, display_behavior_label};

pub fn show(
    ui: &mut Ui,
    state: &mut ShellState,
    store: &ClientStore,
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

        render_behavior_selector(ui, state, store, selected_agent_did);
        ui.add_space(6.0);

        let text_edit = TextEdit::multiline(&mut state.chat.editor.composer_text)
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
                    let effective_behavior =
                        effective_behavior_id(state, store, selected_agent_did);
                    let behavior_locked = state.chat.shell.selected_session_id.is_some();
                    let behavior_label = selected_agent_did
                        .map(|agent_did| {
                            display_behavior_label(store, agent_did, effective_behavior.as_deref())
                        })
                        .unwrap_or_else(|| "Inherited default".to_string());
                    ui.label(
                        RichText::new(format!(
                            "[behavior: {}{}]",
                            behavior_label,
                            if behavior_locked { " · locked" } else { "" }
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
                    submit(state, selected_agent_did);
                }
            });
        });
    });
}

fn render_behavior_selector(
    ui: &mut Ui,
    state: &mut ShellState,
    store: &ClientStore,
    selected_agent_did: Option<&str>,
) {
    let Some(agent_did) = selected_agent_did else {
        return;
    };

    let selected_session_id = state.chat.shell.selected_session_id.as_deref();
    let locked = selected_session_id.is_some();
    let selected_behavior_id = effective_behavior_id(state, store, selected_agent_did);
    let selected_text = display_behavior_label(store, agent_did, selected_behavior_id.as_deref());

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Behavior")
                .monospace()
                .size(10.5)
                .color(theme::palette().text_2),
        );
        if locked {
            ui.label(
                RichText::new("locked to selected session")
                    .monospace()
                    .size(10.5)
                    .color(theme::palette().text_3),
            );
        }
    });

    if locked {
        audit::add_enabled(
            ui,
            audit::targets::CHAT_BEHAVIOR_SELECT,
            false,
            egui::Button::new(selected_text).min_size(egui::vec2(ui.available_width(), 28.0)),
        );
        return;
    }

    let response = ComboBox::from_id_salt(audit::targets::CHAT_BEHAVIOR_SELECT)
        .selected_text(selected_text)
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            for entry in behavior_selection_entries(store, agent_did) {
                let selected = entry.behavior_id.as_deref()
                    == state.chat.editor.selected_behavior_override.as_deref();
                let target = entry
                    .behavior_id
                    .as_deref()
                    .map(audit::targets::chat_behavior_option)
                    .unwrap_or_else(|| "chat.behavior.option.default".to_string());
                let option = ui.selectable_label(selected, &entry.label);
                audit::record(ui, &target, &option);
                if option.clicked() {
                    state.queue_shell_action(PendingShellAction::Chat(
                        PendingChatAction::SelectBehavior {
                            behavior_id: entry.behavior_id.clone(),
                        },
                    ));
                    ui.close();
                }
            }
        });
    audit::record(ui, audit::targets::CHAT_BEHAVIOR_SELECT, &response.response);
}

fn effective_behavior_id(
    state: &ShellState,
    store: &ClientStore,
    selected_agent_did: Option<&str>,
) -> Option<String> {
    state
        .chat
        .shell
        .selected_session_id
        .as_deref()
        .and_then(|session_id| store.session_behavior_id(session_id, selected_agent_did))
        .or_else(|| state.chat.editor.selected_behavior_override.clone())
        .or_else(|| {
            selected_agent_did.and_then(|agent_did| {
                store
                    .default_behavior_id_for_agent(agent_did)
                    .map(ToOwned::to_owned)
            })
        })
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
