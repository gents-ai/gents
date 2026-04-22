use eframe::egui::{self, Sense, Ui};
use egui_commonmark::CommonMarkCache;
use tokio::runtime::Runtime;

use crate::chat::projection::project_chat;
use crate::client::{ClientCore, ClientStore};
use crate::state::ShellState;
use crate::views;
use crate::views::components;

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
    runtime: &Runtime,
    markdown_cache: &mut CommonMarkCache,
) {
    ui.set_width(ui.available_width());
    ui.set_min_width(ui.available_width());

    let Some(store) = store else {
        views::card(
            ui,
            "Chat Unavailable",
            "The local replica is offline. Bootstrap must succeed before the chat activity can render.",
        );
        return;
    };

    let deployment_entries = super::view_model::build_deployment_entries(
        &client.map(ClientCore::peer_statuses).unwrap_or_default(),
        store,
    );
    if deployment_entries.is_empty() || state.setup.workspace_open {
        crate::views::setup::show_embedded_main(ui, state, client, Some(store), runtime);
        return;
    }

    let projection = project_chat(&state.chat, store, client.is_some());
    let selected_agent_did = state.chat.shell.selected_agent_did.clone();
    let selected_session_id = state.chat.shell.selected_session_id.clone();
    let turn_state = projection.turn_state;
    let send_status = projection.send_status;
    let available = ui.available_size();
    ui.allocate_ui_with_layout(
        available,
        egui::Layout::left_to_right(egui::Align::Min),
        |ui| {
            let rail_visible = !state.chat.shell.sidebar_collapsed;
            if rail_visible {
                let sidebar_width = current_sidebar_width(state, available.x);
                ui.allocate_ui_with_layout(
                    egui::vec2(sidebar_width, available.y),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        crate::views::show_sidebar(ui, state, client, Some(store), runtime);
                    },
                );
                render_sidebar_splitter(ui, state, available.y);
            }

            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), available.y),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(ui.available_width());
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
                    ui.add_space(6.0);

                    if selected_session_id.is_none() {
                        render_pre_conversation_main(
                            ui,
                            state,
                            store,
                            selected_agent_did.as_deref(),
                            turn_state,
                            send_status,
                        );
                        return;
                    }

                    let composer_expanded = state.chat.editor.composer_expanded;
                    let composer_height = current_composer_height(state, ui.available_height());
                    let splitter_height = 6.0;
                    let transcript_height =
                        (ui.available_height() - composer_height - splitter_height).max(160.0);

                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), transcript_height),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            transcript::show(
                                ui,
                                state,
                                store,
                                selected_session_id.as_deref(),
                                turn_state,
                                markdown_cache,
                            );
                        },
                    );

                    render_composer_splitter(ui, state);

                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), composer_height),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            state.chat.editor.composer_panel_height =
                                Some(ui.max_rect().height().clamp(
                                    composer_min_height(composer_expanded),
                                    composer_max_height(composer_expanded),
                                ));
                            composer::show(
                                ui,
                                state,
                                store,
                                selected_agent_did.as_deref(),
                                turn_state,
                                send_status,
                            );
                        },
                    );
                },
            );
        },
    );
}

fn default_composer_panel_height(expanded: bool) -> f32 {
    if expanded {
        236.0
    } else {
        156.0
    }
}

fn composer_min_height(expanded: bool) -> f32 {
    if expanded {
        196.0
    } else {
        148.0
    }
}

fn composer_max_height(expanded: bool) -> f32 {
    if expanded {
        380.0
    } else {
        260.0
    }
}

fn current_sidebar_width(state: &ShellState, total_width: f32) -> f32 {
    state
        .chat
        .shell
        .sidebar_width
        .unwrap_or((total_width * 0.18).clamp(224.0, 288.0))
        .clamp(208.0, 360.0)
}

fn current_composer_height(state: &ShellState, available_height: f32) -> f32 {
    let expanded = state.chat.editor.composer_expanded;
    state
        .chat
        .editor
        .composer_panel_height
        .unwrap_or(default_composer_panel_height(expanded))
        .clamp(
            composer_min_height(expanded),
            composer_max_height(expanded)
                .min((available_height - 180.0).max(composer_min_height(expanded))),
        )
}

fn render_pre_conversation_main(
    ui: &mut Ui,
    state: &mut ShellState,
    store: &ClientStore,
    selected_agent_did: Option<&str>,
    turn_state: Option<ClientTurnState>,
    send_status: crate::chat::domain::submission::SendStatus,
) {
    ui.set_width(ui.available_width());
    ui.set_max_width(720.0);
    components::focus_panel(
        ui,
        Some("Chat"),
        "Start the conversation",
        "Send a message below and the first conversation will be created automatically.",
        |_| {},
    );
    ui.add_space(12.0);

    let composer_expanded = state.chat.editor.composer_expanded;
    let composer_height = current_composer_height(state, ui.available_height().max(220.0));
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), composer_height),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            state.chat.editor.composer_panel_height = Some(ui.max_rect().height().clamp(
                composer_min_height(composer_expanded),
                composer_max_height(composer_expanded),
            ));
            composer::show(
                ui,
                state,
                store,
                selected_agent_did,
                turn_state,
                send_status,
            );
        },
    );
}

fn render_sidebar_splitter(ui: &mut Ui, state: &mut ShellState, height: f32) {
    let splitter_width = 8.0;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(splitter_width, height), Sense::click_and_drag());
    paint_splitter(ui, rect, true);

    if response.drag_started() {
        state.chat.shell.sidebar_drag_origin_width = state.chat.shell.sidebar_width;
    }
    if response.dragged() {
        let start = state
            .chat
            .shell
            .sidebar_drag_origin_width
            .unwrap_or_else(|| current_sidebar_width(state, ui.available_width()));
        state.chat.shell.sidebar_width =
            Some((start + response.drag_delta().x).clamp(208.0, 360.0));
    }
    if response.drag_stopped() {
        state.chat.shell.sidebar_drag_origin_width = None;
    }
}

fn render_composer_splitter(ui: &mut Ui, state: &mut ShellState) {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 6.0), Sense::click_and_drag());
    paint_splitter(ui, rect, false);

    if response.drag_started() {
        state.chat.editor.composer_drag_origin_height = state.chat.editor.composer_panel_height;
    }
    if response.dragged() {
        let start =
            state
                .chat
                .editor
                .composer_drag_origin_height
                .unwrap_or(default_composer_panel_height(
                    state.chat.editor.composer_expanded,
                ));
        state.chat.editor.composer_panel_height = Some((start - response.drag_delta().y).clamp(
            composer_min_height(state.chat.editor.composer_expanded),
            composer_max_height(state.chat.editor.composer_expanded),
        ));
    }
    if response.drag_stopped() {
        state.chat.editor.composer_drag_origin_height = None;
    }
}

fn paint_splitter(ui: &Ui, rect: egui::Rect, vertical: bool) {
    let palette = crate::theme::palette();
    let stroke = egui::Stroke::new(1.0, palette.stroke_subtle);
    if vertical {
        let x = rect.center().x;
        ui.painter().line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            stroke,
        );
    } else {
        let y = rect.center().y;
        ui.painter().line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            stroke,
        );
    }
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
        // TODO(ui-polish): distinguish interrupted from failed in display text/icons.
        // For now treat identically — both mean "no complete response."
        Some(ClientTurnState::Failed) | Some(ClientTurnState::Interrupted) => "failed",
        Some(ClientTurnState::Superseded) => "superseded",
        None => "idle",
    }
}

use defra_agent_protocol::client_protocol::ClientTurnState;
