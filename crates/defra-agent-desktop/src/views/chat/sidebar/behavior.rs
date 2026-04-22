use eframe::egui::Ui;

use crate::audit;
use crate::client::ClientStore;
use crate::state::{Activity, PendingChatAction, PendingShellAction, ShellState};
use crate::theme::Palette;
use crate::views::components;

use super::super::view_model::{behavior_selection_entries, effective_behavior_id};

pub(super) fn show(
    ui: &mut Ui,
    palette: Palette,
    state: &mut ShellState,
    store: &ClientStore,
    selected_agent_did: Option<&str>,
    selected_session_id: Option<&str>,
) {
    let Some(agent_did) = selected_agent_did else {
        components::focus_panel(
            ui,
            Some("Behavior"),
            "Select a Deployment",
            "Choose a deployment from the left before picking a behavior override.",
            |_| {},
        );
        return;
    };

    let entries = behavior_selection_entries(store, agent_did);
    if entries.is_empty() {
        components::focus_panel(
            ui,
            Some("Behavior"),
            "No Behaviors",
            "This deployment has no named behaviors yet. Manage the deployment before starting a conversation.",
            |_| {},
        );
        return;
    }

    let selected_behavior_id = effective_behavior_id(state, store, selected_agent_did);
    for entry in entries {
        let selected = selected_behavior_id.as_deref() == entry.represented_behavior_id.as_deref();
        let response = components::inset_list_item(
            ui,
            &entry.label,
            &entry.meta,
            selected,
            if selected {
                palette.accent
            } else {
                palette.text_3
            },
            None,
        );
        let target = behavior_target(entry.represented_behavior_id.as_deref());
        audit::record(ui, &target, &response);
        if response.clicked() {
            if state.activity != Activity::Chat {
                state.queue_shell_action(PendingShellAction::Navigate(Activity::Chat));
            }
            if selected_session_id.is_some() && !selected {
                state.queue_shell_action(PendingShellAction::Chat(
                    PendingChatAction::StartNewConversationDraft,
                ));
            }
            state.queue_shell_action(PendingShellAction::Chat(
                PendingChatAction::SelectBehavior {
                    behavior_id: entry.override_behavior_id.clone(),
                },
            ));
        }
        ui.add_space(6.0);
    }
}

fn behavior_target(behavior_id: Option<&str>) -> String {
    behavior_id
        .map(audit::targets::chat_behavior_option)
        .unwrap_or_else(|| "chat.behavior.option.default".to_string())
}
