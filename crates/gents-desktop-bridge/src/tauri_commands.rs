pub mod chat;
pub mod config;
pub mod e2e;
pub mod lifecycle;
pub mod operations;
pub mod peers;
pub mod tasks;
pub mod tools_explain;

#[cfg(test)]
#[path = "tauri_commands/operations_tests.rs"]
mod operations_tests;

use std::sync::Arc;

use gents_desktop_core::client::ClientCore;
use tauri::{Runtime, AppHandle, Emitter};

use crate::snapshot::build_client_snapshot;
use crate::types::{ClientUpdateEvent, DesktopClientSnapshot};

pub(crate) async fn emit_config_update_and_snapshot<R: Runtime>(
    app: &AppHandle<R>,
    core: &Arc<ClientCore>,
) -> Result<DesktopClientSnapshot, String> {
    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent { reason: "config" },
    );
    build_client_snapshot(Some(core)).await
}
pub mod workspace;
