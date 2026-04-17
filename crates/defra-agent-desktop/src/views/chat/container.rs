use eframe::egui::Ui;
use egui_commonmark::CommonMarkCache;

use crate::chat::projection::project_chat;
use crate::client::{ClientCore, ClientStore};
use crate::state::ShellState;
use crate::views;

use super::nudge::render_first_conversation_nudge;
use super::view_model::{
    build_conversation_buckets, display_name_for_agent, effective_behavior_id,
};
use super::{composer, header, sidebar, transcript};

pub fn prepare_state(
    state: &mut ShellState,
    _client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    let Some(store) = store else {
        return;
    };

    if let Some(agent_did) = state.chat.shell.selected_agent_did.clone() {
        state.status.active_agent = display_name_for_agent(store, &agent_did);
        state.status.runtime_state = store
            .latest_runtime(&agent_did)
            .and_then(|runtime| runtime.process_state.as_deref())
            .unwrap_or("observing")
            .to_string();
    } else {
        state.status.active_agent = "no agent selected".to_string();
        state.status.runtime_state = "idle".to_string();
    }
}

pub fn show_sidebar(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    let palette = crate::theme::palette();

    let Some(store) = store else {
        views::card(
            ui,
            "Chat Unavailable",
            "The desktop client must finish bootstrapping before replicated chat data can render.",
        );
        return;
    };

    let selected_agent = state.chat.shell.selected_agent_did.clone();
    let selected_session = state.chat.shell.selected_session_id.clone();
    let selected_behavior = effective_behavior_id(state, store, selected_agent.as_deref());
    let conversations = selected_agent
        .as_deref()
        .map(|agent_did| {
            let rows = selected_behavior
                .as_deref()
                .map(|behavior_id| store.conversations_for_behavior(agent_did, behavior_id))
                .unwrap_or_else(|| store.conversation_rows(agent_did));
            build_conversation_buckets(&rows, chrono::Utc::now())
        })
        .unwrap_or_default();

    sidebar::show(
        ui,
        palette,
        state,
        client,
        store,
        &conversations,
        selected_agent.as_deref(),
        selected_session.as_deref(),
    );
}

pub fn show_main(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    markdown_cache: &mut CommonMarkCache,
) {
    let Some(store) = store else {
        views::card(
            ui,
            "Chat Unavailable",
            "The local replica is offline. Bootstrap must succeed before the chat activity can render.",
        );
        return;
    };

    let projection = project_chat(&state.chat, store, client.is_some());
    let selected_agent_did = state.chat.shell.selected_agent_did.clone();
    let selected_session_id = state.chat.shell.selected_session_id.clone();
    let turn_state = projection.turn_state;
    let send_status = projection.send_status;
    let show_first_conversation_nudge = projection.show_first_conversation_nudge;

    egui::Panel::bottom("chat_composer_panel")
        .resizable(false)
        .exact_size(248.0)
        .show_inside(ui, |ui| {
            composer::show(
                ui,
                state,
                store,
                selected_agent_did.as_deref(),
                turn_state,
                send_status,
            );
        });
    ui.vertical(|ui| {
        header::show(
            ui,
            state,
            header::HeaderProps {
                store,
                client,
                selected_agent_did: selected_agent_did.as_deref(),
                selected_session_id: selected_session_id.as_deref(),
                turn_state,
            },
        );
        ui.add_space(12.0);
        if show_first_conversation_nudge {
            render_first_conversation_nudge(ui, state, client, selected_agent_did.as_deref());
        } else {
            transcript::show(
                ui,
                state,
                store,
                selected_session_id.as_deref(),
                turn_state,
                markdown_cache,
            );
        }
    });
}

pub fn send_disabled(
    client_available: bool,
    selected_agent_did: Option<&str>,
    composer_text: &str,
    turn_state: Option<ClientTurnState>,
) -> bool {
    !client_available
        || selected_agent_did.is_none()
        || composer_text.trim().is_empty()
        || turn_state.is_some_and(|turn| !turn.is_terminal())
}

pub fn turn_state_label(turn_state: Option<ClientTurnState>) -> &'static str {
    match turn_state {
        Some(ClientTurnState::WaitingForClaim) => "waiting for claim",
        Some(ClientTurnState::Streaming) => "streaming",
        Some(ClientTurnState::Completed) => "completed",
        Some(ClientTurnState::Failed) => "failed",
        Some(ClientTurnState::Superseded) => "superseded",
        None => "idle",
    }
}

use defra_agent_protocol::client_protocol::ClientTurnState;
