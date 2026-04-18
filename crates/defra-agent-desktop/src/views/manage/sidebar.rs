use eframe::egui::{RichText, Ui};

use crate::client::{ClientCore, ClientStore};
use crate::state::ShellState;
use crate::theme;
use crate::views;

pub(super) fn show_sidebar(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    let Some(store) = store else {
        views::card(
            ui,
            "Manage Unavailable",
            "The desktop client must finish bootstrapping before deployment management can render.",
        );
        return;
    };

    let palette = theme::palette();

    ui.add_space(14.0);
    ui.label(
        RichText::new("Manage")
            .family(crate::theme::stencil_family())
            .size(13.0)
            .color(palette.text_1)
            .strong(),
    );
    ui.add_space(8.0);

    let Some(agent_did) = state.manage.selected_agent_did.as_deref() else {
        views::card(
            ui,
            "Select Deployment",
            "Choose a deployment above. Management tabs and editors will scope themselves to that deployment.",
        );
        return;
    };

    let behavior_count = store.behavior_rows(agent_did).len();
    let backend_count = store.inference_backends.len();
    let profile_count = store.inference_profiles.len();
    let task_count = store
        .scheduled_tasks
        .iter()
        .filter(|row| row.agent_did.as_deref() == Some(agent_did))
        .count();

    views::card(
        ui,
        "Selected Deployment",
        &format!(
            "{}\n\n{} behaviors · {} backends · {} profiles · {} scheduled tasks",
            state
                .manage
                .selected_peer_id
                .as_deref()
                .unwrap_or("unknown deployment"),
            behavior_count,
            backend_count,
            profile_count,
            task_count
        ),
    );

    ui.add_space(10.0);
    views::card(
        ui,
        "Workspace",
        "Use the tabs in the main pane to switch between runtime, behaviors, backends, tools, profiles, tasks, and history.",
    );

    if client.is_none() {
        ui.add_space(10.0);
        views::card(
            ui,
            "Client Offline",
            "Editing is disabled until the local client core finishes bootstrapping.",
        );
    }
}
