use super::native::{native_service, run_native};
use super::port::{
    observe_port_readiness, occupied_port_message, sentence, status_unreadable_error,
    user_status_signer, PortReadiness,
};
use super::start_wait::{
    begin_start_wait, finish_start_wait, relock_start, wait_unlocked, LifecycleGuard, StartWait,
};
use super::status::{emit_status, last_runtime_error, store_refusal};
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;
use crate::types::ManagedServerStatus;
use gents_protocol::serve_lifecycle::outdated_runtime_message;
use gents_service::repair::crash_loop::{restarts_since, CRASH_LOOP_RESTARTS};
use gents_service::repair::reason::blocker_for_exit;
use std::future::Future;
use std::path::Path;
use std::time::Duration;
use tauri::{AppHandle, Runtime};

const MANAGED_SERVER_READY_TIMEOUT: Duration = Duration::from_secs(300);
pub(super) const BACKGROUND_APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);
pub(super) const MANAGED_SERVER_POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub(super) enum Readiness {
    Ready(ManagedServerStatus),
    ApprovalRequired,
    Outdated { message: String, selected: bool },
}

pub(super) enum NativeProgress {
    Loaded,
    Stopped,
    AwaitingApproval,
    Exited(gents_service::native_service::ServiceExit),
    Failed(gents_service::native_service::ServiceExit),
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

pub(super) fn managed_selection_outdated_message() -> &'static str {
    "The selected managed runtime predates this app. Update it with `gents runtime service update --home <node-home>` using a compatible packaged build, or explicitly reinstall it through external administration. Its running service and executable will be preserved."
}

pub(super) fn ensure_selected_restart_compatible(
    selected: bool,
    port: &PortReadiness,
) -> Result<(), BridgeError> {
    if selected && matches!(port, PortReadiness::Outdated { .. }) {
        return Err(BridgeError::new(
            BridgeErrorCode::Unsupported,
            managed_selection_outdated_message(),
        ));
    }
    Ok(())
}

pub(super) async fn wait_for_managed_server<R>(
    app: &AppHandle<R>,
    state: &DesktopAppState,
    node_home: &Path,
) -> Result<Readiness, StartError>
where
    R: Runtime,
{
    let selected = run_native(native_service(app, state)?, |service| {
        service.has_managed_executable()
    })
    .await?;
    let result = await_runtime_readiness(
        MANAGED_SERVER_READY_TIMEOUT,
        MANAGED_SERVER_POLL_INTERVAL,
        || async {
            observe_port_readiness(
                node_home,
                user_status_signer(state, node_home).await.as_ref(),
            )
            .await
        },
        || observe_native_progress(app, state),
        || last_runtime_error(state),
    )
    .await;
    result.map(|readiness| preserve_managed_selection(readiness, selected))
}

pub(super) fn preserve_managed_selection(readiness: Readiness, selected: bool) -> Readiness {
    match readiness {
        Readiness::Outdated { .. } if selected => Readiness::Outdated {
            message: managed_selection_outdated_message().into(),
            selected: true,
        },
        readiness => readiness,
    }
}

pub(super) async fn await_runtime_readiness<P, PF, N, NF, E, EF>(
    timeout: Duration,
    interval: Duration,
    mut probe: P,
    mut native: N,
    mut last_error: E,
) -> Result<Readiness, StartError>
where
    E: FnMut() -> EF,
    EF: Future<Output = Option<String>>,
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
            PortReadiness::StatusUnreadable { cause, .. } => {
                return Err(StartError::Bridge(status_unreadable_error(&cause)))
            }
            PortReadiness::Outdated { version } => {
                return Ok(Readiness::Outdated {
                    message: outdated_runtime_message(version.as_deref()),
                    selected: false,
                });
            }
            PortReadiness::Foreign(foreign) => {
                return Err(anyhow::anyhow!(foreign.message()).into())
            }
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
                ))
                .into())
            }
            NativeProgress::Exited(exit) | NativeProgress::Failed(exit)
                if store_refusal(&exit).is_some() =>
            {
                let error = BridgeError::new(
                    BridgeErrorCode::IncompatibleLocalStore,
                    format!(
                        "the native Gents service refused its store: it {}",
                        exit.reason
                    ),
                );
                return Err(match store_refusal(&exit) {
                    Some(kind) => StartError::Refused {
                        kind: RefusedStoreKind(kind),
                        source: error,
                    },
                    None => StartError::Bridge(error),
                });
            }
            NativeProgress::Failed(exit) => {
                let cause = last_error()
                    .await
                    .or_else(|| exit.code.is_none().then(|| exit.reason.clone()));
                return Err(anyhow::anyhow!(sentence(&blocker_for_exit(&exit, cause))).into());
            }
            NativeProgress::Exited(exit) if exit.clean => {
                return Err(anyhow::anyhow!(
                    "the native Gents service exited normally before it published runtime readiness, so it will not be restarted"
                )
                .into())
            }
            NativeProgress::Exited(exit) => {
                let restarts = restarts_since(&mut first_exit_restarts, exit.restarts);
                if restarts >= CRASH_LOOP_RESTARTS {
                    return Err(explain(sentence(&blocker_for_exit(
                        &exit,
                        last_error().await,
                    )))
                    .into());
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
            return Err(anyhow::anyhow!(
                "the native Gents service is running but did not publish runtime readiness within {} seconds",
                timeout.as_secs()
            )
            .into());
        }
        tokio::time::sleep(interval).await;
    }
}

#[derive(Debug)]
pub(super) struct RuntimeStillBooting {
    pub(super) waited: Duration,
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

/// Why a managed start or adoption did not reach readiness. Its chain renders
/// as the user-visible failure text.
#[derive(Debug, thiserror::Error)]
pub(super) enum StartError {
    #[error(transparent)]
    StillBooting(#[from] RuntimeStillBooting),
    #[error("{kind}")]
    Refused {
        kind: RefusedStoreKind,
        source: BridgeError,
    },
    #[error(transparent)]
    Bridge(#[from] BridgeError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl StartError {
    pub(super) fn message(&self) -> String {
        match self {
            Self::Other(error) => format!("{error:#}"),
            typed => gents_protocol::error_chain::ErrorChain(typed).to_string(),
        }
    }

    pub(super) fn bridge(&self) -> Option<&BridgeError> {
        match self {
            Self::Bridge(typed) | Self::Refused { source: typed, .. } => Some(typed),
            Self::Other(error) => BridgeError::carried_by(error),
            Self::StillBooting(_) => None,
        }
    }
}

pub(super) enum BootOutcome<'a> {
    Ready(ManagedServerStatus, LifecycleGuard<'a>),
    NeedsLaunch(LifecycleGuard<'a>, StartWait),
}

pub(super) async fn adopt_booting_runtime<'a>(
    state: &'a DesktopAppState,
    token: &StartWait,
    lifecycle: LifecycleGuard<'a>,
    readiness: impl Future<Output = Result<Readiness, StartError>>,
) -> Result<BootOutcome<'a>, StartError> {
    drop(lifecycle);
    let readiness = wait_unlocked(token, readiness).await?;
    let lifecycle = relock_start(state, token).await?;
    Ok(match readiness {
        Readiness::Ready(status) => BootOutcome::Ready(status, lifecycle),
        Readiness::ApprovalRequired => BootOutcome::NeedsLaunch(lifecycle, token.clone()),
        Readiness::Outdated { message, .. } => return Err(anyhow::anyhow!(message).into()),
    })
}

pub(super) async fn wait_for_booting_managed_server<'a, R>(
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
        return adopted.map_err(|error| BridgeError::untyped(error.message()));
    }
    let result = settle_adoption(state, &token, node_home, adopted).await;
    emit_status(app, state).await;
    result
}

pub(super) async fn settle_adoption<'a>(
    state: &'a DesktopAppState,
    token: &StartWait,
    node_home: &Path,
    adopted: Result<BootOutcome<'a>, StartError>,
) -> Result<BootOutcome<'a>, BridgeError> {
    finish_start_wait(state, token).await;
    let error = match adopted {
        Ok(outcome) => return Ok(outcome),
        Err(error) => error,
    };
    let message = error.message();
    if matches!(error, StartError::StillBooting(_)) {
        return Err(BridgeError::new(
            BridgeErrorCode::RuntimeStillBooting,
            message,
        ));
    }
    tracing::warn!(target: "gents_desktop::managed_server", error = %message, "managed Gents server did not become ready");
    let refused = refused_kind(&error);
    let typed = match error.bridge() {
        Some(typed) => {
            let mut typed = typed.clone();
            typed.message = message;
            typed
        }
        None => BridgeError::new(BridgeErrorCode::EndpointUnreachable, message),
    };
    let mut managed = state.managed_server.lock().await;
    managed.last_error = Some(typed.message.clone());
    managed.incompatible_store = (typed.code == BridgeErrorCode::IncompatibleLocalStore)
        .then(|| refused_runtime_store(node_home, refused));
    Err(typed)
}

#[derive(Debug, Clone, Copy)]
pub(super) struct RefusedStoreKind(
    gents_protocol::operations::install_repair::IncompatibleStoreKind,
);

impl std::fmt::Display for RefusedStoreKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            kind if kind.made_by_older_version() => {
                f.write_str("the store is from an older Gents version")
            }
            _ => f.write_str("the store is from a different Gents version"),
        }
    }
}

pub(super) fn refused_kind(
    error: &StartError,
) -> Option<gents_protocol::operations::install_repair::IncompatibleStoreKind> {
    match error {
        StartError::Refused { kind, .. } => Some(kind.0),
        _ => None,
    }
}

pub(super) fn refused_runtime_store(
    node_home: &Path,
    reported: Option<gents_protocol::operations::install_repair::IncompatibleStoreKind>,
) -> gents_protocol::operations::install_repair::IncompatibleStore {
    let data_path = gents_store::local::home::default_data_dir(node_home);
    let kind = gents_store::local::storage_backend::incompatible_store_kind(&data_path)
        .ok()
        .flatten()
        .or(reported)
        .unwrap_or(
            gents_protocol::operations::install_repair::IncompatibleStoreKind::UnknownLineage,
        );
    let data_path = match kind {
        gents_protocol::operations::install_repair::IncompatibleStoreKind::InsecureKey => {
            node_home.join("keys")
        }
        _ => data_path,
    };
    gents_protocol::operations::install_repair::IncompatibleStore { kind, data_path }
}
