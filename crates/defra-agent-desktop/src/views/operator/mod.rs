mod drafts;
mod editors;
mod rail;
mod recent_failures;
mod request_timeline;
mod runtime;
mod shared;
mod sidebar;

use eframe::egui::{self, RichText, TextEdit, Ui};
use tokio::runtime::Runtime;

use crate::audit;
use crate::client::{ClientCore, ClientStore};
use crate::state::{OperatorSection, ShellState};
use crate::theme;
use crate::views;
use crate::views::chat::build_deployment_entries;

use self::drafts::{
    draft_for_selection, draft_matches_selection, entity_summaries, filter_entity_summaries,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EntitySummary {
    id: String,
    title: String,
    meta: String,
}

pub fn prepare_state(
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    let Some(store) = store else {
        return;
    };

    let peer_statuses = client.map(ClientCore::peer_statuses).unwrap_or_default();
    let deployments = build_deployment_entries(&peer_statuses, store);

    if state
        .operator
        .selected_peer_id
        .as_deref()
        .is_none_or(|peer_id| !deployments.iter().any(|entry| entry.peer_id == peer_id))
    {
        state.operator.selected_peer_id = deployments.first().map(|entry| entry.peer_id.clone());
    }

    if state
        .operator
        .selected_agent_did
        .as_deref()
        .is_none_or(|agent_did| {
            !deployments.iter().any(|entry| entry.agent_did == agent_did)
                && !store
                    .agent_principals
                    .iter()
                    .any(|row| row.agent_did == agent_did)
        })
    {
        state.operator.selected_agent_did = deployments
            .iter()
            .find(|entry| {
                Some(entry.peer_id.as_str()) == state.operator.selected_peer_id.as_deref()
            })
            .map(|entry| entry.agent_did.clone())
            .or_else(|| deployments.first().map(|entry| entry.agent_did.clone()))
            .or_else(|| {
                store
                    .agent_principals
                    .first()
                    .map(|row| row.agent_did.clone())
            });
    }

    let entries = entity_summaries(
        store,
        state.operator.selected_section,
        state.operator.selected_agent_did.as_deref(),
    );
    if state
        .operator
        .selected_entity_id
        .as_deref()
        .is_none_or(|entity_id| !entries.iter().any(|entry| entry.id == entity_id))
    {
        state.operator.selected_entity_id = entries.first().map(|entry| entry.id.clone());
    }

    let selected_entity_id = state.operator.selected_entity_id.clone();
    if !draft_matches_selection(
        &state.operator.draft,
        state.operator.draft_source_entity_id.as_deref(),
        state.operator.selected_section,
        selected_entity_id.as_deref(),
    ) {
        state.operator.draft = selected_entity_id.as_deref().and_then(|entity_id| {
            draft_for_selection(
                store,
                state.operator.selected_section,
                state.operator.selected_agent_did.as_deref(),
                entity_id,
            )
        });
        state.operator.draft_source_entity_id =
            state.operator.draft.as_ref().and(selected_entity_id);
        state.operator.last_apply_error = None;
    }
}

pub fn show_sidebar(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    sidebar::show_sidebar(ui, state, client, store);
}

pub fn show_main(ui: &mut Ui, state: &mut ShellState, store: Option<&ClientStore>) {
    let palette = theme::palette();

    let Some(store) = store else {
        views::card(
            ui,
            "Operator Unavailable",
            "No local replica is available for operator views.",
        );
        return;
    };

    let section = state.operator.selected_section;
    let entries = entity_summaries(store, section, state.operator.selected_agent_did.as_deref());
    let breadcrumb = format!(
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
    );

    ui.vertical(|ui| {
        views::toolbar(
            ui,
            "Operator Console",
            &breadcrumb,
            if state.operator.draft.is_some() {
                "editor: active"
            } else {
                "editor: idle"
            },
        );
        ui.add_space(12.0);

        match section {
            OperatorSection::Runtime => runtime::show_runtime_summary(ui, store, state),
            OperatorSection::Behaviors
            | OperatorSection::Backends
            | OperatorSection::ToolSelections
            | OperatorSection::InferenceProfiles
            | OperatorSection::ScheduledTasks
            | OperatorSection::RequestTimeline
            | OperatorSection::RecentFailures => {
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
                ui.add_space(10.0);

                if entries.is_empty() {
                    views::card(
                        ui,
                        "No Documents",
                        "No documents are currently replicated for this section.",
                    );
                } else {
                    let filtered_entries =
                        filter_entity_summaries(entries, state.operator.entity_filter.as_str());
                    if state
                        .operator
                        .selected_entity_id
                        .as_deref()
                        .is_some_and(|entity_id| {
                            !filtered_entries.iter().any(|entry| entry.id == entity_id)
                        })
                    {
                        state.operator.selected_entity_id =
                            filtered_entries.first().map(|entry| entry.id.clone());
                        let selected_entity_id = state.operator.selected_entity_id.clone();
                        state.operator.draft =
                            selected_entity_id.as_deref().and_then(|entity_id| {
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
                    if filtered_entries.is_empty() {
                        views::card(
                            ui,
                            "No Matches",
                            "The current filter does not match any replicated documents in this section.",
                        );
                    } else {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for entry in filtered_entries {
                                let selected = state.operator.selected_entity_id.as_deref()
                                    == Some(entry.id.as_str());
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
                                audit::record(
                                    ui,
                                    &audit::targets::operator_entity(&entry.id),
                                    &response,
                                );
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
                }
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
