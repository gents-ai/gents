use eframe::egui::{self, RichText, Ui};

use crate::audit;
use crate::chat::controller as chat_controller;
use crate::client::{ClientCore, ClientStore};
use crate::manage::controller as manage_controller;
use crate::manage::{build_deployment_entries, DeploymentEntry};
use crate::state::{Activity, PendingShellAction, ShellState};
use crate::theme;
use crate::views;

pub(super) fn prepare_state(
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    let Some(store) = store else {
        return;
    };

    let peer_statuses = client.map(ClientCore::peer_statuses).unwrap_or_default();
    let deployments = build_deployment_entries(&peer_statuses, store);
    let Some(selected) = current_deployment(state, &deployments).or_else(|| deployments.first())
    else {
        return;
    };

    sync_scoped_deployment(state, selected);
}

pub(super) fn show_sidebar_chrome(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    ui.add_space(12.0);
    render_deployments(ui, state, client, store);
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(6.0);
}

fn render_deployments(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    let palette = theme::palette();

    ui.horizontal(|ui| {
        render_section_label(ui, "Deployments");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if audit::button(
                ui,
                audit::targets::CHAT_OPEN_SETUP,
                RichText::new("+ add")
                    .monospace()
                    .size(10.5)
                    .color(palette.accent),
            )
            .clicked()
            {
                state.queue_shell_action(PendingShellAction::OpenDeploymentSetup);
            }
        });
    });
    ui.add_space(6.0);

    let Some(store) = store else {
        ui.label(
            RichText::new("Loading deployments")
                .monospace()
                .size(10.5)
                .color(palette.text_3),
        );
        return;
    };

    let peer_statuses = client.map(ClientCore::peer_statuses).unwrap_or_default();
    let deployments = build_deployment_entries(&peer_statuses, store);
    if deployments.is_empty() {
        views::card(
            ui,
            "No Deployments Yet",
            "Use Add Deployment to connect this desktop to an agent.",
        );
        return;
    }

        let selected = selected_scoped_deployment(state)
            .map(|(peer_id, agent_did)| (peer_id.to_string(), agent_did.to_string()));
    for deployment in deployments {
        render_deployment_row(
            ui,
            state,
            &deployment,
            selected.as_ref().is_some_and(|(peer_id, agent_did)| {
                peer_id.as_str() == deployment.peer_id && agent_did.as_str() == deployment.agent_did
            }),
        );
    }
}

fn render_deployment_row(
    ui: &mut Ui,
    state: &mut ShellState,
    deployment: &DeploymentEntry,
    selected: bool,
) {
    let palette = theme::palette();
    let manage_target = audit::targets::manage_deployment(&deployment.peer_id);
    let manage_agent_target = audit::targets::manage_agent(&deployment.agent_did);
    let dot_color = if deployment.peer_id.starts_with("local:") || deployment.connected {
        palette.accent
    } else {
        palette.warning
    };
    let status = if deployment.peer_id.starts_with("local:") {
        "local"
    } else if deployment.connected {
        "online"
    } else {
        "saved"
    };

    ui.horizontal(|ui| {
        let manage_width = 84.0;
        let row_width = (ui.available_width() - manage_width - 8.0).max(0.0);
        let row_response = ui
            .allocate_ui_with_layout(
                egui::vec2(row_width, 42.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    views::side_row(
                        ui,
                        &deployment.label,
                        &deployment.agent_label,
                        selected,
                        dot_color,
                        Some(status),
                    )
                },
            )
            .inner;
        audit::record(
            ui,
            &audit::targets::chat_deployment(&deployment.peer_id),
            &row_response,
        );
        audit::record(
            ui,
            &audit::targets::chat_agent(&deployment.agent_did),
            &row_response,
        );
        if row_response.clicked() {
            state.queue_shell_action(PendingShellAction::SelectScopedDeployment {
                peer_id: deployment.peer_id.clone(),
                agent_did: deployment.agent_did.clone(),
            });
            state.setup.workspace_open = false;
            state.setup.show_add_form = false;
        }

        let manage_response = audit::add_sized(
            ui,
            &manage_target,
            [manage_width, 32.0],
            egui::Button::new("Manage")
                .fill(if selected {
                    palette.background_2
                } else {
                    palette.background_0
                })
                .stroke(egui::Stroke::new(1.0, palette.stroke_subtle)),
        );
        audit::record(ui, &manage_agent_target, &manage_response);
        if manage_response.clicked() {
            state.queue_shell_action(PendingShellAction::SelectScopedDeployment {
                peer_id: deployment.peer_id.clone(),
                agent_did: deployment.agent_did.clone(),
            });
            state.setup.workspace_open = false;
            state.setup.show_add_form = false;
            state.queue_shell_action(PendingShellAction::Navigate(Activity::Manage));
        }
    });
    ui.add_space(6.0);
}

fn render_section_label(ui: &mut Ui, title: &str) {
    views::section_kicker(ui, &title.to_ascii_uppercase());
}

fn current_deployment<'a>(
    state: &ShellState,
    deployments: &'a [DeploymentEntry],
) -> Option<&'a DeploymentEntry> {
    selected_scoped_deployment(state)
        .and_then(|(peer_id, agent_did)| {
            deployments.iter().find(|deployment| {
                deployment.peer_id == peer_id && deployment.agent_did == agent_did
            })
        })
        .or_else(|| {
            state
                .manage
                .selected_peer_id
                .as_deref()
                .zip(state.manage.selected_agent_did.as_deref())
                .and_then(|(peer_id, agent_did)| {
                    deployments.iter().find(|deployment| {
                        deployment.peer_id == peer_id && deployment.agent_did == agent_did
                    })
                })
        })
}

fn selected_scoped_deployment(state: &ShellState) -> Option<(&str, &str)> {
    state
        .chat
        .shell
        .selected_peer_id
        .as_deref()
        .zip(state.chat.shell.selected_agent_did.as_deref())
}

fn sync_scoped_deployment(state: &mut ShellState, deployment: &DeploymentEntry) {
    let chat_selected = state.chat.shell.selected_peer_id.as_deref()
        == Some(deployment.peer_id.as_str())
        && state.chat.shell.selected_agent_did.as_deref() == Some(deployment.agent_did.as_str());
    if !chat_selected {
        chat_controller::select_deployment(
            &mut state.chat,
            deployment.peer_id.clone(),
            deployment.agent_did.clone(),
        );
    }

    let manage_selected = state.manage.selected_peer_id.as_deref()
        == Some(deployment.peer_id.as_str())
        && state.manage.selected_agent_did.as_deref() == Some(deployment.agent_did.as_str());
    if !manage_selected {
        manage_controller::select_deployment(
            &mut state.manage,
            deployment.peer_id.clone(),
            deployment.agent_did.clone(),
        );
    }

    state.setup.selected_peer_id = Some(deployment.peer_id.clone());
}
