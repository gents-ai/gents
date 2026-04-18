use eframe::egui::{RichText, TextEdit, Ui};
use tokio::runtime::Runtime;

use crate::audit;
use crate::client::ClientCore;
use crate::state::{Activity, PendingShellAction, ShellState};
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
            audit::targets::SETUP_ADD_LABEL,
            TextEdit::singleline(&mut state.setup.add_label)
                .id_source(audit::targets::SETUP_ADD_LABEL)
                .desired_width(f32::INFINITY)
                .hint_text("Workshop Bay"),
        );
        ui.add_space(6.0);
        ui.label("IROH Address or Ticket");
        audit::add(
            ui,
            audit::targets::SETUP_ADD_ADDR,
            TextEdit::singleline(&mut state.setup.add_addr)
                .id_source(audit::targets::SETUP_ADD_ADDR)
                .desired_width(f32::INFINITY)
                .hint_text("/ip4/127.0.0.1/udp/.... or iroh://..."),
        );
        ui.add_space(6.0);
        ui.label("Agent DID");
        audit::add(
            ui,
            audit::targets::SETUP_ADD_AGENT_DID,
            TextEdit::singleline(&mut state.setup.add_agent_did)
                .id_source(audit::targets::SETUP_ADD_AGENT_DID)
                .desired_width(f32::INFINITY)
                .hint_text("did:defra:amy"),
        );
        ui.add_space(8.0);
        let can_save = !state.setup.add_label.trim().is_empty()
            && !state.setup.add_addr.trim().is_empty()
            && !state.setup.add_agent_did.trim().is_empty();
        ui.horizontal(|ui| {
            if audit::add_enabled(
                ui,
                audit::targets::SETUP_SAVE,
                can_save,
                egui::Button::new("Save Deployment"),
            )
            .clicked()
            {
                match runtime.block_on(client.add_peer(
                    &state.setup.add_label,
                    &state.setup.add_addr,
                    &state.setup.add_agent_did,
                )) {
                    Ok(result) => {
                        let peer_id = result.peer_id.clone();
                        let agent_did = state.setup.add_agent_did.clone();
                        state.setup.selected_peer_id = Some(result.peer_id);
                        state.setup.last_action_message = Some(match result.warning {
                            Some(warning) => format!("Saved {}. {}", result.label, warning),
                            None if result.connected => {
                                format!(
                                    "Saved {} and connected to the deployment successfully.",
                                    result.label
                                )
                            }
                            None => {
                                format!("Saved {} to the local deployment directory.", result.label)
                            }
                        });
                        state.setup.add_label.clear();
                        state.setup.add_addr.clear();
                        state.setup.add_agent_did.clear();
                        state.setup.workspace_open = false;
                        state.setup.show_add_form = false;
                        state.queue_shell_action(PendingShellAction::SelectScopedDeployment {
                            peer_id,
                            agent_did,
                        });
                        if state.chat.shell.selected_peer_id.is_none() {
                            state.queue_shell_action(PendingShellAction::Navigate(Activity::Chat));
                        }
                    }
                    Err(error) => {
                        state.setup.last_action_message =
                            Some(format!("Add deployment failed: {error}"));
                    }
                }
            }

            if audit::button(ui, audit::targets::SETUP_CLEAR, "Clear").clicked() {
                state.setup.add_label.clear();
                state.setup.add_addr.clear();
                state.setup.add_agent_did.clear();
                state.setup.last_action_message = None;
            }
        });
    });
}

pub(super) fn copy_did(ui: &Ui, state: &mut ShellState, client: &ClientCore) {
    ui.copy_text(client.principal().did().to_string());
    state.setup.last_action_message = Some("Copied desktop DID to clipboard.".to_string());
}
