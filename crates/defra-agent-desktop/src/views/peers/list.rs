use eframe::egui::{self, Ui};
use tokio::runtime::Runtime;

use crate::audit;
use crate::client::{ClientCore, ClientStore};
use crate::state::ShellState;
use crate::theme;
use crate::views;

use super::forms::render_add_peer_form;
use super::shared::build_peer_entries;

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

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            views::sidebar_heading(
                ui,
                "Desktop Access",
                Some(if peers.is_empty() { "empty" } else { "saved" }),
            );
        });
        ui.add_space(8.0);

        if peers.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                views::card(
                    ui,
                    "No Saved Deployments",
                    "Use Add Deployment below to save the first remote node for this desktop.",
                );
            });
        } else {
            render_peer_directory(ui, state, &peers);
        }

        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            views::sidebar_heading(ui, "Add Deployment", None);
        });
        ui.add_space(8.0);

        if !peers.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                let toggle_label = if state.peers.show_add_form {
                    "Hide form"
                } else {
                    "Show form"
                };
                if audit::button(ui, audit::targets::PEERS_TOGGLE_ADD_FORM, toggle_label).clicked()
                {
                    state.peers.show_add_form = !state.peers.show_add_form;
                }
            });
            ui.add_space(8.0);
        }

        if state.peers.show_add_form {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                render_add_peer_form(ui, state, client, runtime);
            });
        } else if !peers.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                views::card(
                    ui,
                    "Add Another Deployment",
                    "Use the shell add button or open this form when you want to save another node.",
                );
            });
        }

        if let Some(message) = state.peers.last_action_message.as_deref() {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                views::card(ui, "Last Peer Action", message);
            });
        }
    });
}

fn render_peer_directory(
    ui: &mut Ui,
    state: &mut ShellState,
    peers: &[super::shared::PeerEntry],
) {
    let palette = theme::palette();

    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.vertical(|ui| {
            for peer in peers {
                let selected = state.peers.selected_peer_id.as_deref() == Some(peer.record_id.as_str());
                let response = views::side_row(
                    ui,
                    &peer.label,
                    &peer.agent_label,
                    selected,
                    if peer.connected {
                        palette.accent
                    } else {
                        palette.warning
                    },
                    Some(if peer.connected { "online" } else { "saved" }),
                );
                audit::record(ui, &audit::targets::peers_peer(&peer.record_id), &response);
                audit::record(ui, &audit::targets::peers_agent(&peer.record_id), &response);
                if response.clicked() {
                    state.peers.selected_peer_id = Some(peer.record_id.clone());
                }
                ui.add_space(6.0);
            }
        });
    });
}
