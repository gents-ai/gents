use super::port::{observe_port_readiness, PortReadiness};
use super::status::emit_status;
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;
use crate::tauri_commands::service_executable::{
    ensure_installable_location, resolve_service_executable,
};
use gents_protocol::serve_lifecycle::outdated_runtime_message;
use tauri::{AppHandle, Runtime};

pub(super) fn ensure_launchable_here<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<(), BridgeError>
where
    R: Runtime,
{
    let executable = resolve_service_executable(app, state.policy.desktop_paths.root())?;
    ensure_installable_location(executable.service_path())
}

pub(super) fn native_service<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<gents_server::native_service::NativeServiceManager, BridgeError>
where
    R: Runtime,
{
    build_native_service(app, state, false)
}

pub(super) fn launchable_native_service<R>(
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

pub(super) fn native_error(error: anyhow::Error) -> BridgeError {
    let code = if gents_protocol::error_chain::find_source::<
        gents_server::native_service::UnsupportedNativeServicePlatform,
    >(error.as_ref())
    .is_some()
    {
        BridgeErrorCode::Unsupported
    } else {
        BridgeErrorCode::Backend
    };
    BridgeError::new(code, format!("{error:#}"))
}

pub(super) async fn run_native<T, F>(
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
