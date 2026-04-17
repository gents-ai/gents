use eframe::egui::Ui;

use crate::audit;
use crate::state::{PendingChatAction, PendingShellAction, ShellState};
use crate::theme::Palette;
use crate::views;

use super::ConversationBucket;

pub(super) fn render_select_agent(ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        views::card(
            ui,
            "Select Agent",
            "Choose a deployment to load conversations.",
        );
    });
}

pub(super) fn render_empty(ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        views::card(
            ui,
            "No Conversations",
            "This agent has no conversations yet. Create the first conversation from the main panel before sending a request.",
        );
    });
}

pub(super) fn render_buckets(
    ui: &mut Ui,
    palette: Palette,
    state: &mut ShellState,
    conversations: &[ConversationBucket],
    selected_session_id: Option<&str>,
) {
    for bucket in conversations {
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.vertical(|ui| {
                views::section_kicker(ui, bucket.label);
                for entry in &bucket.entries {
                    let meta = if entry.meta.is_empty() {
                        entry.timestamp_label.clone()
                    } else {
                        format!("{}  {}", entry.meta, entry.timestamp_label)
                    };
                    let selected = selected_session_id == Some(entry.session_id.as_str());
                    let response = views::side_row(
                        ui,
                        &entry.title,
                        &meta,
                        selected,
                        if selected {
                            palette.accent
                        } else {
                            palette.text_3
                        },
                        None,
                    );
                    audit::record(
                        ui,
                        &audit::targets::chat_conversation(&entry.session_id),
                        &response,
                    );
                    if response.clicked() {
                        state.queue_shell_action(PendingShellAction::Chat(
                            PendingChatAction::SelectConversation {
                                session_id: entry.session_id.clone(),
                            },
                        ));
                    }
                }
                ui.add_space(6.0);
            });
        });
    }
}
