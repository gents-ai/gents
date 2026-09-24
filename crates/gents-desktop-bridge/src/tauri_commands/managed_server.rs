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
    ManagedServerResetRequest, ManagedServerResetResult, ManagedServerRestartRequest,
    ManagedServerRootValidation, ManagedServerRootValidationRequest, ManagedServerStartRequest,
    ManagedServerState, ManagedServerStatus, ManagedServerToolCeiling,
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
    let last_exit = if native.job_loaded && !native.running && !native.requires_approval {
        match run_native(native_service(&app, &state)?, |service| service.last_exit()).await {
            Ok(reason) => reason,
            Err(error) => {
                tracing::debug!(target: "gents_desktop::managed_server", error = %error.message, "could not read the native service exit record");
                None
            }
        }
    } else {
        None
    };
    let mut managed = state.managed_server.lock().await;
    let crash_loop = observe_crash_loop(&mut managed.exit_baseline, last_exit.as_ref());
    let mut status = status_from(
        &managed,
        stored.as_ref(),
        Some(&native),
        last_exit.as_ref(),
        crash_loop,
    );
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
    external.approval_required = false;
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
    let lifecycle = state.managed_server_lifecycle.lock().await;
    start_managed_server(&app, request, &state, lifecycle).await
}

type LifecycleGuard<'a> = tokio::sync::MutexGuard<'a, ()>;

const START_CANCELLED: &str =
    "Starting the local agent was cancelled because the agent was stopped, restarted, or reconfigured.";
const START_SUPERSEDED: &str = "This start of the local agent was replaced by a newer start.";

/// A start or restart waiting outside the lifecycle lock, and why it was
/// cancelled.
#[derive(Clone, Default)]
pub struct StartWait {
    token: tokio_util::sync::CancellationToken,
    reason: Arc<std::sync::OnceLock<&'static str>>,
}

impl StartWait {
    fn cancel(&self, reason: &'static str) {
        let _ = self.reason.set(reason);
        self.token.cancel();
    }

    fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    async fn cancelled(&self) {
        self.token.cancelled().await
    }

    fn cancel_message(&self) -> &'static str {
        self.reason.get().copied().unwrap_or(START_CANCELLED)
    }
}

async fn begin_start_wait(state: &DesktopAppState) -> StartWait {
    let token = StartWait::default();
    let mut managed = state.managed_server.lock().await;
    if let Some(previous) = managed.start_wait.replace(token.clone()) {
        previous.cancel(START_SUPERSEDED);
    }
    managed.starting = true;
    managed.last_error = None;
    token
}

/// Ends a start that is waiting for approval or readiness so Stop and Restart
/// can take the lifecycle lock.
pub(super) async fn cancel_start_wait(state: &DesktopAppState) {
    let mut managed = state.managed_server.lock().await;
    if let Some(token) = managed.start_wait.take() {
        token.cancel(START_CANCELLED);
        managed.starting = false;
    }
}

/// Takes the lifecycle lock for an operation that supersedes any start. A
/// wait registered while this caller queued for the lock belongs to a start
/// or restart that has since released it to wait, so it is cancelled too.
pub(super) async fn lock_lifecycle_superseding_start(
    state: &DesktopAppState,
) -> LifecycleGuard<'_> {
    cancel_start_wait(state).await;
    let lifecycle = state.managed_server_lifecycle.lock().await;
    cancel_start_wait(state).await;
    lifecycle
}

async fn finish_start_wait(state: &DesktopAppState, token: &StartWait) {
    let mut managed = state.managed_server.lock().await;
    if !token.is_cancelled() {
        managed.start_wait = None;
        managed.starting = false;
    }
}

async fn wait_unlocked<T>(
    token: &StartWait,
    wait: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    tokio::select! {
        biased;
        _ = token.cancelled() => anyhow::bail!(token.cancel_message()),
        result = wait => result,
    }
}

async fn relock_start<'a>(
    state: &'a DesktopAppState,
    token: &StartWait,
) -> anyhow::Result<LifecycleGuard<'a>> {
    let lifecycle = state.managed_server_lifecycle.lock().await;
    if token.is_cancelled() {
        anyhow::bail!(token.cancel_message());
    }
    Ok(lifecycle)
}

async fn ensure_no_start_waiting(state: &DesktopAppState) -> Result<(), BridgeError> {
    if state.managed_server.lock().await.start_wait.is_some() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "The local agent is still starting. Change start at login after it finishes.",
        ));
    }
    Ok(())
}

fn ensure_not_cancelled(token: &StartWait) -> anyhow::Result<()> {
    if token.is_cancelled() {
        anyhow::bail!(token.cancel_message());
    }
    Ok(())
}

/// Native steps of a managed start. The waits run without the lifecycle lock.
trait ManagedLaunch {
    async fn requires_approval(&self) -> Result<bool, BridgeError>;
    async fn await_approval(&self) -> anyhow::Result<()>;
    async fn start(&self) -> Result<(), BridgeError>;
    async fn await_ready(&self) -> anyhow::Result<Readiness>;
}

struct NativeLaunch<'a, R: Runtime> {
    app: &'a AppHandle<R>,
    state: &'a DesktopAppState,
    agent_home: &'a Path,
    enable_at_login: bool,
}

impl<R: Runtime> ManagedLaunch for NativeLaunch<'_, R> {
    async fn requires_approval(&self) -> Result<bool, BridgeError> {
        native_requires_approval(self.app, self.state).await
    }

    async fn await_approval(&self) -> anyhow::Result<()> {
        await_managed_server_approval(self.app, self.state).await
    }

    async fn start(&self) -> Result<(), BridgeError> {
        let enable_at_login = self.enable_at_login;
        run_native(
            launchable_native_service(self.app, self.state)?,
            move |service| service.start(enable_at_login),
        )
        .await
    }

    async fn await_ready(&self) -> anyhow::Result<Readiness> {
        wait_for_managed_server(self.app, self.state, self.agent_home).await
    }
}

struct LaunchFailure<'a> {
    error: anyhow::Error,
    attempted_start: bool,
    lifecycle: Option<LifecycleGuard<'a>>,
}

async fn launch_managed_server<'a, L: ManagedLaunch>(
    state: &'a DesktopAppState,
    token: &StartWait,
    lifecycle: LifecycleGuard<'a>,
    launch: &L,
) -> Result<(ManagedServerStatus, LifecycleGuard<'a>), LaunchFailure<'a>> {
    let mut lifecycle = Some(lifecycle);
    let mut attempted_start = false;
    let result: anyhow::Result<ManagedServerStatus> = async {
        if launch.requires_approval().await? {
            lifecycle = None;
            wait_unlocked(token, launch.await_approval()).await?;
            lifecycle = Some(relock_start(state, token).await?);
        }
        // A start can launch the process and then fail restoring login state.
        // Roll back the owned attempt even when that final native step fails.
        ensure_not_cancelled(token)?;
        attempted_start = true;
        if let Err(error) = launch.start().await {
            if !launch.requires_approval().await.unwrap_or(false) {
                return Err(error.into());
            }
            lifecycle = None;
            wait_unlocked(token, launch.await_approval()).await?;
            lifecycle = Some(relock_start(state, token).await?);
            ensure_not_cancelled(token)?;
            launch.start().await?;
        }
        let mut resumed_after_approval = false;
        loop {
            lifecycle = None;
            match wait_unlocked(token, launch.await_ready()).await? {
                Readiness::Ready(ready) => {
                    lifecycle = Some(relock_start(state, token).await?);
                    return Ok(ready);
                }
                Readiness::ApprovalRequired if !resumed_after_approval => {
                    resumed_after_approval = true;
                    wait_unlocked(token, launch.await_approval()).await?;
                    lifecycle = Some(relock_start(state, token).await?);
                    ensure_not_cancelled(token)?;
                    launch.start().await?;
                }
                Readiness::ApprovalRequired => {
                    anyhow::bail!(gents_server::native_service::BACKGROUND_APPROVAL_REQUIRED)
                }
            }
        }
    }
    .await;
    match (result, lifecycle) {
        (Ok(ready), Some(lifecycle)) => Ok((ready, lifecycle)),
        (Ok(_), None) => unreachable!("readiness relocks before it returns"),
        (Err(error), lifecycle) => Err(LaunchFailure {
            error,
            attempted_start,
            lifecycle,
        }),
    }
}

async fn start_managed_server<'a, R: Runtime>(
    app: &AppHandle<R>,
    request: ManagedServerStartRequest,
    state: &'a DesktopAppState,
    lifecycle: LifecycleGuard<'a>,
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

    let mut carried_wait = None;
    let mut exited_while_loaded = false;
    let (ready, initial_enabled, _lifecycle) = match matching_external_server(&agent_home).await? {
        Some(external) => (Some(external), false, lifecycle),
        None => {
            let initial_native =
                run_native(native_service(app, state)?, |service| service.status()).await?;
            let enabled = initial_native.enabled;
            exited_while_loaded = initial_native.job_loaded
                && !initial_native.running
                && run_native(native_service(app, state)?, |service| service.last_exit())
                    .await
                    .is_ok_and(|exit| exit.is_some());
            if initial_native.is_active_or_transitioning()
                && !initial_native.requires_approval
                && !exited_while_loaded
            {
                match wait_for_booting_managed_server(app, state, &agent_home, lifecycle).await? {
                    BootOutcome::Ready(ready, lifecycle) => (Some(ready), enabled, lifecycle),
                    BootOutcome::NeedsLaunch(lifecycle, wait) => {
                        carried_wait = Some(wait);
                        (None, enabled, lifecycle)
                    }
                }
            } else {
                (None, enabled, lifecycle)
            }
        }
    };
    if let Some(external) = ready {
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
    let lifecycle = _lifecycle;

    let token = match carried_wait {
        Some(wait) => wait,
        None => begin_start_wait(state).await,
    };
    emit_status(app, state).await;

    let provisioned: anyhow::Result<()> = async {
        if exited_while_loaded {
            // The loaded job will not run again by itself (a clean exit) or
            // is crash looping. Unload it so an updated definition can be
            // installed and the launch below starts it fresh.
            run_native(native_service(app, state)?, |service| service.stop(false)).await?;
        }
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
        Ok(())
    }
    .await;
    let launched = match provisioned {
        Ok(()) => {
            let launch = NativeLaunch {
                app,
                state,
                agent_home: &agent_home,
                enable_at_login: false,
            };
            match launch_managed_server(state, &token, lifecycle, &launch).await {
                Ok((ready, lifecycle)) => {
                    let initialized: anyhow::Result<()> = async {
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
                    initialized.map_err(|error| LaunchFailure {
                        error,
                        attempted_start: true,
                        lifecycle: Some(lifecycle),
                    })
                }
                Err(failure) => Err(failure),
            }
        }
        Err(error) => Err(LaunchFailure {
            error,
            attempted_start: false,
            lifecycle: Some(lifecycle),
        }),
    };

    let _lifecycle = match launched {
        Ok(lifecycle) => {
            finish_start_wait(state, &token).await;
            lifecycle
        }
        Err(failure) => {
            return Err(fail_managed_start(
                app,
                state,
                &token,
                &agent_home,
                initial_enabled,
                failure,
            )
            .await)
        }
    };

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
        status_from(&managed, stored.as_ref(), Some(&native), None, None)
    };
    status.pairing_ready = pairing_is_ready(state, status.agent_did.as_deref()).await;
    Ok(status)
}

/// Ends a failed start or restart: rolls back a native start it attempted,
/// clears its wait, and records the failure. Errs with the cancellation when
/// a stop or restart superseded it first.
async fn settle_launch_failure<C, CF>(
    state: &DesktopAppState,
    token: &StartWait,
    failure: LaunchFailure<'_>,
    cleanup: C,
) -> Result<(String, Option<BridgeError>), BridgeError>
where
    C: FnOnce() -> CF,
    CF: Future<Output = Result<(), BridgeError>>,
{
    let LaunchFailure {
        error,
        attempted_start,
        lifecycle,
    } = failure;
    let cancelled = || BridgeError::untyped(token.cancel_message());
    if token.is_cancelled() {
        return Err(cancelled());
    }
    let typed_error = error.downcast_ref::<BridgeError>().cloned();
    let mut message = format!("{error:#}");
    let _lifecycle = match lifecycle {
        Some(lifecycle) => lifecycle,
        None => relock_start(state, token).await.map_err(|_| cancelled())?,
    };
    if attempted_start {
        if let Err(cleanup) = cleanup().await {
            message =
                combine_cleanup_error(message, "newly started native service", cleanup.message);
        }
    }
    finish_start_wait(state, token).await;
    state.managed_server.lock().await.last_error = Some(message.clone());
    Ok((message, typed_error))
}

async fn install_then_launch<'a, L: ManagedLaunch>(
    state: &'a DesktopAppState,
    token: &StartWait,
    lifecycle: LifecycleGuard<'a>,
    install: impl Future<Output = Result<(), BridgeError>>,
    launch: &L,
) -> Result<(ManagedServerStatus, LifecycleGuard<'a>), LaunchFailure<'a>> {
    if let Err(error) = install.await {
        return Err(LaunchFailure {
            error: error.into(),
            attempted_start: false,
            lifecycle: Some(lifecycle),
        });
    }
    launch_managed_server(state, token, lifecycle, launch).await
}

async fn fail_managed_start<R: Runtime>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
    token: &StartWait,
    agent_home: &Path,
    initial_enabled: bool,
    failure: LaunchFailure<'_>,
) -> BridgeError {
    let settled = settle_launch_failure(state, token, failure, || async {
        run_native(native_service(app, state)?, move |service| {
            service.stop(!initial_enabled)
        })
        .await
    })
    .await;
    let (message, typed_error) = match settled {
        Ok(settled) => settled,
        Err(cancelled) => {
            tracing::info!(target: "gents_desktop::managed_server", "managed Gents server start was cancelled by stop or restart");
            return cancelled;
        }
    };
    tracing::warn!(error = %message, "managed Gents server start failed");
    emit_status(app, state).await;
    if matches!(
        gents::storage_backend::incompatible_store_kind(&agent_home.join("data")),
        Ok(Some(_))
    ) {
        return BridgeError::new(BridgeErrorCode::IncompatibleLocalStore, message);
    }
    match typed_error {
        Some(mut error) => {
            error.message = message;
            error
        }
        None => BridgeError::untyped(message),
    }
}

const RESET_CONSEQUENCE: &str = "Existing local conversations and configuration will be archived in a timestamped backup and will not be imported into the new store.";

#[tauri::command]
pub async fn desktop_managed_server_reset<R: Runtime>(
    request: ManagedServerResetRequest,
    app: AppHandle<R>,
    state: State<'_, DesktopAppState>,
) -> Result<ManagedServerResetResult, BridgeError> {
    ensure_allowed(&state)?;
    let _lifecycle = lock_lifecycle_superseding_start(&state).await;
    let agent_home = state.policy.agent_home.as_deref().ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::Unsupported,
            "managed server requires a local agent home",
        )
    })?;
    reset_incompatible_managed_store(&app, &state, agent_home, request.confirmation.as_deref())
        .await
}

async fn reset_incompatible_managed_store<R: Runtime>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
    configured_home: &Path,
    confirmation: Option<&str>,
) -> Result<ManagedServerResetResult, BridgeError> {
    let native = run_native(native_service(app, state)?, |service| service.status()).await?;
    ensure_managed_runtime_stopped(native.is_active_or_transitioning())?;
    reject_symlink(configured_home, "managed home")?;
    let home = std::fs::canonicalize(configured_home).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::Backend,
            format!(
                "Cannot resolve managed home {}: {error}",
                configured_home.display()
            ),
        )
    })?;
    if home.parent().is_none() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "The managed home is too broad to reset.",
        ));
    }
    // Fail closed when the standard managed endpoint is occupied. A matching
    // server is definitely live; an unknown responder is equally unsafe to
    // overwrite because its data ownership cannot be established.
    let config = gents_server::server_host::ServerConfig::standard(home.clone());
    let managed_address = std::net::SocketAddr::new(config.http_addr, config.http_port);
    let endpoint_state =
        std::net::TcpStream::connect_timeout(&managed_address, Duration::from_millis(150));
    if matching_external_server(&home).await?.is_some() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "A managed server is still listening. Stop it before resetting its store.",
        ));
    }
    ensure_managed_endpoint_stopped(managed_address, endpoint_state.map(|_| ()))?;

    archive_incompatible_managed_store(&home, confirmation)
}

fn ensure_managed_runtime_stopped(running: bool) -> Result<(), BridgeError> {
    if running {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "Stop the managed server before resetting its local store.",
        ));
    }
    Ok(())
}

fn ensure_managed_endpoint_stopped(
    address: std::net::SocketAddr,
    observation: std::io::Result<()>,
) -> Result<(), BridgeError> {
    match observation {
        Ok(()) => Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "A managed server is still listening. Stop it before resetting its store.",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => Ok(()),
        Err(error) => Err(BridgeError::new(
            BridgeErrorCode::Backend,
            format!("Could not prove the managed endpoint {address} is stopped: {error}"),
        )),
    }
}

fn archive_incompatible_managed_store(
    home: &Path,
    confirmation: Option<&str>,
) -> Result<ManagedServerResetResult, BridgeError> {
    let data = home.join("data");
    let init = home.join("init.json");
    reject_symlink_if_present(&data, "managed data")?;
    reject_symlink_if_present(&init, "managed init marker")?;
    let kind = gents::storage_backend::incompatible_store_kind(&data).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::Backend,
            format!("Inspecting {}: {error}", data.display()),
        )
    })?;
    if kind.is_none() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "The managed store is not marked as a legacy or incompatible store; reset was refused.",
        ));
    }
    let confirmation_text = format!("RESET {} AND ARCHIVE LOCAL HISTORY", home.to_string_lossy());
    let base = ManagedServerResetResult {
        managed_home: home.to_string_lossy().into_owned(),
        data_path: data.to_string_lossy().into_owned(),
        confirmation: confirmation_text.clone(),
        consequence: RESET_CONSEQUENCE.into(),
        completed: false,
        backup_path: None,
        archived_paths: Vec::new(),
    };
    let Some(supplied) = confirmation else {
        return Ok(base);
    };
    if supplied != confirmation_text {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "Reset confirmation did not match the exact managed home.",
        ));
    }

    let backups = home.join("backups");
    reject_symlink_if_present(&backups, "managed backup directory")?;
    std::fs::create_dir_all(&backups).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::Backend,
            format!("Creating {}: {error}", backups.display()),
        )
    })?;
    let backups = std::fs::canonicalize(&backups).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::Backend,
            format!("Resolving backups: {error}"),
        )
    })?;
    if backups.parent() != Some(home) {
        return Err(BridgeError::new(
            BridgeErrorCode::PathEscapesRoot,
            "Managed backup directory escaped the configured home.",
        ));
    }
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let mut backup = backups.join(format!("legacy-store-{stamp}-{}", std::process::id()));
    for suffix in 0..100_u8 {
        if !backup.exists() {
            break;
        }
        backup = backups.join(format!(
            "legacy-store-{stamp}-{}-{suffix}",
            std::process::id()
        ));
    }
    std::fs::create_dir(&backup).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::Backend,
            format!("Creating backup {}: {error}", backup.display()),
        )
    })?;
    let backup_data = backup.join("data");
    std::fs::rename(&data, &backup_data).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::Backend,
            format!("Archiving {}: {error}", data.display()),
        )
    })?;
    let mut archived = vec![data.to_string_lossy().into_owned()];
    if init.exists() {
        if let Err(error) = std::fs::rename(&init, backup.join("init.json")) {
            if let Err(rollback) = std::fs::rename(&backup_data, &data) {
                return Err(BridgeError::new(
                    BridgeErrorCode::Backend,
                    format!(
                        "Archiving {} failed ({error}); rollback also failed ({rollback}). The data remains at {} and init.json was not moved.",
                        init.display(),
                        backup_data.display()
                    ),
                ));
            }
            return Err(BridgeError::new(
                BridgeErrorCode::Backend,
                format!(
                    "Archiving {} failed ({error}); data was restored.",
                    init.display()
                ),
            ));
        }
        archived.push(init.to_string_lossy().into_owned());
    }
    Ok(ManagedServerResetResult {
        completed: true,
        backup_path: Some(backup.to_string_lossy().into_owned()),
        archived_paths: archived,
        ..base
    })
}

fn reject_symlink(path: &Path, label: &str) -> Result<(), BridgeError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::Backend,
            format!("Inspecting {label} {}: {error}", path.display()),
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(BridgeError::new(
            BridgeErrorCode::PathEscapesRoot,
            format!("{label} must not be a symbolic link"),
        ));
    }
    Ok(())
}

fn reject_symlink_if_present(path: &Path, label: &str) -> Result<(), BridgeError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(BridgeError::new(
            BridgeErrorCode::PathEscapesRoot,
            format!("{label} must not be a symbolic link"),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(BridgeError::new(
            BridgeErrorCode::Backend,
            format!("Inspecting {label} {}: {error}", path.display()),
        )),
    }
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

const MANAGED_SERVER_READY_TIMEOUT: Duration = Duration::from_secs(300);
const BACKGROUND_APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);
const MANAGED_SERVER_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Supervisor restarts after a failed exit that make a crash loop rather
/// than a single exit waiting out the respawn throttle. Counted from the
/// restart counter at the first failed exit observed: launchd `runs` includes
/// earlier kickstarts and crashes of the same loaded job, so an absolute
/// count would reject a start after old failures.
const CRASH_LOOP_RESTARTS: u64 = 2;

#[derive(Debug)]
enum Readiness {
    Ready(ManagedServerStatus),
    ApprovalRequired,
}

enum NativeProgress {
    Loaded,
    Stopped,
    AwaitingApproval,
    Exited(gents_server::native_service::ServiceExit),
}

async fn observe_native_progress<R: Runtime>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<NativeProgress, BridgeError> {
    run_native(native_service(app, state)?, |service| {
        let status = service.status()?;
        if status.requires_approval {
            return Ok(NativeProgress::AwaitingApproval);
        }
        if !status.job_loaded {
            return Ok(NativeProgress::Stopped);
        }
        if status.running {
            return Ok(NativeProgress::Loaded);
        }
        Ok(match service.last_exit()? {
            Some(exit) => NativeProgress::Exited(exit),
            None => NativeProgress::Loaded,
        })
    })
    .await
}

async fn wait_for_managed_server<R: Runtime>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
    agent_home: &Path,
) -> anyhow::Result<Readiness> {
    await_runtime_readiness(
        MANAGED_SERVER_READY_TIMEOUT,
        MANAGED_SERVER_POLL_INTERVAL,
        || observe_port_readiness(agent_home),
        || observe_native_progress(app, state),
    )
    .await
}

async fn await_runtime_readiness<P, PF, N, NF>(
    timeout: Duration,
    interval: Duration,
    mut probe: P,
    mut native: N,
) -> anyhow::Result<Readiness>
where
    P: FnMut() -> PF,
    PF: Future<Output = Result<PortReadiness, BridgeError>>,
    N: FnMut() -> NF,
    NF: Future<Output = Result<NativeProgress, BridgeError>>,
{
    let started = tokio::time::Instant::now();
    let mut first_exit_restarts = None;
    loop {
        match probe().await? {
            PortReadiness::Ready(status) => return Ok(Readiness::Ready(status)),
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
        match native().await? {
            NativeProgress::AwaitingApproval => return Ok(Readiness::ApprovalRequired),
            NativeProgress::Stopped => anyhow::bail!(
                "the native Gents service stopped after {} seconds, before it published runtime readiness",
                started.elapsed().as_secs()
            ),
            NativeProgress::Exited(exit) if exit.clean => anyhow::bail!(
                "the native Gents service exited normally before it published runtime readiness, so it will not be restarted"
            ),
            NativeProgress::Exited(exit) => {
                let restarts = restarts_since(&mut first_exit_restarts, exit.restarts);
                if restarts >= CRASH_LOOP_RESTARTS {
                    anyhow::bail!(
                        "the native Gents service keeps exiting before it publishes runtime readiness: it {} and was restarted {restarts} times in a row",
                        exit.reason
                    );
                }
            }
            NativeProgress::Loaded => {}
        }
        if started.elapsed() >= timeout {
            anyhow::bail!(
                "the native Gents service is running but did not publish runtime readiness within {} seconds",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(interval).await;
    }
}

enum BootOutcome<'a> {
    Ready(ManagedServerStatus, LifecycleGuard<'a>),
    NeedsLaunch(LifecycleGuard<'a>, StartWait),
}

async fn adopt_booting_runtime<'a>(
    state: &'a DesktopAppState,
    token: &StartWait,
    lifecycle: LifecycleGuard<'a>,
    readiness: impl Future<Output = anyhow::Result<Readiness>>,
) -> anyhow::Result<BootOutcome<'a>> {
    drop(lifecycle);
    let readiness = wait_unlocked(token, readiness).await?;
    let lifecycle = relock_start(state, token).await?;
    Ok(match readiness {
        Readiness::Ready(status) => BootOutcome::Ready(status, lifecycle),
        Readiness::ApprovalRequired => BootOutcome::NeedsLaunch(lifecycle, token.clone()),
    })
}

async fn wait_for_booting_managed_server<'a, R: Runtime>(
    app: &AppHandle<R>,
    state: &'a DesktopAppState,
    agent_home: &Path,
    lifecycle: LifecycleGuard<'a>,
) -> Result<BootOutcome<'a>, BridgeError> {
    let token = begin_start_wait(state).await;
    emit_status(app, state).await;
    tracing::info!(
        target: "gents_desktop::managed_server",
        "waiting for the running native service to publish runtime readiness"
    );
    let adopted = adopt_booting_runtime(
        state,
        &token,
        lifecycle,
        wait_for_managed_server(app, state, agent_home),
    )
    .await;
    if token.is_cancelled() {
        return Err(BridgeError::untyped(token.cancel_message()));
    }
    if matches!(adopted, Ok(BootOutcome::NeedsLaunch(..))) {
        return adopted.map_err(|error| BridgeError::untyped(format!("{error:#}")));
    }
    finish_start_wait(state, &token).await;
    let result = adopted.map_err(|error| {
        let message = format!("{error:#}");
        tracing::warn!(target: "gents_desktop::managed_server", error = %message, "managed Gents server did not become ready");
        BridgeError::new(BridgeErrorCode::EndpointUnreachable, message)
    });
    if let Err(error) = &result {
        state.managed_server.lock().await.last_error = Some(error.message.clone());
    }
    emit_status(app, state).await;
    result
}

async fn native_requires_approval<R: Runtime>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<bool, BridgeError> {
    Ok(
        run_native(native_service(app, state)?, |service| service.status())
            .await?
            .requires_approval,
    )
}

async fn await_managed_server_approval<R: Runtime>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> anyhow::Result<()> {
    await_background_approval(
        BACKGROUND_APPROVAL_TIMEOUT,
        MANAGED_SERVER_POLL_INTERVAL,
        || native_requires_approval(app, state),
        || emit_status(app, state),
    )
    .await
}

async fn await_background_approval<O, OF, W, WF>(
    timeout: Duration,
    interval: Duration,
    mut requires_approval: O,
    mut on_waiting: W,
) -> anyhow::Result<()>
where
    O: FnMut() -> OF,
    OF: Future<Output = Result<bool, BridgeError>>,
    W: FnMut() -> WF,
    WF: Future<Output = ()>,
{
    let started = tokio::time::Instant::now();
    let mut announced = false;
    while requires_approval().await? {
        if !announced {
            tracing::info!(
                target: "gents_desktop::managed_server",
                "waiting for macOS to allow the Gents background item"
            );
            announced = true;
        }
        on_waiting().await;
        if started.elapsed() >= timeout {
            anyhow::bail!(
                "{} Gents waited {} minutes for approval.",
                gents_server::native_service::BACKGROUND_APPROVAL_REQUIRED,
                timeout.as_secs() / 60
            );
        }
        tokio::time::sleep(interval).await;
    }
    Ok(())
}

#[tauri::command]
pub async fn desktop_managed_server_open_login_items<R: Runtime>(
    app: AppHandle<R>,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError> {
    ensure_allowed(&state)?;
    let (opened, result) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = opened.send(gents_server::native_service::open_background_approval_settings());
    })
    .map_err(|error| BridgeError::untyped(format!("opening Login Items settings: {error}")))?;
    result
        .await
        .map_err(|error| BridgeError::untyped(format!("opening Login Items settings: {error}")))?
        .map_err(native_error)
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
    // A durably paired runtime skips pairing below, so its replicated schema
    // is observed on every start; a failed fetch leaves the last state.
    let observed_core = Arc::clone(&core);
    let observed_target = target.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = observe_managed_runtime_schema(&observed_core, &observed_target).await {
            tracing::warn!(
                target: "gents_desktop::managed_server",
                agent_did = %observed_target.agent_did,
                error = %error,
                "managed runtime replicated schema observation failed"
            );
        }
    });

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

/// How long a just-started runtime may take to publish its replicated schema,
/// which it does only after its migrations finish.
const MANAGED_SCHEMA_PUBLISH_WINDOW: Duration = Duration::from_secs(30);

/// Fetch the managed runtime's `/status`, retrying within
/// [`MANAGED_SCHEMA_PUBLISH_WINDOW`] while it is unreachable or has not yet
/// published its replicated schema.
async fn fetch_managed_runtime_status(
    target: &ManagedPairingTarget,
) -> Result<serde_json::Value, String> {
    let mut status_url = reqwest::Url::parse(&target.graphql)
        .map_err(|error| format!("parsing managed runtime GraphQL URL: {error}"))?;
    status_url.set_path("/status");
    status_url.set_query(None);
    status_url.set_fragment(None);
    let deadline = tokio::time::Instant::now() + MANAGED_SCHEMA_PUBLISH_WINDOW;
    loop {
        let fetched = fetch_runtime_connection_payload(status_url.as_str())
            .await
            .map_err(|error| format!("{error:#}"));
        let published = fetched.as_ref().is_ok_and(|status| {
            !status
                .get(gents_protocol::peer_schema::STATUS_REPLICATED_SCHEMA_FIELD)
                .is_some_and(serde_json::Value::is_null)
        });
        if published || tokio::time::Instant::now() >= deadline {
            return fetched;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn observe_managed_runtime_schema(
    core: &ClientCore,
    target: &ManagedPairingTarget,
) -> Result<(), String> {
    let observation = core.begin_runtime_schema_observation(&target.agent_did);
    let status = fetch_managed_runtime_status(target).await?;
    core.finish_runtime_schema_observation(&observation, &status)
        .await
        .map_err(|error| format!("{error:#}"))
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

    let status = fetch_managed_runtime_status(target)
        .await
        .map_err(|error| format!("loading managed runtime enrollment offer: {error}"))?;
    let enrollment = core
        .request_status_enrollment_with_label(&status, Some(&target.agent_name))
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
        approval_required: false,
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
    let _lifecycle = lock_lifecycle_superseding_start(&state).await;
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
    ensure_no_start_waiting(&state).await?;
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
    let lifecycle = lock_lifecycle_superseding_start(&state).await;
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
    let token = begin_start_wait(&state).await;
    emit_status(&app, &state).await;
    let launch = NativeLaunch {
        app: &app,
        state: &state,
        agent_home: &agent_home,
        enable_at_login: was_enabled,
    };
    let install = async {
        run_native(launchable_native_service(&app, &state)?, |service| {
            service.install()
        })
        .await
    };
    let launched = match install_then_launch(&state, &token, lifecycle, install, &launch).await {
        Ok((ready, lifecycle)) => match validate_ready_runtime(&ready, &authority, &agent_home) {
            Ok(()) => Ok(lifecycle),
            Err(error) => Err(LaunchFailure {
                error,
                attempted_start: true,
                lifecycle: Some(lifecycle),
            }),
        },
        Err(failure) => Err(failure),
    };
    let _lifecycle = match launched {
        Ok(lifecycle) => {
            finish_start_wait(&state, &token).await;
            lifecycle
        }
        Err(failure) => {
            return Err(
                fail_managed_start(&app, &state, &token, &agent_home, was_enabled, failure).await,
            )
        }
    };
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

/// Tracks a crash loop across status reads: the supervisor's restart count
/// when a failed exit was first seen, and how far it has advanced since.
/// Returns the restarts since then once they reach the crash-loop threshold.
/// Restarts since `baseline`. A counter below the baseline means the job was
/// reloaded or its counter reset, so the baseline restarts from it.
fn restarts_since(baseline: &mut Option<u64>, counter: u64) -> u64 {
    let first = baseline
        .filter(|first| *first <= counter)
        .unwrap_or(counter);
    *baseline = Some(first);
    counter - first
}

fn observe_crash_loop(
    baseline: &mut Option<u64>,
    last_exit: Option<&gents_server::native_service::ServiceExit>,
) -> Option<u64> {
    match last_exit.filter(|exit| !exit.clean) {
        Some(exit) => {
            let restarts = restarts_since(baseline, exit.restarts);
            (restarts >= CRASH_LOOP_RESTARTS).then_some(restarts)
        }
        None => {
            *baseline = None;
            None
        }
    }
}

fn status_from(
    managed: &crate::state::ManagedServerState,
    stored: Option<&StoredManagedServer>,
    native: Option<&gents_server::native_service::NativeServiceStatus>,
    last_exit: Option<&gents_server::native_service::ServiceExit>,
    crash_loop_restarts: Option<u64>,
) -> ManagedServerStatus {
    let approval_required = native.is_some_and(|status| status.requires_approval);
    let exited_cleanly = last_exit.is_some_and(|exit| exit.clean);
    let crashed = last_exit
        .zip(crash_loop_restarts)
        .filter(|_| !managed.starting && !approval_required)
        .map(|(exit, restarts)| {
            format!(
                "The background agent keeps exiting before it becomes ready: it {} and was restarted {restarts} times in a row. Restart the agent, or check its log.",
                exit.reason
            )
        });
    ManagedServerStatus {
        state: if crashed.is_some() {
            ManagedServerState::Failed
        } else if managed.starting
            || (!approval_required
                && !exited_cleanly
                && native.is_some_and(|status| status.job_loaded))
        {
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
        approval_required,
        error: crashed.or_else(|| managed.last_error.clone()),
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
            let mut status = status_from(&managed, stored.as_ref(), None, None, None);
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
    fn reset_archives_only_store_and_init_while_preserving_identity_and_config() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("managed-home");
        let data = home.join("data");
        std::fs::create_dir_all(&data).unwrap();
        // The command canonicalizes the configured home before archiving.
        // macOS temp paths may otherwise retain the /var -> /private/var alias.
        let home = std::fs::canonicalize(&home).unwrap();
        std::fs::write(data.join("data.lark"), "legacy").unwrap();
        std::fs::write(home.join("init.json"), r#"{"agent_did":"did:key:old"}"#).unwrap();
        std::fs::write(home.join("agent.key"), "identity").unwrap();
        std::fs::write(home.join("p2p.key"), "p2p").unwrap();
        std::fs::write(home.join("managed-server.json"), "preferences").unwrap();

        let preview = archive_incompatible_managed_store(&home, None).unwrap();
        assert!(!preview.completed);
        assert!(data.exists(), "preview must not mutate the store");
        let reset =
            archive_incompatible_managed_store(&home, Some(preview.confirmation.as_str())).unwrap();
        assert!(reset.completed);
        let backup = PathBuf::from(reset.backup_path.unwrap());
        assert_eq!(
            std::fs::read_to_string(backup.join("data/data.lark")).unwrap(),
            "legacy"
        );
        assert!(backup.join("init.json").is_file());
        assert!(!data.exists());
        assert!(!home.join("init.json").exists());
        assert_eq!(
            std::fs::read_to_string(home.join("agent.key")).unwrap(),
            "identity"
        );
        assert_eq!(
            std::fs::read_to_string(home.join("p2p.key")).unwrap(),
            "p2p"
        );
        assert_eq!(
            std::fs::read_to_string(home.join("managed-server.json")).unwrap(),
            "preferences"
        );
    }

    #[test]
    fn reset_rejects_healthy_store_wrong_confirmation_and_unknown_endpoint() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let data = home.join("data");
        std::fs::create_dir(&data).unwrap();
        std::fs::write(data.join("MANIFEST"), b"REGOMAN current").unwrap();
        assert_eq!(
            archive_incompatible_managed_store(home, None)
                .unwrap_err()
                .code,
            BridgeErrorCode::InvalidArgument
        );
        std::fs::remove_file(data.join("MANIFEST")).unwrap();
        std::fs::write(data.join("data.lark"), "legacy").unwrap();
        assert_eq!(
            archive_incompatible_managed_store(home, Some("RESET SOME OTHER HOME"))
                .unwrap_err()
                .code,
            BridgeErrorCode::InvalidArgument
        );
        let address = "127.0.0.1:9191".parse().unwrap();
        assert_eq!(
            ensure_managed_endpoint_stopped(
                address,
                Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "unknown")),
            )
            .unwrap_err()
            .code,
            BridgeErrorCode::Backend
        );
        assert_eq!(
            ensure_managed_endpoint_stopped(address, Ok(()))
                .unwrap_err()
                .code,
            BridgeErrorCode::InvalidArgument
        );
        ensure_managed_endpoint_stopped(
            address,
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "stopped",
            )),
        )
        .unwrap();
        assert_eq!(
            ensure_managed_runtime_stopped(true).unwrap_err().code,
            BridgeErrorCode::InvalidArgument
        );
        ensure_managed_runtime_stopped(false).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn reset_rejects_symlinked_store_and_backup_paths() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("data.lark"), "legacy").unwrap();
        symlink(&outside, home.join("data")).unwrap();
        assert_eq!(
            archive_incompatible_managed_store(&home, None)
                .unwrap_err()
                .code,
            BridgeErrorCode::PathEscapesRoot
        );
        std::fs::remove_file(home.join("data")).unwrap();
        std::fs::create_dir(home.join("data")).unwrap();
        std::fs::write(home.join("data/data.lark"), "legacy").unwrap();
        symlink(&outside, home.join("backups")).unwrap();
        let preview = archive_incompatible_managed_store(&home, None).unwrap();
        assert_eq!(
            archive_incompatible_managed_store(&home, Some(&preview.confirmation))
                .unwrap_err()
                .code,
            BridgeErrorCode::PathEscapesRoot
        );
    }

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
            status_from(&runtime, Some(&stored), None, None, None).state,
            ManagedServerState::Starting
        );
        runtime.starting = false;
        assert_eq!(
            status_from(&runtime, Some(&stored), None, None, None).state,
            ManagedServerState::Failed
        );
        runtime.last_error = None;
        let installed = gents_server::native_service::NativeServiceStatus {
            installed: true,
            running: false,
            job_loaded: false,
            enabled: true,
            requires_approval: false,
            detail: None,
        };
        assert_eq!(
            status_from(&runtime, Some(&stored), Some(&installed), None, None).state,
            ManagedServerState::Stopped
        );
        assert_eq!(
            status_from(&runtime, None, None, None, None).state,
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
        let idle = status_from(
            &ManagedServerRuntimeState::default(),
            Some(&stored),
            None,
            None,
            None,
        );
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
                approval_required: false,
                error: None,
            },
            &gents_server::native_service::NativeServiceStatus {
                installed: true,
                running: false,
                job_loaded: false,
                enabled: true,
                requires_approval: false,
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
            requires_approval: false,
            detail: None,
        };
        let status = status_from(
            &ManagedServerRuntimeState::default(),
            None,
            Some(&native),
            None,
            None,
        );
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
                approval_required: false,
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
            requires_approval: false,
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
                approval_required: false,
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
        let status = status_from(&runtime, None, None, None, None);
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
            approval_required: false,
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

    fn ready_status(agent_did: &str) -> ManagedServerStatus {
        ManagedServerStatus {
            state: ManagedServerState::External,
            auto_start: false,
            agent_name: Some("local".to_string()),
            agent_did: Some(agent_did.to_string()),
            graphql: Some("http://127.0.0.1:9191/graphql".to_string()),
            effective_tool_ceiling: Some(ManagedServerToolCeiling::MetaOnly),
            effective_tool_root: None,
            suggested_tool_root: None,
            pairing_ready: false,
            approval_required: false,
            error: None,
        }
    }

    #[tokio::test]
    async fn a_slow_booting_runtime_is_waited_on_until_it_publishes_readiness() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let probes = AtomicUsize::new(0);
        let ready = await_runtime_readiness(
            Duration::from_secs(5),
            Duration::from_millis(5),
            || {
                let attempt = probes.fetch_add(1, Ordering::SeqCst);
                async move {
                    Ok(if attempt < 5 {
                        PortReadiness::NotListening
                    } else {
                        PortReadiness::Ready(ready_status("did:key:slow"))
                    })
                }
            },
            || async { Ok(NativeProgress::Loaded) },
        )
        .await
        .expect("a loaded job that is still booting is waited on");

        let Readiness::Ready(ready) = ready else {
            panic!("expected readiness");
        };
        assert_eq!(ready.agent_did.as_deref(), Some("did:key:slow"));
        assert_eq!(probes.load(Ordering::SeqCst), 6);
    }

    #[tokio::test]
    async fn readiness_wait_fails_as_soon_as_the_service_stops() {
        let error = await_runtime_readiness(
            Duration::from_secs(60),
            Duration::from_millis(5),
            || async { Ok(PortReadiness::NotListening) },
            || async { Ok(NativeProgress::Stopped) },
        )
        .await
        .expect_err("an exited service must not be waited on");
        assert!(error.to_string().contains("stopped"), "{error}");
    }

    fn service_exit(restarts: u64) -> gents_server::native_service::ServiceExit {
        gents_server::native_service::ServiceExit {
            reason: "exited with code 78".to_string(),
            restarts,
            clean: false,
        }
    }

    #[tokio::test]
    async fn a_crash_looping_service_fails_with_its_exit_reason() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let observations = AtomicUsize::new(0);
        let error = await_runtime_readiness(
            Duration::from_secs(60),
            Duration::from_millis(5),
            || async { Ok(PortReadiness::NotListening) },
            || {
                let seen = observations.fetch_add(1, Ordering::SeqCst) as u64;
                async move { Ok(NativeProgress::Exited(service_exit(1 + seen / 4))) }
            },
        )
        .await
        .expect_err("a crash loop must not be waited out");
        assert!(error.to_string().contains("exited with code 78"), "{error}");
        assert!(
            error.to_string().contains("restarted 2 times in a row"),
            "{error}"
        );
        assert_eq!(observations.load(Ordering::SeqCst), 9);
    }

    #[tokio::test]
    async fn a_single_exit_between_respawns_is_not_a_crash_loop() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let observations = AtomicUsize::new(0);
        let ready = await_runtime_readiness(
            Duration::from_secs(5),
            Duration::from_millis(1),
            || {
                let seen = observations.load(Ordering::SeqCst);
                async move {
                    Ok(if seen >= 30 {
                        PortReadiness::Ready(ready_status("did:key:respawned"))
                    } else {
                        PortReadiness::NotListening
                    })
                }
            },
            || {
                let seen = observations.fetch_add(1, Ordering::SeqCst);
                async move {
                    Ok(if seen < 20 {
                        NativeProgress::Exited(service_exit(0))
                    } else {
                        NativeProgress::Loaded
                    })
                }
            },
        )
        .await
        .expect("an exit that waits out the respawn throttle keeps waiting");
        assert!(matches!(ready, Readiness::Ready(_)));
    }

    #[tokio::test]
    async fn readiness_hands_a_blocked_job_to_the_approval_wait() {
        let readiness = await_runtime_readiness(
            Duration::from_secs(5),
            Duration::from_millis(5),
            || async { Ok(PortReadiness::NotListening) },
            || async { Ok(NativeProgress::AwaitingApproval) },
        )
        .await
        .unwrap();
        assert!(matches!(readiness, Readiness::ApprovalRequired));
    }

    #[tokio::test]
    async fn readiness_wait_is_bounded() {
        let error = await_runtime_readiness(
            Duration::from_millis(20),
            Duration::from_millis(5),
            || async { Ok(PortReadiness::NotListening) },
            || async { Ok(NativeProgress::Loaded) },
        )
        .await
        .expect_err("a runtime that never becomes ready must time out");
        assert!(error
            .to_string()
            .contains("did not publish runtime readiness"));
    }

    fn orchestration_state() -> (tempfile::TempDir, DesktopAppState) {
        use crate::config::{
            AgentHomePolicy, AppMeta, BootstrapPolicy, BridgeConfig, HomePolicy,
            ManagedServerPolicy,
        };
        use crate::snapshot::projection::SnapshotGrants;
        use crate::state::resolve_policy;

        let temp = tempfile::tempdir().expect("temporary bridge roots");
        let policy = resolve_policy(
            &BridgeConfig {
                home: HomePolicy::FixedRoot(temp.path().join("desktop")),
                bootstrap: BootstrapPolicy::LocalRuntimeAllowed {
                    agent_home: AgentHomePolicy::Fixed(temp.path().join("agent")),
                },
                app_meta: AppMeta {
                    app_name: "managed-start-test".to_string(),
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                },
                snapshot_grants: SnapshotGrants::core_only(),
                managed_server: ManagedServerPolicy::Allowed,
            },
            None,
        )
        .expect("fixed bridge policy");
        (temp, DesktopAppState::new(policy))
    }

    #[derive(Default)]
    struct FakeLaunch {
        approval: std::sync::Mutex<std::collections::VecDeque<Result<bool, BridgeError>>>,
        approval_blocks: bool,
        approval_gated: bool,
        approval_release: tokio::sync::Notify,
        starts: std::sync::Mutex<std::collections::VecDeque<Result<(), BridgeError>>>,
        readiness: std::sync::Mutex<std::collections::VecDeque<Readiness>>,
        start_calls: std::sync::atomic::AtomicUsize,
        approval_waits: std::sync::atomic::AtomicUsize,
        waiting: tokio::sync::Notify,
    }

    impl ManagedLaunch for FakeLaunch {
        async fn requires_approval(&self) -> Result<bool, BridgeError> {
            self.approval
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(false))
        }

        async fn await_approval(&self) -> anyhow::Result<()> {
            self.approval_waits
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.waiting.notify_one();
            if self.approval_blocks {
                std::future::pending::<()>().await;
            }
            if self.approval_gated {
                self.approval_release.notified().await;
            }
            Ok(())
        }

        async fn start(&self) -> Result<(), BridgeError> {
            self.start_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.starts.lock().unwrap().pop_front().unwrap_or(Ok(()))
        }

        async fn await_ready(&self) -> anyhow::Result<Readiness> {
            let next = self.readiness.lock().unwrap().pop_front();
            match next {
                Some(readiness) => Ok(readiness),
                None => {
                    self.waiting.notify_one();
                    std::future::pending().await
                }
            }
        }
    }

    impl FakeLaunch {
        fn starts(&self) -> usize {
            self.start_calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[tokio::test]
    async fn start_waits_for_approval_without_the_lifecycle_lock_then_launches() {
        let (_temp, state) = orchestration_state();
        let launch = FakeLaunch {
            approval: std::sync::Mutex::new([Ok(true)].into()),
            readiness: std::sync::Mutex::new([Readiness::Ready(ready_status("did:key:a"))].into()),
            ..Default::default()
        };
        let token = begin_start_wait(&state).await;
        let lifecycle = state.managed_server_lifecycle.lock().await;
        let (ready, _lifecycle) = launch_managed_server(&state, &token, lifecycle, &launch)
            .await
            .ok()
            .expect("start continues after approval");
        assert_eq!(ready.agent_did.as_deref(), Some("did:key:a"));
        assert_eq!(
            launch
                .approval_waits
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(launch.starts(), 1);
    }

    #[tokio::test]
    async fn a_start_refused_for_approval_is_retried_once_after_approval() {
        let (_temp, state) = orchestration_state();
        let launch = FakeLaunch {
            approval: std::sync::Mutex::new([Ok(false), Ok(true)].into()),
            starts: std::sync::Mutex::new(
                [Err(BridgeError::untyped("blocked by Login Items")), Ok(())].into(),
            ),
            readiness: std::sync::Mutex::new([Readiness::Ready(ready_status("did:key:b"))].into()),
            ..Default::default()
        };
        let token = begin_start_wait(&state).await;
        let lifecycle = state.managed_server_lifecycle.lock().await;
        assert!(launch_managed_server(&state, &token, lifecycle, &launch)
            .await
            .is_ok());
        assert_eq!(launch.starts(), 2);
        assert_eq!(
            launch
                .approval_waits
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    #[tokio::test]
    async fn a_failed_approval_check_keeps_the_original_start_error() {
        let (_temp, state) = orchestration_state();
        let launch = FakeLaunch {
            approval: std::sync::Mutex::new(
                [
                    Ok(false),
                    Err(BridgeError::untyped("launchctl unavailable")),
                ]
                .into(),
            ),
            starts: std::sync::Mutex::new([Err(BridgeError::untyped("kickstart failed"))].into()),
            ..Default::default()
        };
        let token = begin_start_wait(&state).await;
        let lifecycle = state.managed_server_lifecycle.lock().await;
        let failure = launch_managed_server(&state, &token, lifecycle, &launch)
            .await
            .err()
            .expect("start fails");
        assert!(failure.error.to_string().contains("kickstart failed"));
        assert!(failure.attempted_start);
        assert!(failure.lifecycle.is_some());
    }

    #[tokio::test]
    async fn a_job_blocked_after_launch_waits_for_approval_and_launches_again() {
        let (_temp, state) = orchestration_state();
        let launch = FakeLaunch {
            readiness: std::sync::Mutex::new(
                [
                    Readiness::ApprovalRequired,
                    Readiness::Ready(ready_status("did:key:c")),
                ]
                .into(),
            ),
            ..Default::default()
        };
        let token = begin_start_wait(&state).await;
        let lifecycle = state.managed_server_lifecycle.lock().await;
        assert!(launch_managed_server(&state, &token, lifecycle, &launch)
            .await
            .is_ok());
        assert_eq!(launch.starts(), 2);
    }

    #[tokio::test]
    async fn stop_during_the_approval_wait_returns_promptly_and_nothing_launches() {
        let (_temp, state) = orchestration_state();
        let launch = FakeLaunch {
            approval: std::sync::Mutex::new([Ok(true)].into()),
            approval_blocks: true,
            ..Default::default()
        };
        let token = begin_start_wait(&state).await;
        let lifecycle = state.managed_server_lifecycle.lock().await;
        let start = launch_managed_server(&state, &token, lifecycle, &launch);
        let stop = async {
            launch.waiting.notified().await;
            tokio::time::timeout(Duration::from_secs(1), async {
                cancel_start_wait(&state).await;
                drop(state.managed_server_lifecycle.lock().await);
            })
            .await
            .expect("stop takes the lifecycle lock while start waits");
        };
        let (failure, ()) =
            tokio::time::timeout(Duration::from_secs(1), async { tokio::join!(start, stop) })
                .await
                .expect("start ends promptly after stop");
        let failure = failure.err().expect("a cancelled start fails");
        assert!(failure.error.to_string().contains("cancelled"));
        assert!(!failure.attempted_start);
        assert_eq!(launch.starts(), 0);
        assert!(token.is_cancelled());
        let managed = state.managed_server.lock().await;
        assert!(!managed.starting);
        assert!(managed.start_wait.is_none());
        assert!(managed.last_error.is_none());
    }

    #[tokio::test]
    async fn restart_during_the_readiness_wait_takes_over_without_a_rollback() {
        let (_temp, state) = orchestration_state();
        let launch = FakeLaunch::default();
        let token = begin_start_wait(&state).await;
        let lifecycle = state.managed_server_lifecycle.lock().await;
        let start = launch_managed_server(&state, &token, lifecycle, &launch);
        let restart = async {
            launch.waiting.notified().await;
            tokio::time::timeout(Duration::from_secs(1), async {
                cancel_start_wait(&state).await;
                state.managed_server_lifecycle.lock().await
            })
            .await
            .expect("restart takes the lifecycle lock while start waits")
        };
        let (failure, restart_lifecycle) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(start, restart)
        })
        .await
        .expect("start ends promptly after restart");
        let failure = failure.err().expect("the superseded start fails");
        assert!(failure.attempted_start);
        assert!(failure.lifecycle.is_none(), "restart owns the lifecycle");
        assert!(
            token.is_cancelled(),
            "the start must not roll back the restarted job"
        );
        assert_eq!(launch.starts(), 1);
        drop(restart_lifecycle);
    }

    #[tokio::test]
    async fn a_booting_runtime_is_adopted_without_holding_the_lifecycle_lock() {
        let (_temp, state) = orchestration_state();
        let token = begin_start_wait(&state).await;
        let lifecycle = state.managed_server_lifecycle.lock().await;
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
        let adopt = adopt_booting_runtime(&state, &token, lifecycle, async {
            ready_rx.await.unwrap();
            Ok(Readiness::Ready(ready_status("did:key:booted")))
        });
        let observer = async {
            tokio::task::yield_now().await;
            drop(
                tokio::time::timeout(
                    Duration::from_secs(1),
                    state.managed_server_lifecycle.lock(),
                )
                .await
                .expect("the lock is free while the runtime boots"),
            );
            ready_tx.send(()).unwrap();
        };
        let (adopted, ()) = tokio::join!(adopt, observer);
        match adopted.unwrap() {
            BootOutcome::Ready(ready, _lifecycle) => {
                assert_eq!(ready.agent_did.as_deref(), Some("did:key:booted"))
            }
            BootOutcome::NeedsLaunch(..) => panic!("a ready runtime is adopted"),
        }

        let token = begin_start_wait(&state).await;
        let lifecycle = state.managed_server_lifecycle.lock().await;
        let blocked = adopt_booting_runtime(&state, &token, lifecycle, async {
            Ok(Readiness::ApprovalRequired)
        })
        .await
        .unwrap();
        assert!(matches!(blocked, BootOutcome::NeedsLaunch(..)));
    }

    #[test]
    fn a_crash_looping_job_is_reported_failed_with_its_exit_reason() {
        let native = gents_server::native_service::NativeServiceStatus {
            installed: true,
            running: false,
            job_loaded: true,
            enabled: true,
            requires_approval: false,
            detail: None,
        };
        let idle = ManagedServerRuntimeState::default();
        let status = status_from(&idle, None, Some(&native), Some(&service_exit(7)), None);
        assert_eq!(
            status.state,
            ManagedServerState::Starting,
            "a high restart count from earlier failures is not a crash loop by itself"
        );
        let status = status_from(&idle, None, Some(&native), Some(&service_exit(9)), Some(2));
        assert_eq!(status.state, ManagedServerState::Failed);
        let error = status.error.unwrap();
        assert!(error.contains("exited with code 78"));
        assert!(error.contains("restarted 2 times in a row"));

        let blocked = gents_server::native_service::NativeServiceStatus {
            requires_approval: true,
            ..native
        };
        let status = status_from(&idle, None, Some(&blocked), None, None);
        assert!(status.approval_required);
        assert_ne!(status.state, ManagedServerState::Starting);
    }

    #[tokio::test]
    async fn pending_background_approval_is_published_then_startup_continues() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let checks = AtomicUsize::new(0);
        let published = AtomicUsize::new(0);
        await_background_approval(
            Duration::from_secs(5),
            Duration::from_millis(5),
            || {
                let attempt = checks.fetch_add(1, Ordering::SeqCst);
                async move { Ok(attempt < 3) }
            },
            || {
                published.fetch_add(1, Ordering::SeqCst);
                async {}
            },
        )
        .await
        .expect("startup continues once approval is granted");

        assert_eq!(checks.load(Ordering::SeqCst), 4);
        assert_eq!(published.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn background_approval_wait_is_bounded_and_names_login_items() {
        let error = await_background_approval(
            Duration::from_millis(20),
            Duration::from_millis(5),
            || async { Ok(true) },
            || async {},
        )
        .await
        .expect_err("approval that never arrives must time out");
        assert!(error.to_string().contains("Login Items"), "{error}");
    }

    #[test]
    fn native_approval_state_is_part_of_the_managed_status() {
        let native = gents_server::native_service::NativeServiceStatus {
            installed: true,
            running: false,
            job_loaded: false,
            enabled: true,
            requires_approval: true,
            detail: None,
        };
        let status = status_from(
            &ManagedServerRuntimeState::default(),
            None,
            Some(&native),
            None,
            None,
        );
        assert!(status.approval_required);
        let observed = project_external_status(ready_status("did:key:ready"), &native);
        assert!(!observed.approval_required);
    }

    #[tokio::test]
    async fn stop_queued_behind_a_locked_start_cancels_it_before_launch() {
        let (_temp, state) = orchestration_state();
        let lifecycle = state.managed_server_lifecycle.lock().await;
        let token = begin_start_wait(&state).await;
        let stop = lock_lifecycle_superseding_start(&state);
        tokio::pin!(stop);
        assert!(
            futures_poll_once(stop.as_mut()).await.is_none(),
            "stop queues behind the locked start"
        );
        let launch = FakeLaunch::default();
        let failure = launch_managed_server(&state, &token, lifecycle, &launch)
            .await
            .err()
            .expect("a start cancelled while it held the lock does not launch");
        assert!(failure.error.to_string().contains("stopped"));
        assert_eq!(launch.starts(), 0);
        drop(failure);
        let _stop_lifecycle = tokio::time::timeout(Duration::from_secs(1), stop)
            .await
            .expect("stop takes the lock");
        let managed = state.managed_server.lock().await;
        assert!(!managed.starting);
        assert!(managed.last_error.is_none());
    }

    #[tokio::test]
    async fn a_newer_start_supersedes_a_waiting_one_with_its_own_message() {
        let (_temp, state) = orchestration_state();
        let first = begin_start_wait(&state).await;
        let second = begin_start_wait(&state).await;
        assert!(first.is_cancelled());
        assert!(!second.is_cancelled());
        assert_eq!(first.cancel_message(), START_SUPERSEDED);
        finish_start_wait(&state, &first).await;
        assert!(
            state.managed_server.lock().await.starting,
            "the superseded start leaves the newer start's state alone"
        );
        cancel_start_wait(&state).await;
        assert_eq!(second.cancel_message(), START_CANCELLED);
    }

    #[tokio::test]
    async fn auto_start_changes_wait_for_a_start_in_progress() {
        let (_temp, state) = orchestration_state();
        let token = begin_start_wait(&state).await;
        assert!(ensure_no_start_waiting(&state).await.is_err());
        finish_start_wait(&state, &token).await;
        assert!(ensure_no_start_waiting(&state).await.is_ok());
    }

    #[tokio::test]
    async fn stop_during_a_restart_readiness_wait_takes_the_lock_and_ends_the_restart() {
        let (_temp, state) = orchestration_state();
        let launch = FakeLaunch::default();
        let lifecycle = lock_lifecycle_superseding_start(&state).await;
        let token = begin_start_wait(&state).await;
        let restart = launch_managed_server(&state, &token, lifecycle, &launch);
        let stop = async {
            launch.waiting.notified().await;
            tokio::time::timeout(
                Duration::from_secs(1),
                lock_lifecycle_superseding_start(&state),
            )
            .await
            .expect("stop takes the lifecycle lock while restart waits")
        };
        let (failure, _stop_lifecycle) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(restart, stop)
        })
        .await
        .expect("restart ends promptly after stop");
        let failure = failure.err().expect("the stopped restart fails");
        assert!(failure.error.to_string().contains("stopped"));
        assert!(failure.attempted_start);
        assert!(failure.lifecycle.is_none());
        assert!(
            token.is_cancelled(),
            "restart must not clean up the stopped job"
        );
        assert!(!state.managed_server.lock().await.starting);
    }

    #[tokio::test]
    async fn stop_queued_behind_a_locked_restart_cancels_its_later_wait() {
        let (_temp, state) = orchestration_state();
        let restart_lifecycle = state.managed_server_lifecycle.lock().await;
        let stop = lock_lifecycle_superseding_start(&state);
        tokio::pin!(stop);
        assert!(futures_poll_once(stop.as_mut()).await.is_none());

        let token = begin_start_wait(&state).await;
        let launch = FakeLaunch::default();
        let restart = launch_managed_server(&state, &token, restart_lifecycle, &launch);
        let stopper = async {
            tokio::time::timeout(Duration::from_secs(1), stop)
                .await
                .expect("stop gets the lock once restart waits")
        };
        let (failure, _stop_lifecycle) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(restart, stopper)
        })
        .await
        .expect("restart ends promptly");
        let failure = failure.err().expect("the restart is cancelled");
        assert!(failure.error.to_string().contains("stopped"));
        assert!(failure.lifecycle.is_none());
        assert!(token.is_cancelled());
        assert!(!state.managed_server.lock().await.starting);
    }

    async fn futures_poll_once<F: Future + Unpin>(future: F) -> Option<F::Output> {
        let mut future = future;
        std::future::poll_fn(|cx| {
            std::task::Poll::Ready(match std::pin::Pin::new(&mut future).poll(cx) {
                std::task::Poll::Ready(output) => Some(output),
                std::task::Poll::Pending => None,
            })
        })
        .await
    }

    #[tokio::test]
    async fn restart_waits_for_approval_without_the_lock_then_launches() {
        let (_temp, state) = orchestration_state();
        let launch = FakeLaunch {
            approval: std::sync::Mutex::new([Ok(true)].into()),
            approval_gated: true,
            readiness: std::sync::Mutex::new([Readiness::Ready(ready_status("did:key:r"))].into()),
            ..Default::default()
        };
        let lifecycle = lock_lifecycle_superseding_start(&state).await;
        let token = begin_start_wait(&state).await;
        let restart = launch_managed_server(&state, &token, lifecycle, &launch);
        let approve = async {
            launch.waiting.notified().await;
            assert_eq!(launch.starts(), 0, "nothing launches before approval");
            assert!(
                state.managed_server_lifecycle.try_lock().is_ok(),
                "the lock is free while restart waits for approval"
            );
            assert!(state.managed_server.lock().await.starting);
            launch.approval_release.notify_one();
        };
        let (restarted, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(restart, approve)
        })
        .await
        .expect("restart continues once approved");
        assert!(restarted.is_ok());
        assert_eq!(launch.starts(), 1);
    }

    #[tokio::test]
    async fn stop_during_a_restart_approval_wait_cancels_it_cleanly() {
        let (_temp, state) = orchestration_state();
        let launch = FakeLaunch {
            approval: std::sync::Mutex::new([Ok(true)].into()),
            approval_blocks: true,
            ..Default::default()
        };
        let lifecycle = lock_lifecycle_superseding_start(&state).await;
        let token = begin_start_wait(&state).await;
        let restart = launch_managed_server(&state, &token, lifecycle, &launch);
        let stop = async {
            launch.waiting.notified().await;
            tokio::time::timeout(
                Duration::from_secs(1),
                lock_lifecycle_superseding_start(&state),
            )
            .await
            .expect("stop takes the lock during the approval wait")
        };
        let (failure, _stop_lifecycle) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(restart, stop)
        })
        .await
        .expect("restart ends promptly after stop");
        let failure = failure.err().expect("the stopped restart fails");
        assert!(failure.error.to_string().contains("stopped"));
        assert!(!failure.attempted_start);
        assert!(failure.lifecycle.is_none());
        assert_eq!(launch.starts(), 0);
        assert!(
            token.is_cancelled(),
            "a cancelled restart skips its cleanup stop"
        );
    }

    #[test]
    fn status_reports_a_crash_loop_only_relative_to_its_first_observation() {
        let mut baseline = None;
        assert_eq!(
            observe_crash_loop(&mut baseline, Some(&service_exit(7))),
            None
        );
        assert_eq!(
            observe_crash_loop(&mut baseline, Some(&service_exit(8))),
            None
        );
        assert_eq!(
            observe_crash_loop(&mut baseline, Some(&service_exit(9))),
            Some(2)
        );
        assert_eq!(observe_crash_loop(&mut baseline, None), None);
        assert_eq!(baseline, None, "a running job resets the baseline");
        assert_eq!(
            observe_crash_loop(&mut baseline, Some(&service_exit(9))),
            None
        );
    }

    #[tokio::test]
    async fn readiness_counts_restarts_from_a_high_first_observation() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let observations = AtomicUsize::new(0);
        let counters = [7, 7, 8, 8, 8, 9];
        let error = await_runtime_readiness(
            Duration::from_secs(60),
            Duration::from_millis(1),
            || async { Ok(PortReadiness::NotListening) },
            || {
                let seen = observations.fetch_add(1, Ordering::SeqCst);
                let restarts = counters[seen.min(counters.len() - 1)];
                async move { Ok(NativeProgress::Exited(service_exit(restarts))) }
            },
        )
        .await
        .expect_err("two new restarts beyond the first observation fail");
        assert!(error.to_string().contains("exited with code 78"), "{error}");
        assert_eq!(
            observations.load(Ordering::SeqCst),
            6,
            "the exit at counter 8 kept waiting; counter 9 failed"
        );
    }

    #[tokio::test]
    async fn a_clean_exit_of_the_loaded_job_is_not_waited_out() {
        let error = await_runtime_readiness(
            Duration::from_secs(60),
            Duration::from_millis(5),
            || async { Ok(PortReadiness::NotListening) },
            || async {
                Ok(NativeProgress::Exited(
                    gents_server::native_service::ServiceExit {
                        reason: "exited normally".to_string(),
                        restarts: 0,
                        clean: true,
                    },
                ))
            },
        )
        .await
        .expect_err("a job that exited normally will not come back by itself");
        assert!(error.to_string().contains("exited normally"), "{error}");
    }

    #[test]
    fn a_loaded_job_that_exited_cleanly_is_reported_stopped() {
        let native = gents_server::native_service::NativeServiceStatus {
            installed: true,
            running: false,
            job_loaded: true,
            enabled: true,
            requires_approval: false,
            detail: None,
        };
        let clean = gents_server::native_service::ServiceExit {
            reason: "exited normally".to_string(),
            restarts: 0,
            clean: true,
        };
        let status = status_from(
            &ManagedServerRuntimeState::default(),
            None,
            Some(&native),
            Some(&clean),
            None,
        );
        assert_eq!(status.state, ManagedServerState::Stopped);
    }

    #[tokio::test]
    async fn a_failed_install_settles_the_restart_instead_of_leaking_its_wait() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let (_temp, state) = orchestration_state();
        let lifecycle = lock_lifecycle_superseding_start(&state).await;
        let token = begin_start_wait(&state).await;
        let launch = FakeLaunch::default();
        let failure = install_then_launch(
            &state,
            &token,
            lifecycle,
            async { Err(BridgeError::untyped("copying the Gents runtime failed")) },
            &launch,
        )
        .await
        .err()
        .expect("a failed install fails the launch");
        assert!(!failure.attempted_start);
        assert_eq!(launch.starts(), 0);

        let cleaned = AtomicBool::new(false);
        let (message, _) = settle_launch_failure(&state, &token, failure, || async {
            cleaned.store(true, Ordering::SeqCst);
            Ok(())
        })
        .await
        .ok()
        .expect("an uncancelled failure is recorded");
        assert!(message.contains("copying the Gents runtime failed"));
        assert!(
            !cleaned.load(Ordering::SeqCst),
            "nothing was started to roll back"
        );
        let managed = state.managed_server.lock().await;
        assert!(!managed.starting);
        assert!(managed.start_wait.is_none());
        assert_eq!(managed.last_error.as_deref(), Some(message.as_str()));
        drop(managed);
        assert!(ensure_no_start_waiting(&state).await.is_ok());
    }

    #[test]
    fn a_reset_restart_counter_restarts_the_crash_loop_baseline() {
        let mut baseline = None;
        assert_eq!(
            observe_crash_loop(&mut baseline, Some(&service_exit(7))),
            None
        );
        assert_eq!(
            observe_crash_loop(&mut baseline, Some(&service_exit(1))),
            None
        );
        assert_eq!(baseline, Some(1), "a lower counter resets the baseline");
        assert_eq!(
            observe_crash_loop(&mut baseline, Some(&service_exit(2))),
            None
        );
        assert_eq!(
            observe_crash_loop(&mut baseline, Some(&service_exit(3))),
            Some(2)
        );
    }

    #[tokio::test]
    async fn readiness_restarts_its_baseline_when_the_counter_drops() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let observations = AtomicUsize::new(0);
        let counters = [7, 1, 2, 3];
        let error = await_runtime_readiness(
            Duration::from_secs(60),
            Duration::from_millis(1),
            || async { Ok(PortReadiness::NotListening) },
            || {
                let seen = observations.fetch_add(1, Ordering::SeqCst);
                let restarts = counters[seen.min(counters.len() - 1)];
                async move { Ok(NativeProgress::Exited(service_exit(restarts))) }
            },
        )
        .await
        .expect_err("two restarts after the reset fail");
        assert!(
            error.to_string().contains("restarted 2 times in a row"),
            "{error}"
        );
        assert_eq!(observations.load(Ordering::SeqCst), 4);
    }

    /// Serves one fixed `/status` body. While `hold` is set, each response
    /// waits for `release` after announcing the request on `received`.
    struct StatusServer {
        port: u16,
        hold: Arc<std::sync::atomic::AtomicBool>,
        received: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        task: tokio::task::JoinHandle<()>,
    }

    impl StatusServer {
        async fn start(body: String) -> Self {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("status listener");
            let port = listener.local_addr().expect("status address").port();
            let hold = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let received = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let (task_hold, task_received, task_release) =
                (hold.clone(), received.clone(), release.clone());
            let task = tokio::spawn(async move {
                loop {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        return;
                    };
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        match stream.read(&mut buffer).await {
                            Ok(0) | Err(_) => break,
                            Ok(read) => request.extend_from_slice(&buffer[..read]),
                        }
                    }
                    if task_hold.load(std::sync::atomic::Ordering::SeqCst) {
                        task_received.notify_one();
                        task_release.notified().await;
                    }
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                }
            });
            Self {
                port,
                hold,
                received,
                release,
                task,
            }
        }

        fn graphql(&self) -> String {
            format!("http://127.0.0.1:{}/api/v0/graphql", self.port)
        }
    }

    async fn skewed_managed_runtime(
        temp: &tempfile::TempDir,
        agent_did: &str,
    ) -> (Arc<ClientCore>, StatusServer) {
        use gents_desktop_core::client::{ClientCoreOptions, DesktopPaths};

        let core = Arc::new(
            ClientCore::start_with_paths_and_options(
                DesktopPaths::from_root(temp.path().join("client")),
                ClientCoreOptions::local_only(),
            )
            .await
            .expect("client core"),
        );
        let mut next_release =
            gents::agent::p2p_reconcile::read_client_replicated_schema(core.node_arc())
                .await
                .expect("local collection versions");
        next_release
            .get_mut("AgentSession")
            .expect("client route replicates AgentSession")
            .version_id = "bafy-next-release".to_string();
        let server = StatusServer::start(
            serde_json::json!({
                "agent_did": agent_did,
                gents_protocol::peer_schema::STATUS_REPLICATED_SCHEMA_FIELD: next_release,
            })
            .to_string(),
        )
        .await;
        core.add_managed_enrollment_peer_for_test(
            agent_did,
            &server.graphql(),
            "/tmp/managed-home",
            1,
        )
        .await
        .expect("paired managed runtime");
        (core, server)
    }

    #[tokio::test]
    async fn paired_managed_runtime_with_skewed_schema_projects_incompatible_sync() {
        use gents_desktop_core::client::{project_sync_health, SyncHealthState};

        let (temp, state) = orchestration_state();
        let agent_did = "did:key:managed-runtime";
        let (core, server) = skewed_managed_runtime(&temp, agent_did).await;
        assert!(core.peer_records().await.iter().any(|peer| {
            peer.agent_did == agent_did
                && peer.is_enrollment()
                && peer.is_managed_runtime()
                && peer.is_chat_ready_at(chrono::Utc::now())
        }));

        let mut updates = core.sync_state_updates();
        start_managed_runtime_pairing(
            &state,
            Arc::clone(&core),
            temp.path().join("agent"),
            ManagedPairingTarget {
                agent_name: "Managed".to_string(),
                agent_did: agent_did.to_string(),
                graphql: server.graphql(),
            },
        )
        .await;
        assert!(
            state.managed_server.lock().await.pairing_task.is_none(),
            "a chat-ready runtime is not re-paired"
        );
        let health = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(health) = project_sync_health(&updates.borrow_and_update()) {
                    if health.state == SyncHealthState::Incompatible {
                        return health;
                    }
                }
                updates.changed().await.expect("sync owner alive");
            }
        })
        .await
        .expect("skewed managed runtime projects incompatible sync");
        assert!(health
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("AgentSession")));

        server.task.abort();
        core.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn managed_schema_fetched_across_a_route_change_is_ignored() {
        let temp = tempfile::tempdir().expect("temp");
        let agent_did = "did:key:managed-runtime";
        let (core, server) = skewed_managed_runtime(&temp, agent_did).await;
        let target = ManagedPairingTarget {
            agent_name: "Managed".to_string(),
            agent_did: agent_did.to_string(),
            graphql: server.graphql(),
        };

        server.hold.store(true, std::sync::atomic::Ordering::SeqCst);
        let in_flight = tokio::spawn({
            let core = Arc::clone(&core);
            let target = target.clone();
            async move { observe_managed_runtime_schema(&core, &target).await }
        });
        tokio::time::timeout(Duration::from_secs(10), server.received.notified())
            .await
            .expect("status request in flight");
        core.add_managed_enrollment_peer_for_test(
            agent_did,
            &server.graphql(),
            "/tmp/managed-home",
            2,
        )
        .await
        .expect("route generation advances");
        server
            .hold
            .store(false, std::sync::atomic::Ordering::SeqCst);
        server.release.notify_one();
        let late = in_flight.await.expect("observation task");
        assert!(late.is_err_and(|error| error.contains("AgentSession")));
        assert!(
            core.sync_state().peer_schema_skew.is_empty(),
            "a response fetched for the replaced generation is not recorded"
        );

        observe_managed_runtime_schema(&core, &target)
            .await
            .unwrap_err();
        assert_eq!(core.sync_state().peer_schema_skew.len(), 1);

        server.task.abort();
        core.shutdown().await.expect("shutdown");
    }
}
