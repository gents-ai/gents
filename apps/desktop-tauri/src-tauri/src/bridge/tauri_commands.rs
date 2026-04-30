pub(crate) mod chat;
pub(crate) mod config;
pub(crate) mod lifecycle;
pub(crate) mod peers;
pub(crate) mod tasks;

use std::sync::Arc;

use defra_agent_desktop_core::client::ClientCore;
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
