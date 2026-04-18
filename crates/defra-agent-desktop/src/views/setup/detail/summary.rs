use eframe::egui::{RichText, Ui};

use crate::theme;

use super::super::shared::{labeled_value, PeerEntry};

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
        labeled_value(
            ui,
            "Status",
            if peer.connected { "online" } else { "saved locally" },
        );
        labeled_value(
            ui,
            "Remote Node",
            peer.remote_node_id
                .as_deref()
                .unwrap_or("unparseable address"),
        );
        labeled_value(ui, "Address", &peer.addr);
    });
}
