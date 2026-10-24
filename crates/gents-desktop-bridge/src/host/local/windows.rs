use gents_protocol::error_chain::ErrorChain;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::time::Duration;
use std::{io, ptr};

use gents_desktop_core::client::DesktopPaths;
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_READ_ATTRIBUTES, READ_CONTROL,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};

mod security;
use security::{descriptor, require_private_file, Sid, WindowsSecurityError};

#[derive(Debug, thiserror::Error)]
pub enum WindowsLocalError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Security(#[from] WindowsSecurityError),
    #[error("client installation is not accepting attachments")]
    NotAccepting(#[source] tokio::time::error::Elapsed),
    #[error("client installation belongs to another Windows user")]
    InstallationOwner,
    #[error("client home must be a directory")]
    HomeNotDirectory,
    #[error("client files must not be junctions, symbolic links or reparse points")]
    ReparsePoint,
    #[error("client files must be regular files or directories")]
    NotRegularEntry,
    #[error("client lock files must not be symbolic links or reparse points")]
    LockReparsePoint,
    #[error("reserving the private client pipe")]
    ReservePipe(#[source] Box<WindowsLocalError>),
    #[error(transparent)]
    Paths(#[from] gents_desktop_core::client::PathsError),
}

pub(super) use WindowsLocalError as PlatformError;

type Result<T, E = WindowsLocalError> = std::result::Result<T, E>;

pub(super) struct Listener {
    next: NamedPipeServer,
    name: OsString,
}

pub async fn connect(paths: &DesktopPaths) -> Result<NamedPipeClient> {
    private_directory(paths.root())?;
    let name = endpoint(paths)?;
    let stream = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ClientOptions::new().open(&name) {
                Ok(stream) => return Ok(stream),
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error),
            }
        }
    })
    .await
    .map_err(WindowsLocalError::NotAccepting)??;
    let mut pid = 0;
    // SAFETY: stream owns a live named-pipe handle; pid is writable for the returned process ID.
    if unsafe { GetNamedPipeServerProcessId(stream.as_raw_handle(), &mut pid) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    if !Sid::current()?.matches(&Sid::for_process(pid)?) {
        return Err(WindowsLocalError::InstallationOwner);
    }
    Ok(stream)
}

pub(super) async fn prepare_home(paths: &DesktopPaths) -> Result<()> {
    paths.ensure_root_dirs().await?;
    private_directory(paths.root())?;
    let mut directories = vec![paths.root().to_owned()];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            if private_entry(&path)? {
                directories.push(path);
            }
        }
    }
    Ok(())
}

fn private_directory(path: &Path) -> Result<()> {
    if !private_entry(path)? {
        return Err(WindowsLocalError::HomeNotDirectory);
    }
    Ok(())
}

fn private_entry(path: &Path) -> Result<bool> {
    let file = OpenOptions::new()
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(WindowsLocalError::ReparsePoint);
    }
    if !(metadata.is_dir() || metadata.is_file()) {
        return Err(WindowsLocalError::NotRegularEntry);
    }
    require_private_file(&file)?;
    Ok(metadata.is_dir())
}

pub(super) fn lock_file(paths: &DesktopPaths, name: &str) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(paths.root().join(name))?;
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(WindowsLocalError::LockReparsePoint);
    }
    require_private_file(&file)?;
    Ok(file)
}

impl Listener {
    /// The caller holds the installation listener lock. Mio retains pipe handles
    /// until cancelled I/O completions are consumed, so a previous listener can
    /// still reserve the name after its Rust handles have been dropped.
    pub(super) async fn bind(paths: &DesktopPaths) -> Result<Self> {
        let name = endpoint(paths)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let next = loop {
            match create_pipe(&name, true) {
                Ok(next) => break next,
                Err(WindowsLocalError::Io(error))
                    if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32)
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(WindowsLocalError::ReservePipe(Box::new(error))),
            }
        };
        Ok(Self { next, name })
    }

    pub(super) async fn accept(&mut self) -> Result<NamedPipeServer> {
        loop {
            self.next.connect().await?;
            // Keep an unconnected instance alive before handing off the connected
            // one, so clients never discover a temporarily absent pipe name.
            let next = create_pipe(&self.name, false)?;
            let connected = std::mem::replace(&mut self.next, next);
            let mut pid = 0;
            // SAFETY: connected owns a live server handle; pid is writable for the peer ID.
            let identified =
                unsafe { GetNamedPipeClientProcessId(connected.as_raw_handle(), &mut pid) } != 0;
            let user = Sid::current()?;
            if identified && Sid::for_process(pid).is_ok_and(|peer| user.matches(&peer)) {
                return Ok(connected);
            }
            tracing::debug!("rejected a client attachment from another Windows account");
        }
    }
}

/// The default named-pipe DACL allows read access to Everyone. Supply a protected
/// user-only DACL and prohibit remote clients before any protocol bytes are read.
fn create_pipe(name: &OsString, first: bool) -> Result<NamedPipeServer> {
    let security = descriptor(&format!("D:P(A;;GA;;;{})", Sid::current()?.text()?))?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.0,
        bInheritHandle: 0,
    };
    // SAFETY: attributes and its security descriptor remain live for this synchronous call.
    let stream = unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(name, ptr::addr_of_mut!(attributes).cast())
    }?;
    Ok(stream)
}

fn endpoint(paths: &DesktopPaths) -> Result<OsString> {
    let root = std::fs::canonicalize(paths.root())?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"gents-client-host\0");
    for unit in root.as_os_str().encode_wide() {
        hash.update(&unit.to_le_bytes());
    }
    hash.update(&0_u16.to_le_bytes());
    hash.update(Sid::current()?.text()?.as_bytes());
    Ok(OsString::from(format!(
        r"\\.\pipe\gents-client-{}",
        hash.finalize().to_hex()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn pipe_keeps_the_name_reserved_while_connections_are_active() {
        tokio::time::timeout(Duration::from_secs(30), async {
            let dir = tempfile::tempdir().unwrap();
            let paths = DesktopPaths::from_root(dir.path().join("client"));
            prepare_home(&paths).await.unwrap();
            let mut listener = Listener::bind(&paths).await.unwrap();
            let name = endpoint(&paths).unwrap();
            assert!(create_pipe(&name, true).is_err());
            let mut first = connect(&paths).await.unwrap();
            let second = connect(&paths);
            tokio::pin!(second);
            assert!(futures::poll!(second.as_mut()).is_pending());
            let mut first_server = listener.accept().await.unwrap();
            let mut second = second.await.unwrap();
            let mut second_server = listener.accept().await.unwrap();
            assert!(create_pipe(&name, true).is_err());
            first.write_all(b"first").await.unwrap();
            second.write_all(b"other").await.unwrap();
            let mut bytes = [0; 5];
            first_server.read_exact(&mut bytes).await.unwrap();
            assert_eq!(&bytes, b"first");
            second_server.read_exact(&mut bytes).await.unwrap();
            assert_eq!(&bytes, b"other");
            first_server.write_all(b"reply").await.unwrap();
            first.read_exact(&mut bytes).await.unwrap();
            assert_eq!(&bytes, b"reply");
            drop((first, first_server, second, second_server, listener));
            assert!(Listener::bind(&paths).await.is_ok());
        })
        .await
        .expect("named-pipe attachment lifecycle deadline");
    }

    #[tokio::test]
    async fn listener_lock_rejects_an_active_owner_before_rebinding() {
        let dir = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(dir.path().join("client"));
        let listener = super::super::LocalListener::bind(&paths).await.unwrap();
        let competing = tokio::time::timeout(
            Duration::from_secs(5),
            super::super::LocalListener::bind(&paths),
        )
        .await
        .expect("an active owner must be rejected before waiting for pipe release");
        assert!(competing.is_err());
        drop(listener);
        let rebound = super::super::LocalListener::bind(&paths).await.unwrap();
        drop(rebound);
    }

    #[tokio::test]
    async fn home_aliases_resolve_to_one_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(dir.path().join("client"));
        prepare_home(&paths).await.unwrap();
        assert_eq!(
            endpoint(&paths).unwrap(),
            endpoint(&DesktopPaths::from_root(paths.root().join("."))).unwrap()
        );
        let other = DesktopPaths::from_root(dir.path().join("other"));
        prepare_home(&other).await.unwrap();
        assert_ne!(endpoint(&paths).unwrap(), endpoint(&other).unwrap());
    }

    #[tokio::test]
    async fn startup_lock_is_an_exclusive_private_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(dir.path().join("client"));
        prepare_home(&paths).await.unwrap();
        let first = lock_file(&paths, "client-start.lock").unwrap();
        let second = lock_file(&paths, "client-start.lock").unwrap();
        first.lock().unwrap();
        assert!(matches!(
            second.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        first.unlock().unwrap();
        assert!(second.try_lock().is_ok());
    }

    #[tokio::test]
    async fn public_identity_and_store_files_are_refused_without_rewriting() {
        use windows_sys::Win32::Security::Authorization::{SetNamedSecurityInfoW, SE_FILE_OBJECT};
        use windows_sys::Win32::Security::{
            GetSecurityDescriptorDacl, DACL_SECURITY_INFORMATION,
            PROTECTED_DACL_SECURITY_INFORMATION,
        };

        for identity in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let paths = DesktopPaths::from_root(dir.path().join("client"));
            prepare_home(&paths).await.unwrap();
            let record = if identity {
                paths.identity_key_path().to_owned()
            } else {
                paths.node_data_dir().join("data").join("segment")
            };
            std::fs::create_dir_all(record.parent().unwrap()).unwrap();
            std::fs::write(&record, b"keep").unwrap();
            prepare_home(&paths).await.unwrap();
            let security = descriptor(&format!(
                "D:P(A;;GA;;;{})(A;;GR;;;WD)",
                Sid::current().unwrap().text().unwrap()
            ))
            .unwrap();
            let mut present = 0;
            let mut dacl = ptr::null_mut();
            let mut defaulted = 0;
            // SAFETY: security owns a valid descriptor and the output pointers are writable.
            assert_ne!(
                unsafe {
                    GetSecurityDescriptorDacl(security.0, &mut present, &mut dacl, &mut defaulted)
                },
                0
            );
            assert_ne!(present, 0);
            let path: Vec<_> = record.as_os_str().encode_wide().chain(Some(0)).collect();
            // SAFETY: path is terminated UTF-16 and dacl lives in security for this synchronous call.
            assert_eq!(
                unsafe {
                    SetNamedSecurityInfoW(
                        path.as_ptr(),
                        SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                        ptr::null_mut(),
                        ptr::null_mut(),
                        dacl,
                        ptr::null_mut(),
                    )
                },
                0
            );
            let error = prepare_home(&paths).await.unwrap_err();
            assert!(
                error.to_string().contains("grant access"),
                "{}",
                ErrorChain(&error)
            );
            assert_eq!(std::fs::read(&record).unwrap(), b"keep");
        }
    }

    #[test]
    fn regular_file_cannot_be_a_client_home() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, b"keep").unwrap();
        assert!(private_directory(&file).is_err());
        assert_eq!(std::fs::read(file).unwrap(), b"keep");
    }
}
