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
    detail::show_main(ui, state, client, store, runtime);
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
