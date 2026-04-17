mod actions;
mod detail;
mod forms;
mod list;
mod shared;

use eframe::egui::Ui;
use tokio::runtime::Runtime;

use crate::client::{ClientCore, ClientStore};
use crate::state::ShellState;

pub fn prepare_state(
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    list::prepare_state(state, client, store);
}

pub fn show_sidebar(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    runtime: &Runtime,
) {
    list::show_sidebar(ui, state, client, store, runtime);
}

pub fn show_main(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    runtime: &Runtime,
) {
    let Some(client) = client else {
        detail::show_main(ui, state, client, store, runtime);
        return;
    };

    let peers = shared::build_peer_entries(client, store);
    if peers.is_empty() {
        detail::show_main(ui, state, Some(client), store, runtime);
        return;
    }

    ui.horizontal_top(|ui| {
        let nav_width = 320.0_f32.min(ui.available_width() * 0.4);
        ui.allocate_ui_with_layout(
            egui::vec2(nav_width, ui.available_height()),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                list::show_sidebar(ui, state, Some(client), store, runtime);
            },
        );
        ui.add_space(12.0);
        ui.vertical(|ui| {
            detail::show_main(ui, state, Some(client), store, runtime);
        });
    });
}

pub fn show_rail(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    runtime: &Runtime,
) {
    detail::show_rail(ui, state, client, store, runtime);
}
