use eframe::egui::Ui;
use tokio::runtime::Runtime;

use crate::audit;
use crate::client::ClientCore;
use crate::state::ShellState;
use crate::views;

use super::super::actions::{remove_selected_peer, render_transport_actions};
use super::super::shared::PeerEntry;
use super::summary::render_peer_summary;

pub(super) fn show_main(
    ui: &mut Ui,
    state: &mut ShellState,
    client: &ClientCore,
    peers: &[PeerEntry],
    runtime: &Runtime,
    selected_peer: Option<&PeerEntry>,
) {
    let breadcrumb = selected_peer
        .map(|peer| format!("{} / {}", peer.label, peer.agent_label))
        .unwrap_or_else(|| "no deployment selected".to_string());
    let badge = selected_peer
        .map(|peer| {
            if peer.connected {
                "replication armed"
            } else {
                "saved only"
            }
        })
        .unwrap_or("idle");

    ui.vertical(|ui| {
        views::toolbar(ui, "Peer Access", &breadcrumb, badge);
        ui.add_space(12.0);

        if let Some(message) = state.peers.last_action_message.as_deref() {
            views::card(ui, "Peer Update", message);
            ui.add_space(10.0);
        }

        let Some(peer) = selected_peer else {
            views::card(
                ui,
                "Select Deployment",
                "Choose a saved deployment from the sidebar or add a new one to inspect transport and replication state.",
            );
            return;
        };

        if let Some(warning) = peer.warning.as_deref() {
            views::card(ui, "Dial Warning", warning);
            ui.add_space(10.0);
        }

        render_transport_actions(ui, state, client, runtime);
        ui.add_space(10.0);
        render_peer_summary(ui, peer);
        ui.add_space(10.0);
        if audit::button(ui, audit::targets::PEERS_REMOVE, "Remove Saved Peer").clicked() {
            remove_selected_peer(state, client, peers, peer, runtime);
        }
    });
}
