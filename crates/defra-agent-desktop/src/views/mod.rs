pub mod chat;
pub mod logs;
pub mod operator;
pub mod peers;

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
    match state.activity {
        Activity::Chat => chat::prepare_state(state, client, store),
        Activity::Operator => operator::prepare_state(state, client, store),
        Activity::Peers => peers::prepare_state(state, client, store),
        Activity::Logs => {}
    }
}

pub fn show_sidebar(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    runtime: &tokio::runtime::Runtime,
) {
    shell::show_sidebar_chrome(ui, state, client, store);
    let _ = runtime;
    chat::show_sidebar(ui, state, client, store);
}

pub fn show_main(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    log_store: &DesktopLogStore,
    runtime: &tokio::runtime::Runtime,
    markdown_cache: &mut CommonMarkCache,
) {
    match state.activity {
        Activity::Chat => chat::show_main(ui, state, client, store, markdown_cache),
        Activity::Operator => operator::show_main(ui, state, store),
        Activity::Peers => peers::show_main(ui, state, client, store, runtime),
        Activity::Logs => logs::show_main(ui, state, log_store),
    }
}

pub fn show_rail(
    ui: &mut Ui,
    state: &mut ShellState,
    client: Option<&ClientCore>,
    store: Option<&ClientStore>,
    log_store: &DesktopLogStore,
    runtime: &tokio::runtime::Runtime,
) {
    match state.activity {
        Activity::Chat => {}
        Activity::Operator => operator::show_rail(ui, state, client, store, runtime),
        Activity::Peers => peers::show_rail(ui, state, client, store, runtime),
        Activity::Logs => logs::show_rail(ui, client, store, log_store),
    }
}
