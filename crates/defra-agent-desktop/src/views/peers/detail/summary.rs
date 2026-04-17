use defra_agent_protocol::schemas::{ALL_COLLECTION_NAMES, RUNTIME_COLLECTION_NAMES};
use eframe::egui::{RichText, Ui};

use crate::client::ClientCore;
use crate::theme;

use super::super::shared::{labeled_value, monospace_row, PeerEntry};

pub(super) fn render_peer_summary(ui: &mut Ui, peer: &PeerEntry) {
    let palette = theme::palette();

    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new("selected deployment")
                .family(theme::stencil_family())
                .size(13.0)
                .color(palette.text_1)
                .strong(),
        );
        ui.add_space(6.0);
        labeled_value(ui, "Label", &peer.label);
        labeled_value(ui, "Agent", &peer.agent_label);
        labeled_value(ui, "Agent DID", &peer.agent_did);
        labeled_value(
            ui,
            "Directory State",
            if peer.connected { "online" } else { "saved" },
        );
        labeled_value(
            ui,
            "Remote Node ID",
            peer.remote_node_id
                .as_deref()
                .unwrap_or("unparseable address"),
        );
        labeled_value(ui, "Address", &peer.addr);
    });
}

pub(super) fn render_replication_watch(ui: &mut Ui, peer: &PeerEntry) {
    let palette = theme::palette();
    let status = if peer.connected {
        "subscribed"
    } else {
        "saved only"
    };

    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new("replication watch")
                .family(theme::stencil_family())
                .size(13.0)
                .color(palette.text_1)
                .strong(),
        );
        ui.add_space(6.0);
        for collection in RUNTIME_COLLECTION_NAMES {
            monospace_row(ui, collection, "runtime", status);
        }
        for collection in ALL_COLLECTION_NAMES {
            monospace_row(ui, collection, "protocol", status);
        }
    });
}

pub(super) fn render_acp_limitations(ui: &mut Ui, client: &ClientCore, peer: &PeerEntry) {
    let palette = theme::palette();

    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new("acp access surface")
                .family(theme::stencil_family())
                .size(13.0)
                .color(palette.text_1)
                .strong(),
        );
        ui.add_space(6.0);
        labeled_value(ui, "Selected Peer", &peer.label);
        labeled_value(ui, "Desktop DID", client.principal().did());
        labeled_value(ui, "Current Grants", "not exposed by embedded node API");
        labeled_value(ui, "Pending Incoming Access", "not exposed by embedded node API");
        labeled_value(ui, "Inline Grant / Deny", "blocked until ACP APIs are surfaced");
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "This desktop can dial peers and arm replication today, but grant enumeration and incoming access queues still require upstream ACP read APIs.",
            )
            .size(12.5)
            .color(palette.text_2),
        );
    });
}
