use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
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
use crate::types::{
    ManagedServerRestartRequest, ManagedServerRootValidation, ManagedServerRootValidationRequest,
    ManagedServerStartRequest, ManagedServerState, ManagedServerStatus, ManagedServerToolCeiling,
};

const MANAGED_SERVER_CONFIG: &str = "managed-server.json";

#[derive(Debug, Clone)]
struct ManagedPairingTarget {
    agent_name: String,
    agent_did: String,
    graphql: String,
}

impl From<gents_server::server_host::ServerReady> for ManagedPairingTarget {
    fn from(ready: gents_server::server_host::ServerReady) -> Self {
        Self {
            agent_name: ready.agent_name,
            agent_did: ready.agent_did,
            graphql: ready.graphql,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredManagedServer {
    enabled: bool,
    agent_name: String,
    #[serde(default)]
    tool_ceiling: Option<ManagedServerToolCeiling>,
    #[serde(default)]
    tool_root: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EffectiveManagedAuthority {
    tool_ceiling: ManagedServerToolCeiling,
    tool_root: Option<PathBuf>,
}

impl EffectiveManagedAuthority {
    fn default_home() -> Result<Self, BridgeError> {
        let home = dirs::home_dir().ok_or_else(|| {
            BridgeError::new(
                BridgeErrorCode::ClientStartFailed,
                "unable to resolve the user home directory for managed-runtime tools",
            )
        })?;
        Ok(Self {
            tool_ceiling: ManagedServerToolCeiling::Readwrite,
            tool_root: Some(validate_tool_root(&home)?),
        })
    }

    fn from_request(
        tool_ceiling: ManagedServerToolCeiling,
        tool_root: Option<&str>,
    ) -> Result<Self, BridgeError> {
        let tool_root = match tool_ceiling {
            ManagedServerToolCeiling::MetaOnly => {
                if tool_root.is_some_and(|root| !root.trim().is_empty()) {
                    return Err(BridgeError::new(
                        BridgeErrorCode::InvalidArgument,
                        "toolRoot must be omitted when toolCeiling is meta-only",
                    ));
                }
                None
            }
            ManagedServerToolCeiling::Readonly | ManagedServerToolCeiling::Readwrite => {
                let root = tool_root
                    .filter(|root| !root.trim().is_empty())
                    .ok_or_else(|| {
                        BridgeError::new(
                            BridgeErrorCode::InvalidArgument,
                            "toolRoot is required when filesystem access is enabled",
                        )
                    })?;
                Some(validate_tool_root(Path::new(root))?)
            }
        };
        Ok(Self {
            tool_ceiling,
            tool_root,
        })
    }

    fn stored(&self) -> (ManagedServerToolCeiling, Option<String>) {
        (
            self.tool_ceiling,
            self.tool_root
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
        )
    }
}

fn validate_tool_root(path: &Path) -> Result<PathBuf, BridgeError> {
    if !path.is_absolute() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "Choose an absolute directory path.",
        ));
    }
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            format!("Cannot access {}: {error}", path.display()),
        )
    })?;
    if !canonical.is_dir() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            format!("{} is not a directory.", canonical.display()),
        ));
    }
    if canonical.parent().is_none() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "The filesystem root is too broad. Choose your home directory or a folder inside it.",
        ));
    }
    if let Some(home) = dirs::home_dir().and_then(|home| std::fs::canonicalize(home).ok()) {
        if home.starts_with(&canonical) && home != canonical {
            return Err(BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                format!(
                    "{} is broader than your home directory. Choose your home directory or a more specific folder.",
                    canonical.display()
                ),
            ));
        }
    }
    std::fs::read_dir(&canonical).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            format!("Cannot read {}: {error}", canonical.display()),
        )
    })?;
    Ok(canonical)
}

#[tauri::command]
pub async fn desktop_managed_server_validate_root(
    request: ManagedServerRootValidationRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ManagedServerRootValidation, BridgeError> {
    ensure_allowed(&state)?;
    let canonical = validate_tool_root(Path::new(request.path.trim()))?;
    Ok(ManagedServerRootValidation {
        canonical_path: canonical.to_string_lossy().into_owned(),
    })
}

#[tauri::command]
pub async fn desktop_managed_server_status(
    state: State<'_, DesktopAppState>,
) -> Result<ManagedServerStatus, BridgeError> {
    ensure_allowed(&state)?;
    let stored = load_preference(&state).await?;
    let managed = state.managed_server.lock().await;
    let mut status = status_from(&managed, stored.as_ref());
    drop(managed);

    // A preserved runtime may be owned by another process. Re-probe it on
    // every idle status read so onboarding retains its live DID while the
    // existing pairing owner converges. Without this, start can discover the
    // process but the next poll projects only the empty in-process handle.
    if should_probe_external_status(&status) {
        if let Some(agent_home) = state.policy.agent_home.as_deref() {
            if let Some(external) = matching_external_server(agent_home).await? {
                status = project_external_status(external, stored.as_ref());
            }
        }
    }
    status.pairing_ready = pairing_is_ready(&state, status.agent_did.as_deref()).await;
    Ok(status)
}

fn should_probe_external_status(status: &ManagedServerStatus) -> bool {
    matches!(
        status.state,
        ManagedServerState::Disabled | ManagedServerState::Stopped
    )
}

fn project_external_status(
    mut external: ManagedServerStatus,
    stored: Option<&StoredManagedServer>,
) -> ManagedServerStatus {
    external.auto_start = stored.is_some_and(|stored| stored.enabled);
    external
}

async fn pairing_is_ready(state: &DesktopAppState, agent_did: Option<&str>) -> bool {
    let (Some(core), Some(agent_did)) = (current_core(state), agent_did) else {
        return false;
    };
    core.peer_records().await.iter().any(|peer| {
        peer.agent_did == agent_did
            && peer.is_enrollment()
            && peer.is_managed_runtime()
            && peer.is_chat_ready_at(chrono::Utc::now())
    })
}

#[tauri::command]
pub async fn desktop_managed_server_start<R: Runtime>(
    app: AppHandle<R>,
    request: ManagedServerStartRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ManagedServerStatus, BridgeError> {
    ensure_allowed(&state)?;
    let _lifecycle = state.managed_server_lifecycle.lock().await;
    start_managed_server_locked(&app, request, &state).await
}

async fn start_managed_server_locked<R: Runtime>(
    app: &AppHandle<R>,
    request: ManagedServerStartRequest,
    state: &DesktopAppState,
) -> Result<ManagedServerStatus, BridgeError> {
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
    let stored = load_preference(state).await?;
    let authority = match request.tool_ceiling {
        Some(ceiling) => {
            EffectiveManagedAuthority::from_request(ceiling, request.tool_root.as_deref())?
        }
        None => match stored.as_ref().and_then(|stored| {
            stored
                .tool_ceiling
                .map(|ceiling| (ceiling, stored.tool_root.as_deref()))
        }) {
            Some((ceiling, root)) => EffectiveManagedAuthority::from_request(ceiling, root)?,
            None => EffectiveManagedAuthority::default_home()?,
        },
    };

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
            let running = authority_from_ready(&ready);
            if running != authority {
                return Err(BridgeError::new(
                    BridgeErrorCode::InvalidArgument,
                    "Changing the managed runtime root or authority requires a restart.",
                ));
            }
            let (tool_ceiling, tool_root) = authority.stored();
            let committed = StoredManagedServer {
                enabled: true,
                agent_name: agent_name.to_string(),
                tool_ceiling: Some(tool_ceiling),
                tool_root,
            };
            save_preference(state, &committed).await?;
            if let Some(core) = current_core(state) {
                start_managed_runtime_pairing(
                    state,
                    core,
                    agent_home.clone(),
                    ManagedPairingTarget::from(ready),
                )
                .await;
            }
            let managed = state.managed_server.lock().await;
            let mut status = status_from(&managed, Some(&committed));
            drop(managed);
            status.pairing_ready = pairing_is_ready(state, status.agent_did.as_deref()).await;
            return Ok(status);
        }
    }

    // Check our in-process handle before probing the port above. Once the
    // first onboarding call has started the managed server, its HTTP status
    // endpoint is indistinguishable from an externally launched server. The
    // second call intentionally commits auto-start after client provisioning;
    // probing first would return early and leave the preference disabled.
    if let Some(external) = matching_external_server(&agent_home).await? {
        let mut external = project_external_status(external, stored.as_ref());
        if external.effective_tool_ceiling != Some(authority.tool_ceiling)
            || external.effective_tool_root.as_deref()
                != authority
                    .tool_root
                    .as_ref()
                    .map(|path| path.to_string_lossy())
                    .as_deref()
        {
            return Err(BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                "The existing managed runtime does not match the reviewed root and authority.",
            ));
        }
        if let (Some(core), Some(target)) = (current_core(state), pairing_target(&external)) {
            core.refresh_local_standard_peer(&agent_home, &target.agent_name)
                .await
                .map_err(|error| BridgeError::untyped(error.to_string()))?;
            start_managed_runtime_pairing(&state, core, agent_home, target).await;
        }
        external.pairing_ready = pairing_is_ready(state, external.agent_did.as_deref()).await;
        return Ok(external);
    }

    {
        let mut managed = state.managed_server.lock().await;
        managed.starting = true;
        managed.last_error = None;
    }
    emit_status(app, state).await;

    let result: anyhow::Result<_> = async {
        gents_server::server_host::ensure_standard_home(
            gents_server::server_host::ProvisionOptions {
                home: agent_home.clone(),
                agent_name: agent_name.to_string(),
                tool_ceiling: authority.tool_ceiling.into(),
                tool_root: authority.tool_root.clone(),
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
            let (tool_ceiling, tool_root) = authority.stored();
            save_preference(
                state,
                &StoredManagedServer {
                    enabled: stored.is_some_and(|stored| stored.enabled),
                    agent_name: agent_name.to_string(),
                    tool_ceiling: Some(tool_ceiling),
                    tool_root,
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
            emit_status(app, state).await;
            return Err(BridgeError::untyped(message));
        }
    }

    emit_status(app, state).await;
    if let Some(core) = current_core(state) {
        start_running_managed_pairing(state, core).await;
    }
    let stored = load_preference(state).await?;
    let managed = state.managed_server.lock().await;
    let mut status = status_from(&managed, stored.as_ref());
    drop(managed);
    status.pairing_ready = pairing_is_ready(state, status.agent_did.as_deref()).await;
    Ok(status)
}

impl From<ManagedServerToolCeiling> for gents_server::server_host::ManagedToolCeiling {
    fn from(value: ManagedServerToolCeiling) -> Self {
        match value {
            ManagedServerToolCeiling::MetaOnly => Self::MetaOnly,
            ManagedServerToolCeiling::Readonly => Self::Readonly,
            ManagedServerToolCeiling::Readwrite => Self::Readwrite,
        }
    }
}

fn authority_from_ready(
    ready: &gents_server::server_host::ServerReady,
) -> EffectiveManagedAuthority {
    EffectiveManagedAuthority {
        tool_ceiling: match ready.tool_ceiling {
            gents_server::server_host::ManagedToolCeiling::MetaOnly => {
                ManagedServerToolCeiling::MetaOnly
            }
            gents_server::server_host::ManagedToolCeiling::Readonly => {
                ManagedServerToolCeiling::Readonly
            }
            gents_server::server_host::ManagedToolCeiling::Readwrite => {
                ManagedServerToolCeiling::Readwrite
            }
        },
        tool_root: ready.tool_root.as_ref().map(PathBuf::from),
    }
}

pub(super) async fn start_running_managed_pairing(state: &DesktopAppState, core: Arc<ClientCore>) {
    let Some(agent_home) = state.policy.agent_home.clone() else {
        tracing::warn!("managed pairing requires a local agent home");
        return;
    };
    let target = state
        .managed_server
        .lock()
        .await
        .server
        .as_ref()
        .map(|server| ManagedPairingTarget::from(server.ready().clone()));
    let (target, external) = match target {
        Some(target) => (Some(target), false),
        None => match matching_external_server(&agent_home).await {
            Ok(status) => (status.as_ref().and_then(pairing_target), true),
            Err(error) => {
                tracing::warn!(
                    target: "gents_desktop::managed_server",
                    error = %error,
                    "failed to inspect external managed runtime for background pairing"
                );
                (None, true)
            }
        },
    };
    let Some(target) = target else { return };
    if external {
        if let Err(error) = core
            .refresh_local_standard_peer(&agent_home, &target.agent_name)
            .await
        {
            tracing::warn!(
                target: "gents_desktop::managed_server",
                agent_did = %target.agent_did,
                error = %error,
                "failed to refresh external managed runtime route before pairing"
            );
            return;
        }
    }
    start_managed_runtime_pairing(state, core, agent_home, target).await;
}

async fn start_managed_runtime_pairing(
    state: &DesktopAppState,
    core: Arc<ClientCore>,
    agent_home: std::path::PathBuf,
    target: ManagedPairingTarget,
) {
    if core.peer_records().await.iter().any(|peer| {
        peer.agent_did == target.agent_did
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
        if let Err(error) = ensure_managed_runtime_pairing(core, &agent_home, &target).await {
            tracing::warn!(
                target: "gents_desktop::managed_server",
                agent_did = %target.agent_did,
                error = %error,
                "background managed runtime pairing failed"
            );
        }
    }));
}

async fn ensure_managed_runtime_pairing(
    core: Arc<ClientCore>,
    agent_home: &std::path::Path,
    target: &ManagedPairingTarget,
) -> Result<(), String> {
    if core.peer_records().await.iter().any(|peer| {
        peer.agent_did == target.agent_did
            && peer.is_enrollment()
            && peer.is_managed_runtime()
            && peer.is_chat_ready_at(chrono::Utc::now())
    }) {
        return Ok(());
    }

    let mut status_url = reqwest::Url::parse(&target.graphql)
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
        .request_status_enrollment_with_label(token, Some(&target.agent_name))
        .await
        .map_err(|error| format!("requesting managed runtime enrollment: {error:#}"))?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut approval_committed = false;
    loop {
        if core.peer_records().await.iter().any(|peer| {
            peer.agent_did == target.agent_did
                && peer.is_enrollment()
                && peer.is_managed_runtime()
                && peer.is_chat_ready_at(chrono::Utc::now())
        }) {
            tracing::info!(
                target: "gents_desktop::managed_server",
                agent_did = %target.agent_did,
                "managed runtime desktop pairing is ready"
            );
            return Ok(());
        }
        if !approval_committed {
            match gents_server::server_host::approve_managed_client_enrollment(
                agent_home,
                &target.graphql,
                &enrollment.request_id,
            )
            .await
            {
                Ok(()) => approval_committed = true,
                Err(error) => {
                    let message = format!("{error:#}");
                    if enrollment_request_is_not_visible_yet(&message) {
                        tracing::debug!(
                            request_id = %enrollment.request_id,
                            "managed enrollment request is not yet visible to operator approval"
                        );
                    } else {
                        return Err(format!("approving managed runtime enrollment: {message}"));
                    }
                }
            }
        }
        core.request_p2p_repair()
            .await
            .map_err(|error| format!("requesting managed route reconciliation: {error:#}"))?;
        if tokio::time::Instant::now() >= deadline {
            return Err("timed out waiting for managed runtime pairing".to_string());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn enrollment_request_is_not_visible_yet(message: &str) -> bool {
    message.contains("no fresh pending enrollment request")
}

fn pairing_target(status: &ManagedServerStatus) -> Option<ManagedPairingTarget> {
    Some(ManagedPairingTarget {
        agent_name: status.agent_name.as_deref()?.trim().to_string(),
        agent_did: status.agent_did.as_deref()?.trim().to_string(),
        graphql: status.graphql.as_deref()?.trim().to_string(),
    })
    .filter(|target| {
        !target.agent_name.is_empty() && !target.agent_did.is_empty() && !target.graphql.is_empty()
    })
}

pub(super) async fn drain_managed_runtime_pairing(state: &DesktopAppState) {
    let task = state.managed_server.lock().await.pairing_task.take();
    if let Some(task) = task {
        if let Err(error) = task.await {
            tracing::warn!(
                target: "gents_desktop::managed_server",
                error = %error,
                "managed pairing task failed while draining"
            );
        }
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
        effective_tool_ceiling: payload
            .get("tool_ceiling")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_tool_ceiling),
        effective_tool_root: payload
            .get("tool_root")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        suggested_tool_root: suggested_tool_root(),
        pairing_ready: false,
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
    stop_managed_server_locked(&app, disable_auto_start, &state).await
}

async fn stop_managed_server_locked<R: Runtime>(
    app: &AppHandle<R>,
    disable_auto_start: bool,
    state: &DesktopAppState,
) -> Result<ManagedServerStatus, BridgeError> {
    let server = {
        let mut managed = state.managed_server.lock().await;
        managed.starting = false;
        managed.last_error = None;
        managed.server.take()
    };
    drain_managed_runtime_pairing(&state).await;
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
    emit_status(app, state).await;
    let stored = load_preference(state).await?;
    let managed = state.managed_server.lock().await;
    Ok(status_from(&managed, stored.as_ref()))
}

#[tauri::command]
pub async fn desktop_managed_server_restart<R: Runtime>(
    app: AppHandle<R>,
    request: ManagedServerRestartRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ManagedServerStatus, BridgeError> {
    ensure_allowed(&state)?;
    let _lifecycle = state.managed_server_lifecycle.lock().await;
    let authority = EffectiveManagedAuthority::from_request(
        request.tool_ceiling,
        request.tool_root.as_deref(),
    )?;
    let previous_did = state
        .managed_server
        .lock()
        .await
        .server
        .as_ref()
        .map(|server| server.ready().agent_did.clone());
    if let (Some(core), Some(agent_did)) = (current_core(&state), previous_did.as_deref()) {
        core.mark_managed_runtime_restarting(agent_did)
            .await
            .map_err(|error| BridgeError::untyped(error.to_string()))?;
    }
    stop_managed_server_locked(&app, false, &state).await?;
    let (tool_ceiling, tool_root) = authority.stored();
    start_managed_server_locked(
        &app,
        ManagedServerStartRequest {
            agent_name: request.agent_name,
            tool_ceiling: Some(tool_ceiling),
            tool_root,
        },
        &state,
    )
    .await
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
    let effective = ready.map(authority_from_ready);
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
        effective_tool_ceiling: effective.as_ref().map(|value| value.tool_ceiling),
        effective_tool_root: effective.as_ref().and_then(|value| {
            value
                .tool_root
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
        }),
        suggested_tool_root: suggested_tool_root(),
        pairing_ready: false,
        error: managed.last_error.clone(),
    }
}

fn suggested_tool_root() -> Option<String> {
    dirs::home_dir()
        .and_then(|path| std::fs::canonicalize(path).ok())
        .map(|path| path.to_string_lossy().into_owned())
}

fn parse_tool_ceiling(value: &str) -> Option<ManagedServerToolCeiling> {
    match value {
        "meta-only" => Some(ManagedServerToolCeiling::MetaOnly),
        "readonly" => Some(ManagedServerToolCeiling::Readonly),
        "readwrite" => Some(ManagedServerToolCeiling::Readwrite),
        _ => None,
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
            tool_ceiling: Some(ManagedServerToolCeiling::Readwrite),
            tool_root: Some("/Users/test".to_string()),
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
    fn idle_status_reprobes_and_preserves_an_external_runtime_identity() {
        let stored = StoredManagedServer {
            enabled: true,
            agent_name: "local".to_string(),
            tool_ceiling: Some(ManagedServerToolCeiling::Readwrite),
            tool_root: Some("/Users/test".to_string()),
        };
        let idle = status_from(&ManagedServerRuntimeState::default(), Some(&stored));
        assert!(should_probe_external_status(&idle));

        let external = project_external_status(
            ManagedServerStatus {
                state: ManagedServerState::External,
                auto_start: false,
                agent_name: Some("local".to_string()),
                agent_did: Some("did:key:preserved".to_string()),
                graphql: Some("http://127.0.0.1:9191/graphql".to_string()),
                effective_tool_ceiling: Some(ManagedServerToolCeiling::Readwrite),
                effective_tool_root: Some("/Users/test".to_string()),
                suggested_tool_root: Some("/Users/test".to_string()),
                pairing_ready: false,
                error: None,
            },
            Some(&stored),
        );

        assert_eq!(external.state, ManagedServerState::External);
        assert_eq!(external.agent_did.as_deref(), Some("did:key:preserved"));
        assert!(external.auto_start);
        assert!(!should_probe_external_status(&external));
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

    #[test]
    fn request_visibility_race_is_retriable() {
        assert!(enrollment_request_is_not_visible_yet(
            "runtime rejected enrollment decision (400 Bad Request): no fresh pending enrollment request enroll-1"
        ));
        assert!(!enrollment_request_is_not_visible_yet(
            "runtime rejected enrollment decision (403 Forbidden): invalid operator signature"
        ));
    }

    #[test]
    fn restart_command_validates_reviewed_authority_through_tauri_ipc() {
        use crate::config::{
            AgentHomePolicy, AppMeta, BootstrapPolicy, BridgeConfig, HomePolicy,
            ManagedServerPolicy,
        };
        use crate::snapshot::projection::SnapshotGrants;
        use crate::state::{resolve_policy, DesktopAppState};
        use tauri::ipc::InvokeBody;
        use tauri::webview::InvokeRequest;

        let temp = tempfile::tempdir().expect("temporary bridge roots");
        let policy = resolve_policy(
            &BridgeConfig {
                home: HomePolicy::FixedRoot(temp.path().join("desktop")),
                bootstrap: BootstrapPolicy::LocalRuntimeAllowed {
                    agent_home: AgentHomePolicy::Fixed(temp.path().join("agent")),
                },
                app_meta: AppMeta {
                    app_name: "managed-restart-test".to_string(),
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                },
                snapshot_grants: SnapshotGrants::core_only(),
                managed_server: ManagedServerPolicy::Allowed,
            },
            None,
        )
        .expect("fixed bridge policy");
        let app = tauri::test::mock_builder()
            .manage(DesktopAppState::new(policy))
            .invoke_handler(tauri::generate_handler![desktop_managed_server_restart])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock desktop bridge app");
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("mock webview");
        let response = tauri::test::get_ipc_response(
            &webview,
            InvokeRequest {
                cmd: "desktop_managed_server_restart".into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: "http://tauri.localhost".parse().expect("invoke URL"),
                body: InvokeBody::Json(serde_json::json!({
                    "request": {
                        "agentName": "Workshop Agent",
                        "toolCeiling": "readwrite",
                        "toolRoot": "relative/path"
                    }
                })),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.to_string(),
            },
        )
        .expect_err("an unvalidated restart root must be rejected");

        let serialized = response.to_string();
        assert!(
            serialized.contains("absolute directory"),
            "restart IPC returned an unexpected error: {response:?}"
        );
    }

    #[test]
    fn root_validation_canonicalizes_symlinks_and_preserves_spaces() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("a folder");
        std::fs::create_dir(&real).unwrap();
        #[cfg(unix)]
        {
            let alias = temp.path().join("alias");
            std::os::unix::fs::symlink(&real, &alias).unwrap();
            assert_eq!(
                validate_tool_root(&alias).unwrap(),
                std::fs::canonicalize(&real).unwrap()
            );
        }
        assert_eq!(
            validate_tool_root(&real).unwrap(),
            std::fs::canonicalize(&real).unwrap()
        );
    }

    #[test]
    fn root_validation_rejects_relative_missing_and_filesystem_root() {
        assert!(validate_tool_root(Path::new("relative")).is_err());
        let temp = tempfile::tempdir().unwrap();
        assert!(validate_tool_root(&temp.path().join("missing")).is_err());
        let root = temp.path().ancestors().last().unwrap();
        assert!(validate_tool_root(root).is_err());
    }

    #[test]
    fn authority_requires_exact_root_shape_for_the_ceiling() {
        assert!(
            EffectiveManagedAuthority::from_request(ManagedServerToolCeiling::Readwrite, None,)
                .is_err()
        );
        assert!(EffectiveManagedAuthority::from_request(
            ManagedServerToolCeiling::MetaOnly,
            Some("/tmp"),
        )
        .is_err());
        assert_eq!(
            EffectiveManagedAuthority::from_request(ManagedServerToolCeiling::MetaOnly, None,)
                .unwrap(),
            EffectiveManagedAuthority {
                tool_ceiling: ManagedServerToolCeiling::MetaOnly,
                tool_root: None,
            }
        );
    }
}
