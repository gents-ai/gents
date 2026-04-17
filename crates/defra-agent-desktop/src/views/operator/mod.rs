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

use eframe::egui::Ui;
use tokio::runtime::Runtime;

use self::drafts::entity_summaries;
use crate::client::{ClientCore, ClientStore};
use crate::state::{OperatorSection, ShellState};
use crate::views;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EntitySummary {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) meta: String,
}

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

pub fn show_main(ui: &mut Ui, state: &mut ShellState, store: Option<&ClientStore>) {
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
    let breadcrumb = entity_list::breadcrumb(state, section);

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
