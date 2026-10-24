use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use gents_desktop_core::client::DesktopPaths;
use tokio::net::{UnixListener, UnixStream};

pub(super) struct Listener {
    listener: UnixListener,
    path: PathBuf,
}

pub async fn connect(paths: &DesktopPaths) -> Result<UnixStream> {
    let metadata = owned_home(paths)?;
    anyhow::ensure!(
        metadata.mode() & 0o077 == 0,
        "client home permissions must exclude other users"
    );
    let path = endpoint(paths)?;
    let metadata = std::fs::symlink_metadata(&path)?;
    anyhow::ensure!(
        metadata.file_type().is_socket() && metadata.uid() == current_user(),
        "client endpoint must be a socket owned by the current user"
    );
    let stream = UnixStream::connect(path)
        .await
        .context("connecting to the client installation")?;
    anyhow::ensure!(
        stream.peer_cred()?.uid() == current_user(),
        "client installation belongs to another user"
    );
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

fn owned_home(paths: &DesktopPaths) -> Result<std::fs::Metadata> {
    let metadata = std::fs::symlink_metadata(paths.root())?;
    if !metadata.is_dir() || metadata.uid() != current_user() {
        bail!("client home must be a directory owned by the current user");
    }
    Ok(metadata)
}

pub(super) fn lock_file(paths: &DesktopPaths, name: &str) -> Result<std::fs::File> {
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
    pub(super) async fn bind(paths: &DesktopPaths) -> Result<Self> {
        let path = endpoint(paths)?;
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(&path)?,
            Ok(_) => bail!("client endpoint is occupied by a non-socket path"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self { listener, path })
    }

    pub(super) async fn accept(&mut self) -> Result<UnixStream> {
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

fn endpoint(paths: &DesktopPaths) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let root = std::fs::canonicalize(paths.root())?;
    let path = root.join("client.sock");
    if path.as_os_str().as_bytes().len() <= 100 {
        return Ok(path);
    }
    let directory = PathBuf::from(format!("/tmp/gents-client-{}", current_user()));
    private_socket_directory(&directory)?;
    let digest = blake3::hash(root.as_os_str().as_bytes());
    Ok(directory.join(format!("{}.sock", &digest.to_hex()[..40])))
}

/// Long history paths cannot fit in sockaddr_un. Only the socket moves into a
/// private per-user directory; store and startup locks remain in the canonical
/// history home. Refuse pre-existing symlinks or non-private directories rather
/// than changing their permissions or trusting a caller-controlled temp path.
fn private_socket_directory(path: &std::path::Path) -> Result<()> {
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).context("creating the client socket directory"),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_dir() && metadata.uid() == current_user() && metadata.mode() & 0o077 == 0,
        "client socket directory must be private and owned by the current user"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{acquire_startup_lock, startup_lock, LocalListener};
    use super::*;
    use gents::file_lock::FileLock;
    use std::time::Duration;

    #[tokio::test]
    async fn long_history_homes_attach_without_moving_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir
            .path()
            .join("history-with-a-long-configured-location-".repeat(4));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("retained-history"), b"keep").unwrap();
        let paths = DesktopPaths::from_root(&root);
        let first = LocalListener::bind(&paths).await.unwrap();
        let socket = endpoint(&paths).unwrap();
        assert!(!socket.starts_with(&root));
        assert!(socket.as_os_str().len() <= 100);
        let _client = connect(&paths).await.unwrap();
        let _accepted = first.listener.listener.accept().await.unwrap();
        assert!(LocalListener::bind(&paths).await.is_err());
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        assert_eq!(socket, endpoint(&DesktopPaths::from_root(alias)).unwrap());
        let other_root = dir.path().join("another-long-history-location-".repeat(5));
        std::fs::create_dir(&other_root).unwrap();
        assert_ne!(
            socket,
            endpoint(&DesktopPaths::from_root(other_root)).unwrap()
        );
        drop(first);
        assert!(!socket.exists());
        let stale = UnixListener::bind(&socket).unwrap();
        drop(stale);
        assert!(LocalListener::bind(&paths).await.is_ok());
        assert_eq!(
            std::fs::read(root.join("retained-history")).unwrap(),
            b"keep"
        );
    }

    #[test]
    fn socket_directory_rejects_symlinks_files_and_shared_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let directory = dir.path().join("private");
        private_socket_directory(&directory).unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&directory, &alias).unwrap();
        assert!(private_socket_directory(&alias).is_err());
        let file = dir.path().join("file");
        std::fs::write(&file, b"keep").unwrap();
        assert!(private_socket_directory(&file).is_err());
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_socket_directory(&directory).is_err());
        assert_eq!(std::fs::metadata(&directory).unwrap().mode() & 0o777, 0o755);
    }

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
