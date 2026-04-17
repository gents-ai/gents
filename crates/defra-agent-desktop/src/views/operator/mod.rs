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

use crate::client::{ClientCore, ClientStore};
use crate::operator::{entity_summaries, EntitySummary};
use crate::state::{OperatorSection, ShellState};
use crate::views;

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
        views::toolbar(ui, "Operator Console", &breadcrumb, section.label());
        ui.add_space(12.0);
        ui.horizontal_top(|ui| {
            let nav_width = 260.0_f32.min(ui.available_width() * 0.34);
            ui.allocate_ui_with_layout(
                egui::vec2(nav_width, ui.available_height()),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    sidebar::show_sidebar(ui, state, None, Some(store));
                },
            );
            ui.add_space(12.0);
            ui.vertical(|ui| match section {
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
            });
        });
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
