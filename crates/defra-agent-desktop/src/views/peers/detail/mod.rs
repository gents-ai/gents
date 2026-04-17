mod main_panel;
mod onboarding;
mod rail;
mod summary;

use eframe::egui::Ui;
use tokio::runtime::Runtime;

use crate::client::{ClientCore, ClientStore};
use crate::state::ShellState;
use crate::views;

use super::shared::{build_peer_entries, selected_peer};

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
        onboarding::render_first_launch_main(ui, state, client, runtime);
        return;
    }

    main_panel::show_main(
        ui,
        state,
        client,
        &peers,
        runtime,
        selected_peer(state, &peers),
    );
}

pub(super) fn show_rail(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    runtime: &Runtime,
) {
    rail::show_rail(ui, state, client, store, runtime);
}
