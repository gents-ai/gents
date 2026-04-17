use eframe::egui::{self, RichText, TextEdit, Ui};

use crate::audit;
use crate::client::ClientStore;
use crate::state::{OperatorSection, PendingOperatorAction, PendingShellAction, ShellState};
use crate::theme;
use crate::views;

use super::drafts::filter_entity_summaries;
use super::EntitySummary;

pub(super) fn breadcrumb(state: &ShellState, section: OperatorSection) -> String {
    format!(
        "{} / {} / {}",
        state
            .operator
            .selected_peer_id
            .as_deref()
            .unwrap_or("local replica"),
        state
            .operator
            .selected_agent_did
            .as_deref()
            .unwrap_or("no agent"),
        section.label(),
    )
}

pub(super) fn show_document_section(
    ui: &mut Ui,
    state: &mut ShellState,
    _store: &ClientStore,
    section: OperatorSection,
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

    let filtered_entries = filter_entity_summaries(entries, state.operator.entity_filter.as_str());

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

fn show_filter_bar(ui: &mut Ui, state: &mut ShellState, section: OperatorSection) {
    let palette = theme::palette();
    let has_new_button = section.supports_new_documents();
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("FILTER")
                .monospace()
                .size(10.5)
                .color(palette.text_3),
        );
        audit::add_sized(
            ui,
            audit::targets::OPERATOR_ENTITY_FILTER,
            [ui.available_width().max(120.0), 28.0],
            TextEdit::singleline(&mut state.operator.entity_filter)
                .id_source(audit::targets::OPERATOR_ENTITY_FILTER)
                .hint_text("name, id, backend, model"),
        );
    });
    if has_new_button {
        ui.add_space(6.0);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), 28.0),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                let response = audit::add_sized(
                    ui,
                    audit::targets::OPERATOR_NEW,
                    egui::vec2(96.0, 28.0),
                    egui::Button::new("New"),
                );
                if response.clicked() {
                    state.queue_shell_action(PendingShellAction::Operator(
                        PendingOperatorAction::StartNewDocument,
                    ));
                }
            },
        );
    }
}

fn render_entity_list(ui: &mut Ui, state: &mut ShellState, filtered_entries: &[EntitySummary]) {
    let palette = theme::palette();
    egui::ScrollArea::vertical().show(ui, |ui| {
        for entry in filtered_entries {
            let selected = state.operator.selected_entity_id.as_deref() == Some(entry.id.as_str());
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
            audit::record(ui, &audit::targets::operator_entity(&entry.id), &response);
            if response.clicked() {
                state.queue_shell_action(PendingShellAction::Operator(
                    PendingOperatorAction::SelectEntity {
                        entity_id: entry.id.clone(),
                    },
                ));
            }
            ui.add_space(6.0);
        }
    });
}
