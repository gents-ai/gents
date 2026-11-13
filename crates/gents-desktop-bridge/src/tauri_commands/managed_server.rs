mod authority;
mod launch;
pub(crate) mod native;
pub(super) mod pairing;
mod port;
mod preference;
mod readiness;
pub(super) mod repair;
pub(super) mod reset;
mod start;
pub(crate) mod start_wait;
pub(crate) mod status;

use crate::config::ManagedServerPolicy;
use crate::contract::MANAGED_SERVER_UPDATED_EVENT;
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::{current_core, DesktopAppState};
use crate::types::{
    AgentWorkspaceRootSelectRequest, HomeResetDisposition, ManagedServerRepairResult,
    ManagedServerResetRequest, ManagedServerResetResult, ManagedServerRestartRequest,
    ManagedServerRootValidation, ManagedServerRootValidationRequest, ManagedServerStartRequest,
    ManagedServerStatus,
};
use authority::{
    runtime_tool_ceiling, validate_ready_runtime, validate_tool_root, EffectiveManagedAuthority,
};
use gents_desktop_core::preference::StoredManagedServer;
use gents_desktop_core::workspace_root::select_agent_workspace_root;
use gents_protocol::operations::service::ServiceLifecycleAction;
use gents_protocol::operations::workspace_root::AgentWorkspaceRoot;
use gents_service::args::ServiceAction;
use launch::{fail_managed_start, install_then_launch, LaunchFailure, NativeLaunch};
use native::{
    ensure_launchable_here, launchable_native_service, native_error, native_service, run_native,
};
use native::{install_action, service_action};
use pairing::{drain_managed_runtime_pairing, pairing_is_ready, start_running_managed_pairing};
use port::{
    ensure_default_port_identity, ensure_native_owns_running_endpoint, matching_external_server,
    observe_port_readiness, user_status_signer, PortReadiness,
};
use preference::{load_bound_preference, refuse_renaming_home, save_confirmed_preference};
use readiness::ensure_selected_restart_compatible;
use reset::{plan_state_reset, retire_incompatible_home};
use start::{reconnect_started_runtime, start_managed_server};
use start_wait::{
    begin_start_wait, cancel_start_wait, ensure_no_start_waiting, finish_start_wait,
    lock_lifecycle_superseding_start,
};
use status::{
    emit_status, managed_server_status_for, observe_managed_server_status, project_external_status,
};
use std::future::Future;
use std::path::Path;
use tauri::{AppHandle, Emitter, Runtime, State};

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

/// Publishes the chosen folder for an agent through the operator's signed
/// access and retargets that agent's host root. The runtime still has to be
/// restarted with the reviewed ceiling afterwards: serve admits only published
/// roots, so this runs first while the runtime answers.
#[tauri::command]
pub async fn desktop_agent_workspace_root_select(
    request: AgentWorkspaceRootSelectRequest,
    state: State<'_, DesktopAppState>,
) -> Result<AgentWorkspaceRoot, BridgeError> {
    ensure_allowed(&state)?;
    let canonical = validate_tool_root(Path::new(request.path.trim()))?;
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
    let access = core
        .operator_access(&request.node_did)
        .map_err(BridgeError::classify_transport_error)?;
    select_agent_workspace_root(
        &access,
        &request.node_did,
        &request.agent_did,
        &canonical.to_string_lossy(),
    )
    .await
    .map_err(BridgeError::classify_transport_error)
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

#[tauri::command]
pub async fn desktop_managed_server_enroll(
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError> {
    ensure_allowed(&state)?;
    let _lifecycle = state.managed_server_lifecycle.lock().await;
    let core = current_core(&state)
        .ok_or_else(|| BridgeError::untyped("desktop client is not running"))?;
    pairing::enroll_managed_runtime(&state, core)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))
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

#[tauri::command]
pub async fn desktop_managed_server_repair<R>(
    app: AppHandle<R>,
    state: State<'_, DesktopAppState>,
) -> Result<ManagedServerRepairResult, BridgeError>
where
    R: Runtime,
{
    repair::repair_managed_server(&app, &state).await
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
        let _ = opened.send(gents_service::native_service::open_background_approval_settings());
    })
    .map_err(|error| BridgeError::untyped(format!("opening Login Items settings: {error}")))?;
    result
        .await
        .map_err(|error| BridgeError::untyped(format!("opening Login Items settings: {error}")))?
        .map_err(native_error)
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
        service_action(
            &service,
            if enabled {
                ServiceAction::Enable
            } else {
                ServiceAction::Disable
            },
        )
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
        Some(node_home) => {
            observe_port_readiness(
                node_home,
                user_status_signer(state, node_home).await.as_ref(),
            )
            .await?
        }
        None => PortReadiness::NotListening,
    };
    let native = run_native(native_service(app, state)?, |service| service.status()).await?;
    ensure_native_owns_running_endpoint(&port, native.job_loaded)?;
    drain_managed_runtime_pairing(state).await;
    if let Err(error) = run_native(native_service(app, state)?, move |service| {
        service_action(
            &service,
            ServiceAction::Stop {
                keep_enabled: !disable_auto_start,
            },
        )
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
    native: &gents_service::native_service::NativeServiceStatus,
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

#[tauri::command]
pub async fn desktop_managed_server_restart<R>(
    app: AppHandle<R>,
    request: ManagedServerRestartRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ManagedServerStatus, BridgeError>
where
    R: Runtime,
{
    restart_managed_server(&app, request, &state, || async { Ok(()) }).await
}

/// Turns on the Engineer's core packs named in `request`, installing from
/// the release one the home lacks. A core pack is enabled through the home's
/// own store, as `init --engineer` installs it, and a running node holds that
/// store, so the node is restarted around it: stopped, the packs enabled,
/// started again. A pack that fails is reported and never keeps the node
/// stopped.
#[tauri::command]
pub async fn desktop_default_packs_approve<R>(
    app: AppHandle<R>,
    request: super::packs::DefaultPacksApproveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<crate::types::DefaultPacksApprovalView, BridgeError>
where
    R: Runtime,
{
    ensure_allowed(&state)?;
    let home = super::packs::home(&state)?;
    let stored = load_bound_preference(&state).await?;
    let restart = stored
        .and_then(|stored| {
            Some(ManagedServerRestartRequest {
                node_name: stored.node_name,
                tool_ceiling: stored.tool_ceiling?,
                tool_root: stored.tool_root,
            })
        })
        .ok_or_else(|| {
            BridgeError::new(
                BridgeErrorCode::Unsupported,
                "starter packs install into the local Node this desktop runs; start it first",
            )
        })?;
    let enabling: Vec<_> = request
        .packs
        .into_iter()
        .map(|approval| approval.pack)
        .collect();
    let outcome = std::sync::Arc::new(tokio::sync::Mutex::new(None));
    let restarted = restart_managed_server(&app, restart, &state, || {
        let (home, outcome) = (home.clone(), outcome.clone());
        async move {
            *outcome.lock().await = Some(
                super::packs::run::<serde_json::Value, _, _>(move || {
                    gents_server::packs::enable_core_packs(home, enabling)
                })
                .await,
            );
            Ok(())
        }
    })
    .await;
    let report = outcome.lock().await.take();
    if let Err(mut error) = restarted {
        // The packs were enabled, or failed, before the node did not come
        // back: the restart error carries what happened to them.
        if let Some(Ok(report)) = &report {
            error.message = format!("{} {}", error.message, installed_before_restart(report));
        }
        return Err(error);
    }
    let report = report
        .ok_or_else(|| BridgeError::untyped("the node restarted without enabling the packs"))??;
    let failed = serde_json::from_value(report["failed"].clone())
        .map_err(|error| BridgeError::untyped(format!("invalid pack report: {error}")))?;
    let listed: crate::types::DefaultPacksView =
        super::packs::run(move || gents_server::packs::core_packs(home)).await?;
    Ok(crate::types::DefaultPacksApprovalView {
        packs: listed.packs,
        failed,
    })
}

/// What enabling did to the packs before its node restart failed. A failed
/// pack the report marks `installed` has a home install record and nothing
/// more is known of it: its own enable may have stopped partway, or it may
/// have been installed before. So it is said to be installed but not
/// finished, with what finishes it.
fn installed_before_restart(report: &serde_json::Value) -> String {
    let names = |packs: Vec<&serde_json::Value>| {
        packs
            .into_iter()
            .filter_map(|pack| pack["pack"].as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let listed = |key: &str| report[key].as_array().into_iter().flatten();
    let (unfinished, absent): (Vec<_>, Vec<_>) =
        listed("failed").partition(|pack| pack["installed"] == true);
    let (installed, unfinished, absent) = (
        names(listed("enabled").collect()),
        names(unfinished),
        names(absent),
    );
    let mut clauses = Vec::new();
    if !installed.is_empty() {
        clauses.push(format!("these starter packs were turned on: {installed}."));
    }
    if !unfinished.is_empty() {
        clauses.push(format!(
            "these are installed but not finished: {unfinished}. Turn them on again once the node starts."
        ));
    }
    if clauses.is_empty() {
        clauses.push("no starter pack was turned on.".to_owned());
    }
    if !absent.is_empty() {
        clauses.push(format!("these were not turned on: {absent}."));
    }
    let mut message = "Before the local node failed to start again,".to_owned();
    for (index, clause) in clauses.iter().enumerate() {
        message.push(' ');
        let mut letters = clause.chars();
        match letters.next().filter(|_| index > 0) {
            Some(first) => {
                message.extend(first.to_uppercase());
                message.push_str(letters.as_str());
            }
            None => message.push_str(clause),
        }
    }
    message
}

async fn restart_managed_server<R, P, WhileStopped>(
    app: &AppHandle<R>,
    request: ManagedServerRestartRequest,
    state: &DesktopAppState,
    while_stopped: P,
) -> Result<ManagedServerStatus, BridgeError>
where
    R: Runtime,
    P: FnOnce() -> WhileStopped,
    WhileStopped: Future<Output = Result<(), BridgeError>>,
{
    ensure_allowed(state)?;
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
    refuse_renaming_home(
        &node_home,
        &request.node_name,
        reset::unfinished_local_setup(state, Some(&node_home)).await,
    )
    .await?;
    let lifecycle = lock_lifecycle_superseding_start(state).await;
    ensure_app_not_quitting(state)?;
    let managed_selection = run_native(native_service(app, state)?, |service| {
        gents_service::lifecycle::admit(&service, ServiceLifecycleAction::Restart)?;
        service.has_managed_executable()
    })
    .await?;
    if managed_selection {
        // A package older than the selected runtime must not deserialize and
        // rewrite its init configuration, discarding fields it cannot know.
        let bytes = tokio::fs::read(node_home.join("init.json"))
            .await
            .map_err(|error| BridgeError::untyped(error.to_string()))?;
        let init: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|error| BridgeError::untyped(error.to_string()))?;
        let ceiling = serde_json::to_value(runtime_tool_ceiling(authority.tool_ceiling))
            .map_err(|error| BridgeError::untyped(error.to_string()))?;
        let root = authority
            .tool_root
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        if init["tool_ceiling"] != ceiling || init["tool_root"].as_str() != root.as_deref() {
            return Err(BridgeError::untyped("This runtime was selected by a managed update. Restart preserves its existing authority; use external administration to change its configuration."));
        }
    }
    ensure_launchable_here(app, state)?;
    let port = observe_port_readiness(
        &node_home,
        user_status_signer(state, &node_home).await.as_ref(),
    )
    .await?;
    ensure_selected_restart_compatible(managed_selection, &port)?;
    let previous_did = match &port {
        PortReadiness::Ready(status) | PortReadiness::StatusUnreadable { public: status, .. } => {
            status.node_did.clone()
        }
        PortReadiness::Foreign(_)
        | PortReadiness::Booting
        | PortReadiness::Outdated { .. }
        | PortReadiness::Occupied(_)
        | PortReadiness::NotListening => None,
    };
    let (tool_ceiling, tool_root) = authority.stored();
    let service_status =
        match run_native(native_service(app, state)?, |service| service.status()).await {
            Ok(status) => status,
            Err(error) => {
                state.managed_server.lock().await.last_error = Some(error.message.clone());
                emit_status(app, state).await;
                return Err(error);
            }
        };
    ensure_native_owns_running_endpoint(&port, service_status.job_loaded)?;
    drain_managed_runtime_pairing(state).await;
    let was_enabled = service_status.enabled;
    let provision = gents_server::server_host::ProvisionOptions {
        home: node_home.clone(),
        node_name: request.node_name.clone(),
        tool_ceiling: runtime_tool_ceiling(tool_ceiling),
        tool_root: authority.tool_root.clone(),
        store_key_custody: state.policy.store_key_custody,
        default_packs: true,
    };
    if let Err(error) = stop_before_reprovision(
        || async {
            run_native(native_service(app, state)?, |service| {
                service_action(&service, ServiceAction::Stop { keep_enabled: true })
            })
            .await
        },
        || async {
            if !managed_selection {
                gents_server::server_host::ensure_standard_home(
                    tauri::async_runtime::handle().inner(),
                    provision,
                )
                .await
                .map_err(|error| BridgeError::untyped(format!("{error:#}")))?;
            }
            while_stopped().await
        },
        || async {
            save_confirmed_preference(
                state,
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
        emit_status(app, state).await;
        return Err(error);
    }
    if let (Some(core), Some(node_did)) = (current_core(state), previous_did.as_deref()) {
        if let Err(error) = core.mark_managed_runtime_restarting(node_did).await {
            let message = format!(
                "The node was stopped, but desktop restart bookkeeping failed: {error:#}. Retry Restart Node."
            );
            state.managed_server.lock().await.last_error = Some(message.clone());
            emit_status(app, state).await;
            return Err(BridgeError::untyped(message));
        }
    }
    let token = begin_start_wait(state).await;
    emit_status(app, state).await;
    let launch = NativeLaunch {
        app,
        state,
        node_home: &node_home,
        enable_at_login: was_enabled,
    };
    let install = async {
        if managed_selection {
            return Ok(());
        }
        ensure_default_port_identity(
            &node_home,
            user_status_signer(state, &node_home).await.as_ref(),
        )
        .await
        .map_err(|mut error| {
            error.message = format!("Your node was stopped; {}", error.message);
            error
        })?;
        run_native(launchable_native_service(app, state)?, |service| {
            service_action(&service, install_action())
        })
        .await
    };
    let launched = match install_then_launch(state, &token, lifecycle, install, &launch).await {
        Ok((ready, lifecycle)) => match validate_ready_runtime(&ready, &authority, &node_home) {
            Ok(()) => Ok(lifecycle),
            Err(error) => Err(LaunchFailure {
                error,
                attempted_start: true,
                preserve_runtime: false,
                lifecycle: Some(lifecycle),
            }),
        },
        Err(failure) => Err(failure),
    };
    let _lifecycle = match launched {
        Ok(lifecycle) => {
            finish_start_wait(state, &token).await;
            state.managed_server.lock().await.incompatible_store = None;
            lifecycle
        }
        Err(failure) => {
            return Err(
                fail_managed_start(app, state, &token, &node_home, was_enabled, failure).await,
            )
        }
    };
    emit_status(app, state).await;
    if let Some(core) = current_core(state) {
        start_running_managed_pairing(state, core).await;
    }
    let native = run_native(native_service(app, state)?, |service| service.status()).await?;
    let mut status = matching_external_server(
        &node_home,
        user_status_signer(state, &node_home).await.as_ref(),
    )
    .await?
    .map(|external| project_external_status(external, &native))
    .ok_or_else(|| BridgeError::untyped("native service did not become ready"))?;
    status.pairing_ready = pairing_is_ready(state, status.node_did.as_deref()).await;
    reconnect_started_runtime(state, &status).await;
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

/// The node this desktop co-hosts and the home whose operator identity
/// administers it; `None` unless that runtime is ready, serves `node_did`
/// and matches the home's identity.
pub(in crate::tauri_commands) async fn co_hosted_runtime(
    state: &DesktopAppState,
    node_did: &str,
) -> Result<
    Option<(
        std::path::PathBuf,
        crate::local_pairing::ManagedPairingTarget,
    )>,
    BridgeError,
> {
    let Some(node_home) = state.policy.node_home.clone() else {
        return Ok(None);
    };
    let target = matching_external_server(
        &node_home,
        user_status_signer(state, &node_home).await.as_ref(),
    )
    .await?
    .as_ref()
    .and_then(pairing::pairing_target);
    Ok(target
        .filter(|target| target.node_did == node_did.trim())
        .map(|target| (node_home, target)))
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

#[cfg(test)]
mod tests;
