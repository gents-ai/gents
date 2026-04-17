use eframe::egui::{self, Ui};

use crate::audit;
use crate::client::{ClientCore, ClientStore};
use crate::state::{OperatorSection, ShellState};
use crate::theme;
use crate::views;
use crate::views::chat::build_deployment_entries;

use super::drafts::section_meta;

pub(super) fn show_sidebar(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    let palette = theme::palette();

    let Some(store) = store else {
        views::card(
            ui,
            "Operator Unavailable",
            "The desktop client must finish bootstrapping before operator documents can render.",
        );
        return;
    };

    let peer_statuses = client.map(ClientCore::peer_statuses).unwrap_or_default();
    let deployments = build_deployment_entries(&peer_statuses, store);

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            views::sidebar_heading(ui, "Deployments", Some("focus"));
        });
        ui.add_space(6.0);

        for deployment in &deployments {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    let meta = format!(
                        "{}  runtime {}",
                        deployment.agent_label,
                        if deployment.connected {
                            "online"
                        } else {
                            "lagging"
                        }
                    );
                    let response = views::side_row(
                        ui,
                        &deployment.label,
                        &meta,
                        state.operator.selected_peer_id.as_deref()
                            == Some(deployment.peer_id.as_str()),
                        if deployment.connected {
                            palette.accent
                        } else {
                            palette.warning
                        },
                        Some(if deployment.connected { "up" } else { "warn" }),
                    );
                    audit::record(
                        ui,
                        &audit::targets::operator_deployment(&deployment.peer_id),
                        &response,
                    );
                    if response.clicked() {
                        state.operator.selected_peer_id = Some(deployment.peer_id.clone());
                        state.operator.selected_agent_did = Some(deployment.agent_did.clone());
                        reset_selection(state);
                    }

                    let response = views::tree_row(
                        ui,
                        &deployment.agent_label,
                        if deployment.connected { "live" } else { "lag" },
                        state.operator.selected_agent_did.as_deref()
                            == Some(deployment.agent_did.as_str()),
                    );
                    audit::record(
                        ui,
                        &audit::targets::operator_agent(&deployment.agent_did),
                        &response,
                    );
                    if response.clicked() {
                        state.operator.selected_peer_id = Some(deployment.peer_id.clone());
                        state.operator.selected_agent_did = Some(deployment.agent_did.clone());
                        reset_selection(state);
                    }
                });
            });
            ui.add_space(10.0);
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            views::sidebar_heading(ui, "Manage", None);
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.vertical(|ui| {
                for section in OperatorSection::MANAGE {
                    let (title, meta) =
                        section_meta(store, section, state.operator.selected_agent_did.as_deref());
                    let response = views::side_row(
                        ui,
                        title,
                        &meta,
                        state.operator.selected_section == section,
                        if state.operator.selected_section == section {
                            palette.accent
                        } else {
                            palette.text_3
                        },
                        None,
                    );
                    audit::record(ui, &audit::targets::operator_section(section), &response);
                    if response.clicked() {
                        state.operator.selected_section = section;
                        reset_selection(state);
                        state.operator.last_apply_error = None;
                    }
                }
            });
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            views::sidebar_heading(ui, "Inspect", None);
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.vertical(|ui| {
                for section in OperatorSection::INSPECT {
                    let response = views::side_row(
                        ui,
                        section.label(),
                        "T10",
                        state.operator.selected_section == section,
                        palette.text_3,
                        None,
                    );
                    audit::record(ui, &audit::targets::operator_section(section), &response);
                    if response.clicked() {
                        state.operator.selected_section = section;
                        state.operator.selected_entity_id = None;
                        state.operator.entity_filter.clear();
                        state.operator.draft = None;
                        state.operator.draft_source_entity_id = None;
                    }
                }
            });
        });
    });
}

fn reset_selection(state: &mut ShellState) {
    state.operator.selected_entity_id = None;
    state.operator.entity_filter.clear();
    state.operator.draft = None;
    state.operator.draft_source_entity_id = None;
}
