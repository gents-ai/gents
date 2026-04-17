use eframe::egui::{self, RichText, Ui};
use tokio::runtime::Runtime;

use crate::audit;
use crate::client::ClientCore;
use crate::state::ShellState;
use crate::theme;

use super::shared::{labeled_value, PeerEntry};

pub(super) fn render_transport_actions(
    ui: &mut Ui,
    state: &mut ShellState,
    client: &ClientCore,
    runtime: &Runtime,
) {
    let palette = theme::palette();
    let health = client.p2p_health();

    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new("transport actions")
                .family(theme::stencil_family())
                .size(13.0)
                .color(palette.text_1)
                .strong(),
        );
        ui.add_space(6.0);
        labeled_value(ui, "P2P Health", health.status_label());
        labeled_value(
            ui,
            "Connected / Replicators",
            &format!(
                "{}/{}",
                health.connected_peer_count, health.replicator_count
            ),
        );
        if let Some(error) = health.last_error.as_deref() {
            labeled_value(ui, "Last Error", error);
        }
        ui.add_space(8.0);
        ui.vertical(|ui| {
            let button_size = egui::vec2(ui.available_width(), 0.0);
            if audit::add_sized(
                ui,
                audit::targets::PEERS_REPAIR_NOW,
                button_size,
                egui::Button::new("Repair Now"),
            )
            .clicked()
            {
                state.peers.last_action_message =
                    Some(match runtime.block_on(client.request_p2p_repair()) {
                        Ok(()) => "Queued a desktop P2P repair cycle.".to_string(),
                        Err(error) => format!("Repair request failed: {error}"),
                    });
            }

            ui.add_space(6.0);
            if audit::add_sized(
                ui,
                audit::targets::PEERS_RESTART_CLIENT,
                button_size,
                egui::Button::new("Restart Client Core"),
            )
            .clicked()
            {
                state.pending_client_restart_reason =
                    Some("manual desktop P2P recovery".to_string());
                state.peers.last_action_message = Some(
                    "Restarting desktop client core to recover the P2P transport.".to_string(),
                );
            }
        });
    });
}

pub(super) fn remove_selected_peer(
    state: &mut ShellState,
    client: &ClientCore,
    peers: &[PeerEntry],
    peer: &PeerEntry,
    runtime: &Runtime,
) {
    match runtime.block_on(client.remove_peer(&peer.record_id)) {
        Ok(result) => {
            let next_peer = peers
                .iter()
                .find(|candidate| candidate.record_id != result.peer_id)
                .cloned();
            state.peers.selected_peer_id = next_peer
                .as_ref()
                .map(|candidate| candidate.record_id.clone());

            if state.chat.shell.selected_peer_id.as_deref() == Some(result.peer_id.as_str())
                || state.chat.shell.selected_agent_did.as_deref() == Some(peer.agent_did.as_str())
            {
                state.chat.shell.selected_peer_id = next_peer
                    .as_ref()
                    .map(|candidate| candidate.record_id.clone());
                state.chat.shell.selected_agent_did = next_peer
                    .as_ref()
                    .map(|candidate| candidate.agent_did.clone());
                state.chat.shell.selected_session_id = None;
                state.chat.editor.selected_behavior_override = None;
            }

            if state.operator.selected_peer_id.as_deref() == Some(result.peer_id.as_str())
                || state.operator.selected_agent_did.as_deref() == Some(peer.agent_did.as_str())
            {
                state.operator.selected_peer_id = next_peer
                    .as_ref()
                    .map(|candidate| candidate.record_id.clone());
                state.operator.selected_agent_did = next_peer
                    .as_ref()
                    .map(|candidate| candidate.agent_did.clone());
                state.operator.selected_entity_id = None;
                state.operator.draft = None;
                state.operator.draft_origin = None;
            }
            state.peers.last_action_message = Some(match result.warning {
                Some(warning) => format!("Removed {}. {}", result.label, warning),
                None => format!("Removed {} from the local peer directory.", result.label),
            });
        }
        Err(error) => {
            state.peers.last_action_message = Some(format!("Remove peer failed: {error}"));
        }
    }
}
