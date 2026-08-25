pub mod chat;
pub mod config;
pub mod e2e;
pub mod inference_setup;
pub mod lifecycle;
pub mod mailbox;
pub mod managed_server;
pub mod operations;
pub mod peers;
pub mod tasks;
pub mod tools_explain;

#[cfg(test)]
#[path = "tauri_commands/operations_tests.rs"]
mod operations_tests;

use std::sync::Arc;

use gents_desktop_core::client::ClientCore;
use tauri::{AppHandle, Emitter, Runtime, State};

use crate::error::BridgeError;
use crate::snapshot::build_client_snapshot_with_grants;
use crate::state::{snapshot_grants, DesktopAppState};
use crate::types::{ClientUpdateEvent, DesktopClientSnapshot};

pub(crate) async fn emit_config_update_and_snapshot<R: Runtime>(
    app: &AppHandle<R>,
    core: &Arc<ClientCore>,
    state: &State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent::coarse("config"),
    );
    build_client_snapshot_with_grants(Some(core), Some(&state.policy), snapshot_grants(state))
        .await
        .map_err(BridgeError::from_legacy_message)
}
pub mod workspace;
