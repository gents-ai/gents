use serde::{Deserialize, Serialize};
use std::future::Future;
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
use crate::tauri_commands::service_executable::resolve_service_executable;
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

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredManagedServer {
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
pub async fn desktop_managed_server_status<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, DesktopAppState>,
) -> Result<ManagedServerStatus, BridgeError> {
    let status = managed_server_status_for(&app, &state).await?;
    let _ = app.emit(MANAGED_SERVER_UPDATED_EVENT, status.clone());
    Ok(status)
}

async fn observe_managed_server_status<R: Runtime>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<ManagedServerStatus, BridgeError> {
    let stored = load_preference(state).await?;
    let native = run_native(native_service(&app, &state)?, |service| service.status()).await?;
    let managed = state.managed_server.lock().await;
    let mut status = status_from(&managed, stored.as_ref(), Some(&native));
    drop(managed);

    // Native process state is not runtime readiness. Probe the status endpoint
    // on every idle/starting read so the frontend gets the live DID and route.
    if should_probe_external_status(&status) {
        if let Some(agent_home) = state.policy.agent_home.as_deref() {
            if let Some(external) = matching_external_server(agent_home).await? {
                status = project_external_status(external, &native);
            }
        }
    }
    status.pairing_ready = pairing_is_ready(state, status.agent_did.as_deref()).await;
    Ok(status)
}

/// Canonical managed-service and runtime-readiness observation shared by the
/// status command and native desktop consumers such as DB Explorer.
pub(crate) async fn managed_server_status_for<R: Runtime>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<ManagedServerStatus, BridgeError> {
    ensure_allowed(state)?;
    observe_managed_server_status(app, state).await
}

fn should_probe_external_status(_status: &ManagedServerStatus) -> bool {
    // Endpoint readiness is authoritative over stale bridge-local errors and
    // native process state. The probe is a bounded single HTTP observation.
    true
}

fn project_external_status(
    mut external: ManagedServerStatus,
    native: &gents_server::native_service::NativeServiceStatus,
) -> ManagedServerStatus {
    external.auto_start = native.enabled;
    if native.job_loaded {
        external.state = ManagedServerState::Running;
    }
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
            None => {
                return Err(BridgeError::new(
                    BridgeErrorCode::InvalidArgument,
                    "Complete local agent setup and review host access before starting the agent.",
                ));
            }
        },
    };

    if let Some(external) = matching_external_server(&agent_home).await? {
        let native = run_native(native_service(app, state)?, |service| service.status()).await?;
        let mut external = project_external_status(external, &native);
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

    let initial_native =
        run_native(native_service(app, state)?, |service| service.status()).await?;
    if initial_native.is_active_or_transitioning() {
        return Err(BridgeError::new(
            BridgeErrorCode::EndpointUnreachable,
            "The native agent service is running or transitioning but has not published runtime readiness. Wait and try again, or use Restart Agent if it does not become ready.",
        ));
    }
    let initial_enabled = initial_native.enabled;

    {
        let mut managed = state.managed_server.lock().await;
        managed.starting = true;
        managed.last_error = None;
    }
    emit_status(app, state).await;

    let mut attempted_native_start = false;
    let result: anyhow::Result<()> = async {
        ensure_default_port_identity(&agent_home).await?;
        gents_server::server_host::ensure_standard_home(
            gents_server::server_host::ProvisionOptions {
                home: agent_home.clone(),
                agent_name: agent_name.to_string(),
                tool_ceiling: authority.tool_ceiling.into(),
                tool_root: authority.tool_root.clone(),
            },
        )
        .await?;
        let (tool_ceiling, tool_root) = authority.stored();
        save_preference(
            state,
            &StoredManagedServer {
                agent_name: agent_name.to_string(),
                tool_ceiling: Some(tool_ceiling),
                tool_root,
            },
        )
        .await?;
        ensure_default_port_identity(&agent_home).await?;
        // Install even when a unit file already exists. `install` leaves a
        // matching definition alone and refuses while the service is active.
        // A stopped unit can still name a previous mount or runtime.
        run_native(launchable_native_service(app, state)?, |service| {
            service.install()
        })
        .await?;
        // A start can launch the process and then fail restoring login state.
        // Roll back the owned attempt even when that final native step fails.
        attempted_native_start = true;
        run_native(launchable_native_service(app, state)?, |service| {
            service.start(false)
        })
        .await?;
        let ready = wait_for_managed_server(&agent_home).await?;
        validate_ready_runtime(&ready, &authority, &agent_home)?;
        if current_core(state).is_none() {
            init_standard_local_runtime(DesktopInitOptions {
                agent_home: agent_home.clone(),
                desktop_paths: state.policy.desktop_paths.clone(),
                label: agent_name.to_string(),
            })
            .await?;
        }
        Ok(())
    }
    .await;

    match result {
        Ok(()) => state.managed_server.lock().await.starting = false,
        Err(error) => {
            let typed_error = error.downcast_ref::<BridgeError>().cloned();
            let mut message = format!("{error:#}");
            if attempted_native_start {
                let cleanup = match native_service(app, state) {
                    Ok(service) => {
                        run_native(service, move |service| service.stop(!initial_enabled)).await
                    }
                    Err(error) => Err(error),
                };
                if let Err(cleanup) = cleanup {
                    message = combine_cleanup_error(
                        message,
                        "newly started native service",
                        cleanup.message,
                    );
                }
            }
            tracing::warn!(error = %message, "managed Gents server start failed");
            let mut managed = state.managed_server.lock().await;
            managed.starting = false;
            managed.last_error = Some(message.clone());
            drop(managed);
            emit_status(app, state).await;
            return Err(match typed_error {
                Some(mut error) => {
                    error.message = message;
                    error
                }
                None => BridgeError::untyped(message),
            });
        }
    }

    emit_status(app, state).await;
    if let Some(core) = current_core(state) {
        start_running_managed_pairing(state, core).await;
    }
    let stored = load_preference(state).await?;
    let native = run_native(native_service(app, state)?, |service| service.status()).await?;
    let mut status = if let Some(external) = matching_external_server(&agent_home).await? {
        project_external_status(external, &native)
    } else {
        let managed = state.managed_server.lock().await;
        status_from(&managed, stored.as_ref(), Some(&native))
    };
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

const MANAGED_SERVER_READY_TIMEOUT: Duration = Duration::from_secs(120);

async fn wait_for_managed_server(agent_home: &Path) -> anyhow::Result<ManagedServerStatus> {
    let deadline = tokio::time::Instant::now() + MANAGED_SERVER_READY_TIMEOUT;
    loop {
        match observe_port_readiness(agent_home).await? {
            PortReadiness::Ready(status) => return Ok(status),
            PortReadiness::Foreign { port, live_did } => {
                let who = if live_did.trim().is_empty() {
                    "a listener that did not advertise an identity".to_string()
                } else {
                    format!("a different Gents identity ({live_did})")
                };
                anyhow::bail!(
                    "port {port} is already in use by {who}. Stop that server before starting the managed agent."
                );
            }
            PortReadiness::NotListening => {}
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "native Gents service started, but it did not publish runtime readiness within {} seconds",
                MANAGED_SERVER_READY_TIMEOUT.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

enum PortReadiness {
    Ready(ManagedServerStatus),
    Foreign { port: u16, live_did: String },
    NotListening,
}

async fn observe_port_readiness(
    agent_home: &std::path::Path,
) -> Result<PortReadiness, BridgeError> {
    let config = gents_server::server_host::ServerConfig::standard(agent_home.to_path_buf());
    let Some(payload) = default_port_payload(Some(agent_home)).await? else {
        return Ok(PortReadiness::NotListening);
    };
    let live_did = payload
        .get("agent_did")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !gents_server::server_host::initialized_home(agent_home) {
        return Ok(PortReadiness::NotListening);
    }
    let initialized_did = read_initialized_did(agent_home).await;
    if ensure_matching_identity(initialized_did.as_deref(), &live_did, config.http_port).is_err() {
        return Ok(PortReadiness::Foreign {
            port: config.http_port,
            live_did,
        });
    }
    Ok(PortReadiness::Ready(managed_status_from_payload(
        payload, &live_did,
    )))
}

fn validate_ready_runtime(
    status: &ManagedServerStatus,
    authority: &EffectiveManagedAuthority,
    agent_home: &Path,
) -> anyhow::Result<()> {
    let expected_did = std::fs::read(agent_home.join("init.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("agent_did")
                .and_then(serde_json::Value::as_str)
                .filter(|did| !did.trim().is_empty())
                .map(str::to_owned)
        });
    let expected_root = authority
        .tool_root
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned());
    if expected_did.is_none()
        || status.agent_did.as_deref() != expected_did.as_deref()
        || status.effective_tool_ceiling != Some(authority.tool_ceiling)
        || status.effective_tool_root.as_deref() != expected_root.as_deref()
    {
        anyhow::bail!(
            "native runtime readiness did not match the initialized identity and reviewed host authority (live did={:?} ceiling={:?} root={:?}; expected did={:?} ceiling={:?} root={:?})",
            status.agent_did,
            status.effective_tool_ceiling,
            status.effective_tool_root,
            expected_did,
            Some(authority.tool_ceiling),
            expected_root
        );
    }
    Ok(())
}

fn native_service<R: Runtime>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<gents_server::native_service::NativeServiceManager, BridgeError> {
    build_native_service(app, state, false)
}

/// The service manager to use before a definition is installed or started. A
/// packaged build copies its runtime out of the temporary mount first, so the
/// service keeps an executable once the desktop app and its mount are gone.
fn launchable_native_service<R: Runtime>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<gents_server::native_service::NativeServiceManager, BridgeError> {
    build_native_service(app, state, true)
}

/// Brings a packaged install up to date at startup. It refreshes the runtime
/// copied out of the package, then reconciles an already-installed definition
/// with the one this version writes, so an upgraded application does not leave
/// a service pointing at the previous release's runtime or environment.
///
/// It never installs a service the user has not chosen, and never starts or
/// stops an agent. A running agent keeps the definition it launched with.
/// Start and restart install the current definition before they launch it.
pub(crate) fn refresh_packaged_install<R: Runtime>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<(), BridgeError> {
    if state.policy.agent_home.is_none() {
        return Ok(());
    }
    // The setup thread is outside the async runtime. Waiting here keeps a
    // start or restart from launching the definition this refresh replaces.
    let _lifecycle = state.managed_server_lifecycle.blocking_lock();
    let executable = resolve_service_executable(app, state.policy.desktop_paths.root())?;
    if executable.install()? {
        tracing::info!(
            target: "gents_desktop::managed_server",
            runtime = %executable.service_path().display(),
            "refreshed the Gents runtime from the application package"
        );
    }
    let service = native_service(app, state)?;
    let status = service.status().map_err(native_error)?;
    if !status.installed {
        return Ok(());
    }
    if status.is_active_or_transitioning() {
        // `install` refuses a live Linux job, and on macOS it bootouts a
        // loaded job whose definition differs. Leave that job alone.
        tracing::info!(
            target: "gents_desktop::managed_server",
            "agent restart pending before the updated service definition applies"
        );
        return Ok(());
    }
    service.install().map_err(native_error)
}

fn build_native_service<R: Runtime>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
    install_runtime: bool,
) -> Result<gents_server::native_service::NativeServiceManager, BridgeError> {
    let home = state.policy.agent_home.clone().ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::Unsupported,
            "managed server requires a local agent home",
        )
    })?;
    let executable = resolve_service_executable(app, state.policy.desktop_paths.root())?;
    if install_runtime {
        executable.install()?;
    }
    let mut config = gents_server::native_service::NativeServiceConfig::new(
        home,
        executable.service_path().to_path_buf(),
    )
    .map_err(native_error)?;
    // Login Items shows the code-signing personal name unless the agent
    // names the desktop bundle that installed it.
    let bundle_id = app.config().identifier.clone();
    if !bundle_id.trim().is_empty() {
        config.associated_bundle_id = Some(bundle_id);
    }
    gents_server::native_service::NativeServiceManager::new(config).map_err(native_error)
}

fn native_error(error: anyhow::Error) -> BridgeError {
    let message = format!("{error:#}");
    let code = if message.contains("supported only on macOS and Linux") {
        BridgeErrorCode::Unsupported
    } else {
        BridgeErrorCode::Backend
    };
    BridgeError::new(code, message)
}

async fn run_native<T, F>(
    service: gents_server::native_service::NativeServiceManager,
    operation: F,
) -> Result<T, BridgeError>
where
    T: Send + 'static,
    F: FnOnce(gents_server::native_service::NativeServiceManager) -> anyhow::Result<T>
        + Send
        + 'static,
{
    tokio::task::spawn_blocking(move || operation(service))
        .await
        .map_err(|error| BridgeError::untyped(format!("native service task failed: {error}")))?
        .map_err(native_error)
}

pub(super) async fn start_running_managed_pairing(state: &DesktopAppState, core: Arc<ClientCore>) {
    let Some(agent_home) = state.policy.agent_home.clone() else {
        tracing::warn!("managed pairing requires a local agent home");
        return;
    };
    let target = match matching_external_server(&agent_home).await {
        Ok(status) => status.as_ref().and_then(pairing_target),
        Err(error) => {
            tracing::warn!(
                target: "gents_desktop::managed_server",
                error = %error,
                "failed to inspect native managed runtime for background pairing"
            );
            None
        }
    };
    let Some(target) = target else { return };
    if let Err(error) = core
        .refresh_local_standard_peer(&agent_home, &target.agent_name)
        .await
    {
        tracing::warn!(
            target: "gents_desktop::managed_server",
            agent_did = %target.agent_did,
            error = %error,
            "failed to refresh native managed runtime route before pairing"
        );
        return;
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
    let Some(payload) = default_port_payload(Some(agent_home)).await? else {
        return Ok(None);
    };
    let live_did = payload
        .get("agent_did")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    // A process on the default port is only *our* managed server when this
    // home is already initialized as that identity. A fresh first-run home
    // must not adopt a neighbor's `gents server` and then fail reading
    // init.json. Start separately rejects a foreign identity on the fixed port.
    if !gents_server::server_host::initialized_home(agent_home) {
        return Ok(None);
    }
    let initialized_did = read_initialized_did(agent_home).await;
    if ensure_matching_identity(initialized_did.as_deref(), &live_did, config.http_port).is_err() {
        return Ok(None);
    }
    Ok(Some(managed_status_from_payload(payload, &live_did)))
}

fn managed_status_from_payload(payload: serde_json::Value, live_did: &str) -> ManagedServerStatus {
    ManagedServerStatus {
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
    }
}

async fn default_port_payload(
    agent_home: Option<&Path>,
) -> Result<Option<serde_json::Value>, BridgeError> {
    let Some(agent_home) = agent_home else {
        return Ok(None);
    };
    let config = gents_server::server_host::ServerConfig::standard(agent_home.to_path_buf());
    match gents_desktop_core::local_runtime::fetch_runtime_connection_payload(&config.status_url())
        .await
    {
        Ok(payload) => Ok(Some(payload)),
        Err(_) => Ok(None),
    }
}

async fn ensure_default_port_identity(agent_home: &Path) -> Result<(), BridgeError> {
    let Some(payload) = default_port_payload(Some(agent_home)).await? else {
        return Ok(());
    };
    let live_did = payload
        .get("agent_did")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let initialized_did = read_initialized_did(agent_home).await;
    ensure_matching_identity(
        initialized_did.as_deref(),
        live_did,
        gents_server::server_host::ServerConfig::standard(agent_home.to_path_buf()).http_port,
    )
}

fn ensure_native_owns_running_endpoint(
    endpoint_running: bool,
    native_job_loaded: bool,
) -> Result<(), BridgeError> {
    if endpoint_running && !native_job_loaded {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "The running local agent is not owned by the native Gents service. Stop it explicitly before using managed controls.",
        ));
    }
    Ok(())
}

fn combine_cleanup_error(original: String, owner: &str, cleanup: String) -> String {
    format!("{original}; additionally failed to stop the {owner}: {cleanup}")
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

#[tauri::command]
pub async fn desktop_managed_server_set_auto_start<R: Runtime>(
    app: AppHandle<R>,
    enabled: bool,
    state: State<'_, DesktopAppState>,
) -> Result<ManagedServerStatus, BridgeError> {
    ensure_allowed(&state)?;
    let _lifecycle = state.managed_server_lifecycle.lock().await;
    if let Err(error) = run_native(native_service(&app, &state)?, move |service| {
        service.set_enabled(enabled)
    })
    .await
    {
        state.managed_server.lock().await.last_error = Some(error.message.clone());
        emit_status(&app, &state).await;
        return Err(error);
    }
    drop(_lifecycle);
    desktop_managed_server_status(app, state).await
}

async fn stop_managed_server_locked<R: Runtime>(
    app: &AppHandle<R>,
    disable_auto_start: bool,
    state: &DesktopAppState,
) -> Result<ManagedServerStatus, BridgeError> {
    {
        let mut managed = state.managed_server.lock().await;
        managed.starting = false;
        managed.last_error = None;
    }
    let endpoint_running = default_port_payload(state.policy.agent_home.as_deref())
        .await?
        .is_some();
    let native = run_native(native_service(app, state)?, |service| service.status()).await?;
    if let Some(agent_home) = state.policy.agent_home.as_deref() {
        ensure_default_port_identity(agent_home).await?;
    }
    ensure_native_owns_running_endpoint(endpoint_running, native.job_loaded)?;
    drain_managed_runtime_pairing(&state).await;
    if let Err(error) = run_native(native_service(app, state)?, move |service| {
        service.stop(disable_auto_start)
    })
    .await
    {
        state.managed_server.lock().await.last_error = Some(error.message.clone());
        emit_status(app, state).await;
        return Err(error);
    }
    emit_status(app, state).await;
    observe_managed_server_status(app, state).await
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
    let agent_home = state.policy.agent_home.clone().ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::Unsupported,
            "managed server requires a local agent home",
        )
    })?;
    let endpoint_running = default_port_payload(Some(&agent_home)).await?.is_some();
    let previous_did = matching_external_server(&agent_home)
        .await?
        .and_then(|status| status.agent_did);
    let (tool_ceiling, tool_root) = authority.stored();
    let service_status =
        match run_native(native_service(&app, &state)?, |service| service.status()).await {
            Ok(status) => status,
            Err(error) => {
                state.managed_server.lock().await.last_error = Some(error.message.clone());
                emit_status(&app, &state).await;
                return Err(error);
            }
        };
    ensure_default_port_identity(&agent_home).await?;
    ensure_native_owns_running_endpoint(endpoint_running, service_status.job_loaded)?;
    drain_managed_runtime_pairing(&state).await;
    let was_enabled = service_status.enabled;
    let provision = gents_server::server_host::ProvisionOptions {
        home: agent_home.clone(),
        agent_name: request.agent_name.clone(),
        tool_ceiling: tool_ceiling.into(),
        tool_root: authority.tool_root.clone(),
    };
    if let Err(error) = stop_before_reprovision(
        || async { run_native(native_service(&app, &state)?, |service| service.stop(false)).await },
        || async {
            gents_server::server_host::ensure_standard_home(provision)
                .await
                .map_err(|error| BridgeError::untyped(format!("{error:#}")))
        },
        || async {
            save_preference(
                &state,
                &StoredManagedServer {
                    agent_name: request.agent_name.clone(),
                    tool_ceiling: Some(tool_ceiling),
                    tool_root: tool_root.clone(),
                },
            )
            .await
        },
    )
    .await
    {
        state.managed_server.lock().await.last_error = Some(error.message.clone());
        emit_status(&app, &state).await;
        return Err(error);
    }
    if let (Some(core), Some(agent_did)) = (current_core(&state), previous_did.as_deref()) {
        if let Err(error) = core.mark_managed_runtime_restarting(agent_did).await {
            let message = format!(
                "The agent was stopped, but desktop restart bookkeeping failed: {error:#}. Retry Restart Agent."
            );
            state.managed_server.lock().await.last_error = Some(message.clone());
            emit_status(&app, &state).await;
            return Err(BridgeError::untyped(message));
        }
    }
    if let Err(error) = run_native(launchable_native_service(&app, &state)?, move |service| {
        service.install()?;
        service.start(was_enabled)
    })
    .await
    {
        // Native start may have launched the process before a later step
        // failed (for example restoring disabled-at-login state on macOS).
        let cleanup = match native_service(&app, &state) {
            Ok(service) => run_native(service, move |service| service.stop(!was_enabled)).await,
            Err(error) => Err(error),
        };
        let message = match cleanup {
            Ok(()) => error.message,
            Err(cleanup) => {
                combine_cleanup_error(error.message, "restarted native service", cleanup.message)
            }
        };
        state.managed_server.lock().await.last_error = Some(message.clone());
        emit_status(&app, &state).await;
        return Err(BridgeError::new(error.code, message));
    }
    let readiness = async {
        let ready = wait_for_managed_server(&agent_home)
            .await
            .map_err(|error| BridgeError::untyped(error.to_string()))?;
        validate_ready_runtime(&ready, &authority, &agent_home)
            .map_err(|error| BridgeError::untyped(error.to_string()))
    }
    .await;
    if let Err(error) = readiness {
        let cleanup = match native_service(&app, &state) {
            Ok(service) => run_native(service, move |service| service.stop(!was_enabled)).await,
            Err(error) => Err(error),
        };
        let message = match cleanup {
            Ok(()) => error.message,
            Err(cleanup) => {
                combine_cleanup_error(error.message, "restarted native service", cleanup.message)
            }
        };
        state.managed_server.lock().await.last_error = Some(message.clone());
        emit_status(&app, &state).await;
        return Err(BridgeError::new(error.code, message));
    }
    emit_status(&app, &state).await;
    if let Some(core) = current_core(&state) {
        start_running_managed_pairing(&state, core).await;
    }
    let native = run_native(native_service(&app, &state)?, |service| service.status()).await?;
    let mut status = matching_external_server(&agent_home)
        .await?
        .map(|external| project_external_status(external, &native))
        .ok_or_else(|| BridgeError::untyped("native service did not become ready"))?;
    status.pairing_ready = pairing_is_ready(&state, status.agent_did.as_deref()).await;
    Ok(status)
}

async fn stop_before_reprovision<S, StopFuture, P, ProvisionFuture, W, WriteFuture>(
    stop: S,
    provision: P,
    write_preference: W,
) -> Result<(), BridgeError>
where
    S: FnOnce() -> StopFuture,
    StopFuture: Future<Output = Result<(), BridgeError>>,
    P: FnOnce() -> ProvisionFuture,
    ProvisionFuture: Future<Output = Result<(), BridgeError>>,
    W: FnOnce() -> WriteFuture,
    WriteFuture: Future<Output = Result<(), BridgeError>>,
{
    stop().await?;
    provision().await?;
    write_preference().await
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
    native: Option<&gents_server::native_service::NativeServiceStatus>,
) -> ManagedServerStatus {
    ManagedServerStatus {
        state: if managed.starting || native.is_some_and(|status| status.job_loaded) {
            ManagedServerState::Starting
        } else if managed.last_error.is_some() {
            ManagedServerState::Failed
        } else if native.is_some_and(|status| status.installed) {
            ManagedServerState::Stopped
        } else {
            ManagedServerState::Disabled
        },
        auto_start: native.is_some_and(|status| status.enabled),
        agent_name: stored.map(|stored| stored.agent_name.clone()),
        agent_did: None,
        graphql: None,
        effective_tool_ceiling: stored.and_then(|value| value.tool_ceiling),
        effective_tool_root: stored.and_then(|value| value.tool_root.clone()),
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
    match value.trim().to_ascii_lowercase().as_str() {
        "meta-only" | "metaonly" | "meta_only" => Some(ManagedServerToolCeiling::MetaOnly),
        "readonly" => Some(ManagedServerToolCeiling::Readonly),
        "readwrite" => Some(ManagedServerToolCeiling::Readwrite),
        _ => None,
    }
}

async fn emit_status<R: Runtime>(app: &AppHandle<R>, state: &DesktopAppState) {
    let status = match observe_managed_server_status(app, state).await {
        Ok(status) => status,
        Err(error) => {
            let stored = load_preference(state).await.ok().flatten();
            let managed = state.managed_server.lock().await;
            let mut status = status_from(&managed, stored.as_ref(), None);
            status.state = ManagedServerState::Failed;
            status.error = Some(error.message);
            status
        }
    };
    let _ = app.emit(MANAGED_SERVER_UPDATED_EVENT, status);
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
    if !matches!(initialized_did, Some(initialized) if !initialized.trim().is_empty() && !live_did.trim().is_empty() && initialized == live_did)
    {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            format!("port {port} does not advertise the initialized Gents identity"),
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
            status_from(&runtime, Some(&stored), None).state,
            ManagedServerState::Starting
        );
        runtime.starting = false;
        assert_eq!(
            status_from(&runtime, Some(&stored), None).state,
            ManagedServerState::Failed
        );
        runtime.last_error = None;
        let installed = gents_server::native_service::NativeServiceStatus {
            installed: true,
            running: false,
            job_loaded: false,
            enabled: true,
            detail: None,
        };
        assert_eq!(
            status_from(&runtime, Some(&stored), Some(&installed)).state,
            ManagedServerState::Stopped
        );
        assert_eq!(
            status_from(&runtime, None, None).state,
            ManagedServerState::Disabled
        );
    }

    #[test]
    fn idle_status_reprobes_and_preserves_an_external_runtime_identity() {
        let stored = StoredManagedServer {
            agent_name: "local".to_string(),
            tool_ceiling: Some(ManagedServerToolCeiling::Readwrite),
            tool_root: Some("/Users/test".to_string()),
        };
        let idle = status_from(&ManagedServerRuntimeState::default(), Some(&stored), None);
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
            &gents_server::native_service::NativeServiceStatus {
                installed: true,
                running: false,
                job_loaded: false,
                enabled: true,
                detail: None,
            },
        );

        assert_eq!(external.state, ManagedServerState::External);
        assert_eq!(external.agent_did.as_deref(), Some("did:key:preserved"));
        assert!(external.auto_start);
        assert!(should_probe_external_status(&external));
    }

    #[test]
    fn native_running_without_endpoint_readiness_is_starting() {
        let native = gents_server::native_service::NativeServiceStatus {
            installed: true,
            running: true,
            job_loaded: true,
            enabled: true,
            detail: None,
        };
        let status = status_from(&ManagedServerRuntimeState::default(), None, Some(&native));
        assert_eq!(status.state, ManagedServerState::Starting);
        assert!(should_probe_external_status(&status));

        let observed = project_external_status(
            ManagedServerStatus {
                state: ManagedServerState::External,
                auto_start: false,
                agent_name: Some("local".to_string()),
                agent_did: Some("did:key:ready".to_string()),
                graphql: Some("http://127.0.0.1:9191/graphql".to_string()),
                effective_tool_ceiling: Some(ManagedServerToolCeiling::MetaOnly),
                effective_tool_root: None,
                suggested_tool_root: None,
                pairing_ready: false,
                error: None,
            },
            &native,
        );
        assert_eq!(observed.state, ManagedServerState::Running);
    }

    #[test]
    fn loaded_native_job_owns_a_ready_endpoint_between_process_states() {
        let native = gents_server::native_service::NativeServiceStatus {
            installed: true,
            running: false,
            job_loaded: true,
            enabled: false,
            detail: None,
        };
        let observed = project_external_status(
            ManagedServerStatus {
                state: ManagedServerState::External,
                auto_start: true,
                agent_name: Some("local".to_string()),
                agent_did: Some("did:key:ready".to_string()),
                graphql: Some("http://127.0.0.1:9191/graphql".to_string()),
                effective_tool_ceiling: Some(ManagedServerToolCeiling::MetaOnly),
                effective_tool_root: None,
                suggested_tool_root: None,
                pairing_ready: false,
                error: None,
            },
            &native,
        );
        assert_eq!(observed.state, ManagedServerState::Running);
        assert!(!observed.auto_start);
    }

    #[test]
    fn stale_local_failure_still_allows_runtime_readiness_probe() {
        let runtime = ManagedServerRuntimeState {
            last_error: Some("an earlier start failed".to_string()),
            ..Default::default()
        };
        let status = status_from(&runtime, None, None);
        assert_eq!(status.state, ManagedServerState::Failed);
        assert!(should_probe_external_status(&status));
    }

    #[tokio::test]
    async fn failed_native_stop_does_not_reprovision_authority() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let provisioned = AtomicBool::new(false);
        let preference_written = AtomicBool::new(false);
        let result = stop_before_reprovision(
            || async { Err(BridgeError::untyped("stop failed")) },
            || async {
                provisioned.store(true, Ordering::SeqCst);
                Ok(())
            },
            || async {
                preference_written.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .await;

        assert!(result.is_err());
        assert!(!provisioned.load(Ordering::SeqCst));
        assert!(!preference_written.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn reviewed_preference_is_written_before_native_restart_attempt() {
        use std::sync::{Arc, Mutex};

        let order = Arc::new(Mutex::new(Vec::new()));
        stop_before_reprovision(
            {
                let order = Arc::clone(&order);
                move || async move {
                    order.lock().unwrap().push("stop");
                    Ok(())
                }
            },
            {
                let order = Arc::clone(&order);
                move || async move {
                    order.lock().unwrap().push("provision");
                    Ok(())
                }
            },
            {
                let order = Arc::clone(&order);
                move || async move {
                    order.lock().unwrap().push("preference");
                    Ok(())
                }
            },
        )
        .await
        .unwrap();
        order.lock().unwrap().push("native-start-failed");

        assert_eq!(
            *order.lock().unwrap(),
            ["stop", "provision", "preference", "native-start-failed"]
        );
    }

    #[test]
    fn external_server_rejects_a_different_initialized_identity() {
        let error = ensure_matching_identity(Some("did:key:local"), "did:key:other", 9191)
            .expect_err("different identity must be rejected");
        assert_eq!(error.code, BridgeErrorCode::InvalidArgument);
        assert!(error.message.contains("port 9191"));
        ensure_matching_identity(Some("did:key:local"), "did:key:local", 9191).unwrap();
        assert!(ensure_matching_identity(None, "did:key:local", 9191).is_err());
        assert!(ensure_matching_identity(Some(""), "did:key:local", 9191).is_err());
        assert!(ensure_matching_identity(Some("did:key:local"), "", 9191).is_err());
    }

    #[test]
    fn managed_stop_rejects_a_manually_owned_endpoint() {
        assert!(ensure_native_owns_running_endpoint(true, false).is_err());
        assert!(ensure_native_owns_running_endpoint(true, true).is_ok());
        assert!(ensure_native_owns_running_endpoint(false, false).is_ok());
    }

    #[test]
    fn cleanup_failure_preserves_the_original_failure() {
        let combined = combine_cleanup_error(
            "readiness failed".to_string(),
            "newly started native service",
            "stop failed".to_string(),
        );
        assert!(combined.contains("readiness failed"));
        assert!(combined.contains("stop failed"));
    }

    #[test]
    fn parse_tool_ceiling_accepts_init_json_and_status_spellings() {
        assert_eq!(
            parse_tool_ceiling("readwrite"),
            Some(ManagedServerToolCeiling::Readwrite)
        );
        assert_eq!(
            parse_tool_ceiling("Readwrite"),
            Some(ManagedServerToolCeiling::Readwrite)
        );
        assert_eq!(
            parse_tool_ceiling("readonly"),
            Some(ManagedServerToolCeiling::Readonly)
        );
        assert_eq!(
            parse_tool_ceiling("meta-only"),
            Some(ManagedServerToolCeiling::MetaOnly)
        );
        assert_eq!(parse_tool_ceiling("unknown"), None);
    }

    #[test]
    fn ready_runtime_requires_live_status_to_carry_reviewed_authority() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("init.json"),
            r#"{"agent_did":"did:key:zReady"}"#,
        )
        .unwrap();
        let authority = EffectiveManagedAuthority {
            tool_ceiling: ManagedServerToolCeiling::Readwrite,
            tool_root: Some(PathBuf::from("/Users/test")),
        };
        let missing_authority = ManagedServerStatus {
            state: ManagedServerState::Running,
            auto_start: false,
            agent_name: Some("Mandrake".into()),
            agent_did: Some("did:key:zReady".into()),
            graphql: Some("http://127.0.0.1:9191/api/v0/graphql".into()),
            effective_tool_ceiling: None,
            effective_tool_root: None,
            suggested_tool_root: None,
            pairing_ready: false,
            error: None,
        };
        let error = validate_ready_runtime(&missing_authority, &authority, temp.path())
            .expect_err("missing /status authority must fail closed");
        assert!(error.to_string().contains("live did="));

        let ready = ManagedServerStatus {
            effective_tool_ceiling: Some(ManagedServerToolCeiling::Readwrite),
            effective_tool_root: Some("/Users/test".into()),
            ..missing_authority
        };
        validate_ready_runtime(&ready, &authority, temp.path()).unwrap();
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
