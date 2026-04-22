pub mod chat;
mod components;
pub mod manage;
pub mod setup;

mod primitives;
mod shell;

use eframe::egui::Ui;
use egui_commonmark::CommonMarkCache;

use crate::client::{ClientCore, ClientStore};
use crate::state::{Activity, ShellState};
use crate::telemetry::DesktopLogStore;

pub(crate) use primitives::{card, section_kicker, side_row, sidebar_heading, toolbar};

pub fn prepare_state(
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
) {
    shell::prepare_state(state, client, store);
    setup::prepare_state(state, client, store);
    match state.activity {
        Activity::Chat => chat::prepare_state(state, client, store),
        Activity::Manage => manage::prepare_state(state, client, store),
    }
}

pub fn show_sidebar(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    _runtime: &tokio::runtime::Runtime,
) {
    shell::show_sidebar_chrome(ui, state, client, store);
    match state.activity {
        Activity::Chat => chat::show_sidebar(ui, state, client, store),
        Activity::Manage => manage::show_sidebar(ui, state, client, store),
    }
}

pub fn show_main(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    _log_store: &DesktopLogStore,
    runtime: &tokio::runtime::Runtime,
    markdown_cache: &mut CommonMarkCache,
) {
    match state.activity {
        Activity::Chat => chat::show_main(ui, state, client, store, runtime, markdown_cache),
        Activity::Manage => manage::show_main(ui, state, client, store),
    }
}

pub fn show_rail(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    _log_store: &DesktopLogStore,
    runtime: &tokio::runtime::Runtime,
) {
    match state.activity {
        Activity::Chat => {}
        Activity::Manage => manage::show_rail(ui, state, client, store, runtime),
    }
}
