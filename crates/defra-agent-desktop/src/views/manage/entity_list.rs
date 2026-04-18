use eframe::egui::{self, TextEdit, Ui};

use crate::audit;
use crate::client::ClientStore;
use crate::state::{ManageSection, PendingManageAction, PendingShellAction, ShellState};
use crate::views;

use super::drafts::filter_entity_summaries;
use super::EntitySummary;

pub(super) fn breadcrumb(state: &ShellState, section: ManageSection) -> String {
    format!(
        "{} / {}",
        state
            .manage
            .selected_agent_did
            .as_deref()
            .unwrap_or("no agent"),
        section.label()
    )
}

pub(super) fn show_document_section(
    ui: &mut Ui,
    state: &mut ShellState,
    _store: &ClientStore,
    section: ManageSection,
    entries: Vec<EntitySummary>,
) {
    show_filter_bar(ui, state, section);
    ui.add_space(10.0);

    if entries.is_empty() {
        views::card(
            ui,
            "No Documents",
            "No documents are currently replicated for this section.",
        );
        return;
    }

    let filtered_entries = filter_entity_summaries(entries, state.manage.entity_filter.as_str());

    if filtered_entries.is_empty() {
        views::card(
            ui,
            "No Matches",
            "The current filter does not match any replicated documents in this section.",
        );
        return;
    }

    render_entity_list(ui, state, &filtered_entries);
}

fn show_filter_bar(ui: &mut Ui, state: &mut ShellState, section: ManageSection) {
    let has_new_button = section.supports_new_documents();
    ui.horizontal(|ui| {
        let input_width = if has_new_button {
            (ui.available_width() - 104.0).max(120.0)
        } else {
            ui.available_width().max(120.0)
        };
        audit::add_sized(
            ui,
            audit::targets::MANAGE_ENTITY_FILTER,
            [input_width, 28.0],
            TextEdit::singleline(&mut state.manage.entity_filter)
                .id_source(audit::targets::MANAGE_ENTITY_FILTER)
                .hint_text("Filter by name, id, backend, or model"),
        );
        if has_new_button {
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
    });
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
