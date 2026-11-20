pub mod allowed_dirs;
pub mod attachments;
pub mod chat;
pub mod config;
pub mod db_explorer;
pub mod e2e;
pub mod external_url;
pub mod inference_setup;
pub mod join_requests;
pub mod lifecycle;
pub mod mailbox;
pub mod managed_server;
pub mod operations;
pub mod packs;
pub mod peers;
pub(crate) mod service_executable;
pub mod tasks;
pub mod ux_plugins;

#[cfg(test)]
#[path = "tauri_commands/operations_tests.rs"]
mod operations_tests;

use std::sync::Arc;

use crate::client_runtime::ClientRuntime;
use tauri::{AppHandle, Emitter, Runtime, State};

use crate::snapshot::build_client_snapshot_with_grants;
use crate::state::{snapshot_grants, DesktopAppState};
use crate::types::{ClientUpdateEvent, DesktopClientSnapshot};
use gents_store::client_error::BridgeError;

pub(crate) async fn emit_config_update_and_snapshot<R>(
    app: &AppHandle<R>,
    core: &Arc<ClientRuntime>,
    state: &State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError>
where
    R: Runtime,
{
    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent::coarse("config"),
    );
    build_client_snapshot_with_grants(Some(core), &state.policy, snapshot_grants(state))
        .await
        .map_err(BridgeError::untyped)
}
pub mod workspace;
