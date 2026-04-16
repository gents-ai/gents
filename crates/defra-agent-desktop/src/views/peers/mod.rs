use std::fmt::Write as _;

use defra_agent_protocol::schemas::{ALL_COLLECTION_NAMES, RUNTIME_COLLECTION_NAMES};
use eframe::egui::{self, RichText, TextEdit, Ui};
use p2p::iroh::parse_public_peer_addr;
use tokio::runtime::Runtime;

use crate::audit;
use crate::client::{ClientCore, ClientStore};
use crate::state::ShellState;
use crate::theme;
use crate::views;

#[derive(Debug, Clone, PartialEq, Eq)]
struct PeerEntry {
    record_id: String,
    label: String,
    agent_did: String,
    agent_label: String,
    addr: String,
    connected: bool,
    warning: Option<String>,
    remote_node_id: Option<String>,
}

pub fn prepare_state(
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    let Some(client) = client else {
        state.peers.selected_peer_id = None;
        return;
    };

    let peers = build_peer_entries(client, store);
    if peers.is_empty() {
        state.peers.selected_peer_id = None;
        state.peers.show_add_form = true;
        return;
    }

    if state
        .peers
        .selected_peer_id
        .as_deref()
        .is_none_or(|record_id| !peers.iter().any(|peer| peer.record_id == record_id))
    {
        state.peers.selected_peer_id = peers.first().map(|peer| peer.record_id.clone());
    }
}

pub fn show_sidebar(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    runtime: &Runtime,
) {
    let Some(client) = client else {
        views::card(
            ui,
            "Peers Unavailable",
            "The desktop client must finish bootstrapping before peer state can render.",
        );
        return;
    };

    let peers = build_peer_entries(client, store);
    let palette = theme::palette();

    if peers.is_empty() {
        egui::ScrollArea::vertical().show(ui, |ui| {
            let short_did = client.principal().short_did();
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                views::sidebar_heading(ui, "Add Deployment", Some("first launch"));
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    views::card(
                        ui,
                        "First Launch",
                        "Your desktop principal is ready. Use the center panel to copy the DID, request ACP access on a remote agent, and add the first deployment ticket or address.",
                    );
                    ui.add_space(10.0);
                    ui.group(|ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new("setup status")
                                .family(theme::stencil_family())
                                .size(12.5)
                                .color(palette.text_1)
                                .strong(),
                        );
                        ui.add_space(6.0);
                        labeled_value(ui, "Principal DID", &short_did);
                        labeled_value(ui, "Peers Saved", "0");
                        labeled_value(
                            ui,
                            "Next Step",
                            "grant DID on remote peer, then add deployment",
                        );
                    });
                });
            });
        });
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            views::sidebar_heading(ui, "My Identity", Some("local"));
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.vertical(|ui| {
                render_identity_card(ui, client);
                ui.add_space(8.0);
                if audit::button(ui, audit::targets::PEERS_MAIN_COPY_DID, "Copy DID").clicked() {
                    copy_did(ui, state, client);
                }
            });
        });

        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            views::sidebar_heading(ui, "Peered Deployments", None);
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            let toggle_label = if state.peers.show_add_form {
                "Hide add form"
            } else {
                "Add deployment"
            };
            if audit::button(ui, audit::targets::PEERS_TOGGLE_ADD_FORM, toggle_label).clicked() {
                state.peers.show_add_form = !state.peers.show_add_form;
            }
        });

        if state.peers.show_add_form {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                render_add_peer_form(ui, state, client, runtime);
            });
        }

        if let Some(message) = state.peers.last_action_message.as_deref() {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                views::card(ui, "Last Peer Action", message);
            });
        }

        ui.add_space(8.0);
        if peers.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                views::card(
                    ui,
                    "No Peers Saved",
                    "Add a deployment ticket or address to start dialing and replication setup.",
                );
            });
        } else {
            for peer in &peers {
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    ui.vertical(|ui| {
                        let status_label = if peer.connected { "online" } else { "saved" };
                        let status_color = if peer.connected {
                            palette.accent
                        } else {
                            palette.warning
                        };
                        let accessory = if peer.connected { "up" } else { "hold" };
                        let meta =
                            format!("{}  {}", peer.agent_label, status_label);

                        let response = views::side_row(
                            ui,
                            &peer.label,
                            &meta,
                            state.peers.selected_peer_id.as_deref()
                                == Some(peer.record_id.as_str()),
                            status_color,
                            Some(accessory),
                        );
                        audit::record(ui, &audit::targets::peers_peer(&peer.record_id), &response);
                        if state.peers.selected_peer_id.as_deref()
                            == Some(peer.record_id.as_str())
                        {
                            ui.scroll_to_rect(response.rect, Some(egui::Align::Center));
                        }
                        if response.clicked() {
                            state.peers.selected_peer_id = Some(peer.record_id.clone());
                        }

                        let tree_tag = if peer.connected { "live" } else { "saved" };
                        let response = views::tree_row(
                            ui,
                            &peer.agent_label,
                            tree_tag,
                            state.peers.selected_peer_id.as_deref()
                                == Some(peer.record_id.as_str()),
                        );
                        audit::record(ui, &audit::targets::peers_agent(&peer.record_id), &response);
                        if response.clicked() {
                            state.peers.selected_peer_id = Some(peer.record_id.clone());
                        }
                    });
                });
                ui.add_space(10.0);
            }
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            views::sidebar_heading(ui, "Pending Access", None);
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            views::card(
                ui,
                "ACP Queue Unavailable",
                "Incoming access requests are not queryable from the current embedded node API, so this MVP can show peer transport state but not a pending Grant/Deny queue yet.",
            );
        });
    });
}

pub fn show_main(
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

        render_peer_summary(ui, peer);
        ui.add_space(10.0);
        render_replication_watch(ui, peer);
        ui.add_space(10.0);
        render_acp_limitations(ui, client, peer);
    });
}

pub fn show_rail(
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
            match runtime.block_on(client.remove_peer(&peer.record_id)) {
                Ok(result) => {
                    let next_peer = peers
                        .iter()
                        .find(|candidate| candidate.record_id != result.peer_id)
                        .cloned();
                    state.peers.selected_peer_id =
                        next_peer.as_ref().map(|candidate| candidate.record_id.clone());

                    if state.chat.selected_peer_id.as_deref() == Some(result.peer_id.as_str())
                        || state.chat.selected_agent_did.as_deref() == Some(peer.agent_did.as_str())
                    {
                        state.chat.selected_peer_id =
                            next_peer.as_ref().map(|candidate| candidate.record_id.clone());
                        state.chat.selected_agent_did =
                            next_peer.as_ref().map(|candidate| candidate.agent_did.clone());
                        state.chat.selected_session_id = None;
                        state.chat.suppress_session_autoselect = true;
                        state.chat.selected_behavior_override = None;
                    }

                    if state.operator.selected_peer_id.as_deref() == Some(result.peer_id.as_str())
                        || state.operator.selected_agent_did.as_deref()
                            == Some(peer.agent_did.as_str())
                    {
                        state.operator.selected_peer_id =
                            next_peer.as_ref().map(|candidate| candidate.record_id.clone());
                        state.operator.selected_agent_did =
                            next_peer.as_ref().map(|candidate| candidate.agent_did.clone());
                        state.operator.selected_entity_id = None;
                        state.operator.draft = None;
                        state.operator.draft_source_entity_id = None;
                    }
                    state.peers.last_action_message = Some(match result.warning {
                        Some(warning) => format!("Removed {}. {}", result.label, warning),
                        None => format!("Removed {} from the local peer directory.", result.label),
                    });
                }
                Err(error) => {
                    state.peers.last_action_message =
                        Some(format!("Remove peer failed: {error}"));
                }
            }
        }
    });
}

fn render_identity_card(ui: &mut Ui, client: &ClientCore) {
    let palette = theme::palette();
    let did = client.principal().did();
    let fingerprint = public_key_fingerprint(client.principal().public_key_bytes());

    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new("Desktop Principal")
                .family(theme::stencil_family())
                .size(12.5)
                .color(palette.text_1)
                .strong(),
        );
        ui.add_space(6.0);
        labeled_value(ui, "DID", did);
        labeled_value(ui, "Peer ID", client.local_peer_id());
        labeled_value(ui, "Key FP", &fingerprint);
        labeled_value(
            ui,
            "Listen",
            client
                .listen_addresses()
                .first()
                .map(String::as_str)
                .unwrap_or("not published"),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "Copy the DID from here when a remote operator needs to grant this desktop access.",
            )
            .size(12.5)
            .color(palette.text_2),
        );
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

fn render_add_peer_form(
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

fn build_peer_entries(client: &ClientCore, store: Option<&ClientStore>) -> Vec<PeerEntry> {
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

fn selected_peer<'a>(state: &ShellState, peers: &'a [PeerEntry]) -> Option<&'a PeerEntry> {
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

fn public_key_fingerprint(bytes: &[u8]) -> String {
    let mut output = String::new();
    for byte in bytes.iter().take(6) {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn labeled_value(ui: &mut Ui, label: &str, value: &str) {
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

fn monospace_row(ui: &mut Ui, left: &str, middle: &str, right: &str) {
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

fn copy_did(ui: &Ui, state: &mut ShellState, client: &ClientCore) {
    ui.copy_text(client.principal().did().to_string());
    state.peers.last_action_message = Some("Copied desktop DID to clipboard.".to_string());
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
