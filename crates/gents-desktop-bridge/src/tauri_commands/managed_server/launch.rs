use super::native::{launchable_native_service, native_service, run_native};
use super::readiness::{
    refused_kind, refused_runtime_store, wait_for_managed_server, Readiness, RuntimeStillBooting,
    BACKGROUND_APPROVAL_TIMEOUT, MANAGED_SERVER_POLL_INTERVAL,
};
use super::start_wait::{
    ensure_not_cancelled, finish_start_wait, relock_start, wait_unlocked, LifecycleGuard, StartWait,
};
use super::status::emit_status;
use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;
use crate::types::ManagedServerStatus;
use std::future::Future;
use std::path::Path;
use std::time::Duration;
use tauri::{AppHandle, Runtime};

pub(super) trait ManagedLaunch {
    async fn requires_approval(&self) -> Result<bool, BridgeError>;
    async fn await_approval(&self) -> anyhow::Result<()>;
    async fn start(&self) -> Result<Launched, BridgeError>;
    async fn await_ready(&self) -> anyhow::Result<Readiness>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Launched {
    Started,
    ApprovalPending(String),
}

pub(super) struct NativeLaunch<'a, R>
where
    R: Runtime,
{
    pub(super) app: &'a AppHandle<R>,
    pub(super) state: &'a DesktopAppState,
    pub(super) node_home: &'a Path,
    pub(super) enable_at_login: bool,
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

pub(super) struct LaunchFailure<'a> {
    pub(super) error: anyhow::Error,
    pub(super) attempted_start: bool,
    pub(super) lifecycle: Option<LifecycleGuard<'a>>,
}

pub(super) async fn launch_managed_server<'a, L>(
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

pub(super) async fn settle_launch_failure<C, CF>(
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

pub(super) async fn install_then_launch<'a, L>(
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

pub(super) async fn fail_managed_start<R>(
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
            gents::store::local::storage_backend::incompatible_store_kind(
                &gents::store::local::home::default_data_dir(node_home)
            ),
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

pub(super) async fn await_background_approval<O, OF, W, WF>(
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

fn combine_cleanup_error(original: String, owner: &str, cleanup: String) -> String {
    format!("{original}; additionally failed to stop the {owner}: {cleanup}")
}
