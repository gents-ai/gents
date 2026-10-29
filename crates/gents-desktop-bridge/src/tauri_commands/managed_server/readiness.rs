use super::native::{native_service, run_native};
use super::port::{
    observe_port_readiness_with_identity_interaction, occupied_port_message, PortReadiness,
};
use super::start_wait::{
    begin_start_wait, finish_start_wait, relock_start, wait_unlocked, LifecycleGuard, StartWait,
};
use super::status::{emit_status, restarts_since};
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;
use crate::types::ManagedServerStatus;
use gents_desktop_core::local_runtime::OperatorIdentityInteraction;
use gents_protocol::serve_lifecycle::outdated_runtime_message;
use std::future::Future;
use std::path::Path;
use std::time::Duration;
use tauri::{AppHandle, Runtime};

const MANAGED_SERVER_READY_TIMEOUT: Duration = Duration::from_secs(300);
pub(super) const BACKGROUND_APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);
pub(super) const MANAGED_SERVER_POLL_INTERVAL: Duration = Duration::from_millis(500);
pub(super) const CRASH_LOOP_RESTARTS: u64 = 2;

#[derive(Debug)]
pub(super) enum Readiness {
    Ready(ManagedServerStatus),
    ApprovalRequired,
}

pub(super) enum NativeProgress {
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

pub(super) async fn wait_for_managed_server<R>(
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
        || {
            observe_port_readiness_with_identity_interaction(
                node_home,
                OperatorIdentityInteraction::Allow,
            )
        },
        || observe_native_progress(app, state),
    )
    .await
}

pub(super) async fn await_runtime_readiness<P, PF, N, NF>(
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

pub(super) enum BootOutcome<'a> {
    Ready(ManagedServerStatus, LifecycleGuard<'a>),
    NeedsLaunch(LifecycleGuard<'a>, StartWait),
}

pub(super) async fn adopt_booting_runtime<'a>(
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
        return adopted.map_err(|error| BridgeError::untyped(format!("{error:#}")));
    }
    let result = settle_adoption(state, &token, node_home, adopted).await;
    emit_status(app, state).await;
    result
}

pub(super) async fn settle_adoption<'a>(
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
struct RefusedStoreKind(gents::storage_backend::IncompatibleStoreKind);

impl std::fmt::Display for RefusedStoreKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            kind if kind.is_older() => f.write_str("the store is from an older Gents version"),
            _ => f.write_str("the store is from a different Gents version"),
        }
    }
}

pub(super) fn refused_kind(
    error: &anyhow::Error,
) -> Option<gents::storage_backend::IncompatibleStoreKind> {
    error
        .downcast_ref::<RefusedStoreKind>()
        .map(|refused| refused.0)
}

pub(super) fn refused_runtime_store(
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
