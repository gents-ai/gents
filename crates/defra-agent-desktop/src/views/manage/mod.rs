mod behavior_context;
pub(crate) mod drafts;
pub(crate) mod editors;
mod entity_list;
mod prepare;
mod rail;
mod recent_failures;
mod request_timeline;
mod runtime;
mod shared;
mod sidebar;

use eframe::egui::{self, RichText, Ui};
use tokio::runtime::Runtime;

use crate::client::{ClientCore, ClientStore};
use crate::manage::{entity_summaries, EntitySummary};
use crate::state::{
    Activity, ManageDraft, ManageSection, PendingManageAction, PendingShellAction, ShellState,
};
use crate::theme;
use crate::views;
use editors::{
    render_backend_editor, render_behavior_editor, render_inference_profile_editor,
    render_scheduled_task_editor, render_tool_selection_editor,
};

pub fn prepare_state(
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    prepare::prepare_state(state, client, store);
}

pub fn show_sidebar(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    sidebar::show_sidebar(ui, state, client, store);
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
    let breadcrumb = entity_list::breadcrumb(state, section);

    ui.vertical(|ui| {
        views::toolbar(ui, "Manage Deployment", &breadcrumb, section.label());
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Back to Chat").clicked() {
                state.queue_shell_action(PendingShellAction::Navigate(Activity::Chat));
            }
        });
        ui.add_space(12.0);
        render_section_tabs(ui, state);
        ui.add_space(12.0);
        match section {
            ManageSection::Runtime => runtime::show_runtime_summary(ui, store, state),
            ManageSection::Behaviors
            | ManageSection::Backends
            | ManageSection::ToolSelections
            | ManageSection::InferenceProfiles
            | ManageSection::ScheduledTasks => {
                render_management_workspace(ui, state, client, store, section, entries);
            }
            ManageSection::RequestTimeline | ManageSection::RecentFailures => {
                entity_list::show_document_section(ui, state, store, section, entries);
            }
        }
    });
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
    let filtered_entries =
        crate::manage::filter_entity_summaries(entries, state.manage.entity_filter.as_str());

    if filtered_entries.is_empty() && !state.manage.entity_filter.trim().is_empty() {
        views::card(
            ui,
            "No Matches",
            "The current filter does not match any replicated documents in this section.",
        );
        return;
    }

    if filtered_entries.is_empty() && !section.supports_new_documents() {
        views::card(
            ui,
            "No Documents",
            "No documents are currently replicated for this section.",
        );
        return;
    }

    ui.horizontal(|ui| {
        let has_new_button = section.supports_new_documents();
        let input_width = if has_new_button {
            (ui.available_width() - 104.0).max(120.0)
        } else {
            ui.available_width().max(120.0)
        };
        crate::audit::add_sized(
            ui,
            crate::audit::targets::MANAGE_ENTITY_FILTER,
            [input_width, 28.0],
            egui::TextEdit::singleline(&mut state.manage.entity_filter)
                .id_source(crate::audit::targets::MANAGE_ENTITY_FILTER)
                .hint_text("Filter by name, id, backend, or model"),
        );
        if has_new_button {
            let response = crate::audit::add_sized(
                ui,
                crate::audit::targets::MANAGE_NEW,
                egui::vec2(96.0, 28.0),
                egui::Button::new("New"),
            );
            if response.clicked() {
                state.queue_shell_action(PendingShellAction::Manage(
                    PendingManageAction::StartNewDocument,
                ));
            }
        }
    });
    ui.add_space(10.0);

    ui.columns(2, |columns| {
        columns[0].set_min_width(260.0);
        egui::ScrollArea::vertical().show(&mut columns[0], |ui| {
            if filtered_entries.is_empty() {
                views::card(
                    ui,
                    "No Documents",
                    "No documents are currently replicated for this section.",
                );
            } else {
                for entry in &filtered_entries {
                    let selected =
                        state.manage.selected_entity_id.as_deref() == Some(entry.id.as_str());
                    let response = views::side_row(
                        ui,
                        &entry.title,
                        &entry.meta,
                        selected,
                        if selected {
                            palette.accent
                        } else {
                            palette.text_3
                        },
                        None,
                    );
                    crate::audit::record(
                        ui,
                        &crate::audit::targets::manage_entity(&entry.id),
                        &response,
                    );
                    if response.clicked() {
                        state.queue_shell_action(PendingShellAction::Manage(
                            PendingManageAction::SelectEntity {
                                entity_id: entry.id.clone(),
                            },
                        ));
                    }
                    ui.add_space(6.0);
                }
            }
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
        ManageSection::ScheduledTasks => {
            if let Some(ManageDraft::ScheduledTask(draft)) = state.manage.draft.as_mut() {
                scroll_editor_body(ui, |ui| render_scheduled_task_editor(ui, draft));
                rail::render_editor_footer(ui, state, client);
            } else {
                views::card(
                    ui,
                    "Scheduled Task Editor",
                    "Select a scheduled task from the list or create a new one to edit it here.",
                );
            }
        }
        _ => {
            let _ = store;
        }
    }
}

fn scroll_editor_body(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    let max_height = (ui.available_height() - 56.0).max(180.0);
    egui::ScrollArea::vertical()
        .max_height(max_height)
        .show(ui, |ui| add_contents(ui));
}
