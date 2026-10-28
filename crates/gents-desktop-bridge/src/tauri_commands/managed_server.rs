#[cfg(test)]
use crate::local_pairing::{
    await_managed_pairing_approval, enrollment_request_is_not_visible_yet, resolve_managed_request,
    select_managed_request, PairingApproval,
};
use crate::local_pairing::{
    ensure_managed_runtime_pairing, observe_managed_runtime_schema, retry_managed_pairing,
    ManagedPairingTarget, PairingFailure, MANAGED_PAIRING_ATTEMPTS, MANAGED_PAIRING_RETRY_DELAY,
};
use gents_desktop_core::home_reset::*;
use gents_desktop_core::preference::{ReviewedHome, StoredManagedServer, MANAGED_SERVER_CONFIG};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Runtime, State};

use gents_desktop_core::client::ClientCore;
#[cfg(test)]
use gents_desktop_core::client::EnrollmentRequestResult;
use gents_desktop_core::local_runtime::{init_standard_local_runtime, DesktopInitOptions};
use gents_protocol::serve_lifecycle::{outdated_runtime_message, ObservedServeLifecycle};

use crate::config::ManagedServerPolicy;
use crate::contract::MANAGED_SERVER_UPDATED_EVENT;
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::{current_core, DesktopAppState};
use crate::tauri_commands::service_executable::{
    ensure_installable_location, resolve_service_executable,
};
use crate::types::{
    HomeResetDisposition, ManagedServerResetRequest, ManagedServerResetResult,
    ManagedServerRestartRequest, ManagedServerRootValidation, ManagedServerRootValidationRequest,
    ManagedServerStartRequest, ManagedServerState, ManagedServerStatus, ManagedServerToolCeiling,
};

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
pub async fn desktop_managed_server_status<R>(
    app: AppHandle<R>,
    state: State<'_, DesktopAppState>,
) -> Result<ManagedServerStatus, BridgeError>
where
    R: Runtime,
{
    let status = managed_server_status_for(&app, &state).await?;
    let _ = app.emit(MANAGED_SERVER_UPDATED_EVENT, status.clone());
    Ok(status)
}

async fn observe_managed_server_status<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<ManagedServerStatus, BridgeError>
where
    R: Runtime,
{
    let stored = load_bound_preference(state).await?;
    let native = run_native(native_service(app, state)?, |service| service.status()).await?;
    let last_exit = if (native.job_loaded || native.failed)
        && !native.running
        && !native.requires_approval
    {
        match run_native(native_service(app, state)?, |service| service.last_exit()).await {
            Ok(reason) => reason,
            Err(error) => {
                tracing::debug!(target: "gents_desktop::managed_server", error = %error.message, "could not read the native service exit record");
                None
            }
        }
    } else {
        None
    };
    let port = match state.policy.node_home.as_deref() {
        Some(node_home) => observe_port_readiness(node_home).await?,
        None => PortReadiness::NotListening,
    };
    let mut managed = state.managed_server.lock().await;
    if native.job_loaded
        || native.background_item == gents_server::native_service::BackgroundItemStatus::Enabled
    {
        managed.approval_refused = false;
    }
    if let (Some(exit), Some(node_home)) = (last_exit.as_ref(), state.policy.node_home.as_deref()) {
        if let Some(kind) = exit.refused_store().filter(|_| !managed.starting) {
            managed.incompatible_store = Some(refused_runtime_store(node_home, Some(kind)));
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
    status.pairing_ready = pairing_is_ready(state, status.node_did.as_deref()).await;
    Ok(status)
}

pub(crate) async fn managed_server_status_for<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<ManagedServerStatus, BridgeError>
where
    R: Runtime,
{
    ensure_allowed(state)?;
    observe_managed_server_status(app, state).await
}

fn project_external_status(
    mut external: ManagedServerStatus,
    native: &gents_server::native_service::NativeServiceStatus,
) -> ManagedServerStatus {
    external.auto_start = native.enabled;
    external.approval_required = native.requires_approval;
    if native.job_loaded {
        external.state = ManagedServerState::Running;
    }
    external
}

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

async fn pairing_is_ready(state: &DesktopAppState, node_did: Option<&str>) -> bool {
    let (Some(core), Some(node_did)) = (current_core(state), node_did) else {
        return false;
    };
    core.peer_records().await.iter().any(|peer| {
        peer.node_did == node_did
            && peer.is_enrollment()
            && peer.is_managed_runtime()
            && peer.is_chat_ready_at(chrono::Utc::now())
    })
}

#[tauri::command]
pub async fn desktop_managed_server_start<R>(
    app: AppHandle<R>,
    request: ManagedServerStartRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ManagedServerStatus, BridgeError>
where
    R: Runtime,
{
    ensure_allowed(&state)?;
    let lifecycle = state.managed_server_lifecycle.lock().await;
    start_managed_server(&app, request, &state, lifecycle).await
}

type LifecycleGuard<'a> = tokio::sync::MutexGuard<'a, ()>;

const START_CANCELLED: &str =
    "Starting the local Node was cancelled because the Node was stopped, restarted, or reconfigured.";
const START_SUPERSEDED: &str = "This start of the local Node was replaced by a newer start.";

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

pub(super) async fn cancel_start_wait(state: &DesktopAppState) {
    let mut managed = state.managed_server.lock().await;
    if let Some(token) = managed.start_wait.take() {
        token.cancel(START_CANCELLED);
        managed.starting = false;
    }
}

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
            "The local Node is still starting. Change start at login after it finishes.",
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

trait ManagedLaunch {
    async fn requires_approval(&self) -> Result<bool, BridgeError>;
    async fn await_approval(&self) -> anyhow::Result<()>;
    async fn start(&self) -> Result<Launched, BridgeError>;
    async fn await_ready(&self) -> anyhow::Result<Readiness>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Launched {
    Started,
    ApprovalPending(String),
}

struct NativeLaunch<'a, R>
where
    R: Runtime,
{
    app: &'a AppHandle<R>,
    state: &'a DesktopAppState,
    node_home: &'a Path,
    enable_at_login: bool,
}

impl<R> ManagedLaunch for NativeLaunch<'_, R>
where
    R: Runtime,
{
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
        wait_for_managed_server(self.app, self.state, self.node_home).await
    }
}

struct LaunchFailure<'a> {
    error: anyhow::Error,
    attempted_start: bool,
    lifecycle: Option<LifecycleGuard<'a>>,
}

async fn launch_managed_server<'a, L>(
    state: &'a DesktopAppState,
    token: &StartWait,
    lifecycle: LifecycleGuard<'a>,
    launch: &L,
) -> Result<(ManagedServerStatus, LifecycleGuard<'a>), LaunchFailure<'a>>
where
    L: ManagedLaunch,
{
    let mut lifecycle = Some(lifecycle);
    let mut attempted_start = false;
    let result: anyhow::Result<ManagedServerStatus> = async {
        if launch.requires_approval().await? {
            lifecycle = None;
            wait_unlocked(token, launch.await_approval()).await?;
            lifecycle = Some(relock_start(state, token).await?);
        }
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

async fn start_when_approved<'a, L>(
    state: &'a DesktopAppState,
    token: &StartWait,
    lifecycle: &mut Option<LifecycleGuard<'a>>,
    launch: &L,
) -> anyhow::Result<()>
where
    L: ManagedLaunch,
{
    let started = tokio::time::Instant::now();
    loop {
        let refusal = match launch.start().await {
            Ok(Launched::Started) => return Ok(()),
            Ok(Launched::ApprovalPending(detail)) => {
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

async fn start_managed_server<'a, R>(
    app: &AppHandle<R>,
    request: ManagedServerStartRequest,
    state: &'a DesktopAppState,
    lifecycle: LifecycleGuard<'a>,
) -> Result<ManagedServerStatus, BridgeError>
where
    R: Runtime,
{
    ensure_app_not_quitting(state)?;
    let node_name = request.node_name.trim();
    if node_name.is_empty() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "nodeName is required",
        ));
    }
    let node_home = state.policy.node_home.clone().ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::Unsupported,
            "managed server requires a local node home",
        )
    })?;
    let stored = load_bound_preference(state).await?;
    let authority = start_authority(
        request.tool_ceiling,
        request.tool_root.as_deref(),
        stored.as_ref(),
    )?;
    refuse_renaming_home(&node_home, node_name).await?;
    gents::home::ensure_home_identity_can_serve(&node_home)
        .map_err(|error| BridgeError::untyped(format!("{error:#}")))?;

    let mut carried_wait = None;
    let mut replace_loaded_job = false;
    let (ready, initial_enabled, _lifecycle) = match matching_external_server(&node_home).await? {
        Some(external) => (Some(external), false, lifecycle),
        None => {
            ensure_launchable_here(app, state)?;
            let outdated = match observe_port_readiness(&node_home).await? {
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
                match wait_for_booting_managed_server(app, state, &node_home, lifecycle).await? {
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
            core.refresh_local_standard_peer(&node_home, &target.node_name)
                .await
                .map_err(|error| BridgeError::untyped(error.to_string()))?;
            start_managed_runtime_pairing(state, core, node_home, target).await;
        }
        external.pairing_ready = pairing_is_ready(state, external.node_did.as_deref()).await;
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
            run_native(native_service(app, state)?, |service| service.stop(false)).await?;
        }
        if gents_server::server_host::initialized_home(&node_home) {
            ensure_default_port_identity(&node_home).await?;
        }
        gents_server::server_host::ensure_standard_home(
            tauri::async_runtime::handle().inner(),
            gents_server::server_host::ProvisionOptions {
                home: node_home.clone(),
                node_name: node_name.to_string(),
                tool_ceiling: runtime_tool_ceiling(authority.tool_ceiling),
                tool_root: authority.tool_root.clone(),
                store_key_custody: state.policy.store_key_custody,
            },
        )
        .await?;
        let (tool_ceiling, tool_root) = authority.stored();
        save_confirmed_preference(
            state,
            &node_home,
            &StoredManagedServer {
                node_name: node_name.to_string(),
                tool_ceiling: Some(tool_ceiling),
                tool_root,
                reviewed_for: None,
            },
        )
        .await?;
        ensure_default_port_identity(&node_home).await?;
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
                node_home: &node_home,
                enable_at_login: false,
            };
            match launch_managed_server(state, &token, lifecycle, &launch).await {
                Ok((ready, lifecycle)) => {
                    let initialized: anyhow::Result<()> = async {
                        validate_ready_runtime(&ready, &authority, &node_home)?;
                        if current_core(state).is_none() {
                            init_standard_local_runtime(DesktopInitOptions {
                                node_home: node_home.clone(),
                                desktop_paths: state.policy.desktop_paths.clone(),
                                label: node_name.to_string(),
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

    match launched {
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
                &node_home,
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
    let mut status = if let Some(external) = matching_external_server(&node_home).await? {
        project_external_status(external, &native)
    } else {
        let managed = state.managed_server.lock().await;
        status_from(&managed, stored.as_ref(), Some(&native), None, None)
    };
    status.pairing_ready = pairing_is_ready(state, status.node_did.as_deref()).await;
    Ok(status)
}

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
    if !still_booting {
        state.managed_server.lock().await.last_error = Some(message.clone());
    }
    Ok((message, typed_error))
}

async fn install_then_launch<'a, L>(
    state: &'a DesktopAppState,
    token: &StartWait,
    lifecycle: LifecycleGuard<'a>,
    install: impl Future<Output = Result<(), BridgeError>>,
    launch: &L,
) -> Result<(ManagedServerStatus, LifecycleGuard<'a>), LaunchFailure<'a>>
where
    L: ManagedLaunch,
{
    if let Err(error) = install.await {
        return Err(LaunchFailure {
            error: error.into(),
            attempted_start: false,
            lifecycle: Some(lifecycle),
        });
    }
    launch_managed_server(state, token, lifecycle, launch).await
}

async fn fail_managed_start<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
    token: &StartWait,
    node_home: &Path,
    initial_enabled: bool,
    failure: LaunchFailure<'_>,
) -> BridgeError
where
    R: Runtime,
{
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
                node_home
            )),
            Ok(Some(_))
        );
    let refused_store = refused.then(|| refused_runtime_store(node_home, refused_kind));
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

#[tauri::command]
pub async fn desktop_managed_server_reset<R>(
    request: ManagedServerResetRequest,
    app: AppHandle<R>,
    state: State<'_, DesktopAppState>,
) -> Result<ManagedServerResetResult, BridgeError>
where
    R: Runtime,
{
    let full_reset = request.full_reset.unwrap_or(false);
    if full_reset && !cfg!(target_os = "macos") {
        return Err(BridgeError::new(
            BridgeErrorCode::Unsupported,
            "Full desktop reset is supported on macOS.",
        ));
    }
    let disposition = request.disposition.unwrap_or(HomeResetDisposition::Archive);
    let Some(confirmation) = request.confirmation.as_deref() else {
        return Ok(plan_state_reset(&state, full_reset).await?.preview());
    };
    let _lifecycle = lock_lifecycle_superseding_start(&state).await;
    retire_incompatible_home(&app, &state, confirmation, disposition, full_reset).await
}

async fn plan_state_reset(
    state: &DesktopAppState,
    full_reset: bool,
) -> Result<HomeResetPlan, BridgeError> {
    let runtime_home = state
        .policy
        .node_home
        .as_deref()
        .filter(|_| state.policy.managed_server == ManagedServerPolicy::Allowed);
    if full_reset {
        let user_home = resolve_user_home(dirs::home_dir())?;
        let home = runtime_home.ok_or_else(|| {
            BridgeError::new(
                BridgeErrorCode::Unsupported,
                "Full reset requires a locally managed runtime.",
            )
        })?;
        let config = gents_server::native_service::NativeServiceConfig::new(
            home.to_path_buf(),
            std::env::current_exe().map_err(|error| BridgeError::untyped(error.to_string()))?,
        )
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
        let platform = gents_server::native_service::NativeServicePlatform::current()
            .map_err(|error| BridgeError::untyped(error.to_string()))?;
        return plan_full_reset(
            Some(home),
            &state.policy.desktop_paths,
            &user_home,
            &config.definition_path(platform),
        );
    }
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
        client_binding(state).await,
    )
}

fn classify_binding(
    reviewed: Option<&ReviewedHome>,
    configured: Option<&HomeIdentity>,
    reviewed_home: Option<&HomeIdentity>,
    reviewed_init_present: bool,
) -> ClientBinding {
    let Some(reviewed) = reviewed else {
        return ClientBinding::Unbound;
    };
    if configured.is_some_and(|home| home.reviewed == *reviewed) {
        return ClientBinding::BoundToHome;
    }
    if configured.is_some_and(|home| home.reviewed.node_did == reviewed.node_did) {
        return ClientBinding::Unbound;
    }
    match reviewed_home {
        Some(home) if home.reviewed.node_did == reviewed.node_did => ClientBinding::Unbound,
        Some(_) => ClientBinding::Orphaned,
        None if reviewed_init_present => ClientBinding::Unbound,
        None => ClientBinding::Orphaned,
    }
}

async fn client_binding(state: &DesktopAppState) -> ClientBinding {
    let Some(node_home) = state.policy.node_home.as_deref() else {
        return ClientBinding::Unbound;
    };
    let reviewed = match load_preference(state).await {
        Ok(stored) => stored.and_then(|stored| stored.reviewed_for),
        Err(error) => {
            tracing::warn!(
                target: "gents_desktop::managed_server",
                error = %error.message,
                "managed-server.json is unreadable; treating the client state as unbound"
            );
            None
        }
    };
    let Some(reviewed) = reviewed else {
        return ClientBinding::Unbound;
    };
    let configured = read_home_identity(node_home).await;
    let reviewed_path = Path::new(&reviewed.node_home);
    let reviewed_home = read_home_identity(reviewed_path).await;
    let Ok(reviewed_init_present) = present(&gents::home::init_config_path(reviewed_path)) else {
        return ClientBinding::Unbound;
    };
    classify_binding(
        Some(&reviewed),
        configured.as_ref(),
        reviewed_home.as_ref(),
        reviewed_init_present,
    )
}

pub(super) async fn retire_orphaned_client_state(
    state: &DesktopAppState,
) -> Result<Option<ManagedServerResetResult>, BridgeError> {
    if state.policy.managed_server != ManagedServerPolicy::Allowed {
        return Ok(None);
    }
    let _lifecycle = state.managed_server_lifecycle.lock().await;
    if client_binding(state).await != ClientBinding::Orphaned {
        return Ok(None);
    }
    drain_managed_runtime_pairing(state).await;
    let _client = super::lifecycle::exclude_client(state).await?;
    let user_home = resolve_user_home(dirs::home_dir())?;
    let plan = plan_home_reset(
        None,
        None,
        None,
        &state.policy.desktop_paths,
        &user_home,
        ClientBinding::Orphaned,
    )?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
    let backup = plan.backup_path(&stamp)?;
    plan.preflight(&backup)?;
    let result = plan.retire(HomeResetDisposition::Archive, Some(&backup))?;
    tracing::info!(
        target: "gents_desktop::managed_server",
        backup = ?result.backup_path,
        retired = result.retired_paths.len(),
        "archived desktop client state paired with a node that is no longer here"
    );
    Ok(Some(result))
}

async fn retire_incompatible_home<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
    confirmation: &str,
    disposition: HomeResetDisposition,
    full_reset: bool,
) -> Result<ManagedServerResetResult, BridgeError>
where
    R: Runtime,
{
    let plan = plan_state_reset(state, full_reset).await?;
    plan.check_confirmation(confirmation, disposition)?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
    let backup = match disposition {
        HomeResetDisposition::Archive => {
            let backup = plan.backup_path(&stamp)?;
            plan.preflight(&backup)?;
            Some(backup)
        }
        HomeResetDisposition::Delete => None,
    };

    if full_reset {
        run_native(native_service(app, state)?, |service| service.stop(true)).await?;
        let native = run_native(native_service(app, state)?, |service| service.status()).await?;
        ensure_managed_runtime_stopped(native.is_active_or_transitioning())?;
    }
    let _store_lock = match plan.runtime.as_ref() {
        Some((home, _)) => {
            if !full_reset {
                run_native(native_service(app, state)?, |service| service.uninstall()).await?;
            }
            let native =
                run_native(native_service(app, state)?, |service| service.status()).await?;
            ensure_managed_runtime_stopped(native.is_active_or_transitioning())?;
            ensure_home_not_served(home).await?;
            Some(lock_home_for_retirement(home)?)
        }
        None => None,
    };
    let _client = if plan.retires_client_state() {
        drain_managed_runtime_pairing(state).await;
        Some(super::lifecycle::exclude_client(state).await?)
    } else {
        None
    };
    let settled = plan_state_reset(state, full_reset).await?;
    settled.check_confirmation(confirmation, disposition)?;

    if full_reset {
        let executable = resolve_service_executable(app, state.policy.desktop_paths.root())?;
        executable.install()?;
        let executable = executable.service_path().to_path_buf();
        let home = state.policy.node_home.clone().ok_or_else(|| {
            BridgeError::new(
                BridgeErrorCode::Unsupported,
                "No managed runtime home is configured.",
            )
        })?;
        let desktop = state.policy.desktop_paths.root().to_path_buf();
        let confirmation = confirmation.to_owned();
        tokio::task::spawn_blocking(move || {
            launch_reset_helper(&executable, &home, &desktop, &confirmation, disposition)
        })
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))??;
        app.exit(0);
        return Ok(settled.preview());
    }
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

fn launch_reset_helper(
    executable: &Path,
    home: &Path,
    desktop: &Path,
    confirmation: &str,
    disposition: HomeResetDisposition,
) -> Result<(), BridgeError> {
    launch_reset_helper_with_timeout(
        executable,
        home,
        desktop,
        confirmation,
        disposition,
        Duration::from_secs(15),
    )
}

fn launch_reset_helper_with_timeout(
    executable: &Path,
    home: &Path,
    desktop: &Path,
    confirmation: &str,
    disposition: HomeResetDisposition,
    timeout: Duration,
) -> Result<(), BridgeError> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let launch = || -> anyhow::Result<()> {
        let mut command = Command::new(executable);
        command
            .args(["desktop", "reset", "--home"])
            .arg(home)
            .arg("--confirm")
            .arg(confirmation)
            .arg("--wait-for-desktop")
            .arg("--desktop-pid")
            .arg(std::process::id().to_string())
            .arg("--desktop-executable")
            .arg(std::env::current_exe()?)
            .env("GENTS_DESKTOP_HOME", desktop)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if disposition == HomeResetDisposition::Delete {
            command.arg("--delete");
        }
        let mut child = command.spawn()?;
        let mut stdout = child.stdout.take().expect("piped reset stdout");
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut ready = [0];
            let read = stdout.read_exact(&mut ready);
            let _ = send.send(read.is_ok() && ready[0] == b'R');
        });
        if receive.recv_timeout(timeout) != Ok(true) {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("The reset helper could not prepare; your local state has not been retired. Run gents desktop reset to review and retry.");
        }
        Ok(())
    };
    launch().map_err(|error| BridgeError::new(BridgeErrorCode::Backend, format!("{error:#}")))
}

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

fn runtime_tool_ceiling(
    value: ManagedServerToolCeiling,
) -> gents_server::server_host::ManagedToolCeiling {
    use gents_server::server_host::ManagedToolCeiling;
    match value {
        ManagedServerToolCeiling::MetaOnly => ManagedToolCeiling::MetaOnly,
        ManagedServerToolCeiling::Readonly => ManagedToolCeiling::Readonly,
        ManagedServerToolCeiling::Readwrite => ManagedToolCeiling::Readwrite,
    }
}

const MANAGED_SERVER_READY_TIMEOUT: Duration = Duration::from_secs(300);
const BACKGROUND_APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);
const MANAGED_SERVER_POLL_INTERVAL: Duration = Duration::from_millis(500);
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
    Failed(gents_server::native_service::ServiceExit),
}

async fn observe_native_progress<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<NativeProgress, BridgeError>
where
    R: Runtime,
{
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

async fn wait_for_managed_server<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
    node_home: &Path,
) -> anyhow::Result<Readiness>
where
    R: Runtime,
{
    await_runtime_readiness(
        MANAGED_SERVER_READY_TIMEOUT,
        MANAGED_SERVER_POLL_INTERVAL,
        || observe_port_readiness(node_home),
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

async fn wait_for_booting_managed_server<'a, R>(
    app: &AppHandle<R>,
    state: &'a DesktopAppState,
    node_home: &Path,
    lifecycle: LifecycleGuard<'a>,
) -> Result<BootOutcome<'a>, BridgeError>
where
    R: Runtime,
{
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
        wait_for_managed_server(app, state, node_home),
    )
    .await;
    if token.is_cancelled() {
        return Err(BridgeError::untyped(token.cancel_message()));
    }
    if matches!(adopted, Ok(BootOutcome::NeedsLaunch(..))) {
        return adopted.map_err(|error| BridgeError::untyped(format!("{error:#}")));
    }
    let result = settle_adoption(state, &token, node_home, adopted).await;
    emit_status(app, state).await;
    result
}

async fn settle_adoption<'a>(
    state: &'a DesktopAppState,
    token: &StartWait,
    node_home: &Path,
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
        .then(|| refused_runtime_store(node_home, refused));
    Err(typed)
}

async fn native_requires_approval<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<bool, BridgeError>
where
    R: Runtime,
{
    let refused = state.managed_server.lock().await.approval_refused;
    Ok(
        run_native(native_service(app, state)?, |service| service.status())
            .await?
            .approval_pending(refused),
    )
}

async fn await_managed_server_approval<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> anyhow::Result<()>
where
    R: Runtime,
{
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
pub async fn desktop_managed_server_open_login_items<R>(
    app: AppHandle<R>,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError>
where
    R: Runtime,
{
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
    Booting,
    Outdated { version: Option<String> },
    Occupied(u16),
    NotListening,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ForeignRuntime {
    port: u16,
    node_did: Option<String>,
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
            node_did: field("node_did"),
            home: field("home"),
        }
    }

    fn message(&self) -> String {
        let port = self.port;
        let Some(did) = self.node_did.as_deref() else {
            return format!(
                "Port {port} is in use by a program that does not advertise a Gents identity, and the local Node needs that port. Quit that program, then start the Node again."
            );
        };
        let (located, home) = match self.home.as_deref() {
            Some(home) => (format!(" from home {home}"), home),
            None => (String::new(), "<its home>"),
        };
        format!(
            "Port {port} is already served by a different Gents Node ({did}){located}, and the local Node needs that port. Stop the other Node with `gents service stop --home {home}` if it runs as a background service, or with Ctrl-C in the terminal running `gents server`. Or restart it on another port with `gents server --home {home} --http-port <port>`."
        )
    }
}

fn occupied_port_message(port: u16) -> String {
    format!(
        "Port {port} is in use by another program that is not a Gents Node, and the local Node needs that port. Quit that program, then start the Node again."
    )
}

async fn observe_port_readiness(node_home: &std::path::Path) -> Result<PortReadiness, BridgeError> {
    let config = gents_server::server_host::ServerConfig::standard(node_home.to_path_buf());
    let Some(payload) = default_port_payload(Some(node_home)).await? else {
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
    let initialized_did = if gents_server::server_host::initialized_home(node_home) {
        read_initialized_did(node_home).await
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
        .get("node_did")
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
    node_home: &Path,
) -> anyhow::Result<()> {
    let expected_did = std::fs::read(node_home.join("init.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("node_did")
                .and_then(serde_json::Value::as_str)
                .filter(|did| !did.trim().is_empty())
                .map(str::to_owned)
        });
    let expected_root = authority
        .tool_root
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned());
    if expected_did.is_none()
        || status.node_did.as_deref() != expected_did.as_deref()
        || status.effective_tool_ceiling != Some(authority.tool_ceiling)
        || status.effective_tool_root.as_deref() != expected_root.as_deref()
    {
        anyhow::bail!(
            "native runtime readiness did not match the initialized identity and reviewed host authority (live did={:?} ceiling={:?} root={:?}; expected did={:?} ceiling={:?} root={:?})",
            status.node_did,
            status.effective_tool_ceiling,
            status.effective_tool_root,
            expected_did,
            Some(authority.tool_ceiling),
            expected_root
        );
    }
    Ok(())
}

fn ensure_launchable_here<R>(app: &AppHandle<R>, state: &DesktopAppState) -> Result<(), BridgeError>
where
    R: Runtime,
{
    let executable = resolve_service_executable(app, state.policy.desktop_paths.root())?;
    ensure_installable_location(executable.service_path())
}

fn native_service<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<gents_server::native_service::NativeServiceManager, BridgeError>
where
    R: Runtime,
{
    build_native_service(app, state, false)
}

fn launchable_native_service<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<gents_server::native_service::NativeServiceManager, BridgeError>
where
    R: Runtime,
{
    build_native_service(app, state, true)
}

pub(crate) fn refresh_packaged_install<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<(), BridgeError>
where
    R: Runtime,
{
    if state.policy.node_home.is_none() {
        return Ok(());
    }
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
        if !status.running
            && service
                .installed_executable_differs()
                .map_err(native_error)?
        {
            tracing::info!(
                target: "gents_desktop::managed_server",
                runtime = %executable.service_path().display(),
                "reinstalling the stopped node service, whose definition names a runtime that moved"
            );
            service.stop(false).map_err(native_error)?;
            return service.install().map_err(native_error);
        }
        tracing::info!(
            target: "gents_desktop::managed_server",
            "agent restart pending before the updated service definition applies"
        );
        return Ok(());
    }
    service.install().map_err(native_error)
}

pub(crate) async fn restart_outdated_managed_job<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<(), BridgeError>
where
    R: Runtime,
{
    let Some(node_home) = state.policy.node_home.clone() else {
        return Ok(());
    };
    let _lifecycle = state.managed_server_lifecycle.lock().await;
    if state.app_exit.is_quitting() {
        return Ok(());
    }
    let PortReadiness::Outdated { version } = observe_port_readiness(&node_home).await? else {
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
        "restarting the node service, whose runtime predates this app"
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

fn build_native_service<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
    install_runtime: bool,
) -> Result<gents_server::native_service::NativeServiceManager, BridgeError>
where
    R: Runtime,
{
    let home = state.policy.node_home.clone().ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::Unsupported,
            "managed server requires a local node home",
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
    let Some(node_home) = state.policy.node_home.clone() else {
        tracing::warn!("managed pairing requires a local node home");
        return;
    };
    let target = match matching_external_server(&node_home).await {
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
        .refresh_local_standard_peer(&node_home, &target.node_name)
        .await
    {
        tracing::warn!(
            target: "gents_desktop::managed_server",
            node_did = %target.node_did,
            error = %error,
            "failed to refresh native managed runtime route before pairing"
        );
        return;
    }
    start_managed_runtime_pairing(state, core, node_home, target).await;
}

async fn start_managed_runtime_pairing(
    state: &DesktopAppState,
    core: Arc<ClientCore>,
    node_home: std::path::PathBuf,
    target: ManagedPairingTarget,
) {
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
                        node_did = %observed_target.node_did,
                        error = %error,
                        "managed runtime replicated schema observation failed"
                    );
                }
            }));
        }
    }

    if core.peer_records().await.iter().any(|peer| {
        peer.node_did == target.node_did
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
            || ensure_managed_runtime_pairing(Arc::clone(&core), &node_home, &target, &cancel),
        )
        .await;
        match paired {
            Ok(()) => {}
            Err(PairingFailure::Cancelled) => {
                tracing::info!(
                    target: "gents_desktop::managed_server",
                    node_did = %target.node_did,
                    "background managed runtime pairing was cancelled"
                );
            }
            Err(error) => {
                tracing::warn!(
                    target: "gents_desktop::managed_server",
                    node_did = %target.node_did,
                    error = %error,
                    "background managed runtime pairing failed"
                );
            }
        }
    }));
}

fn pairing_target(status: &ManagedServerStatus) -> Option<ManagedPairingTarget> {
    Some(ManagedPairingTarget {
        node_name: status.node_name.as_deref()?.trim().to_string(),
        node_did: status.node_did.as_deref()?.trim().to_string(),
        graphql: status.graphql.as_deref()?.trim().to_string(),
    })
    .filter(|target| {
        !target.node_name.is_empty() && !target.node_did.is_empty() && !target.graphql.is_empty()
    })
}

pub(super) async fn drain_managed_runtime_pairing(state: &DesktopAppState) {
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

async fn matching_external_server(
    node_home: &std::path::Path,
) -> Result<Option<ManagedServerStatus>, BridgeError> {
    Ok(match observe_port_readiness(node_home).await? {
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
        node_name: payload
            .get("node_name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        node_did: (!live_did.is_empty()).then(|| live_did.to_string()),
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

fn refused_runtime_store(
    node_home: &Path,
    reported: Option<gents::storage_backend::IncompatibleStoreKind>,
) -> gents::storage_backend::IncompatibleStore {
    let data_path = gents::home::default_data_dir(node_home);
    let kind = gents::storage_backend::incompatible_store_kind(&data_path)
        .ok()
        .flatten()
        .or(reported)
        .unwrap_or(gents::storage_backend::IncompatibleStoreKind::UnknownLineage);
    let data_path = match kind {
        gents::storage_backend::IncompatibleStoreKind::InsecureKey => node_home.join("keys"),
        _ => data_path,
    };
    gents::storage_backend::IncompatibleStore { kind, data_path }
}

async fn default_port_payload(
    node_home: Option<&Path>,
) -> Result<Option<serde_json::Value>, BridgeError> {
    let Some(node_home) = node_home else {
        return Ok(None);
    };
    let config = gents_server::server_host::ServerConfig::standard(node_home.to_path_buf());
    match gents_desktop_core::local_runtime::fetch_runtime_connection_payload(&config.status_url())
        .await
    {
        Ok(payload) => Ok(Some(payload)),
        Err(_) => Ok(None),
    }
}

async fn ensure_default_port_identity(node_home: &Path) -> Result<(), BridgeError> {
    match observe_port_readiness(node_home).await? {
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
            "The running local Node is not owned by the native Gents service. Stop it explicitly before using managed controls.",
        ));
    }
    Ok(())
}

fn combine_cleanup_error(original: String, owner: &str, cleanup: String) -> String {
    format!("{original}; additionally failed to stop the {owner}: {cleanup}")
}

#[tauri::command]
pub async fn desktop_managed_server_stop<R>(
    app: AppHandle<R>,
    disable_auto_start: bool,
    state: State<'_, DesktopAppState>,
) -> Result<ManagedServerStatus, BridgeError>
where
    R: Runtime,
{
    ensure_allowed(&state)?;
    let _lifecycle = lock_lifecycle_superseding_start(&state).await;
    stop_managed_server_locked(&app, disable_auto_start, &state).await
}

#[tauri::command]
pub async fn desktop_managed_server_set_auto_start<R>(
    app: AppHandle<R>,
    enabled: bool,
    state: State<'_, DesktopAppState>,
) -> Result<ManagedServerStatus, BridgeError>
where
    R: Runtime,
{
    ensure_allowed(&state)?;
    let _lifecycle = state.managed_server_lifecycle.lock().await;
    ensure_app_not_quitting(&state)?;
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

async fn stop_managed_server_locked<R>(
    app: &AppHandle<R>,
    disable_auto_start: bool,
    state: &DesktopAppState,
) -> Result<ManagedServerStatus, BridgeError>
where
    R: Runtime,
{
    {
        let mut managed = state.managed_server.lock().await;
        managed.starting = false;
        managed.last_error = None;
        managed.approval_refused = false;
    }
    let port = match state.policy.node_home.as_deref() {
        Some(node_home) => observe_port_readiness(node_home).await?,
        None => PortReadiness::NotListening,
    };
    let native = run_native(native_service(app, state)?, |service| service.status()).await?;
    ensure_native_owns_running_endpoint(&port, native.job_loaded)?;
    drain_managed_runtime_pairing(state).await;
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

pub(crate) async fn prepare_app_exit<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<(), BridgeError>
where
    R: Runtime,
{
    let _lifecycle = state.managed_server_lifecycle.lock().await;
    let service = match native_service(app, state) {
        Ok(service) => service,
        Err(error) if error.code == BridgeErrorCode::Unsupported => return Ok(()),
        Err(error) => return Err(error),
    };
    let native = run_native(service, |service| service.status()).await?;
    if native.enabled {
        return Ok(());
    }
    cancel_start_wait(state).await;
    if should_stop_managed_service_on_exit(&native) {
        stop_managed_server_locked(app, false, state).await?;
    }
    Ok(())
}

fn should_stop_managed_service_on_exit(
    native: &gents_server::native_service::NativeServiceStatus,
) -> bool {
    !native.enabled && (native.job_loaded || native.running || native.is_active_or_transitioning())
}

fn ensure_app_not_quitting(state: &DesktopAppState) -> Result<(), BridgeError> {
    if state.app_exit.is_quitting() {
        return Err(BridgeError::new(
            BridgeErrorCode::InvalidArgument,
            "The desktop is quitting. Wait for the local node to stop before starting it again.",
        ));
    }
    Ok(())
}

pub(crate) async fn start_configured_managed_server<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<(), BridgeError>
where
    R: Runtime,
{
    if state.policy.managed_server != ManagedServerPolicy::Allowed
        || state.policy.node_home.is_none()
    {
        return Ok(());
    }
    let lifecycle = state.managed_server_lifecycle.lock().await;
    if state.app_exit.is_quitting() {
        return Ok(());
    }
    let Some(stored) = load_bound_preference(state).await? else {
        return Ok(());
    };
    if stored.reviewed_for.is_none() || stored.node_name.trim().is_empty() {
        return Ok(());
    }
    let native = run_native(native_service(app, state)?, |service| service.status()).await?;
    if !native.installed || native.running || native.is_active_or_transitioning() {
        return Ok(());
    }
    start_managed_server(
        app,
        ManagedServerStartRequest {
            node_name: stored.node_name,
            tool_ceiling: None,
            tool_root: None,
        },
        state,
        lifecycle,
    )
    .await?;
    Ok(())
}

#[tauri::command]
pub async fn desktop_managed_server_restart<R>(
    app: AppHandle<R>,
    request: ManagedServerRestartRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ManagedServerStatus, BridgeError>
where
    R: Runtime,
{
    ensure_allowed(&state)?;
    let authority = EffectiveManagedAuthority::from_request(
        request.tool_ceiling,
        request.tool_root.as_deref(),
    )?;
    let node_home = state.policy.node_home.clone().ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::Unsupported,
            "managed server requires a local node home",
        )
    })?;
    refuse_renaming_home(&node_home, &request.node_name).await?;
    let lifecycle = lock_lifecycle_superseding_start(&state).await;
    ensure_app_not_quitting(&state)?;
    ensure_launchable_here(&app, &state)?;
    let port = observe_port_readiness(&node_home).await?;
    let previous_did = match &port {
        PortReadiness::Ready(status) => status.node_did.clone(),
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
        home: node_home.clone(),
        node_name: request.node_name.clone(),
        tool_ceiling: runtime_tool_ceiling(tool_ceiling),
        tool_root: authority.tool_root.clone(),
        store_key_custody: state.policy.store_key_custody,
    };
    if let Err(error) = stop_before_reprovision(
        || async { run_native(native_service(&app, &state)?, |service| service.stop(false)).await },
        || async {
            gents_server::server_host::ensure_standard_home(
                tauri::async_runtime::handle().inner(),
                provision,
            )
            .await
            .map_err(|error| BridgeError::untyped(format!("{error:#}")))
        },
        || async {
            save_confirmed_preference(
                &state,
                &node_home,
                &StoredManagedServer {
                    node_name: request.node_name.clone(),
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
    if let (Some(core), Some(node_did)) = (current_core(&state), previous_did.as_deref()) {
        if let Err(error) = core.mark_managed_runtime_restarting(node_did).await {
            let message = format!(
                "The node was stopped, but desktop restart bookkeeping failed: {error:#}. Retry Restart Node."
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
        node_home: &node_home,
        enable_at_login: was_enabled,
    };
    let install = async {
        ensure_default_port_identity(&node_home)
            .await
            .map_err(|mut error| {
                error.message = format!("Your node was stopped; {}", error.message);
                error
            })?;
        run_native(launchable_native_service(&app, &state)?, |service| {
            service.install()
        })
        .await
    };
    let launched = match install_then_launch(&state, &token, lifecycle, install, &launch).await {
        Ok((ready, lifecycle)) => match validate_ready_runtime(&ready, &authority, &node_home) {
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
                fail_managed_start(&app, &state, &token, &node_home, was_enabled, failure).await,
            )
        }
    };
    emit_status(&app, &state).await;
    if let Some(core) = current_core(&state) {
        start_running_managed_pairing(&state, core).await;
    }
    let native = run_native(native_service(&app, &state)?, |service| service.status()).await?;
    let mut status = matching_external_server(&node_home)
        .await?
        .map(|external| project_external_status(external, &native))
        .ok_or_else(|| BridgeError::untyped("native service did not become ready"))?;
    status.pairing_ready = pairing_is_ready(&state, status.node_did.as_deref()).await;
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

fn restarts_since(baseline: &mut Option<u64>, counter: u64) -> u64 {
    let first = baseline
        .filter(|first| *first <= counter)
        .unwrap_or(counter);
    *baseline = Some(first);
    counter - first
}

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
    let refused_exit = last_exit.is_some_and(|exit| settled && exit.incompatible_store());
    let refused_store = refused_exit.then(|| {
        managed
            .incompatible_store
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| {
                "The background Node cannot open its home: it was created by an older Gents version.".to_string()
            })
    });
    let crashed = refused_store.or_else(|| {
        crash_loop
            .filter(|_| settled)
            .map(|(exit, restarts)| {
                format!(
                    "The background Node keeps exiting before it becomes ready: it {} and was restarted {restarts} times in a row. Restart the Node, or check its log.",
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
                            "The background Node failed and will not be restarted: it {} after {} restarts. Start the Node again, or check its log.",
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
        node_name: stored.map(|stored| stored.node_name.clone()),
        node_did: None,
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

async fn emit_status<R>(app: &AppHandle<R>, state: &DesktopAppState)
where
    R: Runtime,
{
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

async fn read_initialized_did(node_home: &std::path::Path) -> Option<String> {
    tokio::fs::read(node_home.join("init.json"))
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("node_did")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
}

async fn read_initialized_name(node_home: &std::path::Path) -> Option<String> {
    tokio::fs::read(node_home.join("init.json"))
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("node_name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
}

async fn refuse_renaming_home(
    node_home: &std::path::Path,
    requested: &str,
) -> Result<(), BridgeError> {
    if !gents_server::server_host::initialized_home(node_home) {
        return Ok(());
    }
    let initialized = read_initialized_name(node_home).await;
    if name_confirmed_by_home(requested, initialized.as_deref()) {
        return Ok(());
    }
    let requested = requested.trim();
    Err(BridgeError::new(
        BridgeErrorCode::InvalidArgument,
        match initialized {
            Some(name) => format!(
                "This computer already has a local Node named {name}, so {requested} was not created. Go back to continue with {name}."
            ),
            None => format!(
                "The local node home at {} has no readable node name; it was left unchanged.",
                node_home.display()
            ),
        },
    ))
}

fn name_confirmed_by_home(requested: &str, initialized: Option<&str>) -> bool {
    initialized.is_some_and(|name| name.trim() == requested.trim())
}

async fn save_confirmed_preference(
    state: &DesktopAppState,
    node_home: &std::path::Path,
    stored: &StoredManagedServer,
) -> Result<(), BridgeError> {
    if name_confirmed_by_home(
        &stored.node_name,
        read_initialized_name(node_home).await.as_deref(),
    ) {
        let reviewed_for = read_home_identity(node_home)
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

async fn read_home_identity(node_home: &std::path::Path) -> Option<HomeIdentity> {
    let home = tokio::fs::canonicalize(node_home).await.ok()?;
    let bytes = tokio::fs::read(node_home.join("init.json")).await.ok()?;
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
            node_home: home.to_string_lossy().into_owned(),
            node_did: text("node_did")?,
        },
        tool_ceiling: text("tool_ceiling").as_deref().and_then(parse_tool_ceiling),
        tool_root: text("tool_root"),
    })
}

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
                "Review host access for this node home before starting it.",
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
    let home = match state.policy.node_home.as_deref() {
        Some(node_home) => read_home_identity(node_home).await,
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

    #[test]
    fn exit_stops_active_service_without_a_desktop_preference_but_respects_login_enablement() {
        let active = gents_server::native_service::NativeServiceStatus {
            installed: true,
            running: true,
            job_loaded: true,
            enabled: false,
            requires_approval: false,
            background_item: gents_server::native_service::BackgroundItemStatus::Unavailable,
            failed: false,
            detail: None,
        };
        assert!(should_stop_managed_service_on_exit(&active));

        let keep_running = gents_server::native_service::NativeServiceStatus {
            enabled: true,
            ..active.clone()
        };
        assert!(!should_stop_managed_service_on_exit(&keep_running));

        let absent = gents_server::native_service::NativeServiceStatus {
            installed: false,
            running: false,
            job_loaded: false,
            enabled: false,
            requires_approval: false,
            background_item: gents_server::native_service::BackgroundItemStatus::Unavailable,
            failed: false,
            detail: None,
        };
        assert!(!should_stop_managed_service_on_exit(&absent));
    }

    fn write_home(node_home: &Path, did: &str, ceiling: &str, root: Option<&str>) {
        let init = serde_json::json!({
            "home": node_home.display().to_string(),
            "node_name": "Forge",
            "node_did": did,
            "key_path": null,
            "tool_ceiling": ceiling,
            "tool_root": root,
        });
        write(&node_home.join("init.json"), &init.to_string());
    }

    async fn remember_forge_grant(state: &DesktopAppState, node_home: &Path, root: &str) {
        save_confirmed_preference(
            state,
            node_home,
            &StoredManagedServer {
                node_name: "Forge".to_string(),
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

    fn invoke_on_forge_home(cmd: &str) -> (tempfile::TempDir, Vec<u8>, String) {
        use tauri::ipc::InvokeBody;
        use tauri::webview::InvokeRequest;

        let (temp, state) = orchestration_state();
        let node_home = state.policy.node_home.clone().expect("node home");
        let desktop_root = state.policy.desktop_paths.root().to_path_buf();
        write_home(&node_home, "did:key:forge", "MetaOnly", None);
        let before = std::fs::read(node_home.join("init.json")).unwrap();
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
                        "nodeName": "Scout",
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
        let after = std::fs::read(node_home.join("init.json")).unwrap();
        assert_eq!(after, before, "{cmd} rewrote the initialized home");
        (temp, after, error.to_string())
    }

    #[test]
    fn start_naming_another_agent_leaves_the_home_untouched() {
        let (_temp, _, error) = invoke_on_forge_home("desktop_managed_server_start");
        assert!(
            error.contains("already has a local Node named Forge"),
            "{error}"
        );
    }

    #[test]
    fn restart_naming_another_agent_leaves_the_home_untouched() {
        let (_temp, _, error) = invoke_on_forge_home("desktop_managed_server_restart");
        assert!(
            error.contains("already has a local Node named Forge"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_remembered_grant_reconnects_only_the_home_it_was_reviewed_for() {
        let (temp, state) = orchestration_state();
        let node_home = state.policy.node_home.clone().expect("node home");
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let root = root.to_string_lossy().into_owned();
        write_home(&node_home, "did:key:forge", "Readwrite", Some(&root));
        remember_forge_grant(&state, &node_home, &root).await;

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
        let node_home = state.policy.node_home.clone().expect("node home");
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let root = root.to_string_lossy().into_owned();
        write_home(&node_home, "did:key:forge", "Readwrite", Some(&root));
        remember_forge_grant(&state, &node_home, &root).await;

        write_home(&node_home, "did:key:other", "MetaOnly", None);
        let before = std::fs::read(node_home.join("init.json")).unwrap();
        let bound = load_bound_preference(&state).await.unwrap();
        assert_eq!(bound.as_ref().unwrap().tool_ceiling, None);
        let error = start_authority(None, None, bound.as_ref()).unwrap_err();
        assert_eq!(error.code, BridgeErrorCode::InvalidArgument);
        assert_eq!(std::fs::read(node_home.join("init.json")).unwrap(), before);

        std::fs::remove_file(node_home.join("init.json")).unwrap();
        let bound = load_bound_preference(&state).await.unwrap();
        assert!(start_authority(None, None, bound.as_ref()).is_err());
        assert!(!node_home.join("init.json").exists());
    }

    #[tokio::test]
    async fn a_home_whose_authority_changed_needs_a_fresh_review() {
        let (temp, state) = orchestration_state();
        let node_home = state.policy.node_home.clone().expect("node home");
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let root = root.to_string_lossy().into_owned();
        write_home(&node_home, "did:key:forge", "Readwrite", Some(&root));
        remember_forge_grant(&state, &node_home, &root).await;
        let remembered = load_preference(&state).await.unwrap();

        write_home(&node_home, "did:key:forge", "Readonly", Some(&root));
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
        let node_home = state.policy.node_home.clone().expect("node home");
        let init = serde_json :: json ! ({ "home" : node_home . display () . to_string () , "node_name" : "Forge" , "node_did" : "did:key:forge" , "key_path" : null , "tool_ceiling" : "Readwrite" , "tool_root" : null , });
        write(&node_home.join("init.json"), &init.to_string());
        let forge = StoredManagedServer {
            node_name: "Forge".to_string(),
            tool_ceiling: Some(ManagedServerToolCeiling::Readwrite),
            tool_root: None,
            reviewed_for: None,
        };
        save_preference(&state, &forge).await.unwrap();
        gents_server::server_host::ensure_standard_home(
            &crate::tests::support::runtime_handle(),
            gents_server::server_host::ProvisionOptions {
                home: node_home.clone(),
                node_name: "Scout".to_string(),
                tool_ceiling: runtime_tool_ceiling(ManagedServerToolCeiling::Readwrite),
                tool_root: None,
                store_key_custody: gents::store_key::StoreKeyCustodyChoice::File,
            },
        )
        .await
        .unwrap();
        save_confirmed_preference(
            &state,
            &node_home,
            &StoredManagedServer {
                node_name: "Scout".to_string(),
                ..forge.clone()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            read_initialized_name(&node_home).await.as_deref(),
            Some("Forge")
        );
        let stored = load_preference(&state).await.unwrap().expect("preference");
        assert_eq!(stored.node_name, "Forge");
        save_confirmed_preference(&state, &node_home, &forge)
            .await
            .unwrap();
        assert_eq!(
            load_preference(&state).await.unwrap().unwrap().node_name,
            "Forge"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stalled_reset_helper_is_killed_without_retiring_state() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("helper");
        std::fs::write(&script, "#!/bin/sh\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let home = temp.path().join("home");
        write(&home.join("init.json"), "original state");
        let before = std::time::Instant::now();
        let result = launch_reset_helper_with_timeout(
            &script,
            &home,
            temp.path(),
            "confirmation",
            HomeResetDisposition::Archive,
            Duration::from_millis(30),
        );
        assert!(result.is_err());
        assert!(before.elapsed() < Duration::from_secs(10));
        assert_eq!(
            std::fs::read_to_string(home.join("init.json")).unwrap(),
            "original state"
        );
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

    fn dirty_home(temp: &Path) -> (PathBuf, gents_desktop_core::client::DesktopPaths) {
        let home = temp.join(".gents");
        write(&home.join("data/MANIFEST"), "REGOMAN old lineage");
        write(
            &home.join("init.json"),
            r#"{"node_did":"did:key:old","node_name":"local"}"#,
        );
        write(&home.join("keys/local.key"), "identity");
        write(&home.join("runtime.json"), "{}");
        write(&home.join("p2p-secret-key"), "p2p");
        write(
            &home.join("grok-port-home/init.json"),
            r#"{"node_did":"did:key:other"}"#,
        );
        write(&home.join("grok-port-home/data/MANIFEST"), "REGOMAN other");
        let desktop = gents_desktop_core::client::DesktopPaths::from_root(temp.join("desktop"));
        write(
            &desktop.node_data_dir().join("MANIFEST"),
            "REGOMAN old client",
        );
        write(desktop.peer_directory_path(), "{}");
        write(
            desktop.principal_metadata_path(),
            r#"{"did":"did:key:z6MkClient","public_key_bytes":[1,2,3]}"#,
        );
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
            ClientBinding::Unbound,
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
            ClientBinding::Unbound,
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
            ClientBinding::Unbound,
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
    fn starting_fresh_from_a_refused_client_store_retires_its_paired_home_before_relaunch() {
        let temp = tempfile::tempdir().unwrap();
        let temp = std::fs::canonicalize(temp.path()).unwrap();
        let (home, desktop) = dirty_home(&temp);

        let plan = plan_home_reset(
            Some(&home),
            None,
            Some(lineage_refusal(desktop.node_data_dir().to_path_buf())),
            &desktop,
            not_the_user_home(),
            ClientBinding::BoundToHome,
        )
        .unwrap();
        let preview = plan.preview();
        assert_eq!(
            preview.managed_home.as_deref(),
            Some(home.to_str().unwrap())
        );
        assert!(preview
            .planned_paths
            .contains(&home.join("data").to_string_lossy().into_owned()));
        assert!(
            preview.delete_confirmation.is_none(),
            "an unopened paired home may belong to a newer build: archive only"
        );
        plan.check_confirmation(&preview.confirmation, HomeResetDisposition::Archive)
            .unwrap();
        let backup = plan.backup_path("20260928T000000.000Z").unwrap();
        plan.retire(HomeResetDisposition::Archive, Some(&backup))
            .unwrap();

        let entries = gents::home::home_entries(&home, &[]).unwrap();
        assert!(entries.owned.is_empty(), "{:?}", entries.owned);
        assert!(!gents_server::server_host::initialized_home(&home));
        assert!(home.join("grok-port-home/data/MANIFEST").is_file());
        assert!(!desktop.node_data_dir().exists());
        assert!(!desktop.root().join(MANAGED_SERVER_CONFIG).exists());
    }

    #[test]
    fn orphaned_client_state_is_archived_without_touching_any_home() {
        let temp = tempfile::tempdir().unwrap();
        let temp = std::fs::canonicalize(temp.path()).unwrap();
        let (home, desktop) = dirty_home(&temp);
        std::fs::write(desktop.node_data_dir().join("MANIFEST"), "REGOMAN").unwrap();

        assert!(
            plan_home_reset(
                None,
                None,
                None,
                &desktop,
                not_the_user_home(),
                ClientBinding::Unbound
            )
            .is_err(),
            "healthy client state alone is not retired"
        );
        let plan = plan_home_reset(
            None,
            None,
            None,
            &desktop,
            not_the_user_home(),
            ClientBinding::Orphaned,
        )
        .unwrap();
        let preview = plan.preview();
        assert_eq!(preview.managed_home, None);
        assert!(preview.stores.is_empty());
        assert!(preview.delete_confirmation.is_none(), "archive only");
        let reset = plan
            .retire(
                HomeResetDisposition::Archive,
                Some(&plan.backup_path("20260928T000000.000Z").unwrap()),
            )
            .unwrap();
        let backup = PathBuf::from(reset.backup_path.unwrap());
        assert!(backup.join("desktop/node/MANIFEST").is_file());
        assert!(backup.join("desktop").join(MANAGED_SERVER_CONFIG).is_file());
        assert!(!desktop.node_data_dir().exists());
        assert!(home.join("data/MANIFEST").is_file());
        assert!(home.join("init.json").is_file());
    }

    #[test]
    fn a_reviewed_home_that_is_gone_or_reidentified_orphans_the_client_state() {
        let reviewed = ReviewedHome {
            node_home: "/homes/a".into(),
            node_did: "did:key:a".into(),
        };
        let identity = |home: &str, did: &str| HomeIdentity {
            reviewed: ReviewedHome {
                node_home: home.into(),
                node_did: did.into(),
            },
            tool_ceiling: None,
            tool_root: None,
        };
        let a = identity("/homes/a", "did:key:a");
        let b = identity("/homes/b", "did:key:b");
        assert_eq!(
            classify_binding(None, None, None, false),
            ClientBinding::Unbound
        );
        assert_eq!(
            classify_binding(Some(&reviewed), Some(&a), Some(&a), true),
            ClientBinding::BoundToHome
        );
        let reidentified = identity("/homes/a", "did:key:b");
        assert_eq!(
            classify_binding(
                Some(&reviewed),
                Some(&reidentified),
                Some(&reidentified),
                true
            ),
            ClientBinding::Orphaned
        );
        assert_eq!(
            classify_binding(Some(&reviewed), None, None, false),
            ClientBinding::Orphaned
        );
        assert_eq!(
            classify_binding(Some(&reviewed), None, None, true),
            ClientBinding::Unbound,
            "an unreadable init marker is not evidence the node is gone"
        );
        assert_eq!(
            classify_binding(Some(&reviewed), Some(&b), Some(&a), true),
            ClientBinding::Unbound,
            "switching to another home while the reviewed one still exists keeps the client"
        );
        assert_eq!(
            classify_binding(
                Some(&reviewed),
                Some(&identity("/homes/moved", "did:key:a")),
                None,
                false
            ),
            ClientBinding::Unbound,
            "the reviewed identity relocated to the configured home keeps the client"
        );
        assert_eq!(
            classify_binding(Some(&reviewed), Some(&b), None, false),
            ClientBinding::Orphaned,
            "the reviewed home is gone and the configured home is another node"
        );
    }

    #[tokio::test]
    async fn client_start_archives_state_paired_with_a_missing_home() {
        let (temp, state) = orchestration_state();
        let desktop = state.policy.desktop_paths.clone();
        write(&desktop.node_data_dir().join("MANIFEST"), "REGOMAN");
        write(desktop.peer_directory_path(), "{}");
        let stored = serde_json::json!({
            "nodeName": "Mandrake",
            "reviewedFor": {
                "nodeHome": temp.path().join("agent").to_string_lossy(),
                "nodeDid": "did:key:gone"
            }
        });
        write(
            &desktop.root().join(MANAGED_SERVER_CONFIG),
            &stored.to_string(),
        );

        let reset = retire_orphaned_client_state(&state)
            .await
            .unwrap()
            .expect("orphaned state is archived");
        assert!(reset.completed);
        assert!(PathBuf::from(reset.backup_path.unwrap())
            .join("desktop/node/MANIFEST")
            .is_file());
        assert!(!desktop.node_data_dir().exists());
        assert!(!desktop.peer_directory_path().exists());
        assert!(!desktop.root().join(MANAGED_SERVER_CONFIG).exists());
        assert!(
            retire_orphaned_client_state(&state)
                .await
                .unwrap()
                .is_none(),
            "nothing is left to retire"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn orphaned_state_that_cannot_be_archived_fails_without_moving_anything() {
        use std::os::unix::fs::PermissionsExt;
        let (temp, state) = orchestration_state();
        let desktop = state.policy.desktop_paths.clone();
        write(&desktop.node_data_dir().join("MANIFEST"), "REGOMAN");
        let stored = serde_json::json!({
            "nodeName": "Mandrake",
            "reviewedFor": {
                "nodeHome": temp.path().join("agent").to_string_lossy(),
                "nodeDid": "did:key:gone"
            }
        });
        write(
            &desktop.root().join(MANAGED_SERVER_CONFIG),
            &stored.to_string(),
        );
        let parent = desktop.root().parent().unwrap().to_path_buf();
        let original = std::fs::metadata(&parent).unwrap().permissions();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = retire_orphaned_client_state(&state).await;
        std::fs::set_permissions(&parent, original).unwrap();

        assert!(
            result.is_err(),
            "startup must not reopen state it failed to retire"
        );
        assert!(desktop.node_data_dir().join("MANIFEST").is_file());
        assert!(desktop.root().join(MANAGED_SERVER_CONFIG).is_file());
    }

    #[tokio::test]
    async fn client_start_keeps_state_paired_with_its_home() {
        let (temp, state) = orchestration_state();
        let desktop = state.policy.desktop_paths.clone();
        let home = temp.path().join("agent");
        write(
            &home.join("init.json"),
            r#"{"node_did":"did:key:here","node_name":"Mandrake"}"#,
        );
        write(&desktop.node_data_dir().join("MANIFEST"), "REGOMAN");
        let stored = serde_json::json!({
            "nodeName": "Mandrake",
            "reviewedFor": {
                "nodeHome": std::fs::canonicalize(&home).unwrap().to_string_lossy(),
                "nodeDid": "did:key:here"
            }
        });
        write(
            &desktop.root().join(MANAGED_SERVER_CONFIG),
            &stored.to_string(),
        );

        assert!(retire_orphaned_client_state(&state)
            .await
            .unwrap()
            .is_none());
        assert!(desktop.node_data_dir().join("MANIFEST").is_file());
        assert!(desktop.root().join(MANAGED_SERVER_CONFIG).is_file());
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
            ClientBinding::Unbound,
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
            ClientBinding::Unbound,
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
        let preview = plan_home_reset(
            Some(&home),
            refused(),
            None,
            &desktop,
            not_the_user_home(),
            ClientBinding::Unbound,
        )
        .unwrap()
        .preview();

        write(&home.join("plugins/new/manifest.json"), "{}");
        let changed = plan_home_reset(
            Some(&home),
            refused(),
            None,
            &desktop,
            not_the_user_home(),
            ClientBinding::Unbound,
        )
        .unwrap();
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
            ClientBinding::Unbound,
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

    fn record_reset_store_key(
        home: &Path,
        desktop: &gents_desktop_core::client::DesktopPaths,
        record: &gents::store_key::StoreEncryption,
    ) {
        let mut init: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.join("init.json")).unwrap()).unwrap();
        init["store_encryption"] = serde_json::to_value(record).unwrap();
        write(&home.join("init.json"), &init.to_string());
        write(
            desktop.store_encryption_path(),
            &serde_json::to_string(record).unwrap(),
        );
    }

    #[test]
    fn reset_file_store_keys_allow_fresh_setup_and_archives_remain_readable() {
        for disposition in [HomeResetDisposition::Delete, HomeResetDisposition::Archive] {
            let temp = tempfile::tempdir().unwrap();
            let temp = std::fs::canonicalize(temp.path()).unwrap();
            let (home, desktop) = dirty_home(&temp);
            let key_file = gents::store_key::home_key_file(&home);
            std::fs::remove_file(home.join("data/MANIFEST")).unwrap();
            std::fs::remove_file(desktop.node_data_dir().join("MANIFEST")).unwrap();
            let record = gents::store_key::StoreEncryption::prepare(
                gents::store_key::StoreKeyCustodyChoice::File,
                &key_file,
                &home.join("data"),
            )
            .unwrap();
            record_reset_store_key(&home, &desktop, &record);
            record.initialize(&key_file, &home.join("data")).unwrap();
            record
                .initialize(desktop.store_key_path(), desktop.node_data_dir())
                .unwrap();
            write(&home.join("data/MANIFEST"), "REGOMAN old lineage");
            write(
                &desktop.node_data_dir().join("MANIFEST"),
                "REGOMAN old client",
            );
            write(&home.join("keys/other.key"), "other identity");
            let plan = plan_home_reset(
                Some(&home),
                Some(lineage_refusal(home.join("data"))),
                None,
                &desktop,
                not_the_user_home(),
                ClientBinding::Unbound,
            )
            .unwrap();
            assert!(plan
                .preview()
                .delete_paths
                .contains(&key_file.to_string_lossy().into_owned()));
            let backup = plan.backup_path("store-keys").unwrap();
            plan.retire_with_key_cleanup(
                disposition,
                (disposition == HomeResetDisposition::Archive).then_some(backup.as_path()),
                |_, _| panic!("file custody never calls Keychain cleanup"),
            )
            .unwrap();
            if disposition == HomeResetDisposition::Archive {
                record
                    .load(
                        &backup.join("home/keys/store.aes256"),
                        &backup.join("home/data"),
                    )
                    .unwrap();
                record
                    .load(
                        &backup.join("desktop/store.key"),
                        &backup.join("desktop/node"),
                    )
                    .unwrap();
            } else {
                assert_eq!(
                    std::fs::read_to_string(home.join("keys/other.key")).unwrap(),
                    "other identity"
                );
            }
            let fresh = gents::store_key::StoreEncryption::prepare(
                gents::store_key::StoreKeyCustodyChoice::File,
                &key_file,
                &home.join("data"),
            )
            .expect("fresh setup after retirement");
            write(&home.join("init.json"), r#"{"node_name":"fresh"}"#);
            record_reset_store_key(&home, &desktop, &fresh);
            for (key_file, data) in [
                (key_file, home.join("data")),
                (
                    desktop.store_key_path().to_path_buf(),
                    desktop.node_data_dir().to_path_buf(),
                ),
            ] {
                fresh
                    .initialize(&key_file, &data)
                    .expect("fresh setup after retirement");
            }
        }
    }

    #[test]
    fn reset_keychain_custody_keeps_archives_and_retryable_deletion_records() {
        for disposition in [HomeResetDisposition::Delete, HomeResetDisposition::Archive] {
            let temp = tempfile::tempdir().unwrap();
            let temp = std::fs::canonicalize(temp.path()).unwrap();
            let (home, desktop) = dirty_home(&temp);
            let record = gents::store_key::StoreEncryption {
                version: gents::store_key::STORE_ENCRYPTION_VERSION,
                custody: gents::store_key::StoreKeyCustody::MacosKeychain {
                    keychain_label: "test-only-label".into(),
                },
            };
            record_reset_store_key(&home, &desktop, &record);
            let plan = plan_home_reset(
                Some(&home),
                Some(lineage_refusal(home.join("data"))),
                None,
                &desktop,
                not_the_user_home(),
                ClientBinding::Unbound,
            )
            .unwrap();
            let backup = plan.backup_path("keychain").unwrap();
            if disposition == HomeResetDisposition::Archive {
                plan.retire_with_key_cleanup(disposition, Some(&backup), |_, _| {
                    panic!("archive must keep Keychain keys")
                })
                .unwrap();
                assert!(backup.join("home/init.json").exists());
                assert!(backup.join("desktop/store-encryption.json").exists());
                continue;
            }
            assert!(plan
                .retire_with_key_cleanup(disposition, None, |_, _| anyhow::bail!("Keychain locked"))
                .is_err());
            assert!(!home.join("data").exists());
            assert!(!desktop.node_data_dir().exists());
            assert!(home.join("init.json").exists());
            assert!(desktop.store_encryption_path().exists());
            let retry = plan_home_reset(
                Some(&home),
                Some(lineage_refusal(home.join("data"))),
                Some(lineage_refusal(desktop.node_data_dir().to_path_buf())),
                &desktop,
                not_the_user_home(),
                ClientBinding::Unbound,
            )
            .unwrap();
            let mut deleted = Vec::new();
            retry
                .retire_with_key_cleanup(disposition, None, |record, path| {
                    deleted.push((record.clone(), path.to_path_buf()));
                    Ok(())
                })
                .unwrap();
            assert_eq!(deleted.len(), 2);
            assert!(deleted.iter().all(|(deleted, _)| deleted == &record));
            assert!(!home.join("init.json").exists());
            assert!(!desktop.store_encryption_path().exists());
        }
    }

    #[test]
    fn reset_pending_upgrade_retires_its_artifacts_and_recorded_custody_together() {
        use gents::store_key::{upgrade, StoreEncryption, StoreKeyCustodyChoice};
        for choice in [StoreKeyCustodyChoice::File, StoreKeyCustodyChoice::Keychain] {
            for disposition in [HomeResetDisposition::Delete, HomeResetDisposition::Archive] {
                let temp = tempfile::tempdir().unwrap();
                let temp = std::fs::canonicalize(temp.path()).unwrap();
                let (home, desktop) = dirty_home(&temp);
                write(&home.join("keys/other.key"), "keep");
                let stores = [
                    (home.join("data"), gents::store_key::home_key_file(&home)),
                    (
                        desktop.node_data_dir().to_path_buf(),
                        desktop.store_key_path().to_path_buf(),
                    ),
                ];
                for (data, key_file) in &stores {
                    let record =
                        StoreEncryption::prepare(choice, key_file, &upgrade::staging_path(data))
                            .unwrap();
                    upgrade::begin(data, &record).unwrap();
                    if choice == StoreKeyCustodyChoice::File {
                        record
                            .initialize(key_file, &upgrade::key_store_path(data).unwrap())
                            .unwrap();
                    }
                    write(&upgrade::staging_path(data).join("partial-copy"), "staged");
                }
                let plan = plan_home_reset(
                    Some(&home),
                    Some(lineage_refusal(home.join("data"))),
                    None,
                    &desktop,
                    not_the_user_home(),
                    ClientBinding::Unbound,
                )
                .unwrap();
                let preview = plan.preview();
                for (data, _) in &stores {
                    for entry in upgrade::retirement_entries(data).unwrap() {
                        let path = entry.to_string_lossy().into_owned();
                        assert!(preview.planned_paths.contains(&path));
                        assert!(preview.delete_paths.contains(&path));
                        assert!(!preview.retained_paths.contains(&path));
                    }
                }
                let backup = plan.backup_path("pending-upgrade").unwrap();
                let mut deleted_keys = 0;
                plan.retire_with_key_cleanup(
                    disposition,
                    (disposition == HomeResetDisposition::Archive).then_some(backup.as_path()),
                    |_, _| {
                        deleted_keys += 1;
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(
                    deleted_keys,
                    if choice == StoreKeyCustodyChoice::Keychain
                        && disposition == HomeResetDisposition::Delete
                    {
                        2
                    } else {
                        0
                    }
                );
                for (data, _) in &stores {
                    assert!(upgrade::pending_record(data).unwrap().is_none());
                    assert!(!upgrade::staging_path(data).exists());
                }
                if disposition == HomeResetDisposition::Archive {
                    for (group, name) in [("home", "data"), ("desktop", "node")] {
                        let archived = backup.join(group).join(name);
                        let record = upgrade::pending_record(&archived).unwrap().unwrap();
                        assert!(upgrade::staging_path(&archived)
                            .join("partial-copy")
                            .exists());
                        if choice == StoreKeyCustodyChoice::File {
                            let key = if group == "home" {
                                backup.join("home/keys/store.aes256")
                            } else {
                                backup.join("desktop/store.key")
                            };
                            record.load(&key, &archived).unwrap();
                        }
                    }
                } else {
                    assert_eq!(
                        std::fs::read_to_string(home.join("keys/other.key")).unwrap(),
                        "keep"
                    );
                }
            }
        }
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
            ClientBinding::Unbound,
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
            ClientBinding::Unbound,
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
        let preview = plan_home_reset(
            Some(&home),
            refused(),
            None,
            &desktop,
            not_the_user_home(),
            ClientBinding::Unbound,
        )
        .unwrap()
        .preview();

        let lock = gents::home::lock_store(&home, &home.join("data")).unwrap();
        let locked = plan_home_reset(
            Some(&home),
            refused(),
            None,
            &desktop,
            not_the_user_home(),
            ClientBinding::Unbound,
        )
        .unwrap();
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
            ClientBinding::Unbound,
        )
        .unwrap();
        let preview = plan.preview();
        assert!(!preview
            .planned_paths
            .iter()
            .any(|path| path.ends_with("/data")));

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

        for user_home in [home.clone(), home.join("person")] {
            assert_eq!(
                plan_home_reset(
                    Some(&home),
                    refused.clone(),
                    None,
                    &desktop,
                    &user_home,
                    ClientBinding::Unbound
                )
                .unwrap_err()
                .code,
                BridgeErrorCode::InvalidArgument
            );
        }
        let user_home = desktop.root().join("person");
        assert_eq!(
            plan_home_reset(
                Some(&home),
                refused.clone(),
                None,
                &desktop,
                &user_home,
                ClientBinding::Unbound
            )
            .unwrap_err()
            .code,
            BridgeErrorCode::InvalidArgument
        );
        #[cfg(unix)]
        {
            let alias = temp.join("alias");
            std::os::unix::fs::symlink(&temp, &alias).unwrap();
            let aliased = std::fs::canonicalize(alias.join(".gents")).unwrap();
            assert_eq!(
                plan_home_reset(
                    Some(&home),
                    refused.clone(),
                    None,
                    &desktop,
                    &aliased,
                    ClientBinding::Unbound
                )
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
            ClientBinding::Unbound,
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
            plan_home_reset(
                Some(&home),
                None,
                None,
                &desktop,
                not_the_user_home(),
                ClientBinding::Unbound
            )
            .unwrap_err()
            .code,
            BridgeErrorCode::InvalidArgument
        );
        std::fs::remove_file(data.join("MANIFEST")).unwrap();
        std::fs::write(data.join("data.lark"), "legacy").unwrap();
        let plan = plan_home_reset(
            Some(&home),
            None,
            None,
            &desktop,
            not_the_user_home(),
            ClientBinding::Unbound,
        )
        .unwrap();
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
            plan_home_reset(
                Some(&home),
                None,
                None,
                &desktop,
                not_the_user_home(),
                ClientBinding::Unbound
            )
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
                not_the_user_home(),
                ClientBinding::Unbound
            )
            .unwrap_err()
            .code,
            BridgeErrorCode::PathEscapesRoot
        );
    }

    #[test]
    fn status_priority_is_starting_then_failed_then_stopped_then_disabled() {
        let stored = StoredManagedServer {
            node_name: "local".to_string(),
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
                node_name: Some("local".to_string()),
                node_did: Some("did:key:preserved".to_string()),
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
        assert_eq!(external.node_did.as_deref(), Some("did:key:preserved"));
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
                node_name: Some("local".to_string()),
                node_did: Some("did:key:ready".to_string()),
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
                node_name: Some("local".to_string()),
                node_did: Some("did:key:ready".to_string()),
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
    fn ready_runtime_requires_live_status_to_carry_reviewed_authority() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("init.json"),
            r#"{"node_did":"did:key:zReady"}"#,
        )
        .unwrap();
        let authority = EffectiveManagedAuthority {
            tool_ceiling: ManagedServerToolCeiling::Readwrite,
            tool_root: Some(PathBuf::from("/Users/test")),
        };
        let missing_authority = ManagedServerStatus {
            state: ManagedServerState::Running,
            auto_start: false,
            node_name: Some("Mandrake".into()),
            node_did: Some("did:key:zReady".into()),
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
            AppMeta, BootstrapPolicy, BridgeConfig, HomePolicy, ManagedServerPolicy, NodeHomePolicy,
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
                    node_home: NodeHomePolicy::Fixed(temp.path().join("agent")),
                },
                app_meta: AppMeta {
                    app_name: "managed-restart-test".to_string(),
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                },
                snapshot_grants: SnapshotGrants::core_only(),
                managed_server: ManagedServerPolicy::Allowed,
                client_host: false,
                store_key_custody: gents::store_key::StoreKeyCustodyChoice::File,
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
                        "nodeName": "Workshop Agent",
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

    fn ready_status(node_did: &str) -> ManagedServerStatus {
        ManagedServerStatus {
            state: ManagedServerState::External,
            auto_start: false,
            node_name: Some("local".to_string()),
            node_did: Some(node_did.to_string()),
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
        assert_eq!(ready.node_did.as_deref(), Some("did:key:slow"));
        assert_eq!(probes.load(Ordering::SeqCst), 6);
    }

    #[test]
    fn this_homes_runtime_is_ready_only_once_it_reports_ready() {
        let did = "did:key:managed";
        let payload = |lifecycle: serde_json::Value| {
            serde_json::json!({
                "node_did": did,
                "node_name": "Managed",
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
        assert_eq!(status.node_did.as_deref(), Some(did));
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
            serde_json::json!({ "node_did": did, "version": "0.18.2", "status": "ok" }),
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
            owner_node: "did:key:managed".to_string(),
            state: state.to_string(),
            expires_at: expires_at.to_string(),
            activation_error: None,
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
            AppMeta, BootstrapPolicy, BridgeConfig, HomePolicy, ManagedServerPolicy, NodeHomePolicy,
        };
        use crate::snapshot::projection::SnapshotGrants;
        use crate::state::resolve_policy;

        let temp = tempfile::tempdir().expect("temporary bridge roots");
        let policy = resolve_policy(
            &BridgeConfig {
                home: HomePolicy::FixedRoot(temp.path().join("desktop")),
                bootstrap: BootstrapPolicy::LocalRuntimeAllowed {
                    node_home: NodeHomePolicy::Fixed(temp.path().join("agent")),
                },
                app_meta: AppMeta {
                    app_name: "managed-start-test".to_string(),
                    app_version: env!("CARGO_PKG_VERSION").to_string(),
                },
                snapshot_grants: SnapshotGrants::core_only(),
                managed_server: ManagedServerPolicy::Allowed,
                store_key_custody: gents::store_key::StoreKeyCustodyChoice::File,
                client_host: false,
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
        assert_eq!(ready.node_did.as_deref(), Some("did:key:a"));
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
        .expect_err("the start fails");
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
            .expect_err("start fails");
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
        let failure = failure.expect_err("a cancelled start fails");
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
        let failure = failure.expect_err("the superseded start fails");
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
                assert_eq!(ready.node_did.as_deref(), Some("did:key:booted"))
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
            .expect_err("a start cancelled while it held the lock does not launch");
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
        let failure = failure.expect_err("the stopped restart fails");
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
        let failure = failure.expect_err("the restart is cancelled");
        assert!(failure.error.to_string().contains("stopped"));
        assert!(failure.lifecycle.is_none());
        assert!(token.is_cancelled());
        assert!(!state.managed_server.lock().await.starting);
    }

    async fn futures_poll_once<F>(future: F) -> Option<F::Output>
    where
        F: Future + Unpin,
    {
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
        let failure = failure.expect_err("the stopped restart fails");
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
            error.contains("another program that is not a Gents Node"),
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
        .expect_err("a failed install fails the launch");
        assert!(!failure.attempted_start);
        assert_eq!(launch.starts(), 0);

        let cleaned = AtomicBool::new(false);
        let (message, _) = settle_launch_failure(&state, &token, failure, || async {
            cleaned.store(true, Ordering::SeqCst);
            Ok(())
        })
        .await
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
            "node_did": "did:key:other",
            "node_name": "other",
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
            node_did: Some("did:key:other".to_string()),
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
                serde_json::json!({ "node_did": "did:key:other" }),
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
        node_did: &str,
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
                "node_did": node_did,
                gents_protocol::peer_schema::STATUS_REPLICATED_SCHEMA_FIELD: next_release,
                gents_protocol::serve_lifecycle::STATUS_LIFECYCLE_FIELD: "ready",
            })
            .to_string(),
        )
        .await;
        core.add_managed_enrollment_peer_for_test(
            node_did,
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
        let node_did = "did:key:managed-runtime";
        let (core, server) = skewed_managed_runtime(&temp, node_did).await;
        assert!(core.peer_records().await.iter().any(|peer| {
            peer.node_did == node_did
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
                node_name: "Managed".to_string(),
                node_did: node_did.to_string(),
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
        let node_did = "did:key:starting-runtime";
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
                "node_did": node_did,
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
                node_name: "Starting".to_string(),
                node_did: node_did.to_string(),
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
        let node_did = "did:key:managed-runtime";
        let (core, route_a) = skewed_managed_runtime(&temp, node_did).await;
        let target_a = ManagedPairingTarget {
            node_name: "Managed".to_string(),
            node_did: node_did.to_string(),
            graphql: route_a.graphql(),
        };
        let route_b = "http://127.0.0.1:1/api/v0/graphql";
        core.add_managed_enrollment_peer_for_test(node_did, route_b, "/tmp/managed-home", 2)
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
        let node_did = "did:key:managed-runtime";
        let (core, server) = skewed_managed_runtime(&temp, node_did).await;
        let target = ManagedPairingTarget {
            node_name: "Managed".to_string(),
            node_did: node_did.to_string(),
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
            node_did,
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
