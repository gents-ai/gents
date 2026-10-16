use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use gents::file_lock::FileLock;
use gents_desktop_core::client::DesktopPaths;
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::ClientHost;

/// The lock protects socket replacement, independently of the client core's
/// store lock. A stale socket alone is never evidence that a store is free.
pub struct LocalListener {
    listener: UnixListener,
    path: PathBuf,
    _lock: FileLock,
}

pub async fn connect(paths: &DesktopPaths) -> Result<UnixStream> {
    UnixStream::connect(endpoint(paths)?)
        .await
        .context("connecting to the client installation")
}

impl LocalListener {
    pub async fn bind(paths: &DesktopPaths) -> Result<Self> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};

        paths.ensure_root_dirs().await?;
        let metadata = std::fs::symlink_metadata(paths.root())?;
        // SAFETY: geteuid has no preconditions and reads only the process effective UID.
        let uid = unsafe { libc::geteuid() };
        if !metadata.is_dir() || metadata.uid() != uid {
            bail!("client home must be a directory owned by the current user");
        }
        std::fs::set_permissions(paths.root(), std::fs::Permissions::from_mode(0o700))?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(paths.root().join("client-host.lock"))?;
        let lock = FileLock::try_exclusive(file)
            .context("another process owns the client installation listener")?;
        let path = endpoint(paths)?;
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(&path)?,
            Ok(_) => bail!("client endpoint is occupied by a non-socket path"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            listener,
            path,
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
        self,
        host: Arc<ClientHost>,
        stop: CancellationToken,
        idle_timeout: Option<Duration>,
    ) -> Result<()> {
        let mut attachments = JoinSet::new();
        let idle = tokio::time::sleep(idle_timeout.unwrap_or(Duration::from_secs(180)));
        tokio::pin!(idle);
        let result: Result<()> = async {
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = &mut idle, if idle_timeout.is_some() && attachments.is_empty() => break,
                    accepted = self.listener.accept() => {
                        let (stream, _) = accepted?;
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

impl Drop for LocalListener {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(%error, "removing the client installation socket failed");
            }
        }
    }
}

fn report_attachment(result: Option<Result<Result<()>, tokio::task::JoinError>>) {
    match result {
        Some(Ok(Err(error))) => tracing::debug!(%error, "client attachment closed"),
        Some(Err(error)) => tracing::error!(%error, "client attachment task failed"),
        _ => {}
    }
}

fn endpoint(paths: &DesktopPaths) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let root = std::fs::canonicalize(paths.root())?;
    let path = root.join("client.sock");
    if path.as_os_str().as_bytes().len() > 100 {
        bail!("client home is too long for a local socket; select a shorter client home");
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_existing_listener_is_not_replaced_and_stale_endpoints_are_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(dir.path());
        let first = LocalListener::bind(&paths).await.unwrap();
        assert!(LocalListener::bind(&paths).await.is_err());
        let _client = connect(&paths).await.unwrap();
        let _accepted = first.listener.accept().await.unwrap();
        let path = first.path.clone();
        drop(first);
        assert!(!path.exists());
        let stale = UnixListener::bind(&path).unwrap();
        drop(stale);
        assert!(LocalListener::bind(&paths).await.is_ok());
    }

    #[tokio::test]
    async fn endpoint_symlinks_and_regular_files_are_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(dir.path());
        let endpoint = endpoint(&paths).unwrap();
        std::fs::write(&endpoint, b"keep").unwrap();
        assert!(LocalListener::bind(&paths).await.is_err());
        assert_eq!(std::fs::read(&endpoint).unwrap(), b"keep");
        std::fs::remove_file(&endpoint).unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, &endpoint).unwrap();
        assert!(LocalListener::bind(&paths).await.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
    }
}
