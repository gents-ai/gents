use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Runtime, State};

use gents_desktop_core::local_runtime::{init_standard_local_runtime, DesktopInitOptions};

use crate::config::ManagedServerPolicy;
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;
use crate::types::{ManagedServerStartRequest, ManagedServerStatus};

const MANAGED_SERVER_EVENT: &str = "desktop://managed-server-updated";
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
        init_standard_local_runtime(DesktopInitOptions {
            agent_home,
            desktop_paths: state.policy.desktop_paths.clone(),
            label: agent_name.to_string(),
        })
        .await?;

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
    desktop_managed_server_status(state).await
}

async fn matching_external_server(
    agent_home: &std::path::Path,
) -> Result<Option<ManagedServerStatus>, BridgeError> {
    let payload = match gents_desktop_core::local_runtime::fetch_runtime_connection_payload(
        "http://127.0.0.1:9191/status",
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
    let initialized_did = tokio::fs::read(agent_home.join("init.json"))
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("agent_did")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    if initialized_did
        .as_deref()
        .is_some_and(|initialized| initialized != live_did)
    {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "port 9191 is occupied by a different Gents identity",
        ));
    }
    Ok(Some(ManagedServerStatus {
        state: "external".to_string(),
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
            "running"
        } else if managed.starting {
            "starting"
        } else if managed.last_error.is_some() {
            "failed"
        } else if stored.is_some_and(|stored| stored.enabled) {
            "stopped"
        } else {
            "disabled"
        }
        .to_string(),
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
    let _ = app.emit(MANAGED_SERVER_EVENT, status_from(&managed, stored.as_ref()));
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
