use eframe::egui::{self, RichText, Ui};
use tokio::runtime::Runtime;

use crate::audit;
use crate::client::{ClientCore, ClientStore};
use crate::state::{
    ManageDraft, ManageSection, PendingManageAction, PendingShellAction, ShellState,
};
use crate::theme;
use crate::views;

use super::behavior_context::render_behavior_context;
use super::recent_failures::render_recent_failure_detail;
use super::request_timeline::render_request_timeline_detail;
use super::runtime::render_runtime_inspector;

pub(super) fn show_rail(
    ui: &mut Ui,
    state: &mut ShellState,
    _client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    _runtime: &Runtime,
) {
    let palette = theme::palette();

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(14.0);
        let (rail_title, rail_meta) = match state.manage.selected_section {
            ManageSection::RequestTimeline
            | ManageSection::RecentFailures => ("Inspector", Some("read only")),
            _ => ("Diagnostics", Some("scoped context")),
        };
        views::sidebar_heading(ui, rail_title, rail_meta);
        ui.add_space(10.0);

        let Some(store) = store else {
            views::card(
                ui,
                "Editor Offline",
                "The editor becomes available once the local replica is online.",
            );
            return;
        };

        match state.manage.selected_section {
            ManageSection::Behaviors => {
                if let Some(ManageDraft::Behavior(draft)) = state.manage.draft.as_mut() {
                    let behavior_id = draft.behavior_id.clone();
                    if let Some(agent_did) = state.manage.selected_agent_did.as_deref() {
                        render_behavior_context(ui, store, agent_did, behavior_id.as_str());
                    } else {
                        views::card(
                            ui,
                            "Behavior Diagnostics",
                            "Select a deployment to inspect recent activity for this behavior.",
                        );
                    }
                } else {
                    views::card(
                        ui,
                        "Behavior Diagnostics",
                        "Select a behavior to inspect recent activity and related documents.",
                    );
                }
            }
            ManageSection::Backends => {
                if state.manage.draft.is_some() {
                    render_runtime_inspector(ui, store, state);
                } else {
                    views::card(
                        ui,
                        "Diagnostics",
                        "Select a backend to inspect the current runtime state for the selected deployment.",
                    );
                }
            }
            ManageSection::ToolSelections => {
                if state.manage.draft.is_some() {
                    render_runtime_inspector(ui, store, state);
                } else {
                    views::card(
                        ui,
                        "Diagnostics",
                        "Select a tool selection to inspect the current runtime state for the selected deployment.",
                    );
                }
            }
            ManageSection::InferenceProfiles => {
                if state.manage.draft.is_some() {
                    render_runtime_inspector(ui, store, state);
                } else {
                    views::card(
                        ui,
                        "Diagnostics",
                        "Select a profile to inspect the current runtime state for the selected deployment.",
                    );
                }
            }
            ManageSection::ScheduledTasks => {
                if state.manage.draft.is_some() {
                    render_runtime_inspector(ui, store, state);
                } else {
                    views::card(
                        ui,
                        "Diagnostics",
                        "Select a scheduled task to inspect the current runtime state for the selected deployment.",
                    );
                }
            }
            ManageSection::RequestTimeline => {
                render_request_timeline_detail(ui, state, store);
            }
            ManageSection::RecentFailures => {
                render_recent_failure_detail(ui, state, store);
            }
        }

        if let Some(error) = state.manage.last_apply_error.as_deref() {
            ui.add_space(10.0);
            ui.label(
                RichText::new(error)
                    .monospace()
                    .size(10.5)
                    .color(palette.warning),
            );
        }
    });
}

pub(super) fn render_editor_footer(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
) {
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        let can_run_now = client.is_some()
            && matches!(
                state.manage.draft,
                Some(ManageDraft::ScheduledTask(ref draft)) if draft.enabled
            );

        if audit::button(ui, audit::targets::MANAGE_DISCARD, "Discard").clicked() {
            state.queue_shell_action(PendingShellAction::Manage(
                PendingManageAction::DiscardDraft,
            ));
        }

        let can_apply = client.is_some() && state.manage.draft.is_some();
        if audit::add_enabled(
            ui,
            audit::targets::MANAGE_APPLY,
            can_apply,
            egui::Button::new("Apply"),
        )
        .clicked()
        {
            state.queue_shell_action(PendingShellAction::Manage(PendingManageAction::ApplyDraft));
        }

        if matches!(state.manage.selected_section, ManageSection::ScheduledTasks)
            && audit::add_enabled(
                ui,
                audit::targets::MANAGE_RUN_NOW,
                can_run_now,
                egui::Button::new("Run Now"),
            )
            .clicked()
        {
            state.queue_shell_action(PendingShellAction::Manage(
                PendingManageAction::RunNowSelectedTask,
            ));
        }
    });
}
