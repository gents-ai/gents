use eframe::egui::{self, RichText, Ui};
use tokio::runtime::Runtime;

use crate::audit;
use crate::client::{ClientCore, ClientStore};
use crate::state::{
    OperatorDraft, OperatorSection, PendingOperatorAction, PendingShellAction, ShellState,
};
use crate::theme;
use crate::views;

use super::behavior_context::render_behavior_context;
use super::editors::{
    render_backend_editor, render_behavior_editor, render_inference_profile_editor,
    render_scheduled_task_editor, render_tool_selection_editor,
};
use super::recent_failures::render_recent_failure_detail;
use super::request_timeline::render_request_timeline_detail;
use super::runtime::render_runtime_inspector;

pub(super) fn show_rail(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    _runtime: &Runtime,
) {
    let palette = theme::palette();

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(14.0);
        let (rail_title, rail_meta) = match state.operator.selected_section {
            OperatorSection::Runtime
            | OperatorSection::RequestTimeline
            | OperatorSection::RecentFailures => ("Inspector", Some("read only")),
            _ => ("Editor", Some("apply / discard")),
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

        match state.operator.selected_section {
            OperatorSection::Runtime => render_runtime_inspector(ui, store, state),
            OperatorSection::Behaviors => {
                if let Some(OperatorDraft::Behavior(draft)) = state.operator.draft.as_mut() {
                    let behavior_id = draft.behavior_id.clone();
                    render_behavior_editor(ui, draft);
                    render_editor_footer(ui, state, client);
                    if let Some(agent_did) = state.operator.selected_agent_did.as_deref() {
                        render_behavior_context(ui, store, agent_did, behavior_id.as_str());
                    }
                } else {
                    views::card(
                        ui,
                        "Behavior Editor",
                        "Select a behavior from the entity list to edit it.",
                    );
                }
            }
            OperatorSection::Backends => {
                if let Some(OperatorDraft::Backend(draft)) = state.operator.draft.as_mut() {
                    render_backend_editor(ui, draft);
                    render_editor_footer(ui, state, client);
                } else {
                    views::card(
                        ui,
                        "Backend Editor",
                        "Select a backend from the entity list to edit it.",
                    );
                }
            }
            OperatorSection::ToolSelections => {
                if let Some(OperatorDraft::ToolSelection(draft)) = state.operator.draft.as_mut() {
                    render_tool_selection_editor(ui, draft);
                    render_editor_footer(ui, state, client);
                } else {
                    views::card(
                        ui,
                        "Tool Selection Editor",
                        "Select a tool selection from the entity list to edit it.",
                    );
                }
            }
            OperatorSection::InferenceProfiles => {
                if let Some(OperatorDraft::InferenceProfile(draft)) = state.operator.draft.as_mut()
                {
                    render_inference_profile_editor(ui, draft);
                    render_editor_footer(ui, state, client);
                } else {
                    views::card(
                        ui,
                        "Inference Profile Editor",
                        "Select a profile from the entity list to edit it.",
                    );
                }
            }
            OperatorSection::ScheduledTasks => {
                if let Some(OperatorDraft::ScheduledTask(draft)) = state.operator.draft.as_mut() {
                    render_scheduled_task_editor(ui, draft);
                    render_editor_footer(ui, state, client);
                } else {
                    views::card(
                        ui,
                        "Scheduled Task Editor",
                        "Select a scheduled task from the entity list to edit it.",
                    );
                }
            }
            OperatorSection::RequestTimeline => {
                render_request_timeline_detail(ui, state, store);
            }
            OperatorSection::RecentFailures => {
                render_recent_failure_detail(ui, state, store);
            }
        }

        if let Some(error) = state.operator.last_apply_error.as_deref() {
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

fn render_editor_footer(ui: &mut Ui, state: &mut ShellState, client: Option<&ClientCore>) {
    let palette = theme::palette();

    ui.add_space(12.0);
    ui.horizontal(|ui| {
        let can_run_now = client.is_some()
            && matches!(
                state.operator.draft,
                Some(OperatorDraft::ScheduledTask(ref draft)) if draft.enabled
            );

        if audit::button(ui, audit::targets::OPERATOR_DISCARD, "Discard").clicked() {
            state.queue_shell_action(PendingShellAction::Operator(
                PendingOperatorAction::DiscardDraft,
            ));
        }

        let can_apply = client.is_some() && state.operator.draft.is_some();
        if audit::add_enabled(
            ui,
            audit::targets::OPERATOR_APPLY,
            can_apply,
            egui::Button::new("Apply"),
        )
        .clicked()
        {
            state.queue_shell_action(PendingShellAction::Operator(
                PendingOperatorAction::ApplyDraft,
            ));
        }

        if matches!(
            state.operator.selected_section,
            OperatorSection::ScheduledTasks
        ) && audit::add_enabled(
            ui,
            audit::targets::OPERATOR_RUN_NOW,
            can_run_now,
            egui::Button::new("Run Now"),
        )
        .clicked()
        {
            state.queue_shell_action(PendingShellAction::Operator(
                PendingOperatorAction::RunNowSelectedTask,
            ));
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new("1:1 document editor")
                    .monospace()
                    .size(10.5)
                    .color(palette.text_3),
            );
        });
    });
}
