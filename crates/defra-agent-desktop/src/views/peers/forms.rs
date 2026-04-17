use eframe::egui::{RichText, TextEdit, Ui};
use tokio::runtime::Runtime;

use crate::audit;
use crate::client::ClientCore;
use crate::state::ShellState;
use crate::theme;

pub(super) fn render_add_peer_form(
    ui: &mut Ui,
    state: &mut ShellState,
    client: &ClientCore,
    runtime: &Runtime,
) {
    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new("Add Deployment")
                .family(theme::stencil_family())
                .size(12.5)
                .strong(),
        );
        ui.add_space(8.0);
        ui.label("Label");
        audit::add(
            ui,
            audit::targets::PEERS_ADD_LABEL,
            TextEdit::singleline(&mut state.peers.add_label)
                .id_source(audit::targets::PEERS_ADD_LABEL)
                .desired_width(f32::INFINITY)
                .hint_text("Workshop Bay"),
        );
        ui.add_space(6.0);
        ui.label("IROH Address or Ticket");
        audit::add(
            ui,
            audit::targets::PEERS_ADD_ADDR,
            TextEdit::singleline(&mut state.peers.add_addr)
                .id_source(audit::targets::PEERS_ADD_ADDR)
                .desired_width(f32::INFINITY)
                .hint_text("/ip4/127.0.0.1/udp/.... or iroh://..."),
        );
        ui.add_space(6.0);
        ui.label("Agent DID");
        audit::add(
            ui,
            audit::targets::PEERS_ADD_AGENT_DID,
            TextEdit::singleline(&mut state.peers.add_agent_did)
                .id_source(audit::targets::PEERS_ADD_AGENT_DID)
                .desired_width(f32::INFINITY)
                .hint_text("did:defra:amy"),
        );
        ui.add_space(8.0);
        let can_save = !state.peers.add_label.trim().is_empty()
            && !state.peers.add_addr.trim().is_empty()
            && !state.peers.add_agent_did.trim().is_empty();
        ui.horizontal(|ui| {
            if audit::add_enabled(
                ui,
                audit::targets::PEERS_SAVE,
                can_save,
                egui::Button::new("Save Peer"),
            )
            .clicked()
            {
                match runtime.block_on(client.add_peer(
                    &state.peers.add_label,
                    &state.peers.add_addr,
                    &state.peers.add_agent_did,
                )) {
                    Ok(result) => {
                        state.peers.selected_peer_id = Some(result.peer_id);
                        state.peers.last_action_message = Some(match result.warning {
                            Some(warning) => format!("Saved {}. {}", result.label, warning),
                            None if result.connected => {
                                format!("Saved {} and dialed the peer successfully.", result.label)
                            }
                            None => format!("Saved {} to the local peer directory.", result.label),
                        });
                        state.peers.add_label.clear();
                        state.peers.add_addr.clear();
                        state.peers.add_agent_did.clear();
                        state.peers.show_add_form = false;
                    }
                    Err(error) => {
                        state.peers.last_action_message =
                            Some(format!("Add deployment failed: {error}"));
                    }
                }
            }

            if audit::button(ui, audit::targets::PEERS_CLEAR, "Clear").clicked() {
                state.peers.add_label.clear();
                state.peers.add_addr.clear();
                state.peers.add_agent_did.clear();
                state.peers.last_action_message = None;
            }
        });
    });
}

pub(super) fn copy_did(ui: &Ui, state: &mut ShellState, client: &ClientCore) {
    ui.copy_text(client.principal().did().to_string());
    state.peers.last_action_message = Some("Copied desktop DID to clipboard.".to_string());
}
