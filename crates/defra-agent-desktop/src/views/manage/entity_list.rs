use eframe::egui::{self, Ui};

use crate::audit;
use crate::client::ClientStore;
use crate::state::{ManageSection, PendingManageAction, PendingShellAction, ShellState};
use crate::views;

use super::EntitySummary;

pub(super) fn show_document_section(
    ui: &mut Ui,
    state: &mut ShellState,
    _store: &ClientStore,
    section: ManageSection,
    entries: Vec<EntitySummary>,
) {
    show_action_bar(ui, state, section);
    ui.add_space(10.0);

    if entries.is_empty() {
        views::card(
            ui,
            "No Documents",
            "No documents are currently replicated for this section.",
        );
        return;
    }

    render_entity_list(ui, state, &entries);
}

fn show_action_bar(ui: &mut Ui, state: &mut ShellState, section: ManageSection) {
    let has_new_button = section.supports_new_documents();
    if has_new_button {
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let response = audit::add_sized(
                    ui,
                    audit::targets::MANAGE_NEW,
                    egui::vec2(96.0, 28.0),
                    egui::Button::new("New"),
                );
                if response.clicked() {
                    state.queue_shell_action(PendingShellAction::Manage(
                        PendingManageAction::StartNewDocument,
                    ));
                }
            });
        });
    }
}

pub(super) fn entity_list_contents(ui: &mut Ui, state: &mut ShellState, entries: &[EntitySummary]) {
    if entries.is_empty() {
        views::card(
            ui,
            "No Documents",
            "No documents are currently replicated for this section.",
        );
        return;
    }

    render_entity_list(ui, state, entries);
}

pub(super) fn show_new_button(ui: &mut Ui, state: &mut ShellState, section: ManageSection) {
    if section.supports_new_documents() {
        let response = audit::add_sized(
            ui,
            audit::targets::MANAGE_NEW,
            egui::vec2(96.0, 28.0),
            egui::Button::new("New"),
        );
        if response.clicked() {
            state.queue_shell_action(PendingShellAction::Manage(
                PendingManageAction::StartNewDocument,
            ));
        }
    }
}

fn render_entity_list(ui: &mut Ui, state: &mut ShellState, filtered_entries: &[EntitySummary]) {
    let palette = crate::theme::palette();
    egui::ScrollArea::vertical().show(ui, |ui| {
        for entry in filtered_entries {
            let selected = state.manage.selected_entity_id.as_deref() == Some(entry.id.as_str());
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
            audit::record(ui, &audit::targets::manage_entity(&entry.id), &response);
            if response.clicked() {
                state.queue_shell_action(PendingShellAction::Manage(
                    PendingManageAction::SelectEntity {
                        entity_id: entry.id.clone(),
                    },
                ));
            }
            ui.add_space(6.0);
        }
    });
}
