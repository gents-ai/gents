use super::authority::suggested_tool_root;
use super::ensure_allowed;
use super::native::{native_service, run_native};
use super::pairing::pairing_is_ready;
use super::port::{
    observe_port_readiness, occupied_port_message, sentence, status_unreadable_error,
    user_status_signer, PortReadiness,
};
use super::preference::load_bound_preference;
use super::readiness::{managed_selection_outdated_message, refused_runtime_store};
use crate::contract::MANAGED_SERVER_UPDATED_EVENT;
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;
use crate::types::{ManagedServerState, ManagedServerStatus};
use gents_desktop_core::preference::StoredManagedServer;
use gents_protocol::operations::install_repair::IncompatibleStoreKind;
use gents_protocol::serve_lifecycle::outdated_runtime_message;
use gents_service::repair::crash_loop;
use gents_service::repair::crash_loop::JobSample;
use gents_service::repair::reason::{blocker_for_exit, read_last_error};
use tauri::{AppHandle, Emitter, Runtime};

/// A store refusal the launch-time repair does not own: a missing store key
/// is set aside by the repair instead of shown as an incompatible home.
pub(super) fn store_refusal(
    exit: &gents_service::native_service::ServiceExit,
) -> Option<IncompatibleStoreKind> {
    exit.refused_store()
        .filter(|kind| *kind != IncompatibleStoreKind::MissingStoreKey)
}

/// The last error the background Node logged: the shared log first, then the
/// log an older desktop install wrote. No user home, no read.
pub(super) async fn last_runtime_error(state: &DesktopAppState) -> Option<String> {
    last_error_in(dirs::home_dir()?, state.policy.desktop_paths.root()).await
}

pub(super) async fn last_error_in(
    user_home: std::path::PathBuf,
    desktop_root: &std::path::Path,
) -> Option<String> {
    let old_desktop_log = crate::logging::runtime_error_log(desktop_root);
    read_last_error(&user_home, Some(&old_desktop_log)).await
}

/// The node home once its node exited because the home was deleted, or once
/// its service stays defined with no `init.json` left in the home (no node
/// starts without it): the exit is read only between respawns, so a read that
/// lands during one, or during a start the desktop retries, would miss it.
/// Kept across later exits, which fail otherwise when a respawn finds the
/// directory recreated mid-delete, until a new setup writes `init.json`.
pub(super) async fn deleted_home(
    kept: Option<std::path::PathBuf>,
    exit: Option<&gents_service::native_service::ServiceExit>,
    service_defined: bool,
    node_home: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let init = gents_store::local::home::init_config_path(node_home);
    if tokio::fs::try_exists(init).await.unwrap_or(true) {
        return None;
    }
    let gone = kept.is_some()
        || service_defined
        || exit.and_then(gents_service::native_service::ServiceExit::refusal)
            == Some(gents_service::native_service::ServiceRefusal::HomeGone);
    gone.then(|| node_home.to_path_buf())
}

/// Whether `now` is an exit whose log tail has not been read yet.
pub(super) fn exit_is_new(
    seen: Option<&gents_service::native_service::ServiceExit>,
    now: Option<&gents_service::native_service::ServiceExit>,
) -> bool {
    now.is_some() && seen != now
}

pub(super) async fn observe_managed_server_status<R>(
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
        Some(node_home) => {
            observe_port_readiness(
                node_home,
                user_status_signer(state, node_home).await.as_ref(),
            )
            .await?
        }
        None => PortReadiness::NotListening,
    };
    let (seen, kept_deleted_home) = {
        let managed = state.managed_server.lock().await;
        (managed.last_exit.clone(), managed.deleted_home.clone())
    };
    let last_exit_error = if exit_is_new(seen.as_ref(), last_exit.as_ref()) {
        Some(last_runtime_error(state).await)
    } else {
        None
    };
    let deleted = match state.policy.node_home.as_deref() {
        Some(node_home) => {
            deleted_home(
                kept_deleted_home,
                last_exit.as_ref(),
                native.job_loaded,
                node_home,
            )
            .await
        }
        None => None,
    };
    let mut managed = state.managed_server.lock().await;
    if let Some(error) = last_exit_error {
        managed.last_exit_error = error;
    }
    managed.last_exit = last_exit.clone();
    managed.deleted_home = deleted;
    if native.job_loaded
        || native.background_item == gents_service::native_service::BackgroundItemStatus::Enabled
    {
        managed.approval_refused = false;
    }
    if let (Some(exit), Some(node_home)) = (last_exit.as_ref(), state.policy.node_home.as_deref()) {
        if let Some(kind) = store_refusal(exit).filter(|_| !managed.starting) {
            managed.incompatible_store = Some(refused_runtime_store(node_home, Some(kind)));
        }
    }
    let crash_loop =
        managed
            .crash_loop
            .observe(job_sample(&port, native.job_loaded, last_exit.as_ref()));
    if crash_loop.is_none() && last_exit.is_none() {
        managed.last_exit_error = None;
    }
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
        PortReadiness::StatusUnreadable { public, cause } => {
            let unreadable = status_unreadable_error(&cause);
            status = project_external_status(public, &native);
            status.error = Some(unreadable.message);
            status.error_code = Some(unreadable.code);
        }
        PortReadiness::Outdated { version } => {
            let selected = run_native(native_service(app, state)?, |service| {
                service.has_managed_executable()
            })
            .await?;
            if selected {
                status.error = Some(managed_selection_outdated_message().into());
                status.error_code = Some(BridgeErrorCode::Unsupported);
            } else if !native.job_loaded && !starting {
                status.state = ManagedServerState::Failed;
                status.error = Some(outdated_runtime_message(version.as_deref()));
            }
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
        PortReadiness::Occupied(_) | PortReadiness::NotListening => {}
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

pub(super) fn project_external_status(
    mut external: ManagedServerStatus,
    native: &gents_service::native_service::NativeServiceStatus,
) -> ManagedServerStatus {
    external.auto_start = native.enabled;
    external.approval_required = native.requires_approval;
    if native.job_loaded {
        external.state = ManagedServerState::Running;
    }
    external
}

pub(super) fn project_foreign_port(
    status: &mut ManagedServerStatus,
    message: &str,
    starting: bool,
) {
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

pub(super) fn job_sample<'a>(
    port: &PortReadiness,
    job_loaded: bool,
    last_exit: Option<&'a gents_service::native_service::ServiceExit>,
) -> JobSample<'a> {
    crash_loop::job_sample(
        matches!(
            port,
            PortReadiness::Ready(_) | PortReadiness::StatusUnreadable { .. }
        ),
        job_loaded,
        last_exit,
    )
}

pub(super) fn status_from(
    managed: &crate::state::ManagedServerState,
    stored: Option<&StoredManagedServer>,
    native: Option<&gents_service::native_service::NativeServiceStatus>,
    last_exit: Option<&gents_service::native_service::ServiceExit>,
    crash_loop: Option<(&gents_service::native_service::ServiceExit, u64)>,
) -> ManagedServerStatus {
    let approval_required =
        native.is_some_and(|status| status.approval_pending(managed.approval_refused));
    let exited_cleanly = last_exit.is_some_and(|exit| exit.clean);
    let settled = !managed.starting && !approval_required;
    let refused_exit = last_exit.is_some_and(|exit| settled && store_refusal(exit).is_some());
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
            .map(|(exit, _)| sentence(&blocker_for_exit(exit, managed.last_exit_error.clone())))
            .or_else(|| {
                last_exit
                    .filter(|exit| {
                        !exit.clean
                            && !managed.starting
                            && native.is_some_and(|status| status.failed)
                    })
                    .map(|exit| {
                        let cause = managed
                            .last_exit_error
                            .clone()
                            .or_else(|| exit.code.is_none().then(|| exit.reason.clone()));
                        sentence(&blocker_for_exit(exit, cause))
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
        deleted_home: managed
            .deleted_home
            .as_ref()
            .map(|home| home.display().to_string()),
    }
}

pub(super) async fn emit_status<R>(app: &AppHandle<R>, state: &DesktopAppState)
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
