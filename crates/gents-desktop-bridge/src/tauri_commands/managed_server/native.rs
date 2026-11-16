use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;
use crate::tauri_commands::service_executable::{
    ensure_installable_location, resolve_service_executable,
};
use gents_service::args::ServiceAction;
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
) -> Result<gents_service::native_service::NativeServiceManager, BridgeError>
where
    R: Runtime,
{
    build_native_service(app, state, false)
}

pub(super) fn launchable_native_service<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
) -> Result<gents_service::native_service::NativeServiceManager, BridgeError>
where
    R: Runtime,
{
    build_native_service(app, state, true)
}

fn build_native_service<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
    install_runtime: bool,
) -> Result<gents_service::native_service::NativeServiceManager, BridgeError>
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
    let mut config = gents_service::native_service::NativeServiceConfig::new(
        home,
        executable.service_path().to_path_buf(),
    )
    .map_err(native_error)?;
    let bundle_id = app.config().identifier.clone();
    if !bundle_id.trim().is_empty() {
        config.associated_bundle_id = Some(bundle_id);
    }
    gents_service::native_service::NativeServiceManager::new(config).map_err(native_error)
}

pub(super) fn native_error(error: anyhow::Error) -> BridgeError {
    let code = if gents_service::native_service::UnsupportedNativeServicePlatform::occurs_in(
        error.as_ref(),
    ) {
        BridgeErrorCode::Unsupported
    } else {
        BridgeErrorCode::Backend
    };
    BridgeError::new(code, format!("{error:#}"))
}

/// Runs one lifecycle action through the service crate's handler, which
/// admits it first. The service manager's own failure comes back unchanged.
pub(super) fn service_action(
    service: &gents_service::native_service::NativeServiceManager,
    action: ServiceAction,
) -> anyhow::Result<()> {
    gents_service::lifecycle::perform(service, &action)
        .map(drop)
        .map_err(Into::into)
}

/// A reinstall that keeps the home's network settings and custom environment.
pub(super) fn install_action() -> ServiceAction {
    ServiceAction::Install {
        network: Default::default(),
        environment_file: None,
    }
}

pub(super) async fn run_native<T, F>(
    service: gents_service::native_service::NativeServiceManager,
    operation: F,
) -> Result<T, BridgeError>
where
    T: Send + 'static,
    F: FnOnce(gents_service::native_service::NativeServiceManager) -> anyhow::Result<T>
        + Send
        + 'static,
{
    tokio::task::spawn_blocking(move || operation(service))
        .await
        .map_err(|error| BridgeError::untyped(format!("native service task failed: {error}")))?
        .map_err(native_error)
}
