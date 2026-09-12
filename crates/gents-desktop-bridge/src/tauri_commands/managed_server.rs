use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Runtime, State};

use gents_desktop_core::client::ClientCore;
use gents_desktop_core::local_runtime::{
    fetch_runtime_connection_payload, init_standard_local_runtime, DesktopInitOptions,
};

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
            let ready = managed
                .server
                .as_ref()
                .expect("managed server checked above")
                .ready()
                .clone();
            drop(managed);
            let committed = StoredManagedServer {
                enabled: true,
                agent_name: agent_name.to_string(),
            };
            save_preference(&state, &committed).await?;
            if let Some(core) = current_core(&state) {
                start_managed_runtime_pairing(&state, core, agent_home.clone(), ready).await;
            }
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

    // The managed desktop binary is the host authority for local work. First
    // run gives that binary the full existing read/write ceiling, rooted at
    // the user's home. The selected Tools document still decides which
    // capabilities a behavior actually receives.
    let tool_root = dirs::home_dir().ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::ClientStartFailed,
            "unable to resolve the user home directory for first-run tools",
        )
    })?;
    let result: anyhow::Result<_> = async {
        gents_server::server_host::ensure_standard_home(
            gents_server::server_host::ProvisionOptions {
                home: agent_home.clone(),
                agent_name: agent_name.to_string(),
                tool_root,
            },
        )
        .await?;
        let mut config = gents_server::server_host::ServerConfig::standard(agent_home.clone());
        config.http_port = first_free_http_port(config.http_port)?;
        let server = gents_server::server_host::start_server(config).await?;

        // Iroh shareable addresses include the process's ephemeral QUIC port.
        // Refresh the persisted local peer after every managed-server start so
        // desktop client startup never dials the previous process's endpoint.
        // The start result is the readiness authority; re-querying the new HTTP
        // server here races its auxiliary P2P endpoints and can drop an otherwise
        // healthy server handle during first-run startup.
        let _client_lifecycle = state.client_lifecycle.lock().await;
        if let Some(core) = current_core(&state) {
            let ready = server.ready();
            let p2p_address = ready
                .p2p_listen_addresses
                .iter()
                .find(|address| address.starts_with("endpoint"))
                .or_else(|| ready.p2p_listen_addresses.first())
                .ok_or_else(|| {
                    anyhow::anyhow!("managed Gents server readiness omitted a P2P listen address")
                })?;
            core.persist_local_standard_peer(
                agent_name,
                p2p_address,
                &ready.agent_did,
                &ready.graphql,
                &agent_home.display().to_string(),
            )
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
            tracing::warn!(error = %message, "managed Gents server start failed");
            let mut managed = state.managed_server.lock().await;
            managed.starting = false;
            managed.last_error = Some(message.clone());
            drop(managed);
            emit_status(&app, &state).await;
            return Err(BridgeError::untyped(message));
        }
    }

    emit_status(&app, &state).await;
    if let Some(core) = current_core(&state) {
        start_running_managed_pairing(&state, core).await;
    }
    drop(_lifecycle);
    desktop_managed_server_status(state).await
}

pub(super) async fn start_running_managed_pairing(state: &DesktopAppState, core: Arc<ClientCore>) {
    let ready = state
        .managed_server
        .lock()
        .await
        .server
        .as_ref()
        .map(|server| server.ready().clone());
    let Some(ready) = ready else {
        return;
    };
    let Some(agent_home) = state.policy.agent_home.clone() else {
        tracing::warn!("managed pairing requires a local agent home");
        return;
    };
    start_managed_runtime_pairing(state, core, agent_home, ready).await;
}

async fn start_managed_runtime_pairing(
    state: &DesktopAppState,
    core: Arc<ClientCore>,
    agent_home: std::path::PathBuf,
    ready: gents_server::server_host::ServerReady,
) {
    if core.peer_records().await.iter().any(|peer| {
        peer.agent_did == ready.agent_did
            && peer.is_enrollment()
            && peer.is_managed_runtime()
            && peer.is_chat_ready_at(chrono::Utc::now())
    }) {
        return;
    }

    let mut managed = state.managed_server.lock().await;
    if managed
        .pairing_task
        .as_ref()
        .is_some_and(|task| !task.inner().is_finished())
    {
        return;
    }
    managed.pairing_task = Some(tauri::async_runtime::spawn(async move {
        if let Err(error) = ensure_managed_runtime_pairing(core, &agent_home, &ready).await {
            tracing::warn!(
                target: "gents_desktop::managed_server",
                agent_did = %ready.agent_did,
                error = %error,
                "background managed runtime pairing failed"
            );
        }
    }));
}

async fn ensure_managed_runtime_pairing(
    core: Arc<ClientCore>,
    agent_home: &std::path::Path,
    ready: &gents_server::server_host::ServerReady,
) -> Result<(), String> {
    if core.peer_records().await.iter().any(|peer| {
        peer.agent_did == ready.agent_did
            && peer.is_enrollment()
            && peer.is_managed_runtime()
            && peer.is_chat_ready_at(chrono::Utc::now())
    }) {
        return Ok(());
    }

    let mut status_url = reqwest::Url::parse(&ready.graphql)
        .map_err(|error| format!("parsing managed runtime GraphQL URL: {error}"))?;
    status_url.set_path("/status");
    status_url.set_query(None);
    status_url.set_fragment(None);
    let status = fetch_runtime_connection_payload(status_url.as_str())
        .await
        .map_err(|error| format!("loading managed runtime enrollment offer: {error:#}"))?;
    let token = status
        .pointer("/enrollment/token")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| "managed runtime did not advertise an enrollment offer".to_string())?;
    let enrollment = core
        .request_status_enrollment_with_label(token, Some(&ready.agent_name))
        .await
        .map_err(|error| format!("requesting managed runtime enrollment: {error:#}"))?;
    gents_server::server_host::approve_managed_client_enrollment(
        agent_home,
        &ready.graphql,
        &enrollment.request_id,
    )
    .await
    .map_err(|error| format!("approving managed runtime enrollment: {error:#}"))?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        core.request_p2p_repair()
            .await
            .map_err(|error| format!("requesting managed route reconciliation: {error:#}"))?;
        if core.peer_records().await.iter().any(|peer| {
            peer.agent_did == ready.agent_did
                && peer.is_enrollment()
                && peer.is_managed_runtime()
                && peer.is_chat_ready_at(chrono::Utc::now())
        }) {
            tracing::info!(
                target: "gents_desktop::managed_server",
                agent_did = %ready.agent_did,
                "managed runtime desktop pairing is ready"
            );
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("timed out waiting for managed runtime pairing".to_string());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
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
    // A process on the default port is only *our* managed server when this
    // home is already initialized as that identity. A fresh first-run home
    // must not adopt a neighbor's `gents server` and then fail reading
    // init.json; identity mismatch also means we should bind another port.
    if !gents_server::server_host::initialized_home(agent_home) {
        return Ok(None);
    }
    let initialized_did = read_initialized_did(agent_home).await;
    if ensure_matching_identity(initialized_did.as_deref(), live_did, config.http_port).is_err() {
        return Ok(None);
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
        if let Some(task) = managed.pairing_task.take() {
            task.abort();
        }
        managed.server.take()
    };
    if let Some(server) = server {
        server
            .shutdown()
            .await
            .map_err(|error| BridgeError::untyped(error.to_string()))?;
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

fn first_free_http_port(preferred: u16) -> anyhow::Result<u16> {
    let extras = [9291_u16, 9391, 9491, 9591, 9691, 9791, 9891];
    for port in
        std::iter::once(preferred).chain(extras.into_iter().filter(|port| *port != preferred))
    {
        if port_is_free(port) {
            return Ok(port);
        }
    }
    anyhow::bail!(
        "no free local port for the hosted agent (tried {preferred} and 9291–9891). Stop the other Gents server occupying those ports and try again."
    )
}

fn port_is_free(port: u16) -> bool {
    // SO_REUSEADDR lets a second bind to 127.0.0.1 succeed while another
    // process already listens on *:port. Probe with connect first.
    if std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(150),
    )
    .is_ok()
    {
        return false;
    }
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
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
            .map_err(|error| BridgeError::untyped(error.to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(BridgeError::untyped(error.to_string())),
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
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let path = state
        .policy
        .desktop_paths
        .root()
        .join(MANAGED_SERVER_CONFIG);
    let bytes = serde_json::to_vec_pretty(stored)
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    tokio::fs::write(path, bytes)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
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

    #[test]
    fn first_free_http_port_skips_a_bound_preferred_port() {
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral bind");
        let preferred = occupied.local_addr().expect("local addr").port();
        let chosen = first_free_http_port(preferred).expect("fallback port");
        assert_ne!(chosen, preferred);
    }
}
