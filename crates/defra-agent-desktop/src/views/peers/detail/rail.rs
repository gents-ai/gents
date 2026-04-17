use eframe::egui::{self, RichText, Ui};
use tokio::runtime::Runtime;

use crate::audit;
use crate::client::{ClientCore, ClientStore};
use crate::state::ShellState;
use crate::theme;
use crate::views;

use super::super::actions::remove_selected_peer;
use super::super::shared::{build_peer_entries, labeled_value, selected_peer};

pub(super) fn show_rail(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    runtime: &Runtime,
) {
    let Some(client) = client else {
        views::card(
            ui,
            "Peer Detail Unavailable",
            "The client core must be online before peer metadata can render.",
        );
        return;
    };

    let peers = build_peer_entries(client, store);
    let selected_peer = selected_peer(state, &peers).cloned();
    let palette = theme::palette();

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(14.0);
        let heading_badge = selected_peer
            .as_ref()
            .map(|peer| if peer.connected { "dialed" } else { "saved" })
            .unwrap_or("idle");
        views::sidebar_heading(ui, "Peer Detail", Some(heading_badge));
        ui.add_space(10.0);

        let Some(peer) = selected_peer else {
            views::card(
                ui,
                "No Peer Selected",
                "Select a deployment from the sidebar to inspect node metadata and local persistence details.",
            );
            return;
        };

        ui.group(|ui| {
            ui.set_width(ui.available_width());
            labeled_value(ui, "Directory Label", &peer.label);
            labeled_value(ui, "Record ID", &peer.record_id);
            labeled_value(ui, "Remote Agent", &peer.agent_did);
            labeled_value(
                ui,
                "Remote Node ID",
                peer.remote_node_id.as_deref().unwrap_or("unparseable address"),
            );
            labeled_value(ui, "IROH Address", &peer.addr);
            labeled_value(
                ui,
                "Connection",
                if peer.connected {
                    "dialed / replication armed"
                } else {
                    "saved only"
                },
            );
            labeled_value(
                ui,
                "Last Warning",
                peer.warning.as_deref().unwrap_or("none"),
            );
        });

        ui.add_space(10.0);
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("local node")
                    .family(theme::stencil_family())
                    .size(13.0)
                    .color(palette.text_1)
                    .strong(),
            );
            ui.add_space(6.0);
            labeled_value(ui, "Principal DID", client.principal().did());
            labeled_value(ui, "Local Peer ID", client.local_peer_id());
            labeled_value(
                ui,
                "Listen Address",
                client
                    .listen_addresses()
                    .first()
                    .map(String::as_str)
                    .unwrap_or("not published"),
            );
            labeled_value(
                ui,
                "Peer Directory",
                &client.paths().peer_directory_path().display().to_string(),
            );
            labeled_value(
                ui,
                "Configured / Dialed",
                &format!(
                    "{}/{}",
                    client.configured_peer_count(),
                    client.dialed_peer_count()
                ),
            );
        });

        ui.add_space(10.0);
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("schema watch")
                    .family(theme::stencil_family())
                    .size(13.0)
                    .color(palette.text_1)
                    .strong(),
            );
            ui.add_space(6.0);
            labeled_value(
                ui,
                "Runtime Collections",
                &defra_agent_protocol::schemas::RUNTIME_COLLECTION_NAMES.len().to_string(),
            );
            labeled_value(
                ui,
                "Protocol Collections",
                &defra_agent_protocol::schemas::ALL_COLLECTION_NAMES.len().to_string(),
            );
            labeled_value(ui, "Remote Schema Match", "not exposed by node API");
        });

        ui.add_space(12.0);
        if audit::button(ui, audit::targets::PEERS_REMOVE, "Remove Saved Peer").clicked() {
            remove_selected_peer(state, client, &peers, &peer, runtime);
        }
    });
}
