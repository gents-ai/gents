use eframe::egui::{self, RichText, Ui};

use crate::audit;
use crate::chat::controller as chat_controller;
use crate::client::{ClientCore, ClientStore};
use crate::operator::controller as operator_controller;
use crate::operator::{build_deployment_entries, DeploymentEntry};
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
    render_shell_identity(ui, state, client);
    ui.add_space(12.0);
    render_deployments(ui, state, client, store);
    ui.add_space(12.0);
    render_chat_entry(ui, state);
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(6.0);
}

fn render_shell_identity(ui: &mut Ui, state: &mut ShellState, client: Option<&ClientCore>) {
    let palette = theme::palette();

    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new("Desktop Identity")
                .family(theme::stencil_family())
                .size(14.0)
                .color(palette.text_1)
                .strong(),
        );
        ui.add_space(6.0);

        let did_button = egui::Button::new(
            RichText::new(state.identity.did_short.as_str())
                .monospace()
                .size(11.0)
                .color(if client.is_some() {
                    palette.text_0
                } else {
                    palette.text_3
                }),
        )
        .min_size(egui::vec2(ui.available_width(), 28.0))
        .fill(palette.background_1)
        .stroke(egui::Stroke::new(1.0, palette.stroke_subtle));
        let response = audit::add_enabled(
            ui,
            audit::targets::PEERS_MAIN_COPY_DID,
            client.is_some(),
            did_button,
        )
        .on_hover_text("Copy full DID");
        if response.clicked() {
            if let Some(client) = client {
                ui.copy_text(client.principal().did().to_string());
                state.peers.last_action_message = Some("Copied desktop DID to clipboard.".to_string());
            }
        }

        ui.add_space(8.0);
        ui.columns(2, |columns| {
            render_activity_button(&mut columns[0], state, Activity::Peers, 30.0);
            render_activity_button(&mut columns[1], state, Activity::Logs, 30.0);
        });
    });
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
                audit::targets::CHAT_OPEN_PEERS_SETUP,
                RichText::new("+ add")
                    .monospace()
                    .size(10.5)
                    .color(palette.accent),
            )
            .clicked()
            {
                state.queue_shell_action(PendingShellAction::OpenPeersSetup);
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
            selected
                .as_ref()
                .is_some_and(|(peer_id, agent_did)| {
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
    let configure_target = audit::targets::operator_deployment(&deployment.peer_id);
    let operator_agent_target = audit::targets::operator_agent(&deployment.agent_did);
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
        let configure_width = 84.0;
        let row_width = (ui.available_width() - configure_width - 8.0).max(0.0);
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
        }

        let configure_response = audit::add_sized(
            ui,
            &configure_target,
            [configure_width, 32.0],
            egui::Button::new("Configure")
                .fill(if selected {
                    palette.background_2
                } else {
                    palette.background_0
                })
                .stroke(egui::Stroke::new(1.0, palette.stroke_subtle)),
        );
        audit::record(ui, &operator_agent_target, &configure_response);
        if configure_response.clicked() {
            state.queue_shell_action(PendingShellAction::SelectScopedDeployment {
                peer_id: deployment.peer_id.clone(),
                agent_did: deployment.agent_did.clone(),
            });
            state.queue_shell_action(PendingShellAction::Navigate(Activity::Operator));
        }
    });
    ui.add_space(6.0);
}

fn render_chat_entry(ui: &mut Ui, state: &mut ShellState) {
    render_section_label(ui, "Chat");
    ui.add_space(6.0);
    render_activity_button(ui, state, Activity::Chat, 34.0);
}

fn render_section_label(ui: &mut Ui, title: &str) {
    views::section_kicker(ui, &title.to_ascii_uppercase());
}

fn render_activity_button(ui: &mut Ui, state: &mut ShellState, activity: Activity, height: f32) {
    let palette = theme::palette();
    let selected = state.activity == activity;
    let button = egui::Button::new(
        RichText::new(activity.label())
            .family(theme::stencil_family())
            .size(13.5)
            .color(if selected {
                palette.text_0
            } else {
                palette.text_1
            }),
    )
    .min_size(egui::vec2(ui.available_width(), height))
    .fill(if selected {
        palette.background_2
    } else {
        palette.background_0
    })
    .stroke(egui::Stroke::new(
        1.0,
        if selected {
            palette.accent_dim
        } else {
            palette.stroke_subtle
        },
    ));
    let response = audit::add_sized(
        ui,
        audit::targets::activity(activity),
        [ui.available_width(), height],
        button,
    )
    .on_hover_text(activity.label());
    if response.clicked() {
        state.queue_shell_action(PendingShellAction::Navigate(activity));
    }
}

fn current_deployment<'a>(
    state: &ShellState,
    deployments: &'a [DeploymentEntry],
) -> Option<&'a DeploymentEntry> {
    selected_scoped_deployment(state)
        .and_then(|(peer_id, agent_did)| {
            deployments
                .iter()
                .find(|deployment| deployment.peer_id == peer_id && deployment.agent_did == agent_did)
        })
        .or_else(|| {
            state.operator.selected_peer_id.as_deref().zip(
                state.operator.selected_agent_did.as_deref(),
            )
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
    let chat_selected = state.chat.shell.selected_peer_id.as_deref() == Some(deployment.peer_id.as_str())
        && state.chat.shell.selected_agent_did.as_deref() == Some(deployment.agent_did.as_str());
    if !chat_selected {
        chat_controller::select_deployment(
            &mut state.chat,
            deployment.peer_id.clone(),
            deployment.agent_did.clone(),
        );
    }

    let operator_selected =
        state.operator.selected_peer_id.as_deref() == Some(deployment.peer_id.as_str())
            && state.operator.selected_agent_did.as_deref() == Some(deployment.agent_did.as_str());
    if !operator_selected {
        operator_controller::select_deployment(
            &mut state.operator,
            deployment.peer_id.clone(),
            deployment.agent_did.clone(),
        );
    }
}
