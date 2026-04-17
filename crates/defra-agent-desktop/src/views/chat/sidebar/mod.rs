mod behavior;
mod conversations;

use eframe::egui::{self, RichText, Ui};

use crate::audit;
use crate::client::{ClientCore, ClientStore};
use crate::state::{Activity, PendingChatAction, PendingShellAction, ShellState};
use crate::theme::Palette;

use super::ConversationBucket;

pub fn show(
    ui: &mut Ui,
    palette: Palette,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: &ClientStore,
    conversations: &[ConversationBucket],
    selected_agent_did: Option<&str>,
    selected_session_id: Option<&str>,
) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        show_behaviors_header(ui, palette);
        ui.add_space(8.0);
        behavior::show(
            ui,
            palette,
            state,
            store,
            selected_agent_did,
            selected_session_id,
        );

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

fn show_behaviors_header(ui: &mut Ui, palette: Palette) {
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.label(
            RichText::new("Behaviors")
                .family(crate::theme::stencil_family())
                .size(13.0)
                .color(palette.text_1)
                .strong(),
        );
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
                if state.activity != Activity::Chat {
                    state.queue_shell_action(PendingShellAction::Navigate(Activity::Chat));
                }
                state.queue_shell_action(PendingShellAction::Chat(
                    PendingChatAction::StartNewConversationDraft,
                ));
            }
        });
    });
}
