use eframe::egui::{self, RichText, Ui};

use crate::client::ClientStore;
use crate::state::ShellState;
use crate::theme;
use crate::views;

use super::editors::{editor_heading, read_only_field, read_only_multiline};

pub(super) fn show_runtime_summary(ui: &mut Ui, store: &ClientStore, state: &ShellState) {
    let palette = theme::palette();

    if let Some(agent_did) = state.manage.selected_agent_did.as_deref() {
        if let Some(runtime_row) = store.latest_runtime(agent_did) {
            ui.group(|ui| {
                ui.label(
                    RichText::new("runtime")
                        .family(theme::stencil_family())
                        .size(13.0)
                        .color(palette.text_1)
                        .strong(),
                );
                ui.add_space(6.0);
                for row in [
                    (
                        "process",
                        runtime_row.process_state.as_deref().unwrap_or("unknown"),
                    ),
                    (
                        "default behavior",
                        runtime_row
                            .default_behavior_id
                            .as_deref()
                            .unwrap_or("unbound"),
                    ),
                    (
                        "runnable",
                        &runtime_row
                            .runnable_behavior_count
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "0".to_string()),
                    ),
                    (
                        "unavailable",
                        &runtime_row
                            .unavailable_behavior_count
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "0".to_string()),
                    ),
                    (
                        "reconcile result",
                        runtime_row
                            .last_reconcile_result
                            .as_deref()
                            .unwrap_or("pending"),
                    ),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(row.0)
                                .monospace()
                                .size(11.0)
                                .color(palette.text_2),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(row.1)
                                    .monospace()
                                    .size(11.0)
                                    .color(palette.text_0),
                            );
                        });
                    });
                }
            });
        } else {
            views::card(
                ui,
                "Runtime Pending",
                "No AgentRuntime row is replicated yet for the selected agent.",
            );
        }
    } else {
        views::card(
            ui,
            "Select Agent",
            "Choose an agent from the deployment tree to inspect runtime state.",
        );
    }
}

pub(super) fn render_runtime_inspector(ui: &mut Ui, store: &ClientStore, state: &ShellState) {
    if let Some(agent_did) = state.manage.selected_agent_did.as_deref() {
        if let Some(runtime_row) = store.latest_runtime(agent_did) {
            editor_heading(ui, "Runtime Inspector");
            read_only_field(ui, "Agent DID", agent_did);
            read_only_field(
                ui,
                "Process State",
                runtime_row.process_state.as_deref().unwrap_or("unknown"),
            );
            read_only_field(
                ui,
                "Reconcile Phase",
                runtime_row.reconcile_phase.as_deref().unwrap_or("unknown"),
            );
            read_only_field(
                ui,
                "Default Behavior",
                runtime_row
                    .default_behavior_id
                    .as_deref()
                    .unwrap_or("unbound"),
            );
            read_only_field(
                ui,
                "Runnable Behaviors",
                &runtime_row
                    .runnable_behavior_count
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "0".to_string()),
            );
            read_only_field(
                ui,
                "Unavailable Behaviors",
                &runtime_row
                    .unavailable_behavior_count
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "0".to_string()),
            );
            read_only_field(
                ui,
                "Last Result",
                runtime_row
                    .last_reconcile_result
                    .as_deref()
                    .unwrap_or("pending"),
            );
            read_only_multiline(
                ui,
                "Last Error",
                runtime_row.last_reconcile_error.as_deref().unwrap_or(""),
                4,
            );
            read_only_field(
                ui,
                "Completed At",
                runtime_row
                    .last_reconcile_completed_at
                    .as_deref()
                    .unwrap_or("unset"),
            );
            read_only_field(
                ui,
                "Observed Behaviors",
                &store
                    .behaviors
                    .iter()
                    .filter(|row| row.agent_did.as_deref() == Some(agent_did))
                    .count()
                    .to_string(),
            );
            read_only_field(
                ui,
                "Tasks",
                &{
                    let behavior_ids: Vec<&str> = store
                        .behavior_rows(agent_did)
                        .iter()
                        .map(|b| b.behavior_id.as_str())
                        .collect();
                    store
                        .tasks
                        .iter()
                        .filter(|row| {
                            row.behavior_id
                                .as_deref()
                                .is_some_and(|bid| behavior_ids.contains(&bid))
                        })
                        .count()
                        .to_string()
                },
            );
            read_only_field(ui, "Schedules", &store.schedules.len().to_string());
        } else {
            views::card(
                ui,
                "Runtime Pending",
                "The selected agent has no replicated AgentRuntime row yet.",
            );
        }
    } else {
        views::card(
            ui,
            "Select Agent",
            "Choose an agent from the deployment tree to inspect runtime state.",
        );
    }
}
