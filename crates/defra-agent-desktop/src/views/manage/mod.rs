mod behavior_context;
pub(crate) mod editors;
mod entity_list;
mod prepare;
mod rail;
mod recent_failures;
mod request_timeline;
mod runtime;
mod shared;

use eframe::egui::{self, RichText, Ui};
use tokio::runtime::Runtime;

use crate::client::{ClientCore, ClientStore};
use crate::manage::{build_deployment_entries, entity_summaries, EntitySummary};
use crate::state::{
    Activity, FireTaskDraft, ManageDraft, ManageSection, PendingManageAction, PendingShellAction,
    ShellState,
};
use crate::theme;
use crate::views;
use editors::{
    render_backend_editor, render_behavior_editor, render_event_trigger_editor,
    render_inference_profile_editor, render_schedule_editor, render_task_editor,
    render_tool_selection_editor,
};

pub fn prepare_state(
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    prepare::prepare_state(state, client, store);
}

pub fn show_sidebar(
    _ui: &mut Ui,
    _state: &mut ShellState,
    _client: Option<&ClientCore>,
    _store: Option<&ClientStore>,
) {
}

pub fn show_main(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    let Some(store) = store else {
        views::card(
            ui,
            "Manage Unavailable",
            "No local replica is available for deployment management.",
        );
        return;
    };

    let section = state.manage.selected_section;
    let entries = entity_summaries(store, section, state.manage.selected_agent_did.as_deref());
    let breadcrumb = manage_toolbar_breadcrumb(state, client, store);

    ui.vertical(|ui| {
        views::toolbar(ui, "Manage Deployment", &breadcrumb, section.label());
        ui.add_space(8.0);
        render_deployment_context(ui, state, client, store);
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button("Back to Chat").clicked() {
                state.queue_shell_action(PendingShellAction::Navigate(Activity::Chat));
            }
        });
        ui.add_space(12.0);
        render_section_tabs(ui, state);
        ui.add_space(12.0);
        match section {
            ManageSection::Behaviors
            | ManageSection::Backends
            | ManageSection::ToolSelections
            | ManageSection::InferenceProfiles
            | ManageSection::Tasks
            | ManageSection::Schedules
            | ManageSection::EventTriggers => {
                render_management_workspace(ui, state, client, store, section, entries);
            }
            ManageSection::RequestTimeline | ManageSection::RecentFailures => {
                entity_list::show_document_section(ui, state, store, section, entries);
            }
        }
    });

    // Floating manual-run modal. Rendered after the workspace so its
    // `egui::Window` paints on top regardless of which section is
    // currently displayed; the controller clears `fire_task_draft` when
    // the operator navigates away so the modal stays scoped to the Task
    // editor.
    render_fire_task_modal(ui, state);
}

pub fn show_rail(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    runtime: &Runtime,
) {
    rail::show_rail(ui, state, client, store, runtime);
}

fn render_section_tabs(ui: &mut Ui, state: &mut ShellState) {
    let selected = state.manage.selected_section;
    egui::ScrollArea::horizontal().show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            for section in ManageSection::MANAGE
                .into_iter()
                .chain(ManageSection::INSPECT)
            {
                let is_selected = section == selected;
                let button = egui::Button::new(RichText::new(section.label()).size(12.5).color(
                    if is_selected {
                        theme::palette().text_0
                    } else {
                        theme::palette().text_2
                    },
                ))
                .fill(if is_selected {
                    theme::palette().background_2
                } else {
                    theme::palette().background_0
                })
                .stroke(egui::Stroke::new(
                    1.0,
                    if is_selected {
                        theme::palette().accent_dim
                    } else {
                        theme::palette().stroke_subtle
                    },
                ));
                if ui.add(button).clicked() {
                    state.queue_shell_action(PendingShellAction::Manage(
                        PendingManageAction::SelectSection { section },
                    ));
                }
            }
        });
    });
}

fn render_management_workspace(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: &ClientStore,
    section: ManageSection,
    entries: Vec<EntitySummary>,
) {
    let palette = theme::palette();
    if entries.is_empty() && !section.supports_new_documents() {
        views::card(
            ui,
            "No Documents",
            "No documents are currently replicated for this section.",
        );
        return;
    }

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Documents")
                .monospace()
                .size(10.5)
                .color(palette.text_2),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            entity_list::show_new_button(ui, state, section);
        });
    });
    ui.add_space(10.0);

    ui.columns(2, |columns| {
        columns[0].set_min_width(260.0);
        egui::ScrollArea::vertical().show(&mut columns[0], |ui| {
            entity_list::entity_list_contents(ui, state, &entries);
        });

        columns[1].group(|ui| {
            ui.set_width(ui.available_width());
            render_editor_workspace(ui, state, client, store, section);
        });
    });
}

fn render_editor_workspace(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: &ClientStore,
    section: ManageSection,
) {
    match section {
        ManageSection::Behaviors => {
            if let Some(ManageDraft::Behavior(draft)) = state.manage.draft.as_mut() {
                scroll_editor_body(ui, |ui| render_behavior_editor(ui, draft));
                rail::render_editor_footer(ui, state, client);
            } else {
                views::card(
                    ui,
                    "Behavior Editor",
                    "Select a behavior from the list or create a new one to edit it here.",
                );
            }
        }
        ManageSection::Backends => {
            if let Some(ManageDraft::Backend(draft)) = state.manage.draft.as_mut() {
                scroll_editor_body(ui, |ui| render_backend_editor(ui, draft));
                rail::render_editor_footer(ui, state, client);
            } else {
                views::card(
                    ui,
                    "Backend Editor",
                    "Select a backend from the list or create a new one to edit it here.",
                );
            }
        }
        ManageSection::ToolSelections => {
            if let Some(ManageDraft::ToolSelection(draft)) = state.manage.draft.as_mut() {
                scroll_editor_body(ui, |ui| render_tool_selection_editor(ui, draft));
                rail::render_editor_footer(ui, state, client);
            } else {
                views::card(
                    ui,
                    "Tool Selection Editor",
                    "Select a tool selection from the list or create a new one to edit it here.",
                );
            }
        }
        ManageSection::InferenceProfiles => {
            if let Some(ManageDraft::InferenceProfile(draft)) = state.manage.draft.as_mut() {
                scroll_editor_body(ui, |ui| render_inference_profile_editor(ui, draft));
                rail::render_editor_footer(ui, state, client);
            } else {
                views::card(
                    ui,
                    "Inference Profile Editor",
                    "Select a profile from the list or create a new one to edit it here.",
                );
            }
        }
        ManageSection::Tasks => {
            if let Some(ManageDraft::Task(draft)) = state.manage.draft.as_mut() {
                // Capture the task identifier before the scroll body
                // closure takes the mutable borrow of `draft`, so the
                // "Run Now" button below can reference it without
                // re-borrowing `state.manage.draft`.
                let task_id = draft.task_id.clone();
                let task_enabled = draft.enabled;
                scroll_editor_body(ui, |ui| render_task_editor(ui, draft, store));
                render_task_run_now_row(ui, state, client, store, &task_id, task_enabled);
                rail::render_editor_footer(ui, state, client);
            } else {
                views::card(
                    ui,
                    "Task Editor",
                    "Select a task from the list or create a new one to edit it here.",
                );
            }
        }
        ManageSection::Schedules => {
            if let Some(ManageDraft::Schedule(draft)) = state.manage.draft.as_mut() {
                scroll_editor_body(ui, |ui| render_schedule_editor(ui, draft));
                rail::render_editor_footer(ui, state, client);
            } else {
                views::card(
                    ui,
                    "Schedule Editor",
                    "Select a schedule from the list or create a new one to edit it here.",
                );
            }
        }
        ManageSection::EventTriggers => {
            if let Some(ManageDraft::EventTrigger(draft)) = state.manage.draft.as_mut() {
                scroll_editor_body(ui, |ui| render_event_trigger_editor(ui, draft));
                rail::render_editor_footer(ui, state, client);
            } else {
                views::card(
                    ui,
                    "Event Trigger Editor",
                    "Select an event trigger from the list or create a new one to edit it here.",
                );
            }
        }
        _ => {
            let _ = store;
        }
    }
}

fn manage_toolbar_breadcrumb(
    state: &ShellState,
    client: Option<&ClientCore>,
    store: &ClientStore,
) -> String {
    let Some(agent_did) = state.manage.selected_agent_did.as_deref() else {
        return "Select a deployment".to_string();
    };
    let selected = state
        .manage
        .selected_peer_id
        .as_deref()
        .zip(Some(agent_did));
    let deployment = client
        .map(ClientCore::peer_statuses)
        .map(|peer_statuses| build_deployment_entries(&peer_statuses, store))
        .and_then(|entries| {
            entries.into_iter().find(|entry| {
                selected.is_some_and(|(peer_id, agent_did)| {
                    entry.peer_id == peer_id && entry.agent_did == agent_did
                })
            })
        });

    deployment
        .map(|entry| format!("{} · {}", entry.label, entry.agent_label))
        .unwrap_or_else(|| agent_did.to_string())
}

fn render_deployment_context(
    ui: &mut Ui,
    state: &ShellState,
    client: Option<&ClientCore>,
    store: &ClientStore,
) {
    let Some(agent_did) = state.manage.selected_agent_did.as_deref() else {
        views::card(
            ui,
            "Select Deployment",
            "Choose a deployment from the sidebar to edit its behaviors and related documents.",
        );
        return;
    };

    let deployment = client
        .map(ClientCore::peer_statuses)
        .map(|peer_statuses| build_deployment_entries(&peer_statuses, store))
        .and_then(|entries| {
            entries.into_iter().find(|entry| {
                state.manage.selected_peer_id.as_deref() == Some(entry.peer_id.as_str())
                    && entry.agent_did == agent_did
            })
        });
    let runtime_row = store.latest_runtime(agent_did);
    let palette = theme::palette();

    ui.group(|ui| {
        ui.horizontal_wrapped(|ui| {
            let title = deployment
                .as_ref()
                .map(|entry| entry.label.as_str())
                .unwrap_or("Selected Deployment");
            ui.label(
                RichText::new(title)
                    .family(theme::stencil_family())
                    .size(13.0)
                    .color(palette.text_1)
                    .strong(),
            );
            ui.label(
                RichText::new(agent_did)
                    .monospace()
                    .size(10.5)
                    .color(palette.text_2),
            );
        });
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if let Some(deployment) = deployment.as_ref() {
                let connection = if deployment.peer_id.starts_with("local:") {
                    "local"
                } else if deployment.connected {
                    "online"
                } else {
                    "saved"
                };
                ui.label(
                    RichText::new(format!("status {connection}"))
                        .monospace()
                        .size(10.5)
                        .color(palette.text_2),
                );
            }
            if let Some(runtime_row) = runtime_row {
                ui.label(
                    RichText::new(format!(
                        "process {}",
                        runtime_row.process_state.as_deref().unwrap_or("unknown")
                    ))
                    .monospace()
                    .size(10.5)
                    .color(palette.text_2),
                );
                ui.label(
                    RichText::new(format!(
                        "default behavior {}",
                        runtime_row
                            .default_behavior_id
                            .as_deref()
                            .unwrap_or("unbound")
                    ))
                    .monospace()
                    .size(10.5)
                    .color(palette.text_2),
                );
            }
            ui.label(
                RichText::new(format!(
                    "behaviors {}",
                    store.behavior_rows(agent_did).len()
                ))
                .monospace()
                .size(10.5)
                .color(palette.text_2),
            );
            let agent_behavior_ids: std::collections::HashSet<String> = store
                .behavior_rows(agent_did)
                .iter()
                .map(|row| row.behavior_id.clone())
                .collect();
            let agent_task_ids: Vec<&str> = store
                .tasks
                .iter()
                .filter(|row| {
                    row.behavior_id
                        .as_deref()
                        .is_some_and(|bid| agent_behavior_ids.contains(bid))
                })
                .map(|row| row.task_id.as_str())
                .collect();
            ui.label(
                RichText::new(format!("tasks {}", agent_task_ids.len()))
                    .monospace()
                    .size(10.5)
                    .color(palette.text_2),
            );
            ui.label(
                RichText::new(format!(
                    "triggers {}",
                    store.event_triggers_for_tasks(&agent_task_ids).len()
                ))
                .monospace()
                .size(10.5)
                .color(palette.text_2),
            );
        });
    });
}

fn scroll_editor_body(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    let max_height = (ui.available_height() - 56.0).max(180.0);
    egui::ScrollArea::vertical()
        .max_height(max_height)
        .show(ui, |ui| add_contents(ui));
}

/// Render the "Run Now" row below the Task editor.
///
/// The button opens the manual-run args modal. It is disabled when the
/// client is offline, the task is disabled, or the draft does not yet
/// correspond to a persisted `Task` row in the store. Requiring a
/// persisted row matches what the controller's submit path looks up
/// (`manage::controller::submit_fire_task_draft` resolves against
/// `client.store().snapshot().tasks`) — without this guard, an unsaved
/// task (or an existing task whose `task_id` has been edited but not
/// saved) would offer a button that then fails with
/// `task ... disappeared from store`.
fn render_task_run_now_row(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: &ClientStore,
    task_id: &str,
    task_enabled: bool,
) {
    ui.separator();
    ui.horizontal(|ui| {
        let persisted_row_exists = task_row_is_persisted(store, task_id);
        let can_fire = task_run_now_enabled(
            client.is_some(),
            task_enabled,
            task_id,
            persisted_row_exists,
        );
        let response = ui.add_enabled(can_fire, egui::Button::new("Run Now"));
        if response.clicked() {
            state.manage.fire_task_draft = Some(FireTaskDraft::new(task_id.to_string()));
        }
        if !task_enabled {
            ui.label(
                RichText::new("task is disabled")
                    .monospace()
                    .size(10.5)
                    .color(theme::palette().text_2),
            );
        } else if !persisted_row_exists {
            // Distinguishing this from "task disabled" matters: the
            // operator's fix is different (save vs. enable).
            ui.label(
                RichText::new("save the task first")
                    .monospace()
                    .size(10.5)
                    .color(theme::palette().text_2),
            );
        }
    });
}

/// True when the Task draft corresponds to a persisted `Task` row in the
/// store. The controller's submit path resolves against
/// `client.store().snapshot().tasks`, so a draft whose `task_id` does not
/// appear there would submit and immediately fail with
/// `task ... disappeared from store`.
fn task_row_is_persisted(store: &ClientStore, task_id: &str) -> bool {
    let trimmed = task_id.trim();
    !trimmed.is_empty() && store.tasks.iter().any(|row| row.task_id == trimmed)
}

/// Pure enablement predicate for the "Run Now" button. Mirrors the
/// guard the controller enforces at submit time so the button is never
/// offered for a state the controller would refuse.
fn task_run_now_enabled(
    client_online: bool,
    task_enabled: bool,
    task_id: &str,
    persisted_row_exists: bool,
) -> bool {
    client_online && task_enabled && !task_id.trim().is_empty() && persisted_row_exists
}

/// Render the manual-run args modal when `fire_task_draft` is set.
///
/// The modal is a floating `egui::Window` with a multi-line JSON
/// editor, a Submit button that parses the JSON and queues the
/// controller submit, and a Cancel button that closes the modal. The
/// last submit error is displayed inline so the operator can correct
/// the input without losing their draft.
fn render_fire_task_modal(ui: &mut Ui, state: &mut ShellState) {
    if state.manage.fire_task_draft.is_none() {
        return;
    }

    // Capture the task_id for the title without holding a borrow on
    // the draft across the window body.
    let title = {
        let draft = state.manage.fire_task_draft.as_ref().unwrap();
        format!("Run Task: {}", draft.task_id)
    };

    let mut close_requested = false;
    let mut submit_requested = false;

    egui::Window::new(title)
        .collapsible(false)
        .resizable(true)
        .default_width(420.0)
        .show(ui.ctx(), |ui| {
            let draft = state.manage.fire_task_draft.as_mut().expect(
                "fire_task_modal invoked with a present draft; guard checked above",
            );
            ui.label("Args (JSON object):");
            ui.add(
                egui::TextEdit::multiline(&mut draft.args_text)
                    .desired_rows(6)
                    .code_editor()
                    .desired_width(f32::INFINITY),
            );
            if let Some(err) = draft.error.as_deref() {
                ui.add_space(4.0);
                ui.colored_label(theme::palette().warning, err);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Submit").clicked() {
                    submit_requested = true;
                }
                if ui.button("Cancel").clicked() {
                    close_requested = true;
                }
            });
        });

    if submit_requested {
        state.queue_shell_action(PendingShellAction::Manage(
            PendingManageAction::SubmitFireTaskDraft,
        ));
    }
    if close_requested {
        state.manage.fire_task_draft = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::client::ClientStoreRows;
    use defra_agent_protocol::row::TaskRow;

    fn store_with_tasks(task_ids: &[&str]) -> ClientStore {
        ClientStore::from_rows(ClientStoreRows {
            tasks: task_ids
                .iter()
                .map(|id| TaskRow {
                    task_id: (*id).to_string(),
                    name: None,
                    description: None,
                    behavior_id: None,
                    prompt_template: None,
                    enabled: Some(true),
                    output_schema_ref: None,
                    created_at: None,
                    updated_at: None,
                })
                .collect(),
            ..ClientStoreRows::default()
        })
    }

    #[test]
    fn task_row_is_persisted_true_for_exact_match() {
        let store = store_with_tasks(&["task-a", "task-b"]);
        assert!(task_row_is_persisted(&store, "task-a"));
        assert!(task_row_is_persisted(&store, "task-b"));
    }

    #[test]
    fn task_row_is_persisted_false_for_empty_or_missing_task_id() {
        let store = store_with_tasks(&["task-a"]);
        // Empty/whitespace-only task_id never matches a stored row.
        assert!(!task_row_is_persisted(&store, ""));
        assert!(!task_row_is_persisted(&store, "   "));
        // An unsaved draft (or an edited-but-unsaved task_id) has no
        // corresponding row in the store yet.
        assert!(!task_row_is_persisted(&store, "task-new"));
    }

    #[test]
    fn task_run_now_disabled_when_row_is_not_persisted() {
        // Pins Finding 2: even with a non-empty task_id, the button must
        // be disabled when no persisted row exists. Submitting from that
        // state would hit `task ... disappeared from store` in the
        // controller.
        assert!(!task_run_now_enabled(
            /* client_online */ true,
            /* task_enabled */ true,
            "task-new",
            /* persisted_row_exists */ false,
        ));
    }

    #[test]
    fn task_run_now_enabled_when_all_gates_pass() {
        assert!(task_run_now_enabled(true, true, "task-a", true));
    }

    #[test]
    fn task_run_now_disabled_when_client_offline() {
        assert!(!task_run_now_enabled(false, true, "task-a", true));
    }

    #[test]
    fn task_run_now_disabled_when_task_disabled() {
        assert!(!task_run_now_enabled(true, false, "task-a", true));
    }

    #[test]
    fn task_run_now_disabled_when_task_id_blank() {
        assert!(!task_run_now_enabled(true, true, "", true));
        assert!(!task_run_now_enabled(true, true, "   ", true));
    }
}
