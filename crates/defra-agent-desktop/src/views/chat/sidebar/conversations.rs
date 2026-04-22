use eframe::egui::Ui;

use crate::audit;
use crate::state::{Activity, PendingChatAction, PendingShellAction, ShellState};
use crate::theme::Palette;
use crate::views;
use crate::views::components;

use super::ConversationBucket;

pub(super) fn render_select_agent(ui: &mut Ui) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.vertical(|ui| {
            ui.set_width(ui.available_width());
            components::focus_panel(
                ui,
                Some("Chat"),
                "Select a Deployment",
                "Choose a deployment from the left to load its conversations and start chatting.",
                |_| {},
            );
        });
    });
}

pub(super) fn render_empty(ui: &mut Ui) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.vertical(|ui| {
            ui.set_width(ui.available_width());
            components::focus_panel(
                ui,
                Some("Chat"),
                "No Conversations Yet",
                "Send the first message and the conversation will appear here automatically.",
                |_| {},
            );
        });
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
                    let response = components::inset_list_item(
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
                        if state.activity != Activity::Chat {
                            state.queue_shell_action(PendingShellAction::Navigate(Activity::Chat));
                        }
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
