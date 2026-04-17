use defra_agent_protocol::schemas::{ALL_COLLECTION_NAMES, RUNTIME_COLLECTION_NAMES};
use eframe::egui::{self, RichText, Ui};
use tokio::runtime::Runtime;

use crate::audit;
use crate::client::{ClientCore, ClientStore};
use crate::state::ShellState;
use crate::theme;
use crate::views;

use super::actions::{remove_selected_peer, render_transport_actions};
use super::forms::{copy_did, render_add_peer_form};
use super::shared::{build_peer_entries, labeled_value, monospace_row, selected_peer, PeerEntry};

pub(super) fn show_main(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    runtime: &Runtime,
) {
    let Some(client) = client else {
        views::card(
            ui,
            "Peer Access Unavailable",
            "The embedded node is offline, so peer diagnostics and replication surfaces cannot render.",
        );
        return;
    };

    let peers = build_peer_entries(client, store);
    if peers.is_empty() {
        render_first_launch_main(ui, state, client, runtime);
        return;
    }

    let selected_peer = selected_peer(state, &peers);
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
        render_replication_watch(ui, peer);
        ui.add_space(10.0);
        render_acp_limitations(ui, client, peer);
    });
}

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
                &RUNTIME_COLLECTION_NAMES.len().to_string(),
            );
            labeled_value(ui, "Protocol Collections", &ALL_COLLECTION_NAMES.len().to_string());
            labeled_value(ui, "Remote Schema Match", "not exposed by node API");
        });

        ui.add_space(12.0);
        if audit::button(ui, audit::targets::PEERS_REMOVE, "Remove Saved Peer").clicked() {
            remove_selected_peer(state, client, &peers, &peer, runtime);
        }
    });
}

fn render_first_launch_main(
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

fn render_peer_summary(ui: &mut Ui, peer: &PeerEntry) {
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

fn render_replication_watch(ui: &mut Ui, peer: &PeerEntry) {
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

fn render_acp_limitations(ui: &mut Ui, client: &ClientCore, peer: &PeerEntry) {
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
