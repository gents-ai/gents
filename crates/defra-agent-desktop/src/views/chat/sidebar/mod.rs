mod conversations;
mod deployments;

use eframe::egui::{self, RichText, Ui};

use crate::audit;
use crate::client::{ClientCore, ClientStore};
use crate::state::{PendingChatAction, PendingShellAction, ShellState};
use crate::theme::Palette;

use super::{ConversationBucket, DeploymentEntry};

pub fn show(
    ui: &mut Ui,
    palette: Palette,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: &ClientStore,
    deployments: &[DeploymentEntry],
    conversations: &[ConversationBucket],
    selected_agent_did: Option<&str>,
    selected_session_id: Option<&str>,
) {
    let _ = store;
    egui::ScrollArea::vertical().show(ui, |ui| {
        show_deployments_header(ui, palette, state);
        ui.add_space(8.0);

        if deployments.is_empty() {
            deployments::render_empty(ui, state);
        } else {
            deployments::render_list(ui, palette, state, deployments, selected_agent_did);
        }

        ui.separator();
        ui.add_space(8.0);
        show_conversations_header(ui, palette, state, client, selected_agent_did);
        ui.add_space(8.0);

        if selected_agent_did.is_none() {
            conversations::render_select_agent(ui);
            return;
        }

        if conversations.is_empty() {
            conversations::render_empty(ui);
            return;
        }

        conversations::render_buckets(ui, palette, state, conversations, selected_session_id);
    });
}

fn show_deployments_header(ui: &mut Ui, palette: Palette, state: &mut ShellState) {
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.label(
            RichText::new("Deployments")
                .family(crate::theme::stencil_family())
                .size(13.0)
                .color(palette.text_1)
                .strong(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(14.0);
            let response = audit::add(
                ui,
                audit::targets::CHAT_OPEN_PEERS_SETUP,
                egui::Button::new(
                    RichText::new("+ peer")
                        .monospace()
                        .size(10.5)
                        .color(palette.accent),
                )
                .min_size(egui::vec2(52.0, 20.0)),
            );
            if response.clicked() {
                state.queue_shell_action(PendingShellAction::OpenPeersSetup);
            }
        });
    });
}

fn show_conversations_header(
    ui: &mut Ui,
    palette: Palette,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    selected_agent_did: Option<&str>,
) {
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.label(
            RichText::new("Conversations")
                .family(crate::theme::stencil_family())
                .size(13.0)
                .color(palette.text_1)
                .strong(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(14.0);
            let enabled = client.is_some() && selected_agent_did.is_some();
            let response = audit::add_enabled(
                ui,
                audit::targets::CHAT_NEW_CONVERSATION,
                enabled,
                egui::Button::new(RichText::new("+ new").monospace().size(10.5).color(
                    if enabled {
                        palette.accent
                    } else {
                        palette.text_3
                    },
                ))
                .min_size(egui::vec2(52.0, 20.0)),
            );
            if response.clicked() {
                state.queue_shell_action(PendingShellAction::Chat(
                    PendingChatAction::CreateConversation,
                ));
            }
        });
    });
}
