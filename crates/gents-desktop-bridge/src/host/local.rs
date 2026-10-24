use std::sync::Arc;
use std::time::Duration;

use gents::file_lock::FileLock;
use gents_desktop_core::client::DesktopPaths;
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

pub use platform::connect;

#[derive(Debug, thiserror::Error)]
pub enum LocalError {
    #[error(transparent)]
    Platform(#[from] platform::PlatformError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("reserving client installation startup")]
    ReserveStartup(#[source] std::fs::TryLockError),
    #[error("another process owns the client installation listener")]
    ListenerOwned(#[source] std::fs::TryLockError),
}

/// The listener lock is independent of the core's store lock. Discovery never
/// permits a second core to open a store held by another process.
pub struct LocalListener {
    listener: platform::Listener,
    _lock: FileLock,
}

/// Serializes service discovery and process creation. The service holds its
/// separate listener lock after the attaching process releases this guard.
pub async fn startup_lock(paths: &DesktopPaths) -> Result<FileLock, LocalError> {
    platform::prepare_home(paths).await?;
    let file = platform::lock_file(paths, "client-start.lock")?;
    acquire_startup_lock(file).await
}

async fn acquire_startup_lock(file: std::fs::File) -> Result<FileLock, LocalError> {
    loop {
        match FileLock::try_exclusive(file.try_clone()?) {
            Ok(lock) => return Ok(lock),
            Err(std::fs::TryLockError::WouldBlock) => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(error) => return Err(LocalError::ReserveStartup(error)),
        }
    }
}

impl LocalListener {
    pub async fn bind(paths: &DesktopPaths) -> Result<Self, LocalError> {
        platform::prepare_home(paths).await?;
        let file = platform::lock_file(paths, "client-host.lock")?;
        let lock = FileLock::try_exclusive(file).map_err(LocalError::ListenerOwned)?;
        Ok(Self {
            listener: platform::Listener::bind(paths).await?,
            _lock: lock,
        })
    }

    pub async fn serve(self, host: Arc<ClientHost>, stop: CancellationToken) -> anyhow::Result<()> {
        self.serve_with_idle_timeout(host, stop, None).await
    }

    pub async fn serve_until_idle(
        self,
        host: Arc<ClientHost>,
        stop: CancellationToken,
    ) -> anyhow::Result<()> {
        self.serve_with_idle_timeout(host, stop, Some(Duration::from_secs(180)))
            .await
    }

    async fn serve_with_idle_timeout(
        mut self,
        host: Arc<ClientHost>,
        stop: CancellationToken,
        idle_timeout: Option<Duration>,
    ) -> anyhow::Result<()> {
        let mut attachments = JoinSet::new();
        let idle = tokio::time::sleep(idle_timeout.unwrap_or(Duration::from_secs(180)));
        tokio::pin!(idle);
        let result: anyhow::Result<()> = async {
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = &mut idle, if idle_timeout.is_some() && attachments.is_empty() => break,
                    accepted = self.listener.accept() => {
                        let stream = accepted?;
                        if attachments.len() >= 64 { continue; }
                        attachments.spawn(host.clone().serve(stream, stop.child_token()));
                    }
                    done = attachments.join_next(), if !attachments.is_empty() => {
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
        stop.cancel();
        while let Some(done) = attachments.join_next().await {
            report_attachment(Some(done));
        }
        result
    }
}

fn report_attachment(result: Option<Result<anyhow::Result<()>, tokio::task::JoinError>>) {
    match result {
        Some(Ok(Err(error))) => tracing::debug!(%error, "client attachment closed"),
        Some(Err(error)) => tracing::error!(%error, "client attachment task failed"),
        _ => {}
    }
}
