use defra_agent_protocol::client_protocol::ClientTurnState;
use defra_agent_protocol::row::AgentRequestRow;
use eframe::egui::{self, RichText, Ui};

use crate::audit;
use crate::client::{ClientCore, ClientStore};
use crate::state::{PendingChatAction, PendingShellAction, ShellState};
use crate::theme;
use crate::views::toolbar;

use super::view_model::simple_behavior_label;

pub(super) struct HeaderProps<'a> {
    pub store: &'a ClientStore,
    pub client: Option<&'a ClientCore>,
    pub selected_agent_did: Option<&'a str>,
    pub selected_session_id: Option<&'a str>,
    pub turn_state: Option<ClientTurnState>,
}

pub(super) fn show(ui: &mut Ui, state: &mut ShellState, props: HeaderProps<'_>) {
    let breadcrumb = breadcrumb(
        props.store,
        props.selected_agent_did,
        props.selected_session_id,
    );
    let title = conversation_title(props.store, props.selected_session_id);
    let latest_request = props
        .selected_session_id
        .and_then(|session_id| latest_request_for_session(props.store, session_id))
        .cloned();
    let retry_enabled = props.client.is_some()
        && props.selected_agent_did.is_some()
        && latest_request.as_ref().is_some_and(|request| {
            request
                .content
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
        })
        && props.turn_state.is_some_and(ClientTurnState::is_terminal);
    let export_enabled = props
        .selected_session_id
        .is_some_and(|session_id| conversation_has_export_rows(props.store, session_id));
    toolbar(ui, "Conversation", &breadcrumb, "");
    ui.add_space(4.0);

    ui.label(
        RichText::new(title)
            .size(23.0)
            .color(theme::palette().text_0)
            .strong(),
    );
    ui.add_space(8.0);

    ui.horizontal(|ui| {
        if let Some(error) = state.chat.editor.last_submission_error.as_deref() {
            ui.label(
                RichText::new(error)
                    .monospace()
                    .size(10.5)
                    .color(theme::palette().warning),
            );
        } else if let Some(message) = state.chat.editor.last_action_message.as_deref() {
            ui.label(
                RichText::new(message)
                    .monospace()
                    .size(10.5)
                    .color(theme::palette().text_2),
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if retry_enabled {
                if audit::add_enabled(
                    ui,
                    audit::targets::CHAT_RETRY,
                    true,
                    egui::Button::new("Retry"),
                )
                .clicked()
                {
                    state.queue_shell_action(PendingShellAction::Chat(
                        PendingChatAction::RetryLatestRequest,
                    ));
                }
            }
            if export_enabled {
                if audit::add_enabled(
                    ui,
                    audit::targets::CHAT_EXPORT,
                    true,
                    egui::Button::new("Export"),
                )
                .clicked()
                {
                    export_conversation(ui, state, props.store, props.selected_session_id);
                }
            }
        });
    });
}

fn latest_request_for_session<'a>(
    store: &'a ClientStore,
    session_id: &str,
) -> Option<&'a AgentRequestRow> {
    store.requests_for_session(session_id).into_iter().last()
}

fn export_conversation(
    ui: &mut Ui,
    state: &mut ShellState,
    store: &ClientStore,
    selected_session_id: Option<&str>,
) {
    let Some(session_id) = selected_session_id else {
        state.chat.editor.last_submission_error =
            Some("select a conversation before exporting".to_string());
        return;
    };

    match export_conversation_json(store, session_id) {
        Ok(payload) => {
            let byte_len = payload.len();
            ui.copy_text(payload.clone());
            state.chat.editor.last_export_payload = Some(payload);
            state.chat.editor.last_submission_error = None;
            state.chat.editor.last_action_message =
                Some(format!("Copied conversation export ({byte_len} bytes)."));
        }
        Err(error) => {
            state.chat.editor.last_submission_error = Some(error.to_string());
        }
    }
}

fn export_conversation_json(store: &ClientStore, session_id: &str) -> serde_json::Result<String> {
    let transcript = store.transcript(session_id);
    let request_ids = store
        .requests_for_session(session_id)
        .iter()
        .map(|request| request.request_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let responses = store
        .responses
        .iter()
        .filter(|response| {
            response.session_id.as_deref() == Some(session_id)
                || response
                    .request_id
                    .as_deref()
                    .is_some_and(|request_id| request_ids.contains(request_id))
        })
        .collect::<Vec<_>>();
    let payload = serde_json::json!({
        "conversation": store.conversations.iter().find(|row| row.session_id == session_id),
        "requests": store.requests_for_session(session_id),
        "responses": responses,
        "messages": transcript.messages,
        "tool_calls": transcript.tool_calls,
        "tool_results": transcript.tool_results,
    });
    serde_json::to_string_pretty(&payload)
}

fn conversation_has_export_rows(store: &ClientStore, session_id: &str) -> bool {
    store
        .conversations
        .iter()
        .any(|row| row.session_id == session_id)
        || !store.requests_for_session(session_id).is_empty()
        || !store.transcript(session_id).messages.is_empty()
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
    let behavior = selected_session_id
        .and_then(|session_id| store.session_behavior_id(session_id, selected_agent_did))
        .or_else(|| {
            selected_agent_did.and_then(|agent_did| {
                store
                    .default_behavior_id_for_agent(agent_did)
                    .map(ToOwned::to_owned)
            })
        })
        .and_then(|behavior_id| {
            selected_agent_did.map(|agent_did| {
                let display_name = store
                    .behavior_row(agent_did, &behavior_id)
                    .and_then(|row| row.display_name.as_deref());
                simple_behavior_label(display_name, Some(behavior_id.as_str()))
            })
        })
        .unwrap_or_else(|| "Inherited default".to_string());

    format!("{agent} / {behavior}")
}

fn conversation_title(store: &ClientStore, selected_session_id: Option<&str>) -> String {
    selected_session_id
        .and_then(|session_id| {
            store
                .conversations
                .iter()
                .find(|row| row.session_id == session_id)
                .and_then(|row| row.title.as_deref())
        })
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("New Conversation")
        .to_string()
}
