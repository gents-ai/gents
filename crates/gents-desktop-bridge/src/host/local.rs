use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use gents::store::local::file_lock::FileLock;
use gents_desktop_core::client::DesktopPaths;
use gents_protocol::client_installation::{Action, Phase, State};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::ClientHost;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

pub use platform::{connect, Connection};

/// Maximum concurrent local attachments admitted by one installation listener.
pub const MAX_ATTACHMENTS: usize = 64;

/// The listener lock is independent of the core's store lock. Discovery never
/// permits a second core to open a store held by another process.
pub struct LocalListener {
    listener: platform::Listener,
    _lock: FileLock,
}

/// Serializes service discovery and process creation. The service holds its
/// separate listener lock after the attaching process releases this guard.
pub async fn startup_lock(paths: &DesktopPaths) -> Result<FileLock> {
    platform::prepare_home(paths).await?;
    let file = platform::lock_file(paths, "client-start.lock")?;
    acquire_startup_lock(file).await
}

async fn acquire_startup_lock(file: std::fs::File) -> Result<FileLock> {
    loop {
        match FileLock::try_exclusive(file.try_clone()?) {
            Ok(lock) => return Ok(lock),
            Err(std::fs::TryLockError::WouldBlock) => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(error) => return Err(error).context("reserving client installation startup"),
        }
    }
}

impl LocalListener {
    pub async fn bind(paths: &DesktopPaths) -> Result<Self> {
        platform::prepare_home(paths).await?;
        let file = platform::lock_file(paths, "client-host.lock")?;
        let lock = FileLock::try_exclusive(file)
            .context("another process owns the client installation listener")?;
        Ok(Self {
            listener: platform::Listener::bind(paths).await?,
            _lock: lock,
        })
    }

    pub async fn serve(self, host: Arc<ClientHost>, stop: CancellationToken) -> Result<()> {
        self.serve_with_idle_timeout(host, stop, None).await
    }

    pub async fn serve_until_idle(
        self,
        host: Arc<ClientHost>,
        stop: CancellationToken,
    ) -> Result<()> {
        self.serve_with_idle_timeout(host, stop, Some(Duration::from_secs(180)))
            .await
    }

    async fn serve_with_idle_timeout(
        mut self,
        host: Arc<ClientHost>,
        stop: CancellationToken,
        idle_timeout: Option<Duration>,
    ) -> Result<()> {
        let mut attachments = JoinSet::new();
        let mut phase = Phase::Accepting;
        let idle = tokio::time::sleep(idle_timeout.unwrap_or(Duration::from_secs(180)));
        tokio::pin!(idle);
        let result: Result<()> = async {
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = &mut idle, if idle_timeout.is_some() && attachments.is_empty() => break,
                    accepted = self.listener.accept() => {
                        let stream = accepted?;
                        if stop.is_cancelled() || attachments.len() >= MAX_ATTACHMENTS { continue; }
                        phase = State { phase, attachments: attachments.len() }
                            .step(Action::Attach).context("client installation is retiring")?.phase;
                        attachments.spawn(host.clone().serve(stream, stop.clone()));
                    }
                    done = attachments.join_next(), if !attachments.is_empty() => {
                        phase = State { phase, attachments: attachments.len() + 1 }
                            .step(Action::Detach).context("client installation attachment count is invalid")?.phase;
                        report_attachment(done);
                        if let Some(timeout) = idle_timeout.filter(|_| attachments.is_empty()) {
                            idle.as_mut().reset(tokio::time::Instant::now() + timeout);
                        }
                    }
                }
            }
            Ok(())
        }
        .await;
        phase = State {
            phase,
            attachments: attachments.len(),
        }
        .step(Action::Retire)
        .context("client installation cannot retire")?
        .phase;
        stop.cancel();
        while let Some(done) = attachments.join_next().await {
            phase = State {
                phase,
                attachments: attachments.len() + 1,
            }
            .step(Action::Detach)
            .context("retiring client attachment count is invalid")?
            .phase;
            report_attachment(Some(done));
        }
        State {
            phase,
            attachments: attachments.len(),
        }
        .step(Action::Release)
        .context("client installation still has attached work")?;
        result
    }
}

fn report_attachment(result: Option<Result<Result<()>, tokio::task::JoinError>>) {
    match result {
        Some(Ok(Err(error))) => tracing::debug!(%error, "client attachment closed"),
        Some(Err(error)) => tracing::error!(%error, "client attachment task failed"),
        _ => {}
    }
}
