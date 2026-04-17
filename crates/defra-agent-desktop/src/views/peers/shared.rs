use std::fmt::Write as _;

use eframe::egui::{self, RichText, Ui};
use p2p::iroh::parse_public_peer_addr;

use crate::client::{ClientCore, ClientStore};
use crate::state::ShellState;
use crate::theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PeerEntry {
    pub(super) record_id: String,
    pub(super) label: String,
    pub(super) agent_did: String,
    pub(super) agent_label: String,
    pub(super) addr: String,
    pub(super) connected: bool,
    pub(super) warning: Option<String>,
    pub(super) remote_node_id: Option<String>,
}

pub(super) fn build_peer_entries(
    client: &ClientCore,
    store: Option<&ClientStore>,
) -> Vec<PeerEntry> {
    let mut peers: Vec<_> = client
        .peer_statuses()
        .into_iter()
        .map(|status| PeerEntry {
            record_id: status.peer_id,
            label: status.label,
            agent_did: status.agent_did.clone(),
            agent_label: display_name_for_agent(store, &status.agent_did),
            addr: status.addr.clone(),
            connected: status.dial_succeeded,
            warning: status.last_error,
            remote_node_id: parse_remote_node_id(&status.addr),
        })
        .collect();

    peers.sort_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.record_id.cmp(&right.record_id))
    });
    peers
}

pub(super) fn selected_peer<'a>(
    state: &ShellState,
    peers: &'a [PeerEntry],
) -> Option<&'a PeerEntry> {
    state
        .peers
        .selected_peer_id
        .as_deref()
        .and_then(|record_id| peers.iter().find(|peer| peer.record_id == record_id))
}

fn display_name_for_agent(store: Option<&ClientStore>, agent_did: &str) -> String {
    store
        .and_then(|store| {
            store
                .agent_principals
                .iter()
                .find(|row| row.agent_did == agent_did)
                .and_then(|row| row.display_name.as_deref())
                .filter(|value| !value.trim().is_empty())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| {
            agent_did
                .rsplit(':')
                .next()
                .filter(|segment| !segment.trim().is_empty())
                .unwrap_or(agent_did)
                .to_string()
        })
}

fn parse_remote_node_id(addr: &str) -> Option<String> {
    parse_public_peer_addr(addr)
        .ok()
        .map(|(peer_id, _)| peer_id.to_string())
}

pub(super) fn public_key_fingerprint(bytes: &[u8]) -> String {
    let mut output = String::new();
    for byte in bytes.iter().take(6) {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

pub(super) fn labeled_value(ui: &mut Ui, label: &str, value: &str) {
    let palette = theme::palette();

    ui.horizontal(|ui| {
        ui.set_width(ui.available_width());
        let label_width = ui.available_width().min(128.0);
        ui.add_sized(
            egui::vec2(label_width, 16.0),
            egui::Label::new(
                RichText::new(label)
                    .monospace()
                    .size(11.0)
                    .color(palette.text_2),
            )
            .wrap_mode(egui::TextWrapMode::Truncate),
        );
        ui.add_sized(
            egui::vec2(ui.available_width().max(0.0), 16.0),
            egui::Label::new(
                RichText::new(value)
                    .monospace()
                    .size(11.0)
                    .color(palette.text_0),
            )
            .wrap_mode(egui::TextWrapMode::Truncate),
        );
    });
}

pub(super) fn monospace_row(ui: &mut Ui, left: &str, middle: &str, right: &str) {
    let palette = theme::palette();

    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{left:<24}"))
                .monospace()
                .size(11.0)
                .color(palette.text_0),
        );
        ui.label(
            RichText::new(format!("{middle:<10}"))
                .monospace()
                .size(11.0)
                .color(palette.text_2),
        );
        ui.label(
            RichText::new(right)
                .monospace()
                .size(11.0)
                .color(palette.text_1),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_key_fingerprint_uses_hex_prefix() {
        assert_eq!(
            public_key_fingerprint(&[0xde, 0xad, 0xbe, 0xef, 0x11, 0x22]),
            "deadbeef1122"
        );
    }

    #[test]
    fn parse_remote_node_id_returns_none_for_invalid_addr() {
        assert_eq!(parse_remote_node_id(""), None);
    }
}
