pub(crate) mod chat;
pub(crate) mod config;
pub(crate) mod e2e;
pub(crate) mod inference_setup;
pub(crate) mod lifecycle;
pub(crate) mod operations;
pub(crate) mod peers;
pub(crate) mod tasks;
pub(crate) mod tools_explain;

#[cfg(test)]
#[path = "tauri_commands/operations_tests.rs"]
mod operations_tests;

use std::sync::Arc;

use gents_desktop_core::client::ClientCore;
use tauri::{AppHandle, Emitter};

use super::snapshot::build_client_snapshot;
use super::types::{ClientUpdateEvent, DesktopClientSnapshot};

pub(super) async fn emit_config_update_and_snapshot(
    app: &AppHandle,
    core: &Arc<ClientCore>,
) -> Result<DesktopClientSnapshot, String> {
    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent { reason: "config" },
    );
    build_client_snapshot(Some(core)).await
}
pub(crate) mod workspace;
