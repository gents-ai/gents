use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use gents_desktop_core::client::DesktopPaths;
use tokio::net::{UnixListener, UnixStream};

#[derive(Debug, thiserror::Error)]
pub enum UnixLocalError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("connecting to the client installation")]
    Connect(#[source] std::io::Error),
    #[error("client home permissions must exclude other users")]
    HomePermissions,
    #[error("client endpoint must be a socket owned by the current user")]
    EndpointNotSocket,
    #[error("client installation belongs to another user")]
    InstallationOwner,
    #[error("client home must be a directory owned by the current user")]
    HomeNotOwned,
    #[error("client endpoint is occupied by a non-socket path")]
    EndpointOccupied,
    #[error("client home is too long for a local socket; select a shorter client home")]
    EndpointTooLong,
    #[error(transparent)]
    Paths(#[from] gents_desktop_core::client::PathsError),
}

pub(super) use UnixLocalError as PlatformError;

type Result<T, E = UnixLocalError> = std::result::Result<T, E>;

pub(super) struct Listener {
    listener: UnixListener,
    path: PathBuf,
}

pub async fn connect(paths: &DesktopPaths) -> Result<UnixStream, UnixLocalError> {
    let metadata = owned_home(paths)?;
    if metadata.mode() & 0o077 != 0 {
        return Err(UnixLocalError::HomePermissions);
    }
    let path = endpoint(paths)?;
    let metadata = std::fs::symlink_metadata(&path)?;
    if !(metadata.file_type().is_socket() && metadata.uid() == current_user()) {
        return Err(UnixLocalError::EndpointNotSocket);
    }
    let stream = UnixStream::connect(path)
        .await
        .map_err(UnixLocalError::Connect)?;
    if stream.peer_cred()?.uid() != current_user() {
        return Err(UnixLocalError::InstallationOwner);
    }
    Ok(stream)
}

pub(super) async fn prepare_home(paths: &DesktopPaths) -> Result<()> {
    paths.ensure_root_dirs().await?;
    owned_home(paths)?;
    std::fs::set_permissions(paths.root(), std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn current_user() -> libc::uid_t {
    // SAFETY: geteuid has no preconditions and reads only the process effective UID.
    unsafe { libc::geteuid() }
}

fn owned_home(paths: &DesktopPaths) -> Result<std::fs::Metadata, UnixLocalError> {
    let metadata = std::fs::symlink_metadata(paths.root())?;
    if !metadata.is_dir() || metadata.uid() != current_user() {
        return Err(UnixLocalError::HomeNotOwned);
    }
    Ok(metadata)
}

pub(super) fn lock_file(paths: &DesktopPaths, name: &str) -> Result<std::fs::File, UnixLocalError> {
    Ok(std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(paths.root().join(name))?)
}

impl Listener {
    pub(super) async fn bind(paths: &DesktopPaths) -> Result<Self, UnixLocalError> {
        let path = endpoint(paths)?;
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(&path)?,
            Ok(_) => return Err(UnixLocalError::EndpointOccupied),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self { listener, path })
    }

    pub(super) async fn accept(&mut self) -> Result<UnixStream, UnixLocalError> {
        Ok(self.listener.accept().await?.0)
    }
}
impl Drop for Listener {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(%error, "removing the client installation socket failed");
            }
        }
    }
}

fn endpoint(paths: &DesktopPaths) -> Result<PathBuf, UnixLocalError> {
    use std::os::unix::ffi::OsStrExt;
    let root = std::fs::canonicalize(paths.root())?;
    let path = root.join("client.sock");
    if path.as_os_str().as_bytes().len() > 100 {
        return Err(UnixLocalError::EndpointTooLong);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::super::{acquire_startup_lock, startup_lock, LocalListener};
    use super::*;
    use gents::file_lock::FileLock;
    use std::time::Duration;

    #[tokio::test]
    async fn concurrent_startup_waits_for_the_previous_discovery_owner() {
        let dir = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(dir.path());
        let first = startup_lock(&paths).await.unwrap();
        let waiting = acquire_startup_lock(lock_file(&paths, "client-start.lock").unwrap());
        tokio::pin!(waiting);
        assert!(futures::poll!(waiting.as_mut()).is_pending());
        drop(first);
        let second = tokio::time::timeout(Duration::from_secs(30), waiting)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            FileLock::try_exclusive(lock_file(&paths, "client-start.lock").unwrap()),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(second);
        assert!(FileLock::try_exclusive(lock_file(&paths, "client-start.lock").unwrap()).is_ok());
    }

    #[tokio::test]
    async fn an_existing_listener_is_not_replaced_and_stale_endpoints_are_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(dir.path());
        let first = LocalListener::bind(&paths).await.unwrap();
        assert!(LocalListener::bind(&paths).await.is_err());
        let _client = connect(&paths).await.unwrap();
        let _accepted = first.listener.listener.accept().await.unwrap();
        let path = first.listener.path.clone();
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

    #[tokio::test]
    async fn attaching_requires_a_private_home_and_rejects_endpoint_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(dir.path());
        let listener = LocalListener::bind(&paths).await.unwrap();
        let _connection = connect(&paths).await.unwrap();
        std::fs::set_permissions(paths.root(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(connect(&paths).await.is_err());
        assert_eq!(
            std::fs::metadata(paths.root()).unwrap().mode() & 0o777,
            0o777
        );
        std::fs::set_permissions(paths.root(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = listener.listener.path.clone();
        let target = dir.path().join("other.sock");
        let _other = UnixListener::bind(&target).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(connect(&paths).await.is_err());
    }
}
