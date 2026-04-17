use eframe::egui::{self, RichText, Ui};

use crate::audit;
use crate::client::ClientCore;
use crate::state::{PendingChatAction, PendingShellAction, ShellState};
use crate::theme;

pub(super) fn render_first_conversation_nudge(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    selected_agent_did: Option<&str>,
) {
    let palette = theme::palette();

    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new("Create Conversation")
                .family(theme::stencil_family())
                .size(16.0)
                .color(palette.text_0)
                .strong(),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "Create a conversation explicitly when this agent has no observed sessions yet. This avoids hiding snapshot lag behind automatic local state repair.",
            )
            .size(13.0)
            .color(palette.text_1)
            .line_height(Some(18.0)),
        );
        ui.add_space(10.0);
        ui.horizontal(|ui| {
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

            if let Some(agent_did) = selected_agent_did {
                ui.label(
                    RichText::new(format!("target {agent_did}"))
                        .monospace()
                        .size(11.0)
                        .color(palette.text_2),
                );
            }
        });
    });
}
