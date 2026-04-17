use eframe::egui::{self, Ui};

use crate::audit;
use crate::client::{ClientCore, ClientStore};
use crate::operator::{build_deployment_entries, section_meta};
use crate::state::{OperatorSection, PendingOperatorAction, PendingShellAction, ShellState};
use crate::theme;
use crate::views;

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
            views::sidebar_heading(ui, "Deployments", None);
        });
        ui.add_space(6.0);

        for deployment in &deployments {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    let meta = format!(
                        "{}  {}",
                        deployment.agent_label,
                        if deployment.connected {
                            "online"
                        } else {
                            "saved"
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
                    audit::record(
                        ui,
                        &audit::targets::operator_agent(&deployment.agent_did),
                        &response,
                    );
                    if response.clicked() {
                        state.queue_shell_action(PendingShellAction::Operator(
                            PendingOperatorAction::SelectDeployment {
                                peer_id: deployment.peer_id.clone(),
                                agent_did: deployment.agent_did.clone(),
                            },
                        ));
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
            views::sidebar_heading(ui, "Config", None);
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
                        state.queue_shell_action(PendingShellAction::Operator(
                            PendingOperatorAction::SelectSection { section },
                        ));
                    }
                }
            });
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            views::sidebar_heading(ui, "History", None);
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.vertical(|ui| {
                for section in OperatorSection::INSPECT {
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
                        state.queue_shell_action(PendingShellAction::Operator(
                            PendingOperatorAction::SelectSection { section },
                        ));
                    }
                }
            });
        });
    });
}
