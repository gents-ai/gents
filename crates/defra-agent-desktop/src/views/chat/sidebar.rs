use eframe::egui::{self, RichText, Ui};

use crate::audit;
use crate::state::{Activity, ShellState};
use crate::theme::Palette;
use crate::views;

use super::{ConversationBucket, DeploymentEntry};

pub fn show(
    ui: &mut Ui,
    palette: Palette,
    state: &mut ShellState,
    deployments: &[DeploymentEntry],
    conversations: &[ConversationBucket],
    selected_agent_did: Option<&str>,
    selected_session_id: Option<&str>,
) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            views::sidebar_heading(ui, "Deployments", Some("+ peer"));
        });
        ui.add_space(8.0);

        if deployments.is_empty() {
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
                        state.activity = Activity::Peers;
                        state.peers.show_add_form = true;
                    }
                });
            });
        } else {
            for deployment in deployments {
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    ui.vertical(|ui| {
                        let meta = format!(
                            "1 agent  {}",
                            deployment.addr
                        );
                        let response = views::side_row(
                            ui,
                            &deployment.label,
                            &meta,
                            state.chat.selected_peer_id.as_deref()
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
                            &audit::targets::chat_deployment(&deployment.peer_id),
                            &response,
                        );
                        if response.clicked() {
                            state.chat.selected_peer_id = Some(deployment.peer_id.clone());
                            state.chat.selected_agent_did = Some(deployment.agent_did.clone());
                            state.chat.selected_session_id = None;
                        }

                        let response = views::tree_row(
                            ui,
                            &deployment.agent_label,
                            if deployment.connected { "live" } else { "lag" },
                            selected_agent_did == Some(deployment.agent_did.as_str()),
                        );
                        audit::record(
                            ui,
                            &audit::targets::chat_agent(&deployment.agent_did),
                            &response,
                        );
                        if response.clicked() {
                            state.chat.selected_peer_id = Some(deployment.peer_id.clone());
                            state.chat.selected_agent_did = Some(deployment.agent_did.clone());
                            state.chat.selected_session_id = None;
                        }

                        if let Some(warning) = deployment.warning.as_deref() {
                            ui.label(
                                RichText::new(warning)
                                    .monospace()
                                    .size(10.0)
                                    .color(palette.warning),
                            );
                        }
                    });
                });
                ui.add_space(10.0);
            }
        }

        ui.separator();
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(
                RichText::new("Conversations")
                    .family(crate::theme::stencil_family())
                    .size(13.0)
                    .color(palette.text_1)
                    .strong(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(14.0);
                let enabled = selected_agent_did.is_some();
                let response = audit::add_enabled(
                    ui,
                    audit::targets::CHAT_NEW_CONVERSATION,
                    enabled,
                    egui::Button::new(
                        RichText::new("+ new")
                            .monospace()
                            .size(10.5)
                            .color(if enabled { palette.accent } else { palette.text_3 }),
                    )
                    .min_size(egui::vec2(52.0, 20.0)),
                );
                if response.clicked() {
                    state.chat.new_conversation_requested = true;
                }
            });
        });
        ui.add_space(8.0);

        if selected_agent_did.is_none() {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                views::card(
                    ui,
                    "Select Agent",
                    "Choose a deployment or agent from the tree to load conversations.",
                );
            });
            return;
        }

        if conversations.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                views::card(
                    ui,
                    "No Conversations",
                    "This agent has no conversations yet. Use the main-pane nudge to create the first conversation before sending the first request.",
                );
            });
            return;
        }

        for bucket in conversations {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    views::section_kicker(ui, bucket.label);
                    for entry in &bucket.entries {
                        let meta = format!("{}  {}", entry.meta, entry.timestamp_label);
                        let response = views::side_row(
                            ui,
                            &entry.title,
                            &meta,
                            selected_session_id == Some(entry.session_id.as_str()),
                            if selected_session_id == Some(entry.session_id.as_str()) {
                                palette.accent
                            } else {
                                palette.text_3
                            },
                            None,
                        );
                        audit::record(
                            ui,
                            &audit::targets::chat_conversation(&entry.session_id),
                            &response,
                        );
                        if response.clicked() {
                            state.chat.selected_session_id = Some(entry.session_id.clone());
                        }
                    }
                    ui.add_space(6.0);
                });
            });
        }
    });
}
