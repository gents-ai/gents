use super::authority::suggested_tool_root;
use super::ensure_allowed;
use super::native::{native_service, run_native};
use super::pairing::pairing_is_ready;
use super::port::{observe_port_readiness, occupied_port_message, PortReadiness};
use super::preference::load_bound_preference;
use super::readiness::{refused_runtime_store, CRASH_LOOP_RESTARTS};
use crate::contract::MANAGED_SERVER_UPDATED_EVENT;
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;
use crate::types::{ManagedServerState, ManagedServerStatus};
use gents_desktop_core::preference::StoredManagedServer;
use gents_protocol::serve_lifecycle::outdated_runtime_message;
use tauri::{AppHandle, Emitter, Runtime};

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

pub(super) fn project_external_status(
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
    last_exit: Option<&'a gents_server::native_service::ServiceExit>,
) -> JobSample<'a> {
    match (port, last_exit) {
        (PortReadiness::Ready(_), _) => JobSample::Ready,
        _ if !job_loaded => JobSample::Unloaded,
        (_, Some(exit)) => JobSample::Exited(exit),
        (_, None) => JobSample::Running,
    }
}

pub(super) fn restarts_since(baseline: &mut Option<u64>, counter: u64) -> u64 {
    let first = baseline
        .filter(|first| *first <= counter)
        .unwrap_or(counter);
    *baseline = Some(first);
    counter - first
}

#[derive(Debug, Default)]
pub struct CrashLoopWatch {
    pub(super) baseline: Option<u64>,
    looping: Option<(gents_server::native_service::ServiceExit, u64)>,
}

pub(super) enum JobSample<'a> {
    Unloaded,
    Ready,
    Running,
    Exited(&'a gents_server::native_service::ServiceExit),
}

impl CrashLoopWatch {
    pub(super) fn observe(
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

pub(super) fn status_from(
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
