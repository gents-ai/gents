use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Runtime, State};

use gents_desktop_core::client::{ClientCore, EnrollmentRequestResult};
use gents_desktop_core::local_runtime::{
    fetch_runtime_connection_payload, init_standard_local_runtime, DesktopInitOptions,
};
use gents_protocol::serve_lifecycle::{outdated_runtime_message, ObservedServeLifecycle};

use crate::config::ManagedServerPolicy;
use crate::contract::MANAGED_SERVER_UPDATED_EVENT;
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::{current_core, DesktopAppState};
use crate::tauri_commands::service_executable::{
    ensure_installable_location, resolve_service_executable,
};
use crate::types::{
    HomeResetDisposition, IncompatibleStoreScope, IncompatibleStoreView, ManagedServerResetRequest,
    ManagedServerResetResult, ManagedServerRestartRequest, ManagedServerRootValidation,
    ManagedServerRootValidationRequest, ManagedServerStartRequest, ManagedServerState,
    ManagedServerStatus, ManagedServerToolCeiling,
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
    /// The canonical home and init identity the ceiling and root were
    /// reviewed for. A remembered grant applies only to that home.
    #[serde(default)]
    reviewed_for: Option<ReviewedHome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewedHome {
    home: String,
    agent_did: String,
}

/// What an initialized home says about itself in `init.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HomeIdentity {
    reviewed: ReviewedHome,
    tool_ceiling: Option<ManagedServerToolCeiling>,
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
    let stored = load_bound_preference(state).await?;
    let native = run_native(native_service(&app, &state)?, |service| service.status()).await?;
    let last_exit = if (native.job_loaded || native.failed)
        && !native.running
        && !native.requires_approval
    {
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
    // Native process state is not runtime readiness. Probe the status endpoint
    // on every read so the frontend gets the live DID and route.
    let port = match state.policy.agent_home.as_deref() {
        Some(agent_home) => observe_port_readiness(agent_home).await?,
        None => PortReadiness::NotListening,
    };
    let mut managed = state.managed_server.lock().await;
    if native.job_loaded
        || native.background_item == gents_server::native_service::BackgroundItemStatus::Enabled
    {
        managed.approval_refused = false;
    }
    if let (Some(exit), Some(agent_home)) = (last_exit.as_ref(), state.policy.agent_home.as_deref())
    {
        if let Some(kind) = exit.refused_store().filter(|_| !managed.starting) {
            managed.incompatible_store = Some(refused_runtime_store(agent_home, Some(kind)));
        }
    }
    let crash_loop =
        managed
            .crash_loop
            .observe(job_sample(&port, native.job_loaded, last_exit.as_ref()));
    let mut status = status_from(
        &managed,
        stored.as_ref(),
        Some(&native),
        last_exit.as_ref(),
        crash_loop
            .as_ref()
            .map(|(exit, restarts)| (exit, *restarts)),
    );
    let starting = managed.starting;
    drop(managed);

    match port {
        PortReadiness::Ready(external) => status = project_external_status(external, &native),
        // Our own job is restarted at launch and reports starting meanwhile.
        // An external one never becomes ready: say so now.
        PortReadiness::Outdated { version } if !native.job_loaded && !starting => {
            status.state = ManagedServerState::Failed;
            status.error = Some(outdated_runtime_message(version.as_deref()));
        }
        PortReadiness::Booting => {
            status.runtime_booting = true;
            if matches!(
                status.state,
                ManagedServerState::Stopped | ManagedServerState::Disabled
            ) {
                status.state = ManagedServerState::Starting;
            }
        }
        PortReadiness::Foreign(foreign) => {
            project_foreign_port(&mut status, &foreign.message(), starting)
        }
        PortReadiness::Occupied(port) if status.state != ManagedServerState::Starting => {
            project_foreign_port(&mut status, &occupied_port_message(port), starting)
        }
        PortReadiness::Occupied(_)
        | PortReadiness::Outdated { .. }
        | PortReadiness::NotListening => {}
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

fn project_external_status(
    mut external: ManagedServerStatus,
    native: &gents_server::native_service::NativeServiceStatus,
) -> ManagedServerStatus {
    external.auto_start = native.enabled;
    // Approval revoked while the runtime still serves stays visible: the
    // next launch of the job will be blocked.
    external.approval_required = native.requires_approval;
    if native.job_loaded {
        external.state = ManagedServerState::Running;
    }
    external
}

/// Another runtime serves the managed port. Its message replaces generic
/// errors, and a loaded job that cannot bind the port is failed rather than
/// waited on. A start in progress reports the conflict itself.
fn project_foreign_port(status: &mut ManagedServerStatus, message: &str, starting: bool) {
    if starting {
        return;
    }
    status.error = Some(match status.error.take() {
        Some(failure) if failure != message => format!("{failure} {message}"),
        _ => message.to_string(),
    });
    if status.state == ManagedServerState::Starting {
        status.state = ManagedServerState::Failed;
    }
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
    managed.approval_refused = false;
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
    async fn start(&self) -> Result<Launched, BridgeError>;
    async fn await_ready(&self) -> anyhow::Result<Readiness>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Launched {
    Started,
    /// macOS refused the start until the user approves the background item.
    /// Carries launchctl's refusal.
    ApprovalPending(String),
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

    async fn start(&self) -> Result<Launched, BridgeError> {
        let enable_at_login = self.enable_at_login;
        let launched = run_native(
            launchable_native_service(self.app, self.state)?,
            move |service| match service.start(enable_at_login) {
                Ok(()) => Ok(Launched::Started),
                Err(error) => {
                    match error
                        .downcast::<gents_server::native_service::BackgroundApprovalPending>()
                    {
                        Ok(pending) => Ok(Launched::ApprovalPending(pending.detail)),
                        Err(error) => Err(error),
                    }
                }
            },
        )
        .await?;
        self.state.managed_server.lock().await.approval_refused =
            matches!(launched, Launched::ApprovalPending(_));
        Ok(launched)
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
        start_when_approved(state, token, &mut lifecycle, launch).await?;
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
                    start_when_approved(state, token, &mut lifecycle, launch).await?;
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
            attempted_start: attempted_start && !error.is::<RuntimeStillBooting>(),
            error,
            lifecycle,
        }),
    }
}

/// Starts the job, waiting for approval whenever macOS refuses it. Each wait
/// polls approval and is bounded by the approval timeout, so a start that
/// macOS keeps refusing fails with the approval message.
async fn start_when_approved<'a, L: ManagedLaunch>(
    state: &'a DesktopAppState,
    token: &StartWait,
    lifecycle: &mut Option<LifecycleGuard<'a>>,
    launch: &L,
) -> anyhow::Result<()> {
    let started = tokio::time::Instant::now();
    loop {
        let refusal = match launch.start().await {
            Ok(Launched::Started) => return Ok(()),
            Ok(Launched::ApprovalPending(detail)) => {
                // A refusal that approval polling cannot observe would
                // return from the wait at once; report it instead.
                if !launch.requires_approval().await.unwrap_or(false) {
                    anyhow::bail!("launchctl could not start Gents: {detail}");
                }
                Some(detail)
            }
            Err(error) => {
                if !launch.requires_approval().await.unwrap_or(false) {
                    return Err(error.into());
                }
                None
            }
        };
        if started.elapsed() >= BACKGROUND_APPROVAL_TIMEOUT {
            match refusal {
                Some(detail) => anyhow::bail!(
                    "{} (launchctl: {detail})",
                    gents_server::native_service::BACKGROUND_APPROVAL_REQUIRED
                ),
                None => anyhow::bail!(gents_server::native_service::BACKGROUND_APPROVAL_REQUIRED),
            }
        }
        *lifecycle = None;
        wait_unlocked(token, launch.await_approval()).await?;
        *lifecycle = Some(relock_start(state, token).await?);
        ensure_not_cancelled(token)?;
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
    let stored = load_bound_preference(state).await?;
    let authority = start_authority(
        request.tool_ceiling,
        request.tool_root.as_deref(),
        stored.as_ref(),
    )?;
    refuse_renaming_home(&agent_home, agent_name).await?;

    let mut carried_wait = None;
    let mut replace_loaded_job = false;
    let (ready, initial_enabled, _lifecycle) = match matching_external_server(&agent_home).await? {
        Some(external) => (Some(external), false, lifecycle),
        None => {
            // Before anything below unloads a job this launch could not replace.
            ensure_launchable_here(app, state)?;
            let outdated = match observe_port_readiness(&agent_home).await? {
                PortReadiness::Outdated { version } => Some(version),
                _ => None,
            };
            let initial_native =
                run_native(native_service(app, state)?, |service| service.status()).await?;
            if let (Some(version), false) = (&outdated, initial_native.job_loaded) {
                return Err(BridgeError::untyped(outdated_runtime_message(
                    version.as_deref(),
                )));
            }
            let enabled = initial_native.enabled;
            replace_loaded_job = outdated.is_some()
                || (initial_native.job_loaded
                    && !initial_native.running
                    && run_native(native_service(app, state)?, |service| {
                        Ok(service.last_exit()?.is_some()
                            || service.installed_executable_differs()?)
                    })
                    .await
                    .unwrap_or(false));
            if initial_native.is_active_or_transitioning()
                && !initial_native.requires_approval
                && !replace_loaded_job
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
        if replace_loaded_job {
            // The loaded job runs a runtime older than this app, will not run
            // again by itself (a clean exit or a runtime that moved) or is
            // crash looping. Unload it so the current definition can be
            // installed and the launch below starts it fresh.
            run_native(native_service(app, state)?, |service| service.stop(false)).await?;
        }
        // A fresh home has no identity to compare yet. Provisioning is safe
        // either way, and the check below names any other runtime on the port.
        if gents_server::server_host::initialized_home(&agent_home) {
            ensure_default_port_identity(&agent_home).await?;
        }
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
        save_confirmed_preference(
            state,
            &agent_home,
            &StoredManagedServer {
                agent_name: agent_name.to_string(),
                tool_ceiling: Some(tool_ceiling),
                tool_root,
                reviewed_for: None,
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
            state.managed_server.lock().await.incompatible_store = None;
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
    let stored = load_bound_preference(state).await?;
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
    let still_booting = error.is::<RuntimeStillBooting>();
    let typed_error = if still_booting {
        Some(BridgeError::new(
            BridgeErrorCode::RuntimeStillBooting,
            error.to_string(),
        ))
    } else {
        error.downcast_ref::<BridgeError>().cloned()
    };
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
    // A runtime still booting is observed as starting, not as a failure.
    if !still_booting {
        state.managed_server.lock().await.last_error = Some(message.clone());
    }
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
    let refused_kind = refused_kind(&failure.error);
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
    state.managed_server.lock().await.approval_refused = false;
    let refused = typed_error
        .as_ref()
        .is_some_and(|error| error.code == BridgeErrorCode::IncompatibleLocalStore)
        || matches!(
            gents::storage_backend::incompatible_store_kind(&gents::home::default_data_dir(
                agent_home
            )),
            Ok(Some(_))
        );
    let refused_store = refused.then(|| refused_runtime_store(agent_home, refused_kind));
    state.managed_server.lock().await.incompatible_store = refused_store.clone();
    emit_status(app, state).await;
    if let Some(store) = refused_store {
        return BridgeError::new(
            BridgeErrorCode::IncompatibleLocalStore,
            format!("{store} ({message})"),
        );
    }
    match typed_error {
        Some(mut error) => {
            error.message = message;
            error
        }
        None => BridgeError::untyped(message),
    }
}

const ARCHIVE_CONSEQUENCE: &str = "The old home is moved, not changed, into a timestamped backup next to it. Nothing in it is imported into the new home.";
const DELETE_CONSEQUENCE: &str =
    "The old home is permanently deleted. Nothing in it is imported into the new home.";

/// Previews, archives, or deletes a local home this version cannot open.
///
/// In scope are the managed runtime home's own entries (a fixed inventory,
/// [`gents::home::RUNTIME_HOME_ENTRIES`]) when the runtime refused its store,
/// and the desktop client's state when the client refused its store or when
/// the runtime home is retired (the client's pairing is bound to that
/// runtime). Everything else in either root stays in place.
///
/// A preview changes nothing and cancels nothing. The confirmation it returns
/// pins the exact planned set: an action whose plan differs, before or after
/// the service is stopped, is refused.
#[tauri::command]
pub async fn desktop_managed_server_reset<R: Runtime>(
    request: ManagedServerResetRequest,
    app: AppHandle<R>,
    state: State<'_, DesktopAppState>,
) -> Result<ManagedServerResetResult, BridgeError> {
    let disposition = request.disposition.unwrap_or(HomeResetDisposition::Archive);
    let Some(confirmation) = request.confirmation.as_deref() else {
        return Ok(plan_state_reset(&state).await?.preview());
    };
    let _lifecycle = lock_lifecycle_superseding_start(&state).await;
    retire_incompatible_home(&app, &state, confirmation, disposition).await
}

async fn plan_state_reset(state: &DesktopAppState) -> Result<HomeResetPlan, BridgeError> {
    let runtime_home = state
        .policy
        .agent_home
        .as_deref()
        .filter(|_| state.policy.managed_server == ManagedServerPolicy::Allowed);
    let runtime_store = state.managed_server.lock().await.incompatible_store.clone();
    let client_store = state
        .bridge
        .lock()
        .expect("desktop bridge lock poisoned")
        .incompatible_client_store
        .clone();
    let user_home = resolve_user_home(dirs::home_dir())?;
    plan_home_reset(
        runtime_home,
        runtime_store,
        client_store,
        &state.policy.desktop_paths,
        &user_home,
    )
}

async fn retire_incompatible_home<R: Runtime>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
    confirmation: &str,
    disposition: HomeResetDisposition,
) -> Result<ManagedServerResetResult, BridgeError> {
    let plan = plan_state_reset(state).await?;
    plan.check_confirmation(confirmation, disposition)?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
    // Refuse a rename across filesystems before anything is stopped.
    let backup = match disposition {
        HomeResetDisposition::Archive => {
            let backup = plan.backup_path(&stamp)?;
            plan.preflight(&backup)?;
            Some(backup)
        }
        HomeResetDisposition::Delete => None,
    };

    // The store lock this version's runtime and `init` take before opening
    // or creating this home's store. It is taken whether or not the store
    // exists and held (never moved) until the entries are retired, so neither
    // can open or create the store meanwhile. Older runtimes do not take it;
    // the service stop and endpoint check above cover them.
    let _store_lock = match plan.runtime.as_ref() {
        Some((home, _)) => {
            // The service definition names this home; a fresh setup installs
            // it again. Removing it keeps login from launching a runtime on an
            // emptied home.
            run_native(native_service(app, state)?, |service| service.uninstall()).await?;
            let native =
                run_native(native_service(app, state)?, |service| service.status()).await?;
            ensure_managed_runtime_stopped(native.is_active_or_transitioning())?;
            ensure_home_not_served(home).await?;
            Some(lock_home_for_retirement(home)?)
        }
        None => None,
    };
    // Held until the entries are retired: no client start can open the
    // store, peer directory or keys meanwhile.
    let _client = if plan.retires_client_state() {
        drain_managed_runtime_pairing(state).await;
        Some(super::lifecycle::exclude_client(state).await?)
    } else {
        None
    };
    // The stopped service and client may have changed the homes: act only on
    // the set the user confirmed.
    let settled = plan_state_reset(state).await?;
    settled.check_confirmation(confirmation, disposition)?;

    let result = settled.retire(disposition, backup.as_deref())?;
    drop(_client);
    drop(_store_lock);
    {
        let mut managed = state.managed_server.lock().await;
        managed.incompatible_store = None;
        managed.last_error = None;
        managed.crash_loop = CrashLoopWatch::default();
    }
    state
        .bridge
        .lock()
        .expect("desktop bridge lock poisoned")
        .incompatible_client_store = None;
    tracing::info!(
        target: "gents_desktop::managed_server",
        disposition = ?disposition,
        backup = ?result.backup_path,
        retired = result.retired_paths.len(),
        retained = result.retained_paths.len(),
        "retired a local home this version cannot open"
    );
    let _ = app.emit(
        crate::contract::CLIENT_UPDATED_EVENT,
        crate::types::ClientUpdateEvent::coarse("lifecycle"),
    );
    emit_status(app, state).await;
    Ok(result)
}

/// Fails closed unless nothing serves the managed home: no runtime answers
/// as its identity and the standard endpoint refuses connections. An unknown
/// responder is equally unsafe, since its data ownership cannot be
/// established.
async fn ensure_home_not_served(home: &Path) -> Result<(), BridgeError> {
    let config = gents_server::server_host::ServerConfig::standard(home.to_path_buf());
    let managed_address = std::net::SocketAddr::new(config.http_addr, config.http_port);
    if matching_external_server(home).await?.is_some() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "A managed server is still listening. Stop it before resetting its store.",
        ));
    }
    let endpoint_state =
        std::net::TcpStream::connect_timeout(&managed_address, Duration::from_millis(150));
    ensure_managed_endpoint_stopped(managed_address, endpoint_state.map(|_| ()))
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

/// The canonical user home a reset measures its roots against. Without it a
/// broad root cannot be recognized, so the reset is refused rather than
/// guessed at.
fn resolve_user_home(user_home: Option<PathBuf>) -> Result<PathBuf, BridgeError> {
    let user_home = user_home
        .filter(|home| !home.as_os_str().is_empty())
        .ok_or_else(|| {
            BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                "The user home directory cannot be determined; reset was refused.",
            )
        })?;
    std::fs::canonicalize(&user_home).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            format!(
                "The user home directory {} cannot be resolved ({error}); reset was refused.",
                user_home.display()
            ),
        )
    })
}

/// A root whose entries a reset may retire: not the filesystem root, and
/// neither the user's home directory nor any ancestor of it. `root` and
/// `user_home` are canonical, so aliases of either compare equal.
fn ensure_retirable_root(root: &Path, label: &str, user_home: &Path) -> Result<(), BridgeError> {
    let broad = root.parent().is_none() || user_home.starts_with(root);
    if broad {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            format!(
                "The {label} {} is too broad to reset; reset was refused.",
                root.display()
            ),
        ));
    }
    Ok(())
}

fn canonical_root(path: &Path, label: &str) -> Result<PathBuf, BridgeError> {
    std::fs::canonicalize(path).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::Backend,
            format!("Cannot resolve {label} {}: {error}", path.display()),
        )
    })
}

/// What a home reset would retire, derived from the refused stores.
#[derive(Debug)]
struct HomeResetPlan {
    /// The canonical managed home and its refused store.
    runtime: Option<(PathBuf, gents::storage_backend::IncompatibleStore)>,
    client: Option<gents::storage_backend::IncompatibleStore>,
    desktop_root: PathBuf,
    runtime_entries: gents::home::HomeEntries,
    /// The runtime entries a Delete removes: the owned entries, except that
    /// `keys/` is narrowed to the managed home's own key file (other homes
    /// may keep keys there). Archive moves all of `keys/`: it is recoverable.
    delete_home_entries: Vec<PathBuf>,
    /// `keys/`, removed after a Delete only if nothing else is left in it.
    delete_prunes_keys: Option<PathBuf>,
    /// Present client-state entries in scope.
    client_entries: Vec<PathBuf>,
}

/// The managed home's own key file inside its `keys/` directory, as its
/// `init.json` names it (`key_path`, else the default for `agent_name`).
fn own_home_key(home: &Path, keys: &Path) -> Option<PathBuf> {
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(gents::home::init_config_path(home)).ok()?).ok()?;
    let key = record
        .get("key_path")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            record
                .get("agent_name")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(|name| gents::home::default_key_path(home, name))
        })?;
    let parent = std::fs::canonicalize(key.parent()?).ok()?;
    let name = key.file_name()?;
    (parent == keys).then(|| keys.join(name))
}

fn lock_home_for_retirement(home: &Path) -> Result<gents::home::StoreLock, BridgeError> {
    gents::home::lock_home_store(home)
        .map_err(|error| BridgeError::new(BridgeErrorCode::InvalidArgument, format!("{error:#}")))
}

fn is_store_lock(path: &Path) -> bool {
    path.parent().is_some_and(|home| {
        gents::home::default_data_dir(home)
            .file_name()
            .is_some_and(|data| {
                path.file_name()
                    .is_some_and(|name| *name == *format!("{}.lock", data.to_string_lossy()))
            })
    })
}

fn present(path: &Path) -> Result<bool, BridgeError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(BridgeError::new(
            BridgeErrorCode::Backend,
            format!("Inspecting {}: {error}", path.display()),
        )),
    }
}

fn plan_home_reset(
    runtime_home: Option<&Path>,
    runtime_store: Option<gents::storage_backend::IncompatibleStore>,
    client_store: Option<gents::storage_backend::IncompatibleStore>,
    desktop_paths: &gents_desktop_core::client::DesktopPaths,
    user_home: &Path,
) -> Result<HomeResetPlan, BridgeError> {
    let inspect = |path: &Path| {
        gents::storage_backend::incompatible_store_kind(path).map_err(|error| {
            BridgeError::new(
                BridgeErrorCode::Backend,
                format!("Inspecting {}: {error}", path.display()),
            )
        })
    };
    let runtime = match runtime_home {
        Some(configured_home) if present(configured_home)? => {
            reject_symlink(configured_home, "managed home")?;
            let home = canonical_root(configured_home, "managed home")?;
            let data = gents::home::default_data_dir(&home);
            reject_symlink_if_present(&data, "managed data")?;
            reject_symlink_if_present(
                &gents::home::init_config_path(&home),
                "managed init marker",
            )?;
            let marked = inspect(&data)?.map(|kind| gents::storage_backend::IncompatibleStore {
                kind,
                data_path: data.clone(),
            });
            marked
                .or(runtime_store.map(|store| match store.kind {
                    gents::storage_backend::IncompatibleStoreKind::InsecureKey => store,
                    _ => gents::storage_backend::IncompatibleStore {
                        data_path: data.clone(),
                        ..store
                    },
                }))
                .map(|store| (home, store))
        }
        _ => None,
    };
    reject_symlink_if_present(desktop_paths.root(), "desktop client state")?;
    reject_symlink_if_present(desktop_paths.node_data_dir(), "desktop client store")?;
    let client = inspect(desktop_paths.node_data_dir())?
        .map(|kind| gents::storage_backend::IncompatibleStore {
            kind,
            data_path: desktop_paths.node_data_dir().to_path_buf(),
        })
        .or(client_store);
    if runtime.is_none() && client.is_none() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "No local store is marked as legacy or incompatible; reset was refused.",
        ));
    }
    let desktop_root = if present(desktop_paths.root())? {
        canonical_root(desktop_paths.root(), "desktop client state")?
    } else {
        desktop_paths.root().to_path_buf()
    };
    if let Some((home, _)) = runtime.as_ref() {
        ensure_retirable_root(home, "managed home", user_home)?;
    }
    let retires_client_state = runtime.is_some() || client.is_some();
    if retires_client_state {
        ensure_retirable_root(&desktop_root, "desktop client state", user_home)?;
    }
    let runtime_entries = match runtime.as_ref() {
        Some((home, _)) => {
            gents::home::home_entries(home, &[desktop_root.clone()]).map_err(|error| {
                BridgeError::new(
                    BridgeErrorCode::Backend,
                    format!("Listing {}: {error:#}", home.display()),
                )
            })?
        }
        None => gents::home::HomeEntries::default(),
    };
    let mut client_entries = Vec::new();
    if retires_client_state {
        // Entries are derived from the canonical root, never from the raw
        // configured path.
        let canonical = gents_desktop_core::client::DesktopPaths::from_root(&desktop_root);
        for entry in canonical
            .client_state_entries()
            .into_iter()
            .chain(std::iter::once(desktop_root.join(MANAGED_SERVER_CONFIG)))
        {
            if present(&entry)? {
                client_entries.push(entry);
            }
        }
    }
    let mut delete_home_entries = Vec::new();
    let mut delete_prunes_keys = None;
    if let Some((home, _)) = runtime.as_ref() {
        let keys = home.join("keys");
        for entry in &runtime_entries.owned {
            if *entry != keys {
                delete_home_entries.push(entry.clone());
                continue;
            }
            if let Some(key) = own_home_key(home, &keys) {
                if present(&key)? {
                    delete_home_entries.push(key);
                }
            }
            delete_prunes_keys = Some(keys.clone());
        }
    }
    Ok(HomeResetPlan {
        runtime,
        client,
        desktop_root,
        runtime_entries,
        delete_home_entries,
        delete_prunes_keys,
        client_entries,
    })
}

impl HomeResetPlan {
    /// The desktop client state is retired with its runtime: its pairing
    /// names the runtime identity being retired.
    fn retires_client_state(&self) -> bool {
        self.client.is_some() || self.runtime.is_some()
    }

    fn anchor(&self) -> &Path {
        self.runtime
            .as_ref()
            .map_or(self.desktop_root.as_path(), |(home, _)| home.as_path())
    }

    fn home_entries(&self, disposition: HomeResetDisposition) -> &[PathBuf] {
        match disposition {
            HomeResetDisposition::Archive => &self.runtime_entries.owned,
            HomeResetDisposition::Delete => &self.delete_home_entries,
        }
    }

    fn planned(&self, disposition: HomeResetDisposition) -> impl Iterator<Item = &PathBuf> {
        self.home_entries(disposition)
            .iter()
            .chain(&self.client_entries)
    }

    /// Deletion is offered only for stores known to come from an older
    /// release; a store another, possibly newer, build wrote may still be
    /// wanted by that build.
    fn deletable(&self) -> bool {
        self.runtime
            .iter()
            .map(|(_, store)| store)
            .chain(&self.client)
            .all(|store| store.kind.is_older())
    }

    /// A digest of everything the action would touch or leave, so a
    /// confirmation authorizes exactly the previewed set.
    fn digest(&self, disposition: HomeResetDisposition) -> String {
        let mut hasher = blake3::Hasher::new();
        let mut line = |label: &str, path: &Path| {
            hasher.update(label.as_bytes());
            hasher.update(path.as_os_str().as_encoded_bytes());
            hasher.update(b"\n");
        };
        line("anchor", self.anchor());
        for path in self.home_entries(disposition) {
            line("home", path);
        }
        for path in &self.client_entries {
            line("desktop", path);
        }
        // The store lock appears once the reset takes it; it is never retired.
        for path in self
            .runtime_entries
            .retained
            .iter()
            .filter(|path| !is_store_lock(path))
        {
            line("retained", path);
        }
        hasher.finalize().to_hex()[..12].to_string()
    }

    fn confirmation(&self, disposition: HomeResetDisposition) -> String {
        let anchor = self.anchor().to_string_lossy();
        let digest = self.digest(disposition);
        match disposition {
            HomeResetDisposition::Archive => {
                format!("RESET {anchor} AND ARCHIVE LOCAL HISTORY [{digest}]")
            }
            HomeResetDisposition::Delete => format!("DELETE {anchor} PERMANENTLY [{digest}]"),
        }
    }

    fn check_confirmation(
        &self,
        supplied: &str,
        disposition: HomeResetDisposition,
    ) -> Result<(), BridgeError> {
        if disposition == HomeResetDisposition::Delete && !self.deletable() {
            return Err(BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                "A store another Gents version wrote is not deleted from here; back it up or keep it.",
            ));
        }
        if supplied != self.confirmation(disposition) {
            return Err(BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                "The home changed since it was reviewed, or the confirmation does not match this action. Review it again.",
            ));
        }
        Ok(())
    }

    fn preview(&self) -> ManagedServerResetResult {
        let view =
            |scope, store: &gents::storage_backend::IncompatibleStore| IncompatibleStoreView {
                scope,
                path: store.data_path.to_string_lossy().into_owned(),
                detail: store.to_string(),
                older: store.kind.is_older(),
                unsafe_key: store.kind
                    == gents::storage_backend::IncompatibleStoreKind::InsecureKey,
            };
        let paths = |paths: &mut dyn Iterator<Item = &PathBuf>| {
            paths
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        ManagedServerResetResult {
            managed_home: self
                .runtime
                .as_ref()
                .map(|(home, _)| home.to_string_lossy().into_owned()),
            desktop_home: self
                .retires_client_state()
                .then(|| self.desktop_root.to_string_lossy().into_owned()),
            stores: self
                .runtime
                .iter()
                .map(|(_, store)| view(IncompatibleStoreScope::Runtime, store))
                .chain(
                    self.client
                        .iter()
                        .map(|store| view(IncompatibleStoreScope::Client, store)),
                )
                .collect(),
            confirmation: self.confirmation(HomeResetDisposition::Archive),
            delete_confirmation: self
                .deletable()
                .then(|| self.confirmation(HomeResetDisposition::Delete)),
            consequence: ARCHIVE_CONSEQUENCE.into(),
            completed: false,
            disposition: None,
            backup_path: None,
            planned_paths: paths(&mut self.planned(HomeResetDisposition::Archive)),
            delete_paths: if self.deletable() {
                paths(&mut self.planned(HomeResetDisposition::Delete))
            } else {
                Vec::new()
            },
            retired_paths: Vec::new(),
            retained_paths: paths(&mut self.runtime_entries.retained.iter()),
        }
    }

    /// A fresh sibling of the anchor: `<anchor>-backup-<stamp>`.
    fn backup_path(&self, stamp: &str) -> Result<PathBuf, BridgeError> {
        let anchor = self.anchor();
        let (Some(parent), Some(name)) = (anchor.parent(), anchor.file_name()) else {
            return Err(BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                "The home has no parent directory to hold a backup.",
            ));
        };
        let base = format!("{}-backup-{stamp}", name.to_string_lossy());
        let mut backup = parent.join(&base);
        for suffix in 1..100_u8 {
            if !present(&backup)? {
                return Ok(backup);
            }
            backup = parent.join(format!("{base}-{suffix}"));
        }
        Err(BridgeError::new(
            BridgeErrorCode::Backend,
            format!("No free backup name next to {}.", anchor.display()),
        ))
    }

    fn groups(&self, disposition: HomeResetDisposition) -> [gents::home::RetireGroup<'_>; 2] {
        [
            gents::home::RetireGroup {
                name: "home",
                entries: self.home_entries(disposition),
            },
            gents::home::RetireGroup {
                name: "desktop",
                entries: &self.client_entries,
            },
        ]
    }

    fn preflight(&self, backup: &Path) -> Result<(), BridgeError> {
        gents::home::archive_preflight(&self.groups(HomeResetDisposition::Archive), backup)
            .map_err(|error| BridgeError::new(BridgeErrorCode::Backend, format!("{error:#}")))
    }

    fn retire(
        &self,
        disposition: HomeResetDisposition,
        backup: Option<&Path>,
    ) -> Result<ManagedServerResetResult, BridgeError> {
        let retire_as = match (disposition, backup) {
            (HomeResetDisposition::Archive, Some(backup)) => {
                self.preflight(backup)?;
                gents::home::RetireDisposition::Archive { backup }
            }
            (HomeResetDisposition::Delete, None) => gents::home::RetireDisposition::Delete,
            _ => {
                return Err(BridgeError::new(
                    BridgeErrorCode::InvalidArgument,
                    "An archive needs a backup location and a deletion has none.",
                ))
            }
        };
        let mut retired = gents::home::retire_entries(&self.groups(disposition), retire_as)
            .map_err(|error| BridgeError::new(BridgeErrorCode::Backend, format!("{error:#}")))?;
        if disposition == HomeResetDisposition::Delete {
            if let Some(keys) = self.delete_prunes_keys.as_ref() {
                // Only an empty directory is removed; another home's key
                // file keeps it in place.
                if std::fs::remove_dir(keys).is_ok() {
                    retired.push(keys.clone());
                }
            }
        }
        Ok(ManagedServerResetResult {
            consequence: match disposition {
                HomeResetDisposition::Archive => ARCHIVE_CONSEQUENCE,
                HomeResetDisposition::Delete => DELETE_CONSEQUENCE,
            }
            .into(),
            completed: true,
            disposition: Some(disposition),
            backup_path: backup.map(|path| path.to_string_lossy().into_owned()),
            retired_paths: retired
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            ..self.preview()
        })
    }
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
    /// The supervisor gave up on the job and will not restart it.
    Failed(gents_server::native_service::ServiceExit),
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
            return Ok(match service.last_exit()?.filter(|_| status.failed) {
                Some(exit) => NativeProgress::Failed(exit),
                None => NativeProgress::Stopped,
            });
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
    // Binding is the only progress a runtime reports before it is ready, so
    // the bound restarts then. Migrations after an upgrade can take longer.
    let mut progress_at = started;
    let mut booting = false;
    let mut first_exit_restarts = None;
    loop {
        let (observed_booting, occupied) = match probe().await? {
            PortReadiness::Ready(status) => return Ok(Readiness::Ready(status)),
            PortReadiness::Outdated { version } => {
                anyhow::bail!("{}", outdated_runtime_message(version.as_deref()))
            }
            PortReadiness::Foreign(foreign) => anyhow::bail!(foreign.message()),
            // A listener without /status may be this runtime before it
            // answers; it only explains a later failure.
            PortReadiness::Occupied(port) => (false, Some(port)),
            PortReadiness::Booting => (true, None),
            PortReadiness::NotListening => (false, None),
        };
        if observed_booting && !booting {
            booting = true;
            progress_at = tokio::time::Instant::now();
        }
        let explain = |failure: String| match occupied {
            Some(port) => anyhow::anyhow!("{failure}. {}", occupied_port_message(port)),
            None => anyhow::anyhow!(failure),
        };
        match native().await? {
            NativeProgress::AwaitingApproval => return Ok(Readiness::ApprovalRequired),
            NativeProgress::Stopped => {
                return Err(explain(format!(
                    "the native Gents service stopped after {} seconds, before it published runtime readiness",
                    started.elapsed().as_secs()
                )))
            }
            NativeProgress::Exited(exit) | NativeProgress::Failed(exit)
                if exit.incompatible_store() =>
            {
                let error = anyhow::Error::new(BridgeError::new(
                    BridgeErrorCode::IncompatibleLocalStore,
                    format!(
                        "the native Gents service refused its store: it {}",
                        exit.reason
                    ),
                ));
                return Err(match exit.refused_store() {
                    Some(kind) => error.context(RefusedStoreKind(kind)),
                    None => error,
                });
            }
            NativeProgress::Failed(exit) => anyhow::bail!(
                "the native Gents service failed before it published runtime readiness and will not be restarted: it {} after {} restarts",
                exit.reason,
                exit.restarts
            ),
            NativeProgress::Exited(exit) if exit.clean => anyhow::bail!(
                "the native Gents service exited normally before it published runtime readiness, so it will not be restarted"
            ),
            NativeProgress::Exited(exit) => {
                let restarts = restarts_since(&mut first_exit_restarts, exit.restarts);
                if restarts >= CRASH_LOOP_RESTARTS {
                    return Err(explain(format!(
                        "the native Gents service keeps exiting before it publishes runtime readiness: it {} and was restarted {restarts} times in a row",
                        exit.reason
                    )));
                }
            }
            NativeProgress::Loaded => {}
        }
        if progress_at.elapsed() >= timeout {
            if observed_booting {
                return Err(RuntimeStillBooting {
                    waited: started.elapsed(),
                }
                .into());
            }
            anyhow::bail!(
                "the native Gents service is running but did not publish runtime readiness within {} seconds",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(interval).await;
    }
}

/// A running job whose runtime is bound but not ready when the wait ends. It
/// is left running rather than rolled back: it may be migrating its data.
#[derive(Debug)]
struct RuntimeStillBooting {
    waited: Duration,
}

impl std::fmt::Display for RuntimeStillBooting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the Gents runtime is still starting after {} seconds, possibly migrating its data; it keeps starting in the background",
            self.waited.as_secs()
        )
    }
}

impl std::error::Error for RuntimeStillBooting {}

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
    let result = settle_adoption(state, &token, agent_home, adopted).await;
    emit_status(app, state).await;
    result
}

/// Ends the wait on a job that was already running. A job still booting at
/// the bound is reported as such, not as a failure, like a launched one.
async fn settle_adoption<'a>(
    state: &'a DesktopAppState,
    token: &StartWait,
    agent_home: &Path,
    adopted: anyhow::Result<BootOutcome<'a>>,
) -> Result<BootOutcome<'a>, BridgeError> {
    finish_start_wait(state, token).await;
    let error = match adopted {
        Ok(outcome) => return Ok(outcome),
        Err(error) => error,
    };
    let message = format!("{error:#}");
    if error.is::<RuntimeStillBooting>() {
        return Err(BridgeError::new(
            BridgeErrorCode::RuntimeStillBooting,
            message,
        ));
    }
    tracing::warn!(target: "gents_desktop::managed_server", error = %message, "managed Gents server did not become ready");
    let refused = refused_kind(&error);
    let typed = match error.downcast_ref::<BridgeError>() {
        Some(typed) => BridgeError {
            message,
            ..typed.clone()
        },
        None => BridgeError::new(BridgeErrorCode::EndpointUnreachable, message),
    };
    let mut managed = state.managed_server.lock().await;
    managed.last_error = Some(typed.message.clone());
    managed.incompatible_store = (typed.code == BridgeErrorCode::IncompatibleLocalStore)
        .then(|| refused_runtime_store(agent_home, refused));
    Err(typed)
}

async fn native_requires_approval<R: Runtime>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<bool, BridgeError> {
    let refused = state.managed_server.lock().await.approval_refused;
    Ok(
        run_native(native_service(app, state)?, |service| service.status())
            .await?
            .approval_pending(refused),
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
    Foreign(ForeignRuntime),
    /// This home's runtime is bound but has not reported it finished starting.
    Booting,
    /// This home's runtime predates the lifecycle field and never reports ready.
    Outdated {
        version: Option<String>,
    },
    /// A program that is not a Gents runtime accepts connections on the port.
    Occupied(u16),
    NotListening,
}

/// A runtime other than this home's that answers on the managed port.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ForeignRuntime {
    port: u16,
    agent_did: Option<String>,
    home: Option<String>,
}

impl ForeignRuntime {
    fn from_payload(port: u16, payload: &serde_json::Value) -> Self {
        let field = |name: &str| {
            payload
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        Self {
            port,
            agent_did: field("agent_did"),
            home: field("home"),
        }
    }

    fn message(&self) -> String {
        let port = self.port;
        let Some(did) = self.agent_did.as_deref() else {
            return format!(
                "Port {port} is in use by a program that does not advertise a Gents identity, and the local agent needs that port. Quit that program, then start the agent again."
            );
        };
        let (located, home) = match self.home.as_deref() {
            Some(home) => (format!(" from home {home}"), home),
            None => (String::new(), "<its home>"),
        };
        format!(
            "Port {port} is already served by a different Gents agent ({did}){located}, and the local agent needs that port. Stop the other agent with `gents service stop --home {home}` if it runs as a background service, or with Ctrl-C in the terminal running `gents server`. Or restart it on another port with `gents server --home {home} --http-port <port>`."
        )
    }
}

fn occupied_port_message(port: u16) -> String {
    format!(
        "Port {port} is in use by another program that is not a Gents agent, and the local agent needs that port. Quit that program, then start the agent again."
    )
}

/// Who answers on the managed port. Any answer on a fresh home, or one that
/// does not carry the initialized identity, belongs to another runtime.
async fn observe_port_readiness(
    agent_home: &std::path::Path,
) -> Result<PortReadiness, BridgeError> {
    let config = gents_server::server_host::ServerConfig::standard(agent_home.to_path_buf());
    let Some(payload) = default_port_payload(Some(agent_home)).await? else {
        let address = std::net::SocketAddr::new(config.http_addr, config.http_port);
        let accepts = tokio::time::timeout(
            Duration::from_millis(250),
            tokio::net::TcpStream::connect(address),
        )
        .await
        .is_ok_and(|connected| connected.is_ok());
        return Ok(if accepts {
            PortReadiness::Occupied(config.http_port)
        } else {
            PortReadiness::NotListening
        });
    };
    let initialized_did = if gents_server::server_host::initialized_home(agent_home) {
        read_initialized_did(agent_home).await
    } else {
        None
    };
    Ok(classify_port_payload(
        config.http_port,
        initialized_did.as_deref(),
        payload,
    ))
}

fn classify_port_payload(
    port: u16,
    initialized_did: Option<&str>,
    payload: serde_json::Value,
) -> PortReadiness {
    let live_did = payload
        .get("agent_did")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    if ensure_matching_identity(initialized_did, &live_did, port).is_err() {
        return PortReadiness::Foreign(ForeignRuntime::from_payload(port, &payload));
    }
    match ObservedServeLifecycle::observe(&payload) {
        ObservedServeLifecycle::Ready => {
            PortReadiness::Ready(managed_status_from_payload(payload, &live_did))
        }
        ObservedServeLifecycle::Starting => PortReadiness::Booting,
        ObservedServeLifecycle::Outdated { version } => PortReadiness::Outdated { version },
    }
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

/// Refuses a launch from a location the service could not keep running from.
fn ensure_launchable_here<R: Runtime>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<(), BridgeError> {
    let executable = resolve_service_executable(app, state.policy.desktop_paths.root())?;
    ensure_installable_location(executable.service_path())
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
    if let Err(error) = ensure_installable_location(executable.service_path()) {
        tracing::warn!(
            target: "gents_desktop::managed_server",
            error = %error.message,
            "leaving the installed service definition unchanged"
        );
        return Ok(());
    }
    let service = native_service(app, state)?;
    let status = service.status().map_err(native_error)?;
    if !status.installed {
        return Ok(());
    }
    if status.is_active_or_transitioning() {
        // A loaded job that is not running and names a runtime that moved
        // cannot launch again. Unload it so its definition can be replaced.
        if !status.running
            && service
                .installed_executable_differs()
                .map_err(native_error)?
        {
            tracing::info!(
                target: "gents_desktop::managed_server",
                runtime = %executable.service_path().display(),
                "reinstalling the stopped agent service, whose definition names a runtime that moved"
            );
            service.stop(false).map_err(native_error)?;
            return service.install().map_err(native_error);
        }
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

/// Our own running job still runs a runtime older than this app, which never
/// reports ready. Restart it on the refreshed definition and executable at
/// launch; status reads report it starting meanwhile rather than stopped.
pub(crate) async fn restart_outdated_managed_job<R: Runtime>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<(), BridgeError> {
    let Some(agent_home) = state.policy.agent_home.clone() else {
        return Ok(());
    };
    let _lifecycle = state.managed_server_lifecycle.lock().await;
    let PortReadiness::Outdated { version } = observe_port_readiness(&agent_home).await? else {
        return Ok(());
    };
    if !run_native(native_service(app, state)?, |service| service.status())
        .await?
        .running
    {
        return Ok(());
    }
    ensure_launchable_here(app, state)?;
    tracing::info!(
        target: "gents_desktop::managed_server",
        version = version.as_deref().unwrap_or("unknown"),
        "restarting the agent service, whose runtime predates this app"
    );
    state.managed_server.lock().await.starting = true;
    emit_status(app, state).await;
    let restarted = run_native(launchable_native_service(app, state)?, |service| {
        service.stop(false)?;
        service.install()?;
        service.start(false)
    })
    .await;
    {
        let mut managed = state.managed_server.lock().await;
        managed.starting = false;
        if let Err(error) = &restarted {
            managed.last_error = Some(format!(
                "{} Restarting it failed: {}",
                outdated_runtime_message(version.as_deref()),
                error.message
            ));
        }
    }
    emit_status(app, state).await;
    restarted
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
        ensure_installable_location(executable.service_path())?;
        executable.install()?;
    }
    let mut config = gents_server::native_service::NativeServiceConfig::new(
        home,
        executable.service_path().to_path_buf(),
    )
    .map_err(native_error)?;
    config.stderr_path = Some(crate::logging::runtime_error_log(
        state.policy.desktop_paths.root(),
    ));
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
    {
        let mut managed = state.managed_server.lock().await;
        if !managed
            .schema_observation_task
            .as_ref()
            .is_some_and(|task| !task.inner().is_finished())
        {
            let observed_core = Arc::clone(&core);
            let observed_target = target.clone();
            managed.schema_observation_task = Some(tauri::async_runtime::spawn(async move {
                if let Err(error) =
                    observe_managed_runtime_schema(&observed_core, &observed_target).await
                {
                    tracing::warn!(
                        target: "gents_desktop::managed_server",
                        agent_did = %observed_target.agent_did,
                        error = %error,
                        "managed runtime replicated schema observation failed"
                    );
                }
            }));
        }
    }

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
    let cancel = tokio_util::sync::CancellationToken::new();
    managed.pairing_cancel = Some(cancel.clone());
    managed.pairing_task = Some(tauri::async_runtime::spawn(async move {
        let paired = retry_managed_pairing(
            &cancel,
            MANAGED_PAIRING_ATTEMPTS,
            MANAGED_PAIRING_RETRY_DELAY,
            || ensure_managed_runtime_pairing(Arc::clone(&core), &agent_home, &target, &cancel),
        )
        .await;
        match paired {
            Ok(()) => {}
            Err(PairingFailure::Cancelled) => {
                tracing::info!(
                    target: "gents_desktop::managed_server",
                    agent_did = %target.agent_did,
                    "background managed runtime pairing was cancelled"
                );
            }
            Err(error) => {
                tracing::warn!(
                    target: "gents_desktop::managed_server",
                    agent_did = %target.agent_did,
                    error = %error,
                    "background managed runtime pairing failed"
                );
            }
        }
    }));
}

/// Background pairing retries a failed attempt this many times in total.
const MANAGED_PAIRING_ATTEMPTS: u32 = 4;
/// The wait before retry `n` is `n` times this.
const MANAGED_PAIRING_RETRY_DELAY: Duration = Duration::from_secs(2);
/// How long one attempt waits for its approved enrollment to become chat-ready.
const MANAGED_PAIRING_APPROVAL_WINDOW: Duration = Duration::from_secs(30);
const MANAGED_PAIRING_POLL_INTERVAL: Duration = Duration::from_millis(250);
const MANAGED_PAIRING_CANCELLED: &str = "managed runtime pairing was cancelled";

/// Why a pairing attempt ended without a chat-ready route.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PairingFailure {
    Cancelled,
    /// Retrying cannot help: the runtime is incompatible, outdated, or is
    /// not the identity this home pairs with.
    Permanent(String),
    Transient(String),
}

/// Failures no retry of the same runtime can resolve.
const PERMANENT_PAIRING_ERRORS: &[&str] = &["incompatible", "does not match", "predates this app"];

impl PairingFailure {
    fn classify(message: String) -> Self {
        if PERMANENT_PAIRING_ERRORS
            .iter()
            .any(|permanent| message.contains(permanent))
        {
            Self::Permanent(message)
        } else {
            Self::Transient(message)
        }
    }
}

impl std::fmt::Display for PairingFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str(MANAGED_PAIRING_CANCELLED),
            Self::Permanent(message) | Self::Transient(message) => f.write_str(message),
        }
    }
}

/// Runs pairing attempts until one succeeds, fails permanently, the attempts
/// run out, or the pairing is cancelled.
async fn retry_managed_pairing<A, AF>(
    cancel: &tokio_util::sync::CancellationToken,
    attempts: u32,
    delay: Duration,
    mut attempt: A,
) -> Result<(), PairingFailure>
where
    A: FnMut() -> AF,
    AF: Future<Output = Result<(), PairingFailure>>,
{
    let mut failure = String::new();
    for number in 1..=attempts {
        match attempt().await {
            Ok(()) => return Ok(()),
            Err(_) if cancel.is_cancelled() => return Err(PairingFailure::Cancelled),
            Err(PairingFailure::Transient(error)) => failure = error,
            Err(terminal) => return Err(terminal),
        }
        if number == attempts {
            break;
        }
        tracing::info!(
            target: "gents_desktop::managed_server",
            attempt = number,
            attempts,
            error = %failure,
            "managed runtime pairing attempt failed; retrying"
        );
        tokio::select! {
            () = tokio::time::sleep(delay * number) => {}
            () = cancel.cancelled() => return Err(PairingFailure::Cancelled),
        }
    }
    Err(PairingFailure::Transient(format!(
        "{failure} (after {attempts} attempts)"
    )))
}

enum PairingApproval {
    Committed,
    NotVisibleYet,
}

/// Approves an authored enrollment and waits for it to become chat-ready.
/// Cancellation is observed between steps; each step is left to finish.
async fn await_managed_pairing_approval<P, PF, A, AF, R, RF>(
    cancel: &tokio_util::sync::CancellationToken,
    window: Duration,
    interval: Duration,
    mut paired: P,
    mut approve: A,
    mut repair: R,
) -> Result<(), PairingFailure>
where
    P: FnMut() -> PF,
    PF: Future<Output = bool>,
    A: FnMut() -> AF,
    AF: Future<Output = Result<PairingApproval, String>>,
    R: FnMut() -> RF,
    RF: Future<Output = Result<(), String>>,
{
    let deadline = tokio::time::Instant::now() + window;
    let mut approval_committed = false;
    loop {
        if cancel.is_cancelled() {
            return Err(PairingFailure::Cancelled);
        }
        if paired().await {
            return Ok(());
        }
        if !approval_committed {
            match approve().await.map_err(PairingFailure::classify)? {
                PairingApproval::Committed => approval_committed = true,
                PairingApproval::NotVisibleYet => {}
            }
        }
        repair().await.map_err(PairingFailure::Transient)?;
        if tokio::time::Instant::now() >= deadline {
            return Err(PairingFailure::Transient(
                "timed out waiting for managed runtime pairing".to_string(),
            ));
        }
        tokio::select! {
            () = tokio::time::sleep(interval) => {}
            () = cancel.cancelled() => return Err(PairingFailure::Cancelled),
        }
    }
}

/// How long a just-started runtime may take to report it finished starting.
/// Its replicated schema and enrollment offer are published by then.
const MANAGED_SCHEMA_PUBLISH_WINDOW: Duration = Duration::from_secs(30);

/// Fetch the managed runtime's `/status`, retrying within
/// [`MANAGED_SCHEMA_PUBLISH_WINDOW`] while it is unreachable or still starting.
async fn fetch_managed_runtime_status(
    graphql: &str,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<serde_json::Value, String> {
    let mut status_url = reqwest::Url::parse(graphql)
        .map_err(|error| format!("parsing managed runtime GraphQL URL: {error}"))?;
    status_url.set_path("/status");
    status_url.set_query(None);
    status_url.set_fragment(None);
    let deadline = tokio::time::Instant::now() + MANAGED_SCHEMA_PUBLISH_WINDOW;
    loop {
        let fetched = tokio::select! {
            fetched = fetch_runtime_connection_payload(status_url.as_str()) => {
                fetched.map_err(|error| format!("{error:#}"))
            }
            () = cancel.cancelled() => return Err("managed runtime status wait was cancelled".to_string()),
        };
        let observed = fetched.as_ref().ok().map(ObservedServeLifecycle::observe);
        if let Some(ObservedServeLifecycle::Outdated { version }) = observed {
            return Err(outdated_runtime_message(version.as_deref()));
        }
        if observed.is_some_and(|observed| observed.is_ready())
            || tokio::time::Instant::now() >= deadline
        {
            return fetched;
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(500)) => {}
            () = cancel.cancelled() => return Err("managed runtime status wait was cancelled".to_string()),
        }
    }
}

async fn observe_managed_runtime_schema(
    core: &ClientCore,
    target: &ManagedPairingTarget,
) -> Result<(), String> {
    let observation = core
        .begin_runtime_schema_observation(&target.agent_did, &target.graphql)
        .ok_or_else(|| {
            format!(
                "managed runtime {} no longer routes through {}",
                target.agent_did, target.graphql
            )
        })?;
    let endpoint = observation
        .endpoint()
        .ok_or_else(|| "managed runtime route has no endpoint".to_string())?;
    let status =
        fetch_managed_runtime_status(endpoint, &tokio_util::sync::CancellationToken::new()).await?;
    core.finish_runtime_schema_observation(&observation, &status)
        .await
        .map_err(|error| format!("{error:#}"))
}

async fn ensure_managed_runtime_pairing(
    core: Arc<ClientCore>,
    agent_home: &std::path::Path,
    target: &ManagedPairingTarget,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(), PairingFailure> {
    if core.peer_records().await.iter().any(|peer| {
        peer.agent_did == target.agent_did
            && peer.is_enrollment()
            && peer.is_managed_runtime()
            && peer.is_chat_ready_at(chrono::Utc::now())
    }) {
        return Ok(());
    }

    let status = fetch_managed_runtime_status(&target.graphql, cancel)
        .await
        .map_err(|error| {
            if cancel.is_cancelled() {
                PairingFailure::Cancelled
            } else {
                PairingFailure::classify(format!(
                    "loading managed runtime enrollment offer: {error}"
                ))
            }
        })?;
    if cancel.is_cancelled() {
        return Err(PairingFailure::Cancelled);
    }
    let live_peer = live_server_peer(&status);
    // Each /status mints a fresh offer, so authoring again would leave the
    // earlier request pending where it could still be approved. A request
    // stays in use until it expires or is denied.
    let request_id = resolve_managed_request(
        cancel,
        || async {
            core.active_status_enrollment_requests()
                .await
                .map(|requests| {
                    select_managed_request(requests, &target.agent_did, live_peer.as_deref())
                })
                .map_err(|error| format!("{error:#}"))
        },
        |request_id| {
            let core = Arc::clone(&core);
            async move {
                core.resend_status_enrollment(&request_id)
                    .await
                    .map_err(|error| format!("{error:#}"))
            }
        },
        // Authoring installs and unwinds bootstrap replication state, so it
        // is not interrupted; cancellation is observed once it returns.
        || async {
            core.request_status_enrollment_with_label(&status, Some(&target.agent_name))
                .await
                .map(|request| request.request_id)
                .map_err(|error| {
                    PairingFailure::classify(format!(
                        "requesting managed runtime enrollment: {error:#}"
                    ))
                })
        },
    )
    .await?;

    await_managed_pairing_approval(
        cancel,
        MANAGED_PAIRING_APPROVAL_WINDOW,
        MANAGED_PAIRING_POLL_INTERVAL,
        || async {
            core.peer_records().await.iter().any(|peer| {
                peer.agent_did == target.agent_did
                    && peer.is_enrollment()
                    && peer.is_managed_runtime()
                    && peer.is_chat_ready_at(chrono::Utc::now())
            })
        },
        || async {
            match gents_server::server_host::approve_managed_client_enrollment(
                agent_home,
                &target.graphql,
                &request_id,
            )
            .await
            {
                Ok(()) => Ok(PairingApproval::Committed),
                Err(error) => {
                    let message = format!("{error:#}");
                    if enrollment_request_is_not_visible_yet(&message) {
                        tracing::debug!(
                            request_id = %request_id,
                            "managed enrollment request is not yet visible to operator approval"
                        );
                        Ok(PairingApproval::NotVisibleYet)
                    } else {
                        Err(format!("approving managed runtime enrollment: {message}"))
                    }
                }
            }
        },
        || async {
            core.request_p2p_repair()
                .await
                .map_err(|error| format!("requesting managed route reconciliation: {error:#}"))
        },
    )
    .await?;
    tracing::info!(
        target: "gents_desktop::managed_server",
        agent_did = %target.agent_did,
        "managed runtime desktop pairing is ready"
    );
    Ok(())
}

/// The peer serving the live runtime's enrollment offer, falling back to the
/// peer its status reports. Used only to select among this desktop's own
/// requests; each was authenticated against its server when authored.
fn live_server_peer(status: &serde_json::Value) -> Option<String> {
    status
        .pointer("/enrollment/token")
        .and_then(serde_json::Value::as_str)
        .and_then(|token| gents_protocol::enrollment::decode_offer(token).ok())
        .map(|offer| offer.server_peer)
        .or_else(|| {
            status
                .get("p2p_peer_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .filter(|peer| !peer.trim().is_empty())
}

/// This desktop's newest unexpired, undenied request to pair with the runtime
/// serving from `live_peer`.
fn select_managed_request(
    requests: Vec<EnrollmentRequestResult>,
    agent_did: &str,
    live_peer: Option<&str>,
) -> Option<EnrollmentRequestResult> {
    let live_peer = live_peer?;
    requests
        .into_iter()
        .filter(|request| request.owner_agent == agent_did && request.server_peer == live_peer)
        .max_by(|left, right| {
            left.expires_at
                .cmp(&right.expires_at)
                .then_with(|| left.request_id.cmp(&right.request_id))
        })
}

/// Reuses a live request, pushing a pending one to the server again since its
/// earlier push may have failed. Authors a new one only after a lookup that
/// succeeded and found none.
async fn resolve_managed_request<L, LF, R, RF, A, AF>(
    cancel: &tokio_util::sync::CancellationToken,
    lookup: L,
    resend: R,
    author: A,
) -> Result<String, PairingFailure>
where
    L: FnOnce() -> LF,
    LF: Future<Output = Result<Option<EnrollmentRequestResult>, String>>,
    R: FnOnce(String) -> RF,
    RF: Future<Output = Result<(), String>>,
    A: FnOnce() -> AF,
    AF: Future<Output = Result<String, PairingFailure>>,
{
    let found = lookup().await;
    // A stop during the lookup must not author or push. Once begun, those
    // steps run to completion so authoring can unwind its replication state.
    if cancel.is_cancelled() {
        return Err(PairingFailure::Cancelled);
    }
    match found {
        Err(error) => Err(PairingFailure::Transient(format!(
            "reading this desktop's enrollment requests: {error}"
        ))),
        Ok(None) => author().await,
        Ok(Some(request)) => {
            if request.state == "pending_approval" {
                resend(request.request_id.clone()).await.map_err(|error| {
                    PairingFailure::classify(format!(
                        "resending managed runtime enrollment: {error}"
                    ))
                })?;
            }
            Ok(request.request_id)
        }
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
    // The schema observation's only write is synchronous after its last
    // await, so aborting it cannot leave partial state. Pairing is awaited
    // because authoring must unwind replication state; only its wait for the
    // runtime's status is cancelled.
    let task = {
        let mut managed = state.managed_server.lock().await;
        if let Some(observation) = managed.schema_observation_task.take() {
            observation.abort();
        }
        if let Some(cancel) = managed.pairing_cancel.take() {
            cancel.cancel();
        }
        managed.pairing_task.take()
    };
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

/// The runtime on the default port when it is this home's. A fresh first-run
/// home never adopts a neighbor's `gents server`.
async fn matching_external_server(
    agent_home: &std::path::Path,
) -> Result<Option<ManagedServerStatus>, BridgeError> {
    // A booting runtime is observed through its native service until it
    // reports ready; discovery and pairing need its runtime.json.
    Ok(match observe_port_readiness(agent_home).await? {
        PortReadiness::Ready(status) => Some(status),
        PortReadiness::Foreign(_)
        | PortReadiness::Booting
        | PortReadiness::Outdated { .. }
        | PortReadiness::Occupied(_)
        | PortReadiness::NotListening => None,
    })
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
        runtime_booting: false,
        error: None,
        error_code: None,
    }
}

/// The refusal kind a failed readiness wait carries from the exit status.
#[derive(Debug, Clone, Copy)]
struct RefusedStoreKind(gents::storage_backend::IncompatibleStoreKind);

impl std::fmt::Display for RefusedStoreKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            kind if kind.is_older() => f.write_str("the store is from an older Gents version"),
            _ => f.write_str("the store is from a different Gents version"),
        }
    }
}

fn refused_kind(error: &anyhow::Error) -> Option<gents::storage_backend::IncompatibleStoreKind> {
    error
        .downcast_ref::<RefusedStoreKind>()
        .map(|refused| refused.0)
}

/// The managed runtime's refused store: the legacy marker when present,
/// otherwise the refusal the runtime reported by its exit status.
fn refused_runtime_store(
    agent_home: &Path,
    reported: Option<gents::storage_backend::IncompatibleStoreKind>,
) -> gents::storage_backend::IncompatibleStore {
    let data_path = gents::home::default_data_dir(agent_home);
    let kind = gents::storage_backend::incompatible_store_kind(&data_path)
        .ok()
        .flatten()
        .or(reported)
        .unwrap_or(gents::storage_backend::IncompatibleStoreKind::UnknownLineage);
    let data_path = match kind {
        // The exit status names the refusal, not the key; its directory
        // under the home is where the runtime keeps keys.
        gents::storage_backend::IncompatibleStoreKind::InsecureKey => agent_home.join("keys"),
        _ => data_path,
    };
    gents::storage_backend::IncompatibleStore { kind, data_path }
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
    match observe_port_readiness(agent_home).await? {
        PortReadiness::Foreign(foreign) => Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            foreign.message(),
        )),
        PortReadiness::Occupied(port) => Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            occupied_port_message(port),
        )),
        PortReadiness::Ready(_)
        | PortReadiness::Booting
        | PortReadiness::Outdated { .. }
        | PortReadiness::NotListening => Ok(()),
    }
}

/// Stop and Restart control only this home's job. This home's runtime
/// serving outside that job blocks them; another home's runtime on the port
/// never does.
fn ensure_native_owns_running_endpoint(
    port: &PortReadiness,
    native_job_loaded: bool,
) -> Result<(), BridgeError> {
    if matches!(
        port,
        PortReadiness::Ready(_) | PortReadiness::Booting | PortReadiness::Outdated { .. }
    ) && !native_job_loaded
    {
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
        managed.approval_refused = false;
    }
    // The returned status names another home's runtime on the port.
    let port = match state.policy.agent_home.as_deref() {
        Some(agent_home) => observe_port_readiness(agent_home).await?,
        None => PortReadiness::NotListening,
    };
    let native = run_native(native_service(app, state)?, |service| service.status()).await?;
    ensure_native_owns_running_endpoint(&port, native.job_loaded)?;
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
    // Refused before taking the lifecycle lock, which supersedes an
    // in-flight start: a refused restart must not cancel it.
    refuse_renaming_home(&agent_home, &request.agent_name).await?;
    let lifecycle = lock_lifecycle_superseding_start(&state).await;
    ensure_launchable_here(&app, &state)?;
    let port = observe_port_readiness(&agent_home).await?;
    let previous_did = match &port {
        PortReadiness::Ready(status) => status.agent_did.clone(),
        PortReadiness::Foreign(_)
        | PortReadiness::Booting
        | PortReadiness::Outdated { .. }
        | PortReadiness::Occupied(_)
        | PortReadiness::NotListening => None,
    };
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
    ensure_native_owns_running_endpoint(&port, service_status.job_loaded)?;
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
            save_confirmed_preference(
                &state,
                &agent_home,
                &StoredManagedServer {
                    agent_name: request.agent_name.clone(),
                    tool_ceiling: Some(tool_ceiling),
                    tool_root: tool_root.clone(),
                    reviewed_for: None,
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
    // Another runtime on the port does not keep this job from stopping, but
    // a job launched into it could only crash loop.
    let install = async {
        ensure_default_port_identity(&agent_home)
            .await
            .map_err(|mut error| {
                error.message = format!("Your agent was stopped; {}", error.message);
                error
            })?;
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
            state.managed_server.lock().await.incompatible_store = None;
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

/// The crash-loop sample for one status read. Only a runtime that reports
/// its lifecycle `ready` counts as ready: one still migrating is bound but can
/// still die, so it samples as running and keeps the evidence.
fn job_sample<'a>(
    port: &PortReadiness,
    job_loaded: bool,
    last_exit: Option<&'a gents_server::native_service::ServiceExit>,
) -> JobSample<'a> {
    match (port, last_exit) {
        (PortReadiness::Ready(_), _) => JobSample::Ready,
        _ if !job_loaded => JobSample::Unloaded,
        (_, Some(exit)) => JobSample::Exited(exit),
        (_, None) => JobSample::Running,
    }
}

/// Restarts since `baseline`. A counter below the baseline means the job was
/// reloaded or its counter reset, so the baseline restarts from it.
fn restarts_since(baseline: &mut Option<u64>, counter: u64) -> u64 {
    let first = baseline
        .filter(|first| *first <= counter)
        .unwrap_or(counter);
    *baseline = Some(first);
    counter - first
}

/// Tracks a crash loop across status reads: the supervisor's restart count
/// when a failed exit was first seen, and the loop once restarts since then
/// reach the threshold. A job that dies seconds into boot is sampled running
/// between respawns, so running samples keep the evidence. Only an unload,
/// runtime readiness, a clean exit or a dropped restart counter clear it.
#[derive(Debug, Default)]
pub struct CrashLoopWatch {
    baseline: Option<u64>,
    looping: Option<(gents_server::native_service::ServiceExit, u64)>,
}

enum JobSample<'a> {
    Unloaded,
    Ready,
    Running,
    Exited(&'a gents_server::native_service::ServiceExit),
}

impl CrashLoopWatch {
    fn observe(
        &mut self,
        sample: JobSample<'_>,
    ) -> Option<(gents_server::native_service::ServiceExit, u64)> {
        match sample {
            JobSample::Unloaded | JobSample::Ready => *self = Self::default(),
            JobSample::Exited(exit) if exit.clean => *self = Self::default(),
            JobSample::Exited(exit) => {
                let restarts = restarts_since(&mut self.baseline, exit.restarts);
                self.looping = (restarts >= CRASH_LOOP_RESTARTS).then(|| (exit.clone(), restarts));
            }
            JobSample::Running => {}
        }
        self.looping.clone()
    }
}

fn status_from(
    managed: &crate::state::ManagedServerState,
    stored: Option<&StoredManagedServer>,
    native: Option<&gents_server::native_service::NativeServiceStatus>,
    last_exit: Option<&gents_server::native_service::ServiceExit>,
    crash_loop: Option<(&gents_server::native_service::ServiceExit, u64)>,
) -> ManagedServerStatus {
    let approval_required =
        native.is_some_and(|status| status.approval_pending(managed.approval_refused));
    let exited_cleanly = last_exit.is_some_and(|exit| exit.clean);
    let settled = !managed.starting && !approval_required;
    // A refused store is final on its first exit; restarts cannot change it.
    let refused_exit = last_exit.is_some_and(|exit| settled && exit.incompatible_store());
    let refused_store = refused_exit.then(|| {
        managed
            .incompatible_store
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| {
                "The background agent cannot open its home: it was created by an older Gents version.".to_string()
            })
    });
    let crashed = refused_store.or_else(|| {
        crash_loop
            .filter(|_| settled)
            .map(|(exit, restarts)| {
                format!(
                    "The background agent keeps exiting before it becomes ready: it {} and was restarted {restarts} times in a row. Restart the agent, or check its log.",
                    exit.reason
                )
            })
            .or_else(|| {
                last_exit
                    .filter(|exit| {
                        !exit.clean
                            && !managed.starting
                            && native.is_some_and(|status| status.failed)
                    })
                    .map(|exit| {
                        format!(
                            "The background agent failed and will not be restarted: it {} after {} restarts. Start the agent again, or check its log.",
                            exit.reason, exit.restarts
                        )
                    })
            })
    });
    let refused = refused_exit
        || (crashed.is_none()
            && !managed.starting
            && managed.last_error.is_some()
            && managed.incompatible_store.is_some());
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
        runtime_booting: false,
        error: crashed.or_else(|| managed.last_error.clone()),
        error_code: refused.then_some(BridgeErrorCode::IncompatibleLocalStore),
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
            let stored = load_bound_preference(state).await.ok().flatten();
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

async fn read_initialized_name(agent_home: &std::path::Path) -> Option<String> {
    tokio::fs::read(agent_home.join("init.json"))
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("agent_name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
}

/// Provisioning rewrites an initialized home's host authority but never its
/// name, so a start or restart naming another agent is refused before
/// anything (stop, provisioning, preferences) touches the home.
async fn refuse_renaming_home(
    agent_home: &std::path::Path,
    requested: &str,
) -> Result<(), BridgeError> {
    if !gents_server::server_host::initialized_home(agent_home) {
        return Ok(());
    }
    let initialized = read_initialized_name(agent_home).await;
    if name_confirmed_by_home(requested, initialized.as_deref()) {
        return Ok(());
    }
    let requested = requested.trim();
    Err(BridgeError::new(
        BridgeErrorCode::InvalidArgument,
        match initialized {
            Some(name) => format!(
                "This computer already has a local agent named {name}, so {requested} was not created. Go back to continue with {name}."
            ),
            None => format!(
                "The local agent home at {} has no readable agent name; it was left unchanged.",
                agent_home.display()
            ),
        },
    ))
}

fn name_confirmed_by_home(requested: &str, initialized: Option<&str>) -> bool {
    initialized.is_some_and(|name| name.trim() == requested.trim())
}

/// Provisioning never renames an initialized home, so a requested name is
/// remembered only once the home's init config carries it. Otherwise the
/// stored preference is left as it was.
async fn save_confirmed_preference(
    state: &DesktopAppState,
    agent_home: &std::path::Path,
    stored: &StoredManagedServer,
) -> Result<(), BridgeError> {
    if name_confirmed_by_home(
        &stored.agent_name,
        read_initialized_name(agent_home).await.as_deref(),
    ) {
        let reviewed_for = read_home_identity(agent_home)
            .await
            .map(|identity| identity.reviewed);
        save_preference(
            state,
            &StoredManagedServer {
                reviewed_for,
                ..stored.clone()
            },
        )
        .await?;
    }
    Ok(())
}

async fn read_home_identity(agent_home: &std::path::Path) -> Option<HomeIdentity> {
    let home = tokio::fs::canonicalize(agent_home).await.ok()?;
    let bytes = tokio::fs::read(agent_home.join("init.json")).await.ok()?;
    let init: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let text = |key: &str| {
        init.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    Some(HomeIdentity {
        reviewed: ReviewedHome {
            home: home.to_string_lossy().into_owned(),
            agent_did: text("agent_did")?,
        },
        tool_ceiling: text("tool_ceiling").as_deref().and_then(parse_tool_ceiling),
        tool_root: text("tool_root"),
    })
}

/// The remembered grant is withheld unless it was reviewed for this exact
/// home and identity, and the home still carries that same authority. A
/// replaced, repointed, missing or re-granted home needs a fresh review.
fn bind_preference(
    mut stored: StoredManagedServer,
    home: Option<&HomeIdentity>,
) -> StoredManagedServer {
    let bound = home.is_some_and(|home| {
        stored.reviewed_for.as_ref() == Some(&home.reviewed)
            && stored.tool_ceiling.is_some()
            && home.tool_ceiling == stored.tool_ceiling
            && home.tool_root == stored.tool_root
    });
    if !bound {
        stored.tool_ceiling = None;
        stored.tool_root = None;
    }
    stored
}

/// A start uses the authority reviewed with it, or else a remembered grant
/// that [`bind_preference`] kept for this home. Failing here happens before
/// provisioning, so a stale grant is never written into a home.
fn start_authority(
    tool_ceiling: Option<ManagedServerToolCeiling>,
    tool_root: Option<&str>,
    bound: Option<&StoredManagedServer>,
) -> Result<EffectiveManagedAuthority, BridgeError> {
    match tool_ceiling {
        Some(ceiling) => EffectiveManagedAuthority::from_request(ceiling, tool_root),
        None => match bound.and_then(|stored| {
            stored
                .tool_ceiling
                .map(|ceiling| (ceiling, stored.tool_root.as_deref()))
        }) {
            Some((ceiling, root)) => EffectiveManagedAuthority::from_request(ceiling, root),
            None => Err(BridgeError::new(
                BridgeErrorCode::InvalidArgument,
                "Review host access for this agent home before starting it.",
            )),
        },
    }
}

async fn load_bound_preference(
    state: &DesktopAppState,
) -> Result<Option<StoredManagedServer>, BridgeError> {
    let Some(stored) = load_preference(state).await? else {
        return Ok(None);
    };
    let home = match state.policy.agent_home.as_deref() {
        Some(agent_home) => read_home_identity(agent_home).await,
        None => None,
    };
    Ok(Some(bind_preference(stored, home.as_ref())))
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

    fn write_home(agent_home: &Path, did: &str, ceiling: &str, root: Option<&str>) {
        let init = serde_json::json!({
            "home": agent_home.display().to_string(),
            "agent_name": "Forge",
            "agent_did": did,
            "key_path": null,
            "tool_ceiling": ceiling,
            "tool_root": root,
        });
        write(&agent_home.join("init.json"), &init.to_string());
    }

    /// Reviewed for the Forge home as it stands, then saved through the
    /// same confirm-and-bind path start and restart use.
    async fn remember_forge_grant(state: &DesktopAppState, agent_home: &Path, root: &str) {
        save_confirmed_preference(
            state,
            agent_home,
            &StoredManagedServer {
                agent_name: "Forge".to_string(),
                tool_ceiling: Some(ManagedServerToolCeiling::Readwrite),
                tool_root: Some(root.to_string()),
                reviewed_for: None,
            },
        )
        .await
        .unwrap();
        assert!(load_preference(state)
            .await
            .unwrap()
            .unwrap()
            .reviewed_for
            .is_some());
    }

    /// Start and restart, driven through their IPC commands, with a
    /// different name and a different authority than the initialized home.
    fn invoke_on_forge_home(cmd: &str) -> (tempfile::TempDir, Vec<u8>, String) {
        use tauri::ipc::InvokeBody;
        use tauri::webview::InvokeRequest;

        let (temp, state) = orchestration_state();
        let agent_home = state.policy.agent_home.clone().expect("agent home");
        let desktop_root = state.policy.desktop_paths.root().to_path_buf();
        write_home(&agent_home, "did:key:forge", "MetaOnly", None);
        let before = std::fs::read(agent_home.join("init.json")).unwrap();
        let root = temp.path().join("work");
        std::fs::create_dir_all(&root).unwrap();
        let app = tauri::test::mock_builder()
            .manage(state)
            .invoke_handler(tauri::generate_handler![
                desktop_managed_server_start,
                desktop_managed_server_restart
            ])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock desktop bridge app");
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("mock webview");
        let error = tauri::test::get_ipc_response(
            &webview,
            InvokeRequest {
                cmd: cmd.into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: "http://tauri.localhost".parse().expect("invoke URL"),
                body: InvokeBody::Json(serde_json::json!({
                    "request": {
                        "agentName": "Scout",
                        "toolCeiling": "readwrite",
                        "toolRoot": root.display().to_string(),
                    }
                })),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.to_string(),
            },
        )
        .expect_err("a request naming another agent must be refused");
        assert!(!desktop_root.join(MANAGED_SERVER_CONFIG).exists());
        let after = std::fs::read(agent_home.join("init.json")).unwrap();
        assert_eq!(after, before, "{cmd} rewrote the initialized home");
        (temp, after, error.to_string())
    }

    #[test]
    fn start_naming_another_agent_leaves_the_home_untouched() {
        let (_temp, _, error) = invoke_on_forge_home("desktop_managed_server_start");
        assert!(
            error.contains("already has a local agent named Forge"),
            "{error}"
        );
    }

    #[test]
    fn restart_naming_another_agent_leaves_the_home_untouched() {
        let (_temp, _, error) = invoke_on_forge_home("desktop_managed_server_restart");
        assert!(
            error.contains("already has a local agent named Forge"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_remembered_grant_reconnects_only_the_home_it_was_reviewed_for() {
        let (temp, state) = orchestration_state();
        let agent_home = state.policy.agent_home.clone().expect("agent home");
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let root = root.to_string_lossy().into_owned();
        write_home(&agent_home, "did:key:forge", "Readwrite", Some(&root));
        remember_forge_grant(&state, &agent_home, &root).await;

        let bound = load_bound_preference(&state).await.unwrap();
        let authority = start_authority(None, None, bound.as_ref()).expect("reviewed home");
        assert_eq!(
            authority.stored(),
            (ManagedServerToolCeiling::Readwrite, Some(root.clone()))
        );
    }

    #[tokio::test]
    async fn a_replaced_or_missing_home_needs_a_fresh_review_and_is_not_rewritten() {
        let (temp, state) = orchestration_state();
        let agent_home = state.policy.agent_home.clone().expect("agent home");
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let root = root.to_string_lossy().into_owned();
        write_home(&agent_home, "did:key:forge", "Readwrite", Some(&root));
        remember_forge_grant(&state, &agent_home, &root).await;

        // Another identity now lives at the same path.
        write_home(&agent_home, "did:key:other", "MetaOnly", None);
        let before = std::fs::read(agent_home.join("init.json")).unwrap();
        let bound = load_bound_preference(&state).await.unwrap();
        assert_eq!(bound.as_ref().unwrap().tool_ceiling, None);
        let error = start_authority(None, None, bound.as_ref()).unwrap_err();
        assert_eq!(error.code, BridgeErrorCode::InvalidArgument);
        assert_eq!(std::fs::read(agent_home.join("init.json")).unwrap(), before);

        std::fs::remove_file(agent_home.join("init.json")).unwrap();
        let bound = load_bound_preference(&state).await.unwrap();
        assert!(start_authority(None, None, bound.as_ref()).is_err());
        assert!(!agent_home.join("init.json").exists());
    }

    #[tokio::test]
    async fn a_home_whose_authority_changed_needs_a_fresh_review() {
        let (temp, state) = orchestration_state();
        let agent_home = state.policy.agent_home.clone().expect("agent home");
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let root = root.to_string_lossy().into_owned();
        write_home(&agent_home, "did:key:forge", "Readwrite", Some(&root));
        remember_forge_grant(&state, &agent_home, &root).await;
        let remembered = load_preference(&state).await.unwrap();

        write_home(&agent_home, "did:key:forge", "Readonly", Some(&root));
        assert_eq!(
            load_preference(&state).await.unwrap().unwrap().tool_ceiling,
            remembered.unwrap().tool_ceiling
        );
        let bound = load_bound_preference(&state).await.unwrap();
        assert!(start_authority(None, None, bound.as_ref()).is_err());
    }

    #[tokio::test]
    async fn reprovisioning_with_another_name_leaves_the_home_and_preference_alone() {
        let (_temp, state) = orchestration_state();
        let agent_home = state.policy.agent_home.clone().expect("agent home");
        let init = serde_json::json!({
            "home": agent_home.display().to_string(),
            "agent_name": "Forge",
            "agent_did": "did:key:forge",
            "key_path": null,
            "tool_ceiling": "Readwrite",
            "tool_root": null,
        });
        write(&agent_home.join("init.json"), &init.to_string());
        let forge = StoredManagedServer {
            agent_name: "Forge".to_string(),
            tool_ceiling: Some(ManagedServerToolCeiling::Readwrite),
            tool_root: None,
            reviewed_for: None,
        };
        save_preference(&state, &forge).await.unwrap();

        // What restart does with a stale requested name.
        gents_server::server_host::ensure_standard_home(
            gents_server::server_host::ProvisionOptions {
                home: agent_home.clone(),
                agent_name: "Scout".to_string(),
                tool_ceiling: ManagedServerToolCeiling::Readwrite.into(),
                tool_root: None,
            },
        )
        .await
        .unwrap();
        save_confirmed_preference(
            &state,
            &agent_home,
            &StoredManagedServer {
                agent_name: "Scout".to_string(),
                ..forge.clone()
            },
        )
        .await
        .unwrap();

        assert_eq!(
            read_initialized_name(&agent_home).await.as_deref(),
            Some("Forge")
        );
        let stored = load_preference(&state).await.unwrap().expect("preference");
        assert_eq!(stored.agent_name, "Forge");

        save_confirmed_preference(&state, &agent_home, &forge)
            .await
            .unwrap();
        assert_eq!(
            load_preference(&state).await.unwrap().unwrap().agent_name,
            "Forge"
        );
    }

    #[tokio::test]
    async fn requested_name_is_remembered_only_when_the_home_confirms_it() {
        let home = tempfile::tempdir().unwrap();
        assert!(!name_confirmed_by_home(
            "Scout",
            read_initialized_name(home.path()).await.as_deref()
        ));
        write(
            &home.path().join("init.json"),
            r#"{"agent_did":"did:key:forge","agent_name":"Forge"}"#,
        );
        let initialized = read_initialized_name(home.path()).await;
        assert_eq!(initialized.as_deref(), Some("Forge"));
        assert!(!name_confirmed_by_home("Scout", initialized.as_deref()));
        assert!(name_confirmed_by_home("Forge", initialized.as_deref()));
    }

    fn not_the_user_home() -> &'static Path {
        Path::new("/nonexistent-user-home")
    }

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn lineage_refusal(data_path: PathBuf) -> gents::storage_backend::IncompatibleStore {
        gents::storage_backend::IncompatibleStore {
            kind: gents::storage_backend::IncompatibleStoreKind::UnknownLineage,
            data_path,
        }
    }

    /// A 0.18-style default home: the managed agent's own files at the root
    /// of `~/.gents`, another agent's home nested beside them, and desktop
    /// client state (with the packaged runtime copy) elsewhere.
    fn dirty_home(temp: &Path) -> (PathBuf, gents_desktop_core::client::DesktopPaths) {
        let home = temp.join(".gents");
        write(&home.join("data/MANIFEST"), "REGOMAN old lineage");
        write(
            &home.join("init.json"),
            r#"{"agent_did":"did:key:old","agent_name":"local"}"#,
        );
        write(&home.join("keys/local.key"), "identity");
        write(&home.join("runtime.json"), "{}");
        write(&home.join("p2p-secret-key"), "p2p");
        write(
            &home.join("grok-port-home/init.json"),
            r#"{"agent_did":"did:key:other"}"#,
        );
        write(&home.join("grok-port-home/data/MANIFEST"), "REGOMAN other");
        let desktop = gents_desktop_core::client::DesktopPaths::from_root(temp.join("desktop"));
        write(
            &desktop.node_data_dir().join("MANIFEST"),
            "REGOMAN old client",
        );
        write(desktop.peer_directory_path(), "{}");
        write(desktop.principal_metadata_path(), "{}");
        write(desktop.identity_key_path(), "principal");
        write(desktop.iroh_secret_key_path(), "iroh");
        write(&desktop.root().join(MANAGED_SERVER_CONFIG), "{}");
        write(&desktop.root().join("runtime/gents"), "packaged runtime");
        (std::fs::canonicalize(&home).unwrap(), desktop)
    }

    #[test]
    fn archive_moves_the_home_and_client_state_to_a_sibling_backup_and_spares_other_homes() {
        let temp = tempfile::tempdir().unwrap();
        let temp = std::fs::canonicalize(temp.path()).unwrap();
        let (home, desktop) = dirty_home(&temp);

        let plan = plan_home_reset(
            Some(&home),
            Some(lineage_refusal(PathBuf::from("ignored"))),
            None,
            &desktop,
            not_the_user_home(),
        )
        .unwrap();
        let preview = plan.preview();
        assert!(!preview.completed);
        assert_eq!(
            preview.managed_home.as_deref(),
            Some(home.to_str().unwrap())
        );
        assert_eq!(preview.stores.len(), 1);
        assert_eq!(preview.stores[0].scope, IncompatibleStoreScope::Runtime);
        assert_eq!(preview.stores[0].path, home.join("data").to_str().unwrap());
        assert_eq!(
            preview.retained_paths,
            vec![home.join("grok-port-home").to_string_lossy().into_owned()]
        );
        assert!(home.join("data").exists(), "a preview changes nothing");
        assert_eq!(
            plan.check_confirmation(
                preview.delete_confirmation.as_deref().unwrap(),
                HomeResetDisposition::Archive
            )
            .unwrap_err()
            .code,
            BridgeErrorCode::InvalidArgument,
            "each action has its own confirmation"
        );
        plan.check_confirmation(preview.confirmation.as_str(), HomeResetDisposition::Archive)
            .unwrap();

        let reset = plan
            .retire(
                HomeResetDisposition::Archive,
                Some(&plan.backup_path("20260924T000000.000Z").unwrap()),
            )
            .unwrap();

        assert!(reset.completed);
        assert_eq!(reset.disposition, Some(HomeResetDisposition::Archive));
        let backup = PathBuf::from(reset.backup_path.unwrap());
        assert_eq!(backup, temp.join(".gents-backup-20260924T000000.000Z"));
        assert_eq!(
            std::fs::read_to_string(backup.join("home/data/MANIFEST")).unwrap(),
            "REGOMAN old lineage"
        );
        for moved in [
            "init.json",
            "keys/local.key",
            "runtime.json",
            "p2p-secret-key",
        ] {
            assert!(backup.join("home").join(moved).exists(), "{moved} archived");
            assert!(!home.join(moved).exists(), "{moved} left the home");
        }
        for moved in [
            "node/MANIFEST",
            "peers.json",
            "principal.json",
            "principal.ed25519.key",
            "node.iroh.key",
            MANAGED_SERVER_CONFIG,
        ] {
            assert!(
                backup.join("desktop").join(moved).exists(),
                "{moved} archived"
            );
            assert!(
                !desktop.root().join(moved).exists(),
                "{moved} left the client"
            );
        }
        assert_eq!(
            std::fs::read_to_string(home.join("grok-port-home/data/MANIFEST")).unwrap(),
            "REGOMAN other",
            "another agent's home under the managed home is untouched"
        );
        assert_eq!(
            std::fs::read_to_string(desktop.root().join("runtime/gents")).unwrap(),
            "packaged runtime",
            "the packaged runtime is not client state"
        );
        assert!(
            home.is_dir(),
            "the home directory itself stays for the fresh start"
        );
    }

    #[test]
    fn delete_removes_only_the_home_and_client_state_it_previewed() {
        let temp = tempfile::tempdir().unwrap();
        let temp = std::fs::canonicalize(temp.path()).unwrap();
        let (home, desktop) = dirty_home(&temp);
        let outside = temp.join("outside");
        write(&outside.join("keep.txt"), "keep");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, home.join("linked")).unwrap();

        let plan = plan_home_reset(
            Some(&home),
            Some(lineage_refusal(home.join("data"))),
            None,
            &desktop,
            not_the_user_home(),
        )
        .unwrap();
        let preview = plan.preview();
        plan.check_confirmation(
            preview.delete_confirmation.as_deref().unwrap(),
            HomeResetDisposition::Delete,
        )
        .unwrap();
        let reset = plan.retire(HomeResetDisposition::Delete, None).unwrap();

        assert!(reset.completed);
        assert_eq!(reset.backup_path, None);
        assert!(!home.join("data").exists());
        assert!(!home.join("keys").exists());
        assert!(!desktop.node_data_dir().exists());
        assert!(!desktop.identity_key_path().exists());
        assert!(home.join("grok-port-home/init.json").is_file());
        assert!(desktop.root().join("runtime/gents").is_file());
        assert_eq!(
            std::fs::read_to_string(outside.join("keep.txt")).unwrap(),
            "keep",
            "a symbolic link is removed, never followed"
        );
        assert!(std::fs::read_dir(&temp).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("backup")));
    }

    #[test]
    fn keeping_the_home_is_a_preview_that_touches_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let temp = std::fs::canonicalize(temp.path()).unwrap();
        let (home, desktop) = dirty_home(&temp);
        let listing = |dir: &Path| {
            let mut names = std::fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            names.sort();
            names
        };
        let (home_before, desktop_before) = (listing(&home), listing(desktop.root()));

        let plan = plan_home_reset(
            Some(&home),
            Some(lineage_refusal(home.join("data"))),
            None,
            &desktop,
            not_the_user_home(),
        )
        .unwrap();
        let _ = plan.preview();
        assert!(plan
            .check_confirmation("RESET SOME OTHER HOME", HomeResetDisposition::Archive)
            .is_err());

        assert_eq!(listing(&home), home_before);
        assert_eq!(listing(desktop.root()), desktop_before);
    }

    #[test]
    fn a_refused_client_store_alone_retires_only_client_state() {
        let temp = tempfile::tempdir().unwrap();
        let temp = std::fs::canonicalize(temp.path()).unwrap();
        let (home, desktop) = dirty_home(&temp);

        let plan = plan_home_reset(
            Some(&home),
            None,
            Some(lineage_refusal(desktop.node_data_dir().to_path_buf())),
            &desktop,
            not_the_user_home(),
        )
        .unwrap();
        let preview = plan.preview();
        assert_eq!(preview.managed_home, None);
        assert_eq!(preview.stores[0].scope, IncompatibleStoreScope::Client);
        let reset = plan
            .retire(
                HomeResetDisposition::Archive,
                Some(&plan.backup_path("20260924T000000.000Z").unwrap()),
            )
            .unwrap();

        let backup = PathBuf::from(reset.backup_path.unwrap());
        assert_eq!(backup, temp.join("desktop-backup-20260924T000000.000Z"));
        assert!(backup.join("desktop/node/MANIFEST").is_file());
        assert!(
            home.join("data/MANIFEST").is_file(),
            "the runtime home is not in scope"
        );
        assert!(home.join("init.json").is_file());
    }

    #[test]
    fn backups_user_files_and_unknown_homes_are_retained_and_shown() {
        let temp = tempfile::tempdir().unwrap();
        let temp = std::fs::canonicalize(temp.path()).unwrap();
        let (home, desktop) = dirty_home(&temp);
        write(
            &home.join("backups/legacy-store-1/data/MANIFEST"),
            "older backup",
        );
        write(&home.join("notes.md"), "mine");
        write(
            &home.join("half-onboarded/data/MANIFEST"),
            "no init.json yet",
        );
        write(&home.join("a/b/c/d/e/init.json"), "{}");
        write(&home.join("data.lock"), "4242");

        let plan = plan_home_reset(
            Some(&home),
            Some(lineage_refusal(home.join("data"))),
            None,
            &desktop,
            not_the_user_home(),
        )
        .unwrap();
        let preview = plan.preview();
        for retained in ["a", "backups", "data.lock", "half-onboarded", "notes.md"] {
            let path = home.join(retained).to_string_lossy().into_owned();
            assert!(
                preview.retained_paths.contains(&path),
                "{retained} retained"
            );
            assert!(
                !preview.planned_paths.contains(&path),
                "{retained} not planned"
            );
        }
        assert!(preview
            .planned_paths
            .contains(&home.join("data").to_string_lossy().into_owned()));
        assert!(preview
            .planned_paths
            .contains(&desktop.identity_key_path().to_string_lossy().into_owned()));

        plan.retire(HomeResetDisposition::Delete, None).unwrap();
        assert!(home.join("backups/legacy-store-1/data/MANIFEST").is_file());
        assert!(home.join("notes.md").is_file());
        assert!(home.join("half-onboarded/data/MANIFEST").is_file());
        assert!(home.join("a/b/c/d/e/init.json").is_file());
        assert!(home.join("data.lock").is_file());
    }

    #[test]
    fn a_confirmation_authorizes_only_the_previewed_set() {
        let temp = tempfile::tempdir().unwrap();
        let temp = std::fs::canonicalize(temp.path()).unwrap();
        let (home, desktop) = dirty_home(&temp);
        let refused = || Some(lineage_refusal(home.join("data")));
        let preview = plan_home_reset(Some(&home), refused(), None, &desktop, not_the_user_home())
            .unwrap()
            .preview();

        // A runtime-owned entry appears after the review.
        write(&home.join("plugins/new/manifest.json"), "{}");
        let changed =
            plan_home_reset(Some(&home), refused(), None, &desktop, not_the_user_home()).unwrap();
        for (supplied, disposition) in [
            (preview.confirmation.as_str(), HomeResetDisposition::Archive),
            (
                preview.delete_confirmation.as_deref().unwrap(),
                HomeResetDisposition::Delete,
            ),
        ] {
            assert_eq!(
                changed
                    .check_confirmation(supplied, disposition)
                    .unwrap_err()
                    .code,
                BridgeErrorCode::InvalidArgument
            );
        }
        let reviewed = changed.preview();
        changed
            .check_confirmation(
                reviewed.delete_confirmation.as_deref().unwrap(),
                HomeResetDisposition::Delete,
            )
            .unwrap();
        assert!(reviewed
            .delete_confirmation
            .as_deref()
            .unwrap()
            .starts_with(&format!("DELETE {} PERMANENTLY [", home.display())));
    }

    #[test]
    fn delete_removes_only_this_homes_key_and_keeps_other_keys() {
        let temp = tempfile::tempdir().unwrap();
        let temp = std::fs::canonicalize(temp.path()).unwrap();
        let (home, desktop) = dirty_home(&temp);
        write(
            &home.join("keys/other-agent.key"),
            "another home's identity",
        );
        write(&home.join("packs/p/manifest.json"), "{}");

        let plan = plan_home_reset(
            Some(&home),
            Some(lineage_refusal(home.join("data"))),
            None,
            &desktop,
            not_the_user_home(),
        )
        .unwrap();
        let preview = plan.preview();
        let delete_paths = preview.delete_paths.clone();
        assert!(delete_paths.contains(&home.join("keys/local.key").to_string_lossy().into_owned()));
        assert!(!delete_paths.contains(&home.join("keys").to_string_lossy().into_owned()));
        assert!(delete_paths.contains(&home.join("packs").to_string_lossy().into_owned()));
        assert!(
            preview
                .planned_paths
                .contains(&home.join("keys").to_string_lossy().into_owned()),
            "an archive still moves the whole key directory"
        );

        plan.check_confirmation(
            preview.delete_confirmation.as_deref().unwrap(),
            HomeResetDisposition::Delete,
        )
        .unwrap();
        plan.retire(HomeResetDisposition::Delete, None).unwrap();
        assert!(!home.join("keys/local.key").exists());
        assert_eq!(
            std::fs::read_to_string(home.join("keys/other-agent.key")).unwrap(),
            "another home's identity"
        );
    }

    #[test]
    fn a_foreign_store_is_never_deleted_by_the_bridge() {
        let temp = tempfile::tempdir().unwrap();
        let temp = std::fs::canonicalize(temp.path()).unwrap();
        let (home, desktop) = dirty_home(&temp);
        let plan = plan_home_reset(
            Some(&home),
            Some(gents::storage_backend::IncompatibleStore {
                kind: gents::storage_backend::IncompatibleStoreKind::ForeignVersion,
                data_path: home.join("data"),
            }),
            None,
            &desktop,
            not_the_user_home(),
        )
        .unwrap();
        let preview = plan.preview();
        assert_eq!(preview.delete_confirmation, None);
        assert!(preview.delete_paths.is_empty());
        let forged = format!(
            "DELETE {} PERMANENTLY [{}]",
            home.display(),
            plan.digest(HomeResetDisposition::Delete)
        );
        assert_eq!(
            plan.check_confirmation(&forged, HomeResetDisposition::Delete)
                .unwrap_err()
                .code,
            BridgeErrorCode::InvalidArgument
        );
        plan.check_confirmation(&preview.confirmation, HomeResetDisposition::Archive)
            .unwrap();
    }

    #[test]
    fn an_insecure_old_key_scopes_the_home_like_a_refused_store() {
        let temp = tempfile::tempdir().unwrap();
        let temp = std::fs::canonicalize(temp.path()).unwrap();
        let (home, desktop) = dirty_home(&temp);
        let refused = refused_runtime_store(
            &home,
            Some(gents::storage_backend::IncompatibleStoreKind::InsecureKey),
        );
        assert_eq!(refused.data_path, home.join("keys"));
        let preview = plan_home_reset(
            Some(&home),
            Some(refused),
            None,
            &desktop,
            not_the_user_home(),
        )
        .unwrap()
        .preview();
        assert!(preview.stores[0].unsafe_key);
        assert!(preview.stores[0].older);
        assert!(preview.delete_confirmation.is_some());
        assert!(preview.stores[0].detail.contains("unsafe file permissions"));
    }

    #[test]
    fn an_unknown_or_unresolvable_user_home_refuses_the_reset() {
        let temp = tempfile::tempdir().unwrap();
        for missing in [None, Some(PathBuf::new())] {
            let error = resolve_user_home(missing).unwrap_err();
            assert_eq!(error.code, BridgeErrorCode::InvalidArgument);
            assert!(
                error.message.contains("cannot be determined"),
                "{}",
                error.message
            );
        }
        let error = resolve_user_home(Some(temp.path().join("absent"))).unwrap_err();
        assert!(
            error.message.contains("cannot be resolved"),
            "{}",
            error.message
        );
        assert_eq!(
            resolve_user_home(Some(temp.path().to_path_buf())).unwrap(),
            std::fs::canonicalize(temp.path()).unwrap()
        );
    }

    #[test]
    fn the_store_lock_neither_moves_nor_changes_the_confirmation() {
        let temp = tempfile::tempdir().unwrap();
        let temp = std::fs::canonicalize(temp.path()).unwrap();
        let (home, desktop) = dirty_home(&temp);
        let refused = || Some(lineage_refusal(home.join("data")));
        let preview = plan_home_reset(Some(&home), refused(), None, &desktop, not_the_user_home())
            .unwrap()
            .preview();

        let lock = gents::home::lock_store(&home, &home.join("data")).unwrap();
        let locked =
            plan_home_reset(Some(&home), refused(), None, &desktop, not_the_user_home()).unwrap();
        locked
            .check_confirmation(&preview.confirmation, HomeResetDisposition::Archive)
            .unwrap();
        let reset = locked
            .retire(
                HomeResetDisposition::Archive,
                Some(&locked.backup_path("20260924T000000.000Z").unwrap()),
            )
            .unwrap();
        assert!(lock.path().is_file(), "the held lock stays in place");
        assert!(!home.join("data").exists());
        assert!(!reset
            .retired_paths
            .iter()
            .any(|path| path.ends_with("data.lock")));
    }

    /// A home refused only for an insecure key may have no store. The reset
    /// still holds the home's store lock, so an opener that creates the
    /// store (init or serve taking `lock_store`) cannot run while `init.json`
    /// and `keys/` are retired, and a store it creates afterwards is not.
    #[tokio::test]
    async fn an_opener_cannot_create_the_store_while_a_storeless_home_is_retired() {
        use std::sync::Arc;
        let temp = tempfile::tempdir().unwrap();
        let temp = std::fs::canonicalize(temp.path()).unwrap();
        let (home, desktop) = dirty_home(&temp);
        std::fs::remove_dir_all(home.join("data")).unwrap();
        let refused = refused_runtime_store(
            &home,
            Some(gents::storage_backend::IncompatibleStoreKind::InsecureKey),
        );

        let lock = lock_home_for_retirement(&home).unwrap();
        let plan = plan_home_reset(
            Some(&home),
            Some(refused),
            None,
            &desktop,
            not_the_user_home(),
        )
        .unwrap();
        let preview = plan.preview();
        assert!(!preview
            .planned_paths
            .iter()
            .any(|path| path.ends_with("/data")));

        // An opener races the retirement: it creates the store and takes the
        // store lock, as `gents init` and `gents server` do.
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let opener = {
            let barrier = Arc::clone(&barrier);
            let home = home.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                std::fs::create_dir_all(home.join("data")).unwrap();
                gents::home::lock_store(&home, &home.join("data")).is_ok()
            })
        };
        barrier.wait().await;
        let opened = opener.await.unwrap();
        let reset = plan
            .retire(
                HomeResetDisposition::Archive,
                Some(&plan.backup_path("20260924T000000.000Z").unwrap()),
            )
            .unwrap();

        assert!(!opened, "the opener met the reset's lock");
        assert!(!reset
            .retired_paths
            .iter()
            .any(|path| path.ends_with("data.lock") || path.ends_with("/data")));
        assert!(!home.join("init.json").exists());
        assert!(!home.join("keys").exists());
        assert!(
            home.join("data").is_dir(),
            "a store created under the lock is not retired"
        );
        drop(lock);
        assert!(gents::home::lock_store(&home, &home.join("data")).is_ok());
    }

    #[test]
    fn broad_roots_are_never_retired() {
        let temp = tempfile::tempdir().unwrap();
        let temp = std::fs::canonicalize(temp.path()).unwrap();
        let (home, desktop) = dirty_home(&temp);
        let refused = Some(lineage_refusal(home.join("data")));

        // The managed home is the user's home, or an ancestor of it.
        for user_home in [home.clone(), home.join("person")] {
            assert_eq!(
                plan_home_reset(Some(&home), refused.clone(), None, &desktop, &user_home)
                    .unwrap_err()
                    .code,
                BridgeErrorCode::InvalidArgument
            );
        }
        // The desktop state root is the user's home or one of its ancestors.
        let user_home = desktop.root().join("person");
        assert_eq!(
            plan_home_reset(Some(&home), refused.clone(), None, &desktop, &user_home)
                .unwrap_err()
                .code,
            BridgeErrorCode::InvalidArgument
        );
        // A path alias of the user's home is the same root.
        #[cfg(unix)]
        {
            let alias = temp.join("alias");
            std::os::unix::fs::symlink(&temp, &alias).unwrap();
            let aliased = std::fs::canonicalize(alias.join(".gents")).unwrap();
            assert_eq!(
                plan_home_reset(Some(&home), refused.clone(), None, &desktop, &aliased)
                    .unwrap_err()
                    .code,
                BridgeErrorCode::InvalidArgument
            );
        }
        assert!(ensure_retirable_root(Path::new("/"), "home", not_the_user_home()).is_err());
        assert!(
            home.join("data/MANIFEST").is_file(),
            "planning deletes nothing"
        );
    }

    #[test]
    fn a_store_from_another_build_is_not_reported_as_older() {
        let temp = tempfile::tempdir().unwrap();
        let temp = std::fs::canonicalize(temp.path()).unwrap();
        let (home, desktop) = dirty_home(&temp);
        let preview = plan_home_reset(
            Some(&home),
            Some(gents::storage_backend::IncompatibleStore {
                kind: gents::storage_backend::IncompatibleStoreKind::ForeignVersion,
                data_path: home.join("data"),
            }),
            None,
            &desktop,
            not_the_user_home(),
        )
        .unwrap()
        .preview();
        assert!(!preview.stores[0].older);
        assert!(preview.stores[0].detail.contains("different Gents version"));
    }

    #[test]
    fn reset_rejects_healthy_store_wrong_confirmation_and_unknown_endpoint() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let data = home.join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("MANIFEST"), b"REGOMAN current").unwrap();
        let desktop =
            gents_desktop_core::client::DesktopPaths::from_root(temp.path().join("desktop"));
        assert_eq!(
            plan_home_reset(Some(&home), None, None, &desktop, not_the_user_home())
                .unwrap_err()
                .code,
            BridgeErrorCode::InvalidArgument
        );
        std::fs::remove_file(data.join("MANIFEST")).unwrap();
        std::fs::write(data.join("data.lark"), "legacy").unwrap();
        let plan = plan_home_reset(Some(&home), None, None, &desktop, not_the_user_home()).unwrap();
        assert_eq!(
            plan.check_confirmation("RESET SOME OTHER HOME", HomeResetDisposition::Archive)
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
    fn reset_rejects_symlinked_homes_and_stores() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("data.lark"), "legacy").unwrap();
        symlink(&outside, home.join("data")).unwrap();
        let desktop =
            gents_desktop_core::client::DesktopPaths::from_root(temp.path().join("desktop"));
        assert_eq!(
            plan_home_reset(Some(&home), None, None, &desktop, not_the_user_home())
                .unwrap_err()
                .code,
            BridgeErrorCode::PathEscapesRoot
        );
        let linked_home = temp.path().join("linked-home");
        symlink(&home, &linked_home).unwrap();
        assert_eq!(
            plan_home_reset(
                Some(&linked_home),
                None,
                None,
                &desktop,
                not_the_user_home()
            )
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
            reviewed_for: None,
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
            background_item: Default::default(),
            failed: false,
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
                runtime_booting: false,
                error: None,
                error_code: None,
            },
            &gents_server::native_service::NativeServiceStatus {
                installed: true,
                running: false,
                job_loaded: false,
                enabled: true,
                requires_approval: false,
                background_item: Default::default(),
                failed: false,
                detail: None,
            },
        );

        assert_eq!(external.state, ManagedServerState::External);
        assert_eq!(external.agent_did.as_deref(), Some("did:key:preserved"));
        assert!(external.auto_start);
    }

    #[test]
    fn native_running_without_endpoint_readiness_is_starting() {
        let native = gents_server::native_service::NativeServiceStatus {
            installed: true,
            running: true,
            job_loaded: true,
            enabled: true,
            requires_approval: false,
            background_item: Default::default(),
            failed: false,
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
                runtime_booting: false,
                error: None,
                error_code: None,
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
            background_item: Default::default(),
            failed: false,
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
                runtime_booting: false,
                error: None,
                error_code: None,
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
        let ours = || PortReadiness::Ready(ready_status("did:key:local"));
        assert!(ensure_native_owns_running_endpoint(&ours(), false).is_err());
        assert!(ensure_native_owns_running_endpoint(&ours(), true).is_ok());
        assert!(ensure_native_owns_running_endpoint(&PortReadiness::NotListening, false).is_ok());
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
            runtime_booting: false,
            error: None,
            error_code: None,
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
            runtime_booting: false,
            error: None,
            error_code: None,
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

    #[test]
    fn this_homes_runtime_is_ready_only_once_it_reports_ready() {
        let did = "did:key:managed";
        let payload = |lifecycle: serde_json::Value| {
            serde_json::json!({
                "agent_did": did,
                "agent_name": "Managed",
                "graphql": "http://127.0.0.1:9191/api/v0/graphql",
                gents_protocol::serve_lifecycle::STATUS_LIFECYCLE_FIELD: lifecycle,
            })
        };
        for booting in [serde_json::Value::Null, serde_json::json!("starting")] {
            assert!(matches!(
                classify_port_payload(9191, Some(did), payload(booting)),
                PortReadiness::Booting
            ));
        }
        let PortReadiness::Ready(status) =
            classify_port_payload(9191, Some(did), payload(serde_json::json!("ready")))
        else {
            panic!("a runtime that reports ready is ready");
        };
        assert_eq!(status.agent_did.as_deref(), Some(did));
        assert!(matches!(
            classify_port_payload(
                9191,
                Some("did:key:other"),
                payload(serde_json::json!("ready"))
            ),
            PortReadiness::Foreign(ForeignRuntime { port: 9191, .. })
        ));
    }

    #[tokio::test]
    async fn a_booting_runtime_is_waited_on_until_it_reports_ready() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let probes = AtomicUsize::new(0);
        let ready = await_runtime_readiness(
            Duration::from_secs(5),
            Duration::from_millis(5),
            || {
                let attempt = probes.fetch_add(1, Ordering::SeqCst);
                async move {
                    Ok(if attempt < 3 {
                        PortReadiness::Booting
                    } else {
                        PortReadiness::Ready(ready_status("did:key:booted"))
                    })
                }
            },
            || async { Ok(NativeProgress::Loaded) },
        )
        .await
        .expect("a bound runtime that is still starting is waited on");
        assert!(matches!(ready, Readiness::Ready(_)));
        assert_eq!(probes.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn a_runtime_that_predates_readiness_fails_the_wait_at_once() {
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            await_runtime_readiness(
                Duration::from_secs(300),
                Duration::from_millis(5),
                || async {
                    Ok(PortReadiness::Outdated {
                        version: Some("0.18.2".to_string()),
                    })
                },
                || async { Ok(NativeProgress::Loaded) },
            ),
        )
        .await
        .expect("an outdated runtime is not waited on")
        .unwrap_err();
        assert!(
            error.to_string().contains("(v0.18.2) predates this app"),
            "{error}"
        );
    }

    #[test]
    fn an_older_runtime_without_the_lifecycle_field_is_outdated() {
        let did = "did:key:managed";
        let observed = classify_port_payload(
            9191,
            Some(did),
            serde_json::json!({ "agent_did": did, "version": "0.18.2", "status": "ok" }),
        );
        let PortReadiness::Outdated { version } = observed else {
            panic!("an older runtime is outdated, not booting");
        };
        assert_eq!(version.as_deref(), Some("0.18.2"));
    }

    #[tokio::test]
    async fn a_booting_runtime_past_the_bound_is_left_running() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let probes = AtomicUsize::new(0);
        let error = await_runtime_readiness(
            Duration::from_millis(60),
            Duration::from_millis(5),
            || {
                let attempt = probes.fetch_add(1, Ordering::SeqCst);
                async move {
                    Ok(if attempt < 4 {
                        PortReadiness::NotListening
                    } else {
                        PortReadiness::Booting
                    })
                }
            },
            || async { Ok(NativeProgress::Loaded) },
        )
        .await
        .expect_err("the wait is bounded");
        assert!(error.is::<RuntimeStillBooting>(), "{error}");

        // The bound restarts at bind: time spent before it does not count.
        let started = std::time::Instant::now();
        await_runtime_readiness(
            Duration::from_millis(200),
            Duration::from_millis(5),
            || {
                let elapsed = started.elapsed();
                async move {
                    Ok(if elapsed < Duration::from_millis(150) {
                        PortReadiness::NotListening
                    } else if elapsed < Duration::from_millis(300) {
                        PortReadiness::Booting
                    } else {
                        PortReadiness::Ready(ready_status("did:key:migrated"))
                    })
                }
            },
            || async { Ok(NativeProgress::Loaded) },
        )
        .await
        .expect("binding restarts the bound");
    }

    #[tokio::test]
    async fn pairing_retries_a_failed_attempt_within_its_bounds() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let cancel = tokio_util::sync::CancellationToken::new();
        let calls = AtomicU32::new(0);
        retry_managed_pairing(&cancel, 4, Duration::from_millis(1), || async {
            if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                Err(PairingFailure::Transient("runtime_not_ready".to_string()))
            } else {
                Ok(())
            }
        })
        .await
        .expect("a later attempt pairs");
        assert_eq!(calls.load(Ordering::SeqCst), 3);

        let calls = AtomicU32::new(0);
        let error = retry_managed_pairing(&cancel, 3, Duration::from_millis(1), || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err::<(), _>(PairingFailure::Transient("offer_mint_failed".to_string()))
        })
        .await
        .expect_err("attempts are bounded");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        let error = error.to_string();
        assert!(
            error.contains("offer_mint_failed") && error.contains("3 attempts"),
            "{error}"
        );
    }

    fn managed_request(
        request_id: &str,
        server_peer: &str,
        state: &str,
        expires_at: &str,
    ) -> EnrollmentRequestResult {
        EnrollmentRequestResult {
            request_id: request_id.to_string(),
            network_id: "network".to_string(),
            admin_did: "did:key:admin".to_string(),
            server_peer: server_peer.to_string(),
            owner_agent: "did:key:managed".to_string(),
            state: state.to_string(),
            expires_at: expires_at.to_string(),
        }
    }

    #[tokio::test]
    async fn a_failed_request_lookup_never_authors_another_request() {
        let authored = std::sync::atomic::AtomicU32::new(0);
        let cancel = tokio_util::sync::CancellationToken::new();
        let failure = resolve_managed_request(
            &cancel,
            || async { Err("decoding persisted enrollment request".to_string()) },
            |_| async { Ok(()) },
            || async {
                authored.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok("enroll-new".to_string())
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(failure, PairingFailure::Transient(_)), "{failure}");
        assert_eq!(authored.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_stop_during_the_request_lookup_authors_and_pushes_nothing() {
        use std::sync::atomic::{AtomicU32, Ordering};

        for existing in [
            None,
            Some(managed_request(
                "enroll-1",
                "peer-a",
                "pending_approval",
                "2026-09-24T12:00:00Z",
            )),
        ] {
            let cancel = tokio_util::sync::CancellationToken::new();
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
            let (authored, resent) = (AtomicU32::new(0), AtomicU32::new(0));
            let resolving = resolve_managed_request(
                &cancel,
                || async move {
                    entered_tx.send(()).unwrap();
                    release_rx.await.unwrap();
                    Ok(existing)
                },
                |_| {
                    resent.fetch_add(1, Ordering::SeqCst);
                    async { Ok(()) }
                },
                || async {
                    authored.fetch_add(1, Ordering::SeqCst);
                    Ok("enroll-new".to_string())
                },
            );
            let stop = async {
                entered_rx.await.unwrap();
                cancel.cancel();
                release_tx.send(()).unwrap();
            };
            let (result, ()) = tokio::join!(resolving, stop);
            assert_eq!(result.unwrap_err(), PairingFailure::Cancelled);
            assert_eq!(authored.load(Ordering::SeqCst), 0);
            assert_eq!(resent.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn a_request_whose_push_failed_is_pushed_again_when_reused() {
        let store = std::sync::Mutex::new(None::<EnrollmentRequestResult>);
        let (resent, authored) = (
            std::sync::Mutex::new(Vec::<String>::new()),
            std::sync::atomic::AtomicU32::new(0),
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        let attempt = || {
            resolve_managed_request(
                &cancel,
                || async { Ok(store.lock().unwrap().clone()) },
                |request_id| {
                    resent.lock().unwrap().push(request_id);
                    async { Ok(()) }
                },
                || async {
                    authored.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    // Written locally, then the push to the server fails.
                    *store.lock().unwrap() = Some(managed_request(
                        "enroll-1",
                        "peer-a",
                        "pending_approval",
                        "2026-09-24T12:00:00Z",
                    ));
                    Err(PairingFailure::Transient(
                        "pushing enrollment request".to_string(),
                    ))
                },
            )
        };
        assert!(attempt().await.is_err());
        assert_eq!(attempt().await.unwrap(), "enroll-1");
        assert_eq!(authored.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(*resent.lock().unwrap(), vec!["enroll-1".to_string()]);

        // An approved request needs no push.
        *store.lock().unwrap() = Some(managed_request(
            "enroll-1",
            "peer-a",
            "approved",
            "2026-09-24T12:00:00Z",
        ));
        assert_eq!(attempt().await.unwrap(), "enroll-1");
        assert_eq!(resent.lock().unwrap().len(), 1);
    }

    #[test]
    fn the_newest_request_for_the_live_server_peer_is_reused() {
        let requests = vec![
            managed_request(
                "enroll-old",
                "peer-a",
                "pending_approval",
                "2026-09-24T10:00:00Z",
            ),
            managed_request(
                "enroll-new",
                "peer-a",
                "pending_approval",
                "2026-09-24T11:00:00Z",
            ),
            managed_request(
                "enroll-other",
                "peer-b",
                "pending_approval",
                "2026-09-24T12:00:00Z",
            ),
        ];
        let selected = select_managed_request(requests.clone(), "did:key:managed", Some("peer-a"));
        assert_eq!(selected.unwrap().request_id, "enroll-new");
        assert!(
            select_managed_request(requests.clone(), "did:key:managed", Some("peer-c")).is_none()
        );
        assert!(
            select_managed_request(requests.clone(), "did:key:other", Some("peer-a")).is_none()
        );
        assert!(
            select_managed_request(requests, "did:key:managed", None).is_none(),
            "without the live peer no request is reused"
        );
    }

    #[tokio::test]
    async fn pairing_is_not_retried_after_a_permanent_failure() {
        use std::sync::atomic::{AtomicU32, Ordering};

        for message in [
            "requesting managed runtime enrollment: refusing to enroll with an incompatible runtime",
            "loading managed runtime enrollment offer: the running agent (v0.18.2) predates this app; restart it so it runs this version",
            "requesting managed runtime enrollment: enrollment server DID does not match the offer",
        ] {
            let cancel = tokio_util::sync::CancellationToken::new();
            let calls = AtomicU32::new(0);
            let failure = PairingFailure::classify(message.to_string());
            assert!(matches!(failure, PairingFailure::Permanent(_)), "{message}");
            let error = retry_managed_pairing(&cancel, 4, Duration::from_millis(1), || {
                calls.fetch_add(1, Ordering::SeqCst);
                let failure = failure.clone();
                async move { Err::<(), _>(failure) }
            })
            .await
            .unwrap_err();
            assert_eq!(calls.load(Ordering::SeqCst), 1, "{message}");
            assert_eq!(error.to_string(), message);
        }
        assert!(matches!(
            PairingFailure::classify(
                "approving managed runtime enrollment: connection refused".to_string()
            ),
            PairingFailure::Transient(_)
        ));
    }

    #[tokio::test]
    async fn pairing_retry_ends_when_cancelled_during_its_backoff() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let retry = retry_managed_pairing(&cancel, 4, Duration::from_secs(60), || async {
            Err::<(), _>(PairingFailure::Transient("runtime_not_ready".to_string()))
        });
        let cancelled = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.cancel();
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(retry, cancelled)
        })
        .await
        .expect("cancellation ends the backoff");
        assert_eq!(result.unwrap_err(), PairingFailure::Cancelled);
    }

    #[tokio::test]
    async fn cancelling_the_approval_wait_returns_without_waiting_out_its_window() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let cancel = tokio_util::sync::CancellationToken::new();
        let approvals = AtomicU32::new(0);
        let wait = await_managed_pairing_approval(
            &cancel,
            Duration::from_secs(600),
            Duration::from_millis(5),
            || async { false },
            || async {
                approvals.fetch_add(1, Ordering::SeqCst);
                Ok(PairingApproval::NotVisibleYet)
            },
            || async { Ok(()) },
        );
        let cancelled = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel.cancel();
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(wait, cancelled)
        })
        .await
        .expect("a drain is not held for the approval window");
        assert_eq!(result.unwrap_err(), PairingFailure::Cancelled);
        assert!(
            approvals.load(Ordering::SeqCst) > 1,
            "approval was retried while waiting"
        );
    }

    #[tokio::test]
    async fn an_approval_is_committed_once_then_waits_for_chat_readiness() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let cancel = tokio_util::sync::CancellationToken::new();
        let (checks, approvals, repairs) =
            (AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0));
        await_managed_pairing_approval(
            &cancel,
            Duration::from_secs(5),
            Duration::from_millis(1),
            || async { checks.fetch_add(1, Ordering::SeqCst) >= 3 },
            || async {
                approvals.fetch_add(1, Ordering::SeqCst);
                Ok(PairingApproval::Committed)
            },
            || async {
                repairs.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .expect("paired");
        assert_eq!(approvals.load(Ordering::SeqCst), 1);
        assert_eq!(repairs.load(Ordering::SeqCst), 3);
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
            code: Some(78),
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

    fn refused_store_exit(restarts: u64) -> gents_server::native_service::ServiceExit {
        gents_server::native_service::ServiceExit {
            reason: format!(
                "exited with code {}",
                gents_server::native_service::INCOMPATIBLE_STORE_EXIT_CODE
            ),
            restarts,
            clean: false,
            code: Some(gents_server::native_service::INCOMPATIBLE_STORE_EXIT_CODE),
        }
    }

    #[tokio::test]
    async fn a_refused_store_fails_the_start_typed_on_its_first_exit() {
        let error = await_runtime_readiness(
            Duration::from_secs(60),
            Duration::from_millis(5),
            || async { Ok(PortReadiness::NotListening) },
            || async { Ok(NativeProgress::Exited(refused_store_exit(0))) },
        )
        .await
        .expect_err("a refused store is final");
        let typed = error
            .downcast_ref::<BridgeError>()
            .expect("typed bridge error");
        assert_eq!(typed.code, BridgeErrorCode::IncompatibleLocalStore);
    }

    /// A runtime that binds, reports booting, then refuses its store fails
    /// typed on that exit rather than being waited on as a slow migration.
    #[tokio::test]
    async fn a_refused_store_fails_typed_even_while_status_reports_booting() {
        for code in [
            gents_server::native_service::INCOMPATIBLE_STORE_EXIT_CODE,
            gents_server::native_service::INSECURE_KEY_EXIT_CODE,
            gents_server::native_service::FOREIGN_STORE_EXIT_CODE,
        ] {
            let error = await_runtime_readiness(
                Duration::from_secs(60),
                Duration::from_millis(5),
                || async { Ok(PortReadiness::Booting) },
                || async move {
                    Ok(NativeProgress::Exited(
                        gents_server::native_service::ServiceExit {
                            reason: format!("exited with code {code}"),
                            restarts: 0,
                            clean: false,
                            code: Some(code),
                        },
                    ))
                },
            )
            .await
            .expect_err("a refused store is final");
            assert_eq!(
                error.downcast_ref::<BridgeError>().map(|error| error.code),
                Some(BridgeErrorCode::IncompatibleLocalStore),
                "exit {code}"
            );
            assert!(refused_kind(&error).is_some(), "exit {code}");
        }
    }

    #[test]
    fn a_refused_store_exit_reports_an_incompatible_home_not_a_crash_loop() {
        let native = gents_server::native_service::NativeServiceStatus {
            installed: true,
            running: false,
            job_loaded: true,
            enabled: true,
            requires_approval: false,
            detail: None,
            background_item: Default::default(),
            failed: false,
        };
        let managed = ManagedServerRuntimeState {
            incompatible_store: Some(lineage_refusal(PathBuf::from("/tmp/.gents/data"))),
            ..Default::default()
        };
        let status = status_from(
            &managed,
            None,
            Some(&native),
            Some(&refused_store_exit(0)),
            None,
        );
        assert_eq!(status.state, ManagedServerState::Failed);
        assert_eq!(
            status.error_code,
            Some(BridgeErrorCode::IncompatibleLocalStore)
        );
        assert!(status
            .error
            .as_deref()
            .is_some_and(|error| error.contains("/tmp/.gents/data")));

        let starting = ManagedServerRuntimeState {
            starting: true,
            ..Default::default()
        };
        let status = status_from(
            &starting,
            None,
            Some(&native),
            Some(&refused_store_exit(0)),
            None,
        );
        assert_eq!(status.state, ManagedServerState::Starting);
        assert_eq!(
            status.error_code, None,
            "a start in progress reports its own failure"
        );

        let exit = service_exit(3);
        let crashed = status_from(
            &ManagedServerRuntimeState::default(),
            None,
            Some(&native),
            Some(&exit),
            Some((&exit, 3)),
        );
        assert_eq!(crashed.state, ManagedServerState::Failed);
        assert_eq!(crashed.error_code, None);
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
        starts: std::sync::Mutex<std::collections::VecDeque<Result<Launched, BridgeError>>>,
        readiness: std::sync::Mutex<std::collections::VecDeque<Readiness>>,
        start_calls: std::sync::atomic::AtomicUsize,
        approval_waits: std::sync::atomic::AtomicUsize,
        waiting: tokio::sync::Notify,
        still_booting: bool,
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

        async fn start(&self) -> Result<Launched, BridgeError> {
            self.start_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.starts
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(Launched::Started))
        }

        async fn await_ready(&self) -> anyhow::Result<Readiness> {
            let next = self.readiness.lock().unwrap().pop_front();
            match next {
                Some(readiness) => Ok(readiness),
                None if self.still_booting => Err(RuntimeStillBooting {
                    waited: Duration::from_secs(300),
                }
                .into()),
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
    async fn adopting_a_job_still_booting_at_the_bound_reports_it_booting() {
        let (_temp, state) = orchestration_state();
        let token = begin_start_wait(&state).await;
        let lifecycle = state.managed_server_lifecycle.lock().await;
        let adopted = adopt_booting_runtime(&state, &token, lifecycle, async {
            Err(RuntimeStillBooting {
                waited: Duration::from_secs(300),
            }
            .into())
        })
        .await;
        let Err(error) = settle_adoption(&state, &token, Path::new("/tmp/home"), adopted).await
        else {
            panic!("a job still booting does not adopt as ready");
        };
        assert_eq!(error.code, BridgeErrorCode::RuntimeStillBooting);
        let managed = state.managed_server.lock().await;
        assert!(
            managed.last_error.is_none(),
            "status keeps reporting it starting"
        );
        assert!(!managed.starting);
    }

    #[tokio::test]
    async fn a_start_still_booting_is_reported_as_booting_not_failed() {
        let (_temp, state) = orchestration_state();
        let token = begin_start_wait(&state).await;
        let lifecycle = state.managed_server_lifecycle.lock().await;
        let (_, typed) = settle_launch_failure(
            &state,
            &token,
            LaunchFailure {
                error: RuntimeStillBooting {
                    waited: Duration::from_secs(300),
                }
                .into(),
                attempted_start: false,
                lifecycle: Some(lifecycle),
            },
            || async { panic!("a booting runtime is not rolled back") },
        )
        .await
        .expect("settled");
        assert_eq!(
            typed.expect("typed").code,
            BridgeErrorCode::RuntimeStillBooting
        );
        let managed = state.managed_server.lock().await;
        assert!(
            managed.last_error.is_none(),
            "status keeps reporting it starting"
        );
        assert!(!managed.starting);
    }

    #[tokio::test]
    async fn a_start_still_booting_at_the_bound_is_not_rolled_back() {
        let (_temp, state) = orchestration_state();
        let launch = FakeLaunch {
            still_booting: true,
            ..Default::default()
        };
        let token = begin_start_wait(&state).await;
        let lifecycle = state.managed_server_lifecycle.lock().await;
        let Err(failure) = launch_managed_server(&state, &token, lifecycle, &launch).await else {
            panic!("a runtime still booting at the bound fails the start");
        };
        assert_eq!(launch.starts(), 1);
        assert!(
            !failure.attempted_start,
            "a job that may be migrating is left running"
        );
        assert!(failure.error.is::<RuntimeStillBooting>());
    }

    #[tokio::test]
    async fn a_start_refused_for_approval_is_retried_once_after_approval() {
        let (_temp, state) = orchestration_state();
        let launch = FakeLaunch {
            approval: std::sync::Mutex::new([Ok(false), Ok(true)].into()),
            starts: std::sync::Mutex::new(
                [
                    Err(BridgeError::untyped("blocked by Login Items")),
                    Ok(Launched::Started),
                ]
                .into(),
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
    async fn a_start_macos_keeps_refusing_is_retried_after_each_approval_wait() {
        let (_temp, state) = orchestration_state();
        let refused = || Ok(Launched::ApprovalPending("Bootstrap failed: 5".to_string()));
        let launch = FakeLaunch {
            approval: std::sync::Mutex::new([Ok(false), Ok(true), Ok(true), Ok(true)].into()),
            starts: std::sync::Mutex::new(
                [refused(), refused(), refused(), Ok(Launched::Started)].into(),
            ),
            readiness: std::sync::Mutex::new([Readiness::Ready(ready_status("d:k:x"))].into()),
            ..Default::default()
        };
        let token = begin_start_wait(&state).await;
        let lifecycle = state.managed_server_lifecycle.lock().await;
        assert!(launch_managed_server(&state, &token, lifecycle, &launch)
            .await
            .is_ok());
        assert_eq!(launch.starts(), 4);
        assert_eq!(
            launch
                .approval_waits
                .load(std::sync::atomic::Ordering::SeqCst),
            3
        );
    }

    #[tokio::test]
    async fn a_refusal_approval_polling_cannot_observe_fails_with_its_detail() {
        let (_temp, state) = orchestration_state();
        let launch = FakeLaunch {
            starts: std::sync::Mutex::new(
                [Ok(Launched::ApprovalPending(
                    "Bootstrap failed: 5: Input/output error".to_string(),
                ))]
                .into(),
            ),
            ..Default::default()
        };
        let token = begin_start_wait(&state).await;
        let lifecycle = state.managed_server_lifecycle.lock().await;
        let failure = tokio::time::timeout(
            Duration::from_secs(1),
            launch_managed_server(&state, &token, lifecycle, &launch),
        )
        .await
        .expect("an unobservable refusal is not retried in a loop")
        .err()
        .expect("the start fails");
        assert!(
            failure.error.to_string().contains("Input/output error"),
            "{}",
            failure.error
        );
        assert_eq!(launch.starts(), 1);
        assert_eq!(
            launch
                .approval_waits
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    #[test]
    fn a_runtime_still_migrating_keeps_the_crash_loop_evidence() {
        let mut watch = CrashLoopWatch::default();
        for restarts in [1, 2, 3] {
            watch.observe(job_sample(
                &PortReadiness::NotListening,
                true,
                Some(&service_exit(restarts)),
            ));
            // Bound with its identity but not `ready`: migrations can still fail.
            assert!(
                matches!(
                    job_sample(&PortReadiness::Booting, true, None),
                    JobSample::Running
                ),
                "a booting runtime samples as running"
            );
            watch.observe(job_sample(&PortReadiness::Booting, true, None));
        }
        assert_eq!(
            watch
                .observe(job_sample(&PortReadiness::Booting, true, None))
                .map(|(_, restarts)| restarts),
            Some(2)
        );
        assert!(watch
            .observe(job_sample(
                &PortReadiness::Ready(ready_status("did:key:ready")),
                true,
                None
            ))
            .is_none());
    }

    #[test]
    fn a_refused_start_of_an_unregistered_item_is_reported_as_awaiting_approval() {
        let native = gents_server::native_service::NativeServiceStatus {
            installed: true,
            running: false,
            job_loaded: false,
            enabled: false,
            requires_approval: false,
            background_item: gents_server::native_service::BackgroundItemStatus::NotRegistered,
            failed: false,
            detail: None,
        };
        let fresh = ManagedServerRuntimeState::default();
        assert!(!status_from(&fresh, None, Some(&native), None, None).approval_required);
        let refused = ManagedServerRuntimeState {
            approval_refused: true,
            ..Default::default()
        };
        assert!(status_from(&refused, None, Some(&native), None, None).approval_required);
    }

    #[tokio::test]
    async fn a_new_start_forgets_an_earlier_refusal() {
        let (_temp, state) = orchestration_state();
        state.managed_server.lock().await.approval_refused = true;
        let _token = begin_start_wait(&state).await;
        assert!(!state.managed_server.lock().await.approval_refused);
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
            background_item: Default::default(),
            failed: false,
            detail: None,
        };
        let idle = ManagedServerRuntimeState::default();
        let status = status_from(&idle, None, Some(&native), Some(&service_exit(7)), None);
        assert_eq!(
            status.state,
            ManagedServerState::Starting,
            "a high restart count from earlier failures is not a crash loop by itself"
        );
        let exit = service_exit(9);
        let status = status_from(&idle, None, Some(&native), Some(&exit), Some((&exit, 2)));
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
            background_item: Default::default(),
            failed: false,
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
        assert!(
            observed.approval_required,
            "approval revoked while the runtime serves stays visible"
        );
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

    fn looping_restarts(watch: &mut CrashLoopWatch, sample: JobSample<'_>) -> Option<u64> {
        watch.observe(sample).map(|(_, restarts)| restarts)
    }

    #[test]
    fn status_reports_a_crash_loop_only_relative_to_its_first_observation() {
        let mut watch = CrashLoopWatch::default();
        assert_eq!(
            looping_restarts(&mut watch, JobSample::Exited(&service_exit(7))),
            None
        );
        assert_eq!(
            looping_restarts(&mut watch, JobSample::Exited(&service_exit(8))),
            None
        );
        assert_eq!(
            looping_restarts(&mut watch, JobSample::Exited(&service_exit(9))),
            Some(2)
        );
        assert_eq!(looping_restarts(&mut watch, JobSample::Unloaded), None);
        assert_eq!(
            looping_restarts(&mut watch, JobSample::Exited(&service_exit(9))),
            None,
            "an unloaded job starts a new baseline"
        );
    }

    #[test]
    fn a_job_that_dies_seconds_into_boot_is_a_crash_loop_across_running_samples() {
        // With a 1s status poll and launchd's respawn throttle, most samples
        // see the respawned process running for its few seconds of boot.
        let mut watch = CrashLoopWatch::default();
        let samples = [
            (JobSample::Running, None),
            (JobSample::Exited(&service_exit(3)), None),
            (JobSample::Running, None),
            (JobSample::Running, None),
            (JobSample::Exited(&service_exit(4)), None),
            (JobSample::Running, None),
            (JobSample::Exited(&service_exit(5)), Some(2)),
            (JobSample::Running, Some(2)),
            (JobSample::Running, Some(2)),
        ];
        for (index, (sample, expected)) in samples.into_iter().enumerate() {
            assert_eq!(
                looping_restarts(&mut watch, sample),
                expected,
                "sample {index}"
            );
        }
        let (exit, _) = watch.observe(JobSample::Running).expect("still looping");
        assert_eq!(exit.reason, "exited with code 78");

        assert_eq!(looping_restarts(&mut watch, JobSample::Ready), None);
        assert_eq!(
            looping_restarts(&mut watch, JobSample::Exited(&service_exit(6))),
            None,
            "readiness starts a new baseline"
        );
        assert_eq!(
            looping_restarts(
                &mut watch,
                JobSample::Exited(&gents_server::native_service::ServiceExit {
                    reason: "exited normally".to_string(),
                    restarts: 9,
                    clean: true,
                    code: Some(0),
                })
            ),
            None
        );
    }

    #[test]
    fn a_crash_loop_seen_between_respawns_is_reported_failed_with_its_reason() {
        let native = gents_server::native_service::NativeServiceStatus {
            installed: true,
            running: true,
            job_loaded: true,
            enabled: true,
            requires_approval: false,
            background_item: Default::default(),
            failed: false,
            detail: None,
        };
        let mut watch = CrashLoopWatch::default();
        for restarts in [1, 2, 3] {
            watch.observe(JobSample::Exited(&service_exit(restarts)));
            watch.observe(JobSample::Running);
        }
        let looping = watch.observe(JobSample::Running);
        let status = status_from(
            &ManagedServerRuntimeState::default(),
            None,
            Some(&native),
            None,
            looping.as_ref().map(|(exit, restarts)| (exit, *restarts)),
        );
        assert_eq!(status.state, ManagedServerState::Failed);
        assert!(status.error.unwrap().contains("exited with code 78"));
    }

    #[test]
    fn a_linux_unit_the_supervisor_gave_up_on_is_failed_with_its_exit_cause() {
        let failed = gents_server::native_service::NativeServiceStatus {
            installed: true,
            running: false,
            job_loaded: false,
            enabled: false,
            requires_approval: false,
            background_item: Default::default(),
            failed: true,
            detail: Some("failed".to_string()),
        };
        let exit = gents_server::native_service::ServiceExit {
            reason: "exit-code (exit status 1)".to_string(),
            restarts: 5,
            clean: false,
            code: None,
        };
        let idle = ManagedServerRuntimeState::default();
        let status = status_from(&idle, None, Some(&failed), Some(&exit), None);
        assert_eq!(status.state, ManagedServerState::Failed);
        let error = status.error.unwrap();
        assert!(error.contains("exit-code (exit status 1)"), "{error}");
        assert!(error.contains("will not be restarted"), "{error}");

        let starting = ManagedServerRuntimeState {
            starting: true,
            ..Default::default()
        };
        assert_eq!(
            status_from(&starting, None, Some(&failed), Some(&exit), None).state,
            ManagedServerState::Starting,
            "a new start owns its own outcome"
        );
    }

    #[tokio::test]
    async fn a_crash_loop_behind_a_non_gents_listener_names_the_port() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let observations = AtomicUsize::new(0);
        let error = await_runtime_readiness(
            Duration::from_secs(60),
            Duration::from_millis(1),
            || async { Ok(PortReadiness::Occupied(9191)) },
            || {
                let seen = observations.fetch_add(1, Ordering::SeqCst) as u64;
                async move { Ok(NativeProgress::Exited(service_exit(seen))) }
            },
        )
        .await
        .expect_err("a crash loop fails");
        let error = error.to_string();
        assert!(error.contains("exited with code 78"), "{error}");
        assert!(
            error.contains("another program that is not a Gents agent"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn readiness_fails_with_the_cause_when_the_supervisor_gives_up() {
        let error = await_runtime_readiness(
            Duration::from_secs(60),
            Duration::from_millis(5),
            || async { Ok(PortReadiness::NotListening) },
            || async {
                Ok(NativeProgress::Failed(
                    gents_server::native_service::ServiceExit {
                        reason: "start-limit-hit (exit status 1)".to_string(),
                        restarts: 5,
                        clean: false,
                        code: None,
                    },
                ))
            },
        )
        .await
        .expect_err("a unit the supervisor gave up on is not waited on");
        assert!(error.to_string().contains("start-limit-hit"), "{error}");
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
                        code: Some(0),
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
            background_item: Default::default(),
            failed: false,
            detail: None,
        };
        let clean = gents_server::native_service::ServiceExit {
            reason: "exited normally".to_string(),
            restarts: 0,
            clean: true,
            code: Some(0),
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
        let mut watch = CrashLoopWatch::default();
        for restarts in [7, 8, 9] {
            watch.observe(JobSample::Exited(&service_exit(restarts)));
        }
        assert_eq!(looping_restarts(&mut watch, JobSample::Running), Some(2));
        assert_eq!(
            looping_restarts(&mut watch, JobSample::Exited(&service_exit(1))),
            None,
            "a lower counter is a reloaded job"
        );
        assert_eq!(watch.baseline, Some(1));
        assert_eq!(
            looping_restarts(&mut watch, JobSample::Exited(&service_exit(2))),
            None
        );
        assert_eq!(
            looping_restarts(&mut watch, JobSample::Exited(&service_exit(3))),
            Some(2)
        );
    }

    #[test]
    fn any_runtime_on_the_port_of_a_fresh_home_is_foreign_and_named() {
        let payload = serde_json::json!({
            "agent_did": "did:key:other",
            "agent_name": "other",
            "home": "/Users/test/other-home",
            gents_protocol::serve_lifecycle::STATUS_LIFECYCLE_FIELD: "ready",
        });
        let PortReadiness::Foreign(foreign) = classify_port_payload(9191, None, payload.clone())
        else {
            panic!("a fresh home adopts no runtime");
        };
        let message = foreign.message();
        assert!(message.contains("did:key:other"), "{message}");
        assert!(message.contains("/Users/test/other-home"), "{message}");
        assert!(
            message.contains("gents service stop --home /Users/test/other-home"),
            "{message}"
        );
        assert!(message.contains("--http-port"), "{message}");

        assert!(matches!(
            classify_port_payload(9191, Some("did:key:mine"), payload.clone()),
            PortReadiness::Foreign(_)
        ));
        assert!(matches!(
            classify_port_payload(9191, Some("did:key:other"), payload),
            PortReadiness::Ready(_)
        ));

        let PortReadiness::Foreign(anonymous) =
            classify_port_payload(9191, Some("did:key:mine"), serde_json::json!({}))
        else {
            panic!("a listener without an identity is not this home's");
        };
        assert!(anonymous
            .message()
            .contains("does not advertise a Gents identity"));
    }

    #[test]
    fn a_foreign_runtime_fails_a_loaded_job_and_names_itself_when_stopped() {
        let foreign = ForeignRuntime {
            port: 9191,
            agent_did: Some("did:key:other".to_string()),
            home: None,
        };
        let loaded = gents_server::native_service::NativeServiceStatus {
            installed: true,
            running: true,
            job_loaded: true,
            enabled: true,
            requires_approval: false,
            background_item: Default::default(),
            failed: false,
            detail: None,
        };
        let idle = ManagedServerRuntimeState::default();
        let mut status = status_from(&idle, None, Some(&loaded), None, None);
        project_foreign_port(&mut status, &foreign.message(), false);
        assert_eq!(
            status.state,
            ManagedServerState::Failed,
            "a job that cannot bind its port is not waited on"
        );
        assert!(status.error.as_deref().unwrap().contains("did:key:other"));

        let stopped = gents_server::native_service::NativeServiceStatus {
            running: false,
            job_loaded: false,
            ..loaded
        };
        let mut status = status_from(&idle, None, Some(&stopped), None, None);
        project_foreign_port(&mut status, &foreign.message(), false);
        assert_eq!(status.state, ManagedServerState::Stopped);
        assert!(status.error.as_deref().unwrap().contains("<its home>"));

        let mut status = status_from(&idle, None, Some(&stopped), None, None);
        project_foreign_port(&mut status, &foreign.message(), true);
        assert!(
            status.error.is_none(),
            "a start in progress reports it itself"
        );
    }

    #[test]
    fn another_homes_runtime_never_blocks_stop_or_restart() {
        for initialized_did in [None, Some("did:key:mine")] {
            let port = classify_port_payload(
                9191,
                initialized_did,
                serde_json::json!({ "agent_did": "did:key:other" }),
            );
            assert!(matches!(port, PortReadiness::Foreign(_)));
            for job_loaded in [false, true] {
                ensure_native_owns_running_endpoint(&port, job_loaded)
                    .expect("stopping this home's job is never refused for another runtime");
            }
        }
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
        requests: Arc<std::sync::atomic::AtomicUsize>,
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
            let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (task_hold, task_received, task_release, task_requests) = (
                hold.clone(),
                received.clone(),
                release.clone(),
                requests.clone(),
            );
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
                    task_requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
                requests,
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
                gents_protocol::serve_lifecycle::STATUS_LIFECYCLE_FIELD: "ready",
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
        {
            let managed = state.managed_server.lock().await;
            assert!(
                managed.pairing_task.is_none(),
                "a chat-ready runtime is not re-paired"
            );
            assert!(managed.schema_observation_task.is_some());
        }
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

        drain_managed_runtime_pairing(&state).await;
        assert!(state
            .managed_server
            .lock()
            .await
            .schema_observation_task
            .is_none());
        server.task.abort();
        core.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn drain_cancels_pairing_while_it_waits_for_the_runtime_status() {
        use gents_desktop_core::client::{ClientCoreOptions, DesktopPaths};

        let (temp, state) = orchestration_state();
        let agent_did = "did:key:starting-runtime";
        let core = Arc::new(
            ClientCore::start_with_paths_and_options(
                DesktopPaths::from_root(temp.path().join("client")),
                ClientCoreOptions::local_only(),
            )
            .await
            .expect("client core"),
        );
        let server = StatusServer::start(
            serde_json::json!({
                "agent_did": agent_did,
                gents_protocol::peer_schema::STATUS_REPLICATED_SCHEMA_FIELD: null,
            })
            .to_string(),
        )
        .await;
        start_managed_runtime_pairing(
            &state,
            Arc::clone(&core),
            temp.path().join("agent"),
            ManagedPairingTarget {
                agent_name: "Starting".to_string(),
                agent_did: agent_did.to_string(),
                graphql: server.graphql(),
            },
        )
        .await;
        assert!(state.managed_server.lock().await.pairing_task.is_some());
        tokio::time::timeout(Duration::from_secs(10), async {
            while server.requests.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("pairing is waiting for the runtime status");

        tokio::time::timeout(
            Duration::from_secs(5),
            drain_managed_runtime_pairing(&state),
        )
        .await
        .expect("drain does not wait out the status publish window");
        let managed = state.managed_server.lock().await;
        assert!(managed.pairing_task.is_none());
        assert!(managed.pairing_cancel.is_none());
        assert!(managed.schema_observation_task.is_none());
        drop(managed);

        server.task.abort();
        core.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn managed_schema_observation_refuses_a_target_whose_route_was_replaced() {
        let temp = tempfile::tempdir().expect("temp");
        let agent_did = "did:key:managed-runtime";
        let (core, route_a) = skewed_managed_runtime(&temp, agent_did).await;
        let target_a = ManagedPairingTarget {
            agent_name: "Managed".to_string(),
            agent_did: agent_did.to_string(),
            graphql: route_a.graphql(),
        };
        let route_b = "http://127.0.0.1:1/api/v0/graphql";
        core.add_managed_enrollment_peer_for_test(agent_did, route_b, "/tmp/managed-home", 2)
            .await
            .expect("route B replaces route A");

        route_a
            .hold
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let error = observe_managed_runtime_schema(&core, &target_a)
            .await
            .unwrap_err();
        assert!(error.contains("no longer routes through"), "{error}");
        assert!(
            core.sync_state().peer_schema_skew.is_empty(),
            "route A's status is never recorded against route B"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(200), route_a.received.notified())
                .await
                .is_err(),
            "route A is not fetched once its route is replaced"
        );

        route_a.task.abort();
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
