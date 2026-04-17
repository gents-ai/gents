use eframe::egui::{RichText, Ui};
use tokio::runtime::Runtime;

use crate::audit;
use crate::client::ClientCore;
use crate::state::ShellState;
use crate::theme;
use crate::views;

use super::super::forms::{copy_did, render_add_peer_form};
use super::super::shared::labeled_value;

pub(super) fn render_first_launch_main(
    ui: &mut Ui,
    state: &mut ShellState,
    client: &ClientCore,
    runtime: &Runtime,
) {
    let palette = theme::palette();

    ui.vertical(|ui| {
        views::toolbar(ui, "Peer Access", "first launch / no deployments", "setup");
        ui.add_space(16.0);
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("First Launch")
                    .family(theme::stencil_family())
                    .size(20.0)
                    .color(palette.text_0)
                    .strong(),
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    "The embedded node has already generated and persisted a desktop principal. Copy that DID, get it granted on a remote agent, then add the first deployment address or ticket here.",
                )
                .size(13.0)
                .color(palette.text_1)
                .line_height(Some(18.0)),
            );
            ui.add_space(12.0);
            ui.columns(2, |columns| {
                columns[0].group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new("Desktop Identity")
                            .family(theme::stencil_family())
                            .size(13.0)
                            .color(palette.text_1)
                            .strong(),
                    );
                    ui.add_space(6.0);
                    labeled_value(ui, "DID", client.principal().did());
                    labeled_value(ui, "Peer ID", client.local_peer_id());
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
                        "Directory Path",
                        &client.paths().peer_directory_path().display().to_string(),
                    );
                    ui.add_space(8.0);
                    if audit::button(ui, audit::targets::PEERS_ONBOARDING_COPY_DID, "Copy DID")
                        .clicked()
                    {
                        copy_did(ui, state, client);
                    }
                });

                columns[1].group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new("Add Your First Deployment")
                            .family(theme::stencil_family())
                            .size(13.0)
                            .color(palette.text_1)
                            .strong(),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "Paste the remote IROH address or ticket plus the agent DID you expect to observe.",
                        )
                        .size(12.5)
                        .color(palette.text_2),
                    );
                    ui.add_space(8.0);
                    render_add_peer_form(ui, state, client, runtime);
                });
            });
            if let Some(message) = state.peers.last_action_message.as_deref() {
                ui.add_space(10.0);
                views::card(ui, "Setup Update", message);
            }
        });
    });
}
