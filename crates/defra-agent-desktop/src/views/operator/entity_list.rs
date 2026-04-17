use eframe::egui::{self, RichText, TextEdit, Ui};

use crate::audit;
use crate::client::ClientStore;
use crate::state::{OperatorSection, ShellState};
use crate::theme;
use crate::views;

use super::drafts::{draft_for_selection, filter_entity_summaries};
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
    store: &ClientStore,
    section: OperatorSection,
    entries: Vec<EntitySummary>,
) {
    show_filter_bar(ui, state);
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
    sync_filtered_selection(state, store, section, &filtered_entries);

    if filtered_entries.is_empty() {
        views::card(
            ui,
            "No Matches",
            "The current filter does not match any replicated documents in this section.",
        );
        return;
    }

    render_entity_list(ui, state, store, section, &filtered_entries);
}

fn show_filter_bar(ui: &mut Ui, state: &mut ShellState) {
    let palette = theme::palette();
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
            [ui.available_width(), 28.0],
            TextEdit::singleline(&mut state.operator.entity_filter)
                .id_source(audit::targets::OPERATOR_ENTITY_FILTER)
                .hint_text("name, id, backend, model"),
        );
    });
}

fn sync_filtered_selection(
    state: &mut ShellState,
    store: &ClientStore,
    section: OperatorSection,
    filtered_entries: &[EntitySummary],
) {
    if state
        .operator
        .selected_entity_id
        .as_deref()
        .is_some_and(|entity_id| !filtered_entries.iter().any(|entry| entry.id == entity_id))
    {
        state.operator.selected_entity_id = filtered_entries.first().map(|entry| entry.id.clone());
        let selected_entity_id = state.operator.selected_entity_id.clone();
        state.operator.draft = selected_entity_id.as_deref().and_then(|entity_id| {
            draft_for_selection(
                store,
                section,
                state.operator.selected_agent_did.as_deref(),
                entity_id,
            )
        });
        state.operator.draft_source_entity_id =
            state.operator.draft.as_ref().and(selected_entity_id);
        state.operator.last_apply_error = None;
    }
}

fn render_entity_list(
    ui: &mut Ui,
    state: &mut ShellState,
    store: &ClientStore,
    section: OperatorSection,
    filtered_entries: &[EntitySummary],
) {
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
                state.operator.selected_entity_id = Some(entry.id.clone());
                state.operator.draft = draft_for_selection(
                    store,
                    section,
                    state.operator.selected_agent_did.as_deref(),
                    &entry.id,
                );
                state.operator.draft_source_entity_id =
                    state.operator.draft.as_ref().map(|_| entry.id.clone());
                state.operator.last_apply_error = None;
            }
            ui.add_space(6.0);
        }
    });
}
