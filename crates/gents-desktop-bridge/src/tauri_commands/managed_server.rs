use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Runtime, State};

use gents_desktop_core::local_runtime::{init_standard_local_runtime, DesktopInitOptions};

use crate::config::ManagedServerPolicy;
use crate::contract::MANAGED_SERVER_UPDATED_EVENT;
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::{current_core, DesktopAppState};
use crate::types::{ManagedServerStartRequest, ManagedServerState, ManagedServerStatus};

const MANAGED_SERVER_CONFIG: &str = "managed-server.json";

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredManagedServer {
    enabled: bool,
    agent_name: String,
}

#[tauri::command]
pub async fn desktop_managed_server_status(
    state: State<'_, DesktopAppState>,
) -> Result<ManagedServerStatus, BridgeError> {
    ensure_allowed(&state)?;
    let stored = load_preference(&state).await?;
    let managed = state.managed_server.lock().await;
    Ok(status_from(&managed, stored.as_ref()))
}

#[tauri::command]
pub async fn desktop_managed_server_start<R: Runtime>(
    app: AppHandle<R>,
    request: ManagedServerStartRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ManagedServerStatus, BridgeError> {
    ensure_allowed(&state)?;
    let _lifecycle = state.managed_server_lifecycle.lock().await;
    let agent_name = request.agent_name.trim();
    if agent_name.is_empty() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "agentName is required",
        ));
    }
    let agent_home = state.policy.agent_home.clone().ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::Unsupported,
            "managed server requires a local agent home",
        )
    })?;
    let stored = load_preference(&state).await?;

    {
        let managed = state.managed_server.lock().await;
        if managed.server.is_some() {
            drop(managed);
            let committed = StoredManagedServer {
                enabled: true,
                agent_name: agent_name.to_string(),
            };
            save_preference(&state, &committed).await?;
            let managed = state.managed_server.lock().await;
            return Ok(status_from(&managed, Some(&committed)));
        }
    }

    // Check our in-process handle before probing the port above. Once the
    // first onboarding call has started the managed server, its HTTP status
    // endpoint is indistinguishable from an externally launched server. The
    // second call intentionally commits auto-start after client provisioning;
    // probing first would return early and leave the preference disabled.
    if let Some(external) = matching_external_server(&agent_home).await? {
        return Ok(external);
    }

    {
        let mut managed = state.managed_server.lock().await;
        managed.starting = true;
        managed.last_error = None;
    }
    emit_status(&app, &state).await;

    let tool_root = agent_home
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| agent_home.clone());
    let result: anyhow::Result<_> = async {
        gents_server::server_host::ensure_standard_home(
            gents_server::server_host::ProvisionOptions {
                home: agent_home.clone(),
                agent_name: agent_name.to_string(),
                tool_root,
            },
        )
        .await?;
        let server = gents_server::server_host::start_server(
            gents_server::server_host::ServerConfig::standard(agent_home.clone()),
        )
        .await?;

        // Iroh shareable addresses include the process's ephemeral QUIC port.
        // Refresh the persisted local peer after every managed-server start so
        // desktop client startup never dials the previous process's endpoint.
        let _client_lifecycle = state.client_lifecycle.lock().await;
        if let Some(core) = current_core(&state) {
            core.refresh_local_standard_peer(&agent_home, agent_name)
                .await?;
        } else {
            init_standard_local_runtime(DesktopInitOptions {
                agent_home,
                desktop_paths: state.policy.desktop_paths.clone(),
                label: agent_name.to_string(),
            })
            .await?;
        }

        Ok(server)
    }
    .await;

    match result {
        Ok(server) => {
            save_preference(
                &state,
                &StoredManagedServer {
                    enabled: stored.is_some_and(|stored| stored.enabled),
                    agent_name: agent_name.to_string(),
                },
            )
            .await?;
            let mut managed = state.managed_server.lock().await;
            managed.starting = false;
            managed.server = Some(server);
        }
        Err(error) => {
            let message = format!("{error:#}");
            let mut managed = state.managed_server.lock().await;
            managed.starting = false;
            managed.last_error = Some(message.clone());
            drop(managed);
            emit_status(&app, &state).await;
            return Err(BridgeError::from_legacy_message(message));
        }
    }

    emit_status(&app, &state).await;
    drop(_lifecycle);
    desktop_managed_server_status(state).await
}

async fn matching_external_server(
    agent_home: &std::path::Path,
) -> Result<Option<ManagedServerStatus>, BridgeError> {
    let config = gents_server::server_host::ServerConfig::standard(agent_home.to_path_buf());
    let payload = match gents_desktop_core::local_runtime::fetch_runtime_connection_payload(
        &config.status_url(),
    )
    .await
    {
        Ok(payload) => payload,
        Err(_) => return Ok(None),
    };
    let live_did = payload
        .get("agent_did")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if gents_server::server_host::initialized_home(agent_home) {
        let initialized_did = read_initialized_did(agent_home).await;
        ensure_matching_identity(initialized_did.as_deref(), live_did, config.http_port)?;
    }
    Ok(Some(ManagedServerStatus {
        state: ManagedServerState::External,
        auto_start: false,
        agent_name: payload
            .get("agent_name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        agent_did: (!live_did.is_empty()).then(|| live_did.to_string()),
        graphql: payload
            .get("desktop_graphql")
            .or_else(|| payload.get("graphql"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        error: None,
    }))
}

#[tauri::command]
pub async fn desktop_managed_server_stop<R: Runtime>(
    app: AppHandle<R>,
    disable_auto_start: bool,
    state: State<'_, DesktopAppState>,
) -> Result<ManagedServerStatus, BridgeError> {
    ensure_allowed(&state)?;
    let _lifecycle = state.managed_server_lifecycle.lock().await;
    let server = {
        let mut managed = state.managed_server.lock().await;
        managed.starting = false;
        managed.last_error = None;
        managed.server.take()
    };
    if let Some(server) = server {
        server
            .shutdown()
            .await
            .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    }
    if disable_auto_start {
        let mut stored = load_preference(&state).await?.unwrap_or_default();
        stored.enabled = false;
        save_preference(&state, &stored).await?;
    }
    emit_status(&app, &state).await;
    drop(_lifecycle);
    desktop_managed_server_status(state).await
}

fn ensure_allowed(state: &DesktopAppState) -> Result<(), BridgeError> {
    if state.policy.managed_server != ManagedServerPolicy::Allowed {
        return Err(BridgeError::new(
            BridgeErrorCode::Unsupported,
            "managed local server hosting is disabled for this desktop host",
        ));
    }
    Ok(())
}

fn status_from(
    managed: &crate::state::ManagedServerState,
    stored: Option<&StoredManagedServer>,
) -> ManagedServerStatus {
    let ready = managed.server.as_ref().map(|server| server.ready());
    ManagedServerStatus {
        state: if ready.is_some() {
            ManagedServerState::Running
        } else if managed.starting {
            ManagedServerState::Starting
        } else if managed.last_error.is_some() {
            ManagedServerState::Failed
        } else if stored.is_some_and(|stored| stored.enabled) {
            ManagedServerState::Stopped
        } else {
            ManagedServerState::Disabled
        },
        auto_start: stored.is_some_and(|stored| stored.enabled),
        agent_name: ready
            .map(|ready| ready.agent_name.clone())
            .or_else(|| stored.map(|stored| stored.agent_name.clone())),
        agent_did: ready.map(|ready| ready.agent_did.clone()),
        graphql: ready.map(|ready| ready.graphql.clone()),
        error: managed.last_error.clone(),
    }
}

async fn emit_status<R: Runtime>(app: &AppHandle<R>, state: &DesktopAppState) {
    let stored = load_preference(state).await.ok().flatten();
    let managed = state.managed_server.lock().await;
    let _ = app.emit(
        MANAGED_SERVER_UPDATED_EVENT,
        status_from(&managed, stored.as_ref()),
    );
}

async fn read_initialized_did(agent_home: &std::path::Path) -> Option<String> {
    tokio::fs::read(agent_home.join("init.json"))
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("agent_did")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
}

fn ensure_matching_identity(
    initialized_did: Option<&str>,
    live_did: &str,
    port: u16,
) -> Result<(), BridgeError> {
    if initialized_did.is_some_and(|initialized| initialized != live_did) {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            format!("port {port} is occupied by a different Gents identity"),
        ));
    }
    Ok(())
}

async fn load_preference(
    state: &DesktopAppState,
) -> Result<Option<StoredManagedServer>, BridgeError> {
    let path = state
        .policy
        .desktop_paths
        .root()
        .join(MANAGED_SERVER_CONFIG);
    match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| BridgeError::from_legacy_message(error.to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(BridgeError::from_legacy_message(error.to_string())),
    }
}

async fn save_preference(
    state: &DesktopAppState,
    stored: &StoredManagedServer,
) -> Result<(), BridgeError> {
    state
        .policy
        .desktop_paths
        .ensure_root_dirs()
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    let path = state
        .policy
        .desktop_paths
        .root()
        .join(MANAGED_SERVER_CONFIG);
    let bytes = serde_json::to_vec_pretty(stored)
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    tokio::fs::write(path, bytes)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ManagedServerState as ManagedServerRuntimeState;

    #[test]
    fn status_priority_is_starting_then_failed_then_stopped_then_disabled() {
        let stored = StoredManagedServer {
            enabled: true,
            agent_name: "local".to_string(),
        };
        let mut runtime = ManagedServerRuntimeState {
            starting: true,
            last_error: Some("boom".to_string()),
            ..Default::default()
        };
        assert_eq!(
            status_from(&runtime, Some(&stored)).state,
            ManagedServerState::Starting
        );
        runtime.starting = false;
        assert_eq!(
            status_from(&runtime, Some(&stored)).state,
            ManagedServerState::Failed
        );
        runtime.last_error = None;
        assert_eq!(
            status_from(&runtime, Some(&stored)).state,
            ManagedServerState::Stopped
        );
        assert_eq!(
            status_from(&runtime, None).state,
            ManagedServerState::Disabled
        );
    }

    #[test]
    fn external_server_rejects_a_different_initialized_identity() {
        let error = ensure_matching_identity(Some("did:key:local"), "did:key:other", 9191)
            .expect_err("different identity must be rejected");
        assert_eq!(error.code, BridgeErrorCode::InvalidArgument);
        assert!(error.message.contains("port 9191"));
        ensure_matching_identity(Some("did:key:local"), "did:key:local", 9191).unwrap();
    }
}
