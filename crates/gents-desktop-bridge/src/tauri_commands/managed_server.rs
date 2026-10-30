mod authority;
mod launch;
pub(crate) mod native;
pub(super) mod pairing;
mod port;
mod preference;
mod readiness;
pub(super) mod reset;
mod start;
pub(crate) mod start_wait;
pub(crate) mod status;

use crate::config::ManagedServerPolicy;
use crate::contract::MANAGED_SERVER_UPDATED_EVENT;
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::{current_core, DesktopAppState};
use crate::types::{
    HomeResetDisposition, ManagedServerResetRequest, ManagedServerResetResult,
    ManagedServerRestartRequest, ManagedServerRootValidation, ManagedServerRootValidationRequest,
    ManagedServerStartRequest, ManagedServerStatus,
};
use authority::{
    runtime_tool_ceiling, validate_ready_runtime, validate_tool_root, EffectiveManagedAuthority,
};
use gents_desktop_core::local_runtime::OperatorIdentityInteraction;
use gents_desktop_core::preference::StoredManagedServer;
use launch::{fail_managed_start, install_then_launch, LaunchFailure, NativeLaunch};
use native::{
    ensure_launchable_here, launchable_native_service, native_error, native_service, run_native,
};
use pairing::{drain_managed_runtime_pairing, pairing_is_ready, start_running_managed_pairing};
use port::{
    ensure_default_port_identity, ensure_native_owns_running_endpoint, matching_external_server,
    observe_port_readiness, PortReadiness,
};
use preference::{load_bound_preference, refuse_renaming_home, save_confirmed_preference};
use reset::{plan_state_reset, retire_incompatible_home};
use start::start_managed_server;
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
    let mut status = matching_external_server(&node_home, OperatorIdentityInteraction::Allow)
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

#[cfg(test)]
mod tests;
