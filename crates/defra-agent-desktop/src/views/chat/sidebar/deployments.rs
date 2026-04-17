use eframe::egui::{RichText, Ui};

use crate::audit;
use crate::state::{PendingChatAction, PendingShellAction, ShellState};
use crate::theme;
use crate::theme::Palette;
use crate::views;

use super::DeploymentEntry;

pub(super) fn render_empty(ui: &mut Ui, state: &mut ShellState) {
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.vertical(|ui| {
            views::card(
                ui,
                "Add Deployment",
                "No saved peers yet. Open the Peers activity to copy this desktop DID and add the first remote deployment address or ticket.",
            );
            ui.add_space(8.0);
            if audit::button(
                ui,
                audit::targets::CHAT_OPEN_PEERS_SETUP,
                "Open Peers Setup",
            )
            .clicked()
            {
                state.queue_shell_action(PendingShellAction::OpenPeersSetup);
            }
        });
    });
}

pub(super) fn render_list(
    ui: &mut Ui,
    palette: Palette,
    state: &mut ShellState,
    deployments: &[DeploymentEntry],
    _selected_agent_did: Option<&str>,
) {
    for deployment in deployments {
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.vertical(|ui| {
                let selected = state.chat.shell.selected_peer_id.as_deref()
                    == Some(deployment.peer_id.as_str());
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
                    selected,
                    if deployment.connected {
                        palette.accent
                    } else {
                        palette.warning
                    },
                    Some(if deployment.connected { "up" } else { "warn" }),
                );
                audit::record(
                    ui,
                    &audit::targets::chat_deployment(&deployment.peer_id),
                    &response,
                );
                audit::record(
                    ui,
                    &audit::targets::chat_agent(&deployment.agent_did),
                    &response,
                );
                if response.clicked() {
                    queue_select_deployment(state, deployment);
                }

                if selected {
                    if let Some(warning) = deployment.warning.as_deref() {
                        ui.label(
                            RichText::new(warning)
                                .monospace()
                                .size(10.0)
                                .color(theme::palette().warning),
                        );
                    }
                    ui.label(
                        RichText::new(deployment.addr.as_str())
                            .monospace()
                            .size(10.0)
                            .color(theme::palette().text_3),
                    );
                }
            });
        });
        ui.add_space(10.0);
    }
}

fn queue_select_deployment(state: &mut ShellState, deployment: &DeploymentEntry) {
    state.queue_shell_action(PendingShellAction::Chat(
        PendingChatAction::SelectDeployment {
            peer_id: deployment.peer_id.clone(),
            agent_did: deployment.agent_did.clone(),
        },
    ));
}
