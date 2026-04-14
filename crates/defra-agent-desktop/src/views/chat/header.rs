use defra_agent_protocol::client_protocol::ClientTurnState;
use eframe::egui::{self, RichText, Ui};

use crate::audit;
use crate::client::ClientStore;
use crate::state::ShellState;
use crate::theme;
use crate::views;

use super::turn_state_label;

pub fn show(
    ui: &mut Ui,
    state: &ShellState,
    store: &ClientStore,
    selected_agent_did: Option<&str>,
    selected_session_id: Option<&str>,
    turn_state: Option<ClientTurnState>,
) {
    let breadcrumb = breadcrumb(store, selected_agent_did, selected_session_id);
    views::toolbar(
        ui,
        "Conversation",
        &breadcrumb,
        turn_state_label(turn_state),
    );
    ui.add_space(8.0);

    ui.horizontal(|ui| {
        ui.group(|ui| {
            ui.label(
                RichText::new(format!(
                    "TURN  {}",
                    turn_state_label(turn_state).to_uppercase()
                ))
                .monospace()
                .size(10.5)
                .color(match turn_state {
                    Some(ClientTurnState::Streaming) => theme::palette().accent,
                    Some(ClientTurnState::Failed) => theme::palette().danger,
                    Some(ClientTurnState::Completed) => theme::palette().text_1,
                    Some(ClientTurnState::Superseded) => theme::palette().warning,
                    _ => theme::palette().text_2,
                }),
            );
        });
        ui.add_space(6.0);
        audit::add_enabled(
            ui,
            audit::targets::CHAT_RETRY,
            false,
            egui::Button::new("Retry"),
        );
        audit::add_enabled(
            ui,
            audit::targets::CHAT_EXPORT,
            false,
            egui::Button::new("Export"),
        );
        if let Some(error) = state.chat.last_submission_error.as_deref() {
            ui.add_space(12.0);
            ui.label(
                RichText::new(error)
                    .monospace()
                    .size(10.5)
                    .color(theme::palette().warning),
            );
        }
    });
}

fn breadcrumb(
    store: &ClientStore,
    selected_agent_did: Option<&str>,
    selected_session_id: Option<&str>,
) -> String {
    let agent = selected_agent_did
        .map(|agent_did| {
            store
                .agent_principals
                .iter()
                .find(|row| row.agent_did == agent_did)
                .and_then(|row| row.display_name.as_deref())
                .unwrap_or(agent_did)
                .to_string()
        })
        .unwrap_or_else(|| "no agent".to_string());
    let conversation = selected_session_id
        .and_then(|session_id| {
            store
                .conversations
                .iter()
                .find(|row| row.session_id == session_id)
                .and_then(|row| row.title.as_deref())
        })
        .unwrap_or("new conversation");

    format!(
        "{} / {} / {}",
        state_label(selected_agent_did),
        agent,
        conversation
    )
}

fn state_label(selected_agent_did: Option<&str>) -> &'static str {
    if selected_agent_did.is_some() {
        "deployment"
    } else {
        "replica"
    }
}
