use eframe::egui::{self, RichText, Ui};
use tokio::runtime::Runtime;

use crate::audit;
use crate::client::{ClientCore, ClientStore};
use crate::state::ShellState;
use crate::theme;
use crate::views;

use super::forms::{copy_did, render_add_peer_form};
use super::shared::{build_peer_entries, labeled_value};

pub(super) fn prepare_state(
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    let Some(client) = client else {
        state.peers.selected_peer_id = None;
        return;
    };

    let peers = build_peer_entries(client, store);
    if peers.is_empty() {
        state.peers.selected_peer_id = None;
        state.peers.show_add_form = true;
        return;
    }

    if state
        .peers
        .selected_peer_id
        .as_deref()
        .is_none_or(|record_id| !peers.iter().any(|peer| peer.record_id == record_id))
    {
        state.peers.selected_peer_id = peers.first().map(|peer| peer.record_id.clone());
    }
}

pub(super) fn show_sidebar(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    runtime: &Runtime,
) {
    let Some(client) = client else {
        views::card(
            ui,
            "Peers Unavailable",
            "The desktop client must finish bootstrapping before peer state can render.",
        );
        return;
    };

    let peers = build_peer_entries(client, store);
    let palette = theme::palette();

    if peers.is_empty() {
        egui::ScrollArea::vertical().show(ui, |ui| {
            let short_did = client.principal().short_did();
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                views::sidebar_heading(ui, "Add Deployment", Some("first launch"));
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    views::card(
                        ui,
                        "First Launch",
                        "Your desktop principal is ready. Use the center panel to copy the DID, request ACP access on a remote agent, and add the first deployment ticket or address.",
                    );
                    ui.add_space(10.0);
                    ui.group(|ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new("setup status")
                                .family(theme::stencil_family())
                                .size(12.5)
                                .color(palette.text_1)
                                .strong(),
                        );
                        ui.add_space(6.0);
                        labeled_value(ui, "Principal DID", &short_did);
                        labeled_value(ui, "Peers Saved", "0");
                        labeled_value(
                            ui,
                            "Next Step",
                            "grant DID on remote peer, then add deployment",
                        );
                    });
                });
            });
        });
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            views::sidebar_heading(ui, "My Identity", Some("local"));
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.vertical(|ui| {
                render_identity_card(ui, client);
                ui.add_space(8.0);
                if audit::button(ui, audit::targets::PEERS_MAIN_COPY_DID, "Copy DID").clicked() {
                    copy_did(ui, state, client);
                }
            });
        });

        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            views::sidebar_heading(ui, "Deployments", None);
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            let toggle_label = if state.peers.show_add_form {
                "Hide form"
            } else {
                "Add Deployment"
            };
            if audit::button(ui, audit::targets::PEERS_TOGGLE_ADD_FORM, toggle_label).clicked() {
                state.peers.show_add_form = !state.peers.show_add_form;
            }
        });

        if state.peers.show_add_form {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                render_add_peer_form(ui, state, client, runtime);
            });
        }

        if let Some(message) = state.peers.last_action_message.as_deref() {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                views::card(ui, "Last Peer Action", message);
            });
        }

        ui.add_space(8.0);
        if peers.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                views::card(
                    ui,
                    "No Peers Saved",
                    "Add a deployment ticket or address to start dialing and replication setup.",
                );
            });
        } else {
            for peer in &peers {
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    ui.vertical(|ui| {
                        let status_label = if peer.connected { "online" } else { "saved" };
                        let status_color = if peer.connected {
                            palette.accent
                        } else {
                            palette.warning
                        };
                        let accessory = if peer.connected { "up" } else { "hold" };
                        let meta = format!("{}  {}", peer.agent_label, status_label);

                        let response = views::side_row(
                            ui,
                            &peer.label,
                            &meta,
                            state.peers.selected_peer_id.as_deref()
                                == Some(peer.record_id.as_str()),
                            status_color,
                            Some(accessory),
                        );
                        audit::record(ui, &audit::targets::peers_peer(&peer.record_id), &response);
                        if state.peers.selected_peer_id.as_deref() == Some(peer.record_id.as_str())
                        {
                            ui.scroll_to_rect(response.rect, Some(egui::Align::Center));
                        }
                        audit::record(ui, &audit::targets::peers_agent(&peer.record_id), &response);
                        if response.clicked() {
                            state.peers.selected_peer_id = Some(peer.record_id.clone());
                        }
                    });
                });
                ui.add_space(10.0);
            }
        }
    });
}

fn render_identity_card(ui: &mut Ui, client: &ClientCore) {
    let palette = theme::palette();
    let did = client.principal().did();

    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new("Desktop Principal")
                .family(theme::stencil_family())
                .size(12.5)
                .color(palette.text_1)
                .strong(),
        );
        ui.add_space(6.0);
        labeled_value(ui, "DID", did);
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "Copy the DID from here when a remote operator needs to grant this desktop access.",
            )
            .size(12.5)
            .color(palette.text_2),
        );
    });
}
