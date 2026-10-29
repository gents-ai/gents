use super::authority::{runtime_tool_ceiling, validate_ready_runtime};
use super::ensure_app_not_quitting;
use super::launch::{fail_managed_start, launch_managed_server, LaunchFailure, NativeLaunch};
use super::native::{
    ensure_launchable_here, launchable_native_service, native_service, run_native,
};
use super::pairing::{
    pairing_is_ready, pairing_target, start_managed_runtime_pairing, start_running_managed_pairing,
};
use super::port::{
    ensure_default_port_identity, matching_external_server, observe_port_readiness, PortReadiness,
};
use super::preference::{
    load_bound_preference, refuse_renaming_home, save_confirmed_preference, start_authority,
};
use super::readiness::{wait_for_booting_managed_server, BootOutcome};
use super::start_wait::{begin_start_wait, finish_start_wait, LifecycleGuard};
use super::status::{emit_status, project_external_status, status_from};
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::{current_core, DesktopAppState};
use crate::types::{ManagedServerStartRequest, ManagedServerStatus};
use gents_desktop_core::local_runtime::{
    init_standard_local_runtime, DesktopInitOptions, OperatorIdentityInteraction,
};
use gents_desktop_core::preference::StoredManagedServer;
use gents_protocol::serve_lifecycle::outdated_runtime_message;
use tauri::{AppHandle, Runtime};

pub(super) async fn start_managed_server<'a, R>(
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
    gents::home::ensure_home_identity_can_serve(&node_home).map_err(|error| {
        BridgeError::untyped(gents_protocol::error_chain::ErrorChain(&error).to_string())
    })?;

    let mut carried_wait = None;
    let mut replace_loaded_job = false;
    let (ready, initial_enabled, _lifecycle) =
        match matching_external_server(&node_home, OperatorIdentityInteraction::Allow).await? {
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
                    match wait_for_booting_managed_server(app, state, &node_home, lifecycle).await?
                    {
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
    let mut status = if let Some(external) =
        matching_external_server(&node_home, OperatorIdentityInteraction::Allow).await?
    {
        project_external_status(external, &native)
    } else {
        let managed = state.managed_server.lock().await;
        status_from(&managed, stored.as_ref(), Some(&native), None, None)
    };
    status.pairing_ready = pairing_is_ready(state, status.node_did.as_deref()).await;
    Ok(status)
}
