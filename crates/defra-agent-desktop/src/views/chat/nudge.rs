use eframe::egui::{self, Ui};

use crate::audit;
use crate::client::ClientCore;
use crate::state::{PendingChatAction, PendingShellAction, ShellState};
use crate::views::components;

pub(super) fn render_first_conversation_nudge(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    selected_agent_did: Option<&str>,
) {
    components::focus_panel(
        ui,
        Some("Chat"),
        "Create Conversation",
        "This agent has no observed conversations yet. Create one explicitly, then send the first request.",
        |ui| {
            let can_create = client.is_some() && selected_agent_did.is_some();
            if audit::add_enabled(
                ui,
                audit::targets::CHAT_CREATE_CONVERSATION,
                can_create,
                egui::Button::new("Create Conversation"),
            )
            .clicked()
            {
                state.queue_shell_action(PendingShellAction::Chat(
                    PendingChatAction::CreateConversation,
                ));
            }
        },
    );
}
