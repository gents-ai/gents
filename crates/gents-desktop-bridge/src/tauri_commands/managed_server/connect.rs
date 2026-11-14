use std::path::Path;

use gents_protocol::serve_lifecycle::outdated_runtime_message;
use tauri::{AppHandle, Runtime, State};

use super::pairing::enroll_managed_runtime;
use super::port::{
    observe_port_readiness, occupied_port_message, status_unreadable_error, user_status_signer,
    PortReadiness,
};
use super::start::reconnect_started_runtime;
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::{current_core, DesktopAppState};
use crate::types::{DesktopInitRequest, ManagedServerStatus};

const CONNECT_AGAIN: &str = "then choose Connect this desktop again";

/// Joins this desktop to the node at its local home as an enrolled client
/// (Lean `DesktopConnect.connect`): a home that cannot be joined is reported
/// before the desktop changes anything; otherwise the desktop's identity and
/// route are made only when missing, its client is started, and it enrolls
/// through the same pairing handler the setup wizard's local path runs.
pub(super) async fn connect_local_node<R>(
    app: &AppHandle<R>,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError>
where
    R: Runtime,
{
    let node_home = state.policy.node_home.clone().ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::Unsupported,
            "this desktop has no local node home to connect to",
        )
    })?;
    if let Err(store) = gents_service::repair::home::refuse_older_home(&node_home).await {
        return Err(BridgeError::new(
            BridgeErrorCode::Unsupported,
            store.to_string(),
        ));
    }
    let status = serving_node(&state, &node_home).await?;
    if current_core(&state).is_none() {
        if !route_saved(&state, status.node_did.as_deref()).await {
            super::super::lifecycle::desktop_init_local_standard(
                DesktopInitRequest {
                    label: status.node_name.clone(),
                    dangerously_overwrite: false,
                    reset: false,
                },
                state.clone(),
            )
            .await?;
        }
        super::super::lifecycle::desktop_client_start(app.clone(), state.clone()).await?;
    }
    let core = current_core(&state).ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::ClientStartFailed,
            "the desktop client started but is no longer running; choose Connect this desktop again",
        )
    })?;
    let lifecycle = state.managed_server_lifecycle.lock().await;
    let enrolled = enroll_managed_runtime(&state, core).await;
    drop(lifecycle);
    enrolled.map_err(|failure| {
        BridgeError::new(
            BridgeErrorCode::Pairing,
            format!("{failure}; {CONNECT_AGAIN}"),
        )
    })?;
    reconnect_started_runtime(&state, &status).await;
    Ok(())
}

async fn serving_node(
    state: &DesktopAppState,
    node_home: &Path,
) -> Result<ManagedServerStatus, BridgeError> {
    let refused = |code, message: String| Err(BridgeError::new(code, message));
    match observe_port_readiness(node_home, user_status_signer(state, node_home).await.as_ref())
        .await?
    {
        PortReadiness::Ready(status) => Ok(status),
        PortReadiness::StatusUnreadable { cause, .. } => Err(status_unreadable_error(&cause)),
        PortReadiness::NotListening | PortReadiness::Booting => refused(
            BridgeErrorCode::EndpointUnreachable,
            format!(
                "The node at {home} is not running. Start it with `gents runtime server --home {home}` in a terminal, {CONNECT_AGAIN}",
                home = node_home.display()
            ),
        ),
        PortReadiness::Outdated { version } => refused(
            BridgeErrorCode::Unsupported,
            format!(
                "{}, {CONNECT_AGAIN}",
                outdated_runtime_message(version.as_deref())
            ),
        ),
        PortReadiness::Foreign(foreign) => refused(BridgeErrorCode::Unsupported, foreign.message()),
        PortReadiness::Occupied(port) => {
            refused(BridgeErrorCode::Unsupported, occupied_port_message(port))
        }
    }
}

async fn route_saved(state: &DesktopAppState, node_did: Option<&str>) -> bool {
    let Some(node_did) = node_did else {
        return false;
    };
    gents_desktop_core::client::load_peer_records(state.policy.desktop_paths.peer_directory_path())
        .await
        .is_ok_and(|peers| peers.iter().any(|peer| peer.node_did == node_did))
}
