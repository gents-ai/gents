use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::approval::PluginApprovalError;
use crate::pack::BindAccess;

type Result<T, E = BoundDirError> = std::result::Result<T, E>;

const PRIVATE_ROOT: &str = ".gents-bind";

const STALE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone)]
pub struct BoundDir {
    dir: PathBuf,
    target: PathBuf,
    original: PathBuf,
    access: BindAccess,
    consent: Option<super::approval::workspace::ActionConsent>,
    _private: Option<Arc<tempfile::TempDir>>,
}

impl BoundDir {
    pub fn new(requested: &Path, within: Option<&Path>) -> Result<Self> {
        let canonical = requested
            .canonicalize()
            .map_err(|source| BoundDirError::Unreadable {
                path: requested.to_path_buf(),
                source,
            })?;
        if !canonical.is_dir() {
            return Err(BoundDirError::NotDirectory { path: canonical });
        }
        if let Some(within) = within {
            let within = within
                .canonicalize()
                .map_err(|source| BoundDirError::Unreadable {
                    path: within.to_path_buf(),
                    source,
                })?;
            if !canonical.starts_with(&within) {
                return Err(BoundDirError::OutsideBindRoot {
                    path: canonical,
                    within,
                });
            }
        }
        Ok(Self {
            target: canonical.clone(),
            original: canonical.clone(),
            dir: canonical,
            access: BindAccess::ReadWrite,
            consent: None,
            _private: None,
        })
    }

    pub(crate) fn folder(dir: &Path, access: BindAccess) -> Result<Self> {
        pin(dir, PinKind::Folder)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            target: dir.to_path_buf(),
            original: dir.to_path_buf(),
            access,
            consent: None,
            _private: None,
        })
    }

    pub(crate) fn beside(file: &Path, access: BindAccess) -> Result<Self> {
        let dir = file.parent().ok_or_else(|| BoundDirError::NoFolder {
            path: file.to_path_buf(),
        })?;
        let mut bound = Self::folder(dir, access)?;
        bound.target = file.to_path_buf();
        bound.original = file.to_path_buf();
        Ok(bound)
    }

    pub(crate) fn for_file(file: &Path, access: BindAccess) -> Result<Self> {
        let name = file.file_name().ok_or_else(|| BoundDirError::NoFileName {
            path: file.to_path_buf(),
        })?;
        let identity = pin(file, PinKind::File)?;
        let mut last = None;
        for root in link_roots(file) {
            let private = match private_folder(&root) {
                Ok(private) => private,
                Err(error) => {
                    last = Some(error);
                    continue;
                }
            };
            let dir = private.path().canonicalize()?;
            let link = dir.join(name);
            match std::fs::hard_link(file, &link) {
                Ok(()) => {
                    if identity != identity_of(&link) {
                        return Err(BoundDirError::ChangedWhileShared {
                            path: file.to_path_buf(),
                        });
                    }
                    return Ok(Self {
                        dir,
                        target: link,
                        original: file.to_path_buf(),
                        access,
                        consent: None,
                        _private: Some(Arc::new(private)),
                    });
                }
                Err(error) => last = Some(error.into()),
            }
        }
        let reason = last.map(|error| format!(": {error}")).unwrap_or_default();
        Err(NotLinkable(format!(
            "{} cannot be shared on its own{reason}",
            file.display()
        ))
        .into())
    }

    // A swap after this check and before the guest's first read is contained by WASI preopen resolution.
    pub fn recheck(&self) -> Result<()> {
        if let Some(consent) = &self.consent {
            consent.validate()?;
        }
        for path in [&self.dir, &self.target] {
            let now = path
                .canonicalize()
                .map_err(|source| BoundDirError::NoLongerExists {
                    path: path.to_path_buf(),
                    source,
                })?;
            if &now != path {
                return Err(BoundDirError::BoundPathChanged {
                    path: path.to_path_buf(),
                    now,
                });
            }
        }
        if super::allowed::is_protected(&self.original) {
            return Err(BoundDirError::Protected);
        }
        Ok(())
    }

    pub(crate) fn with_consent(
        mut self,
        consent: Option<super::approval::workspace::ActionConsent>,
    ) -> Self {
        self.consent = consent;
        self
    }

    pub(super) fn validate_invocation(
        &self,
        plugin: &str,
        artifact_digest: &str,
        input_digest: &str,
    ) -> Result<()> {
        if let Some(consent) = &self.consent {
            consent.validate_plugin(plugin, artifact_digest, input_digest)?;
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    pub fn target(&self) -> &Path {
        &self.target
    }

    pub fn original(&self) -> &Path {
        &self.original
    }

    pub fn access(&self) -> BindAccess {
        self.access
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BoundDirError {
    #[error("creating {}", path.display())]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{} does not exist or cannot be read", path.display())]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    NotLinkable(#[from] NotLinkable),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{} is not a directory", path.display())]
    NotDirectory { path: PathBuf },
    #[error("{} is outside {}, the only directory this call may bind", path.display(), within.display())]
    OutsideBindRoot { path: PathBuf, within: PathBuf },
    #[error("{} cannot be read", path.display())]
    CannotRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{} is not a file", path.display())]
    NotFile { path: PathBuf },
    #[error("{} is not a folder", path.display())]
    NotFolder { path: PathBuf },
    #[error("{} now resolves to {}; the path changed after it was validated", path.display(), resolved.display())]
    PathResolvesElsewhere { path: PathBuf, resolved: PathBuf },
    #[error("{} has no folder", path.display())]
    NoFolder { path: PathBuf },
    #[error("{} has no file name", path.display())]
    NoFileName { path: PathBuf },
    #[error("{} changed while it was being shared", path.display())]
    ChangedWhileShared { path: PathBuf },
    #[error("{} no longer exists or cannot be read", path.display())]
    NoLongerExists {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{} now resolves to {}; the bound path changed after it was validated", path.display(), now.display())]
    BoundPathChanged { path: PathBuf, now: PathBuf },
    #[error("Runtime permission files cannot be shared with a plugin.")]
    Protected,
    #[error("{} is not owned by this user", path.display())]
    NotOwned { path: PathBuf },
    #[error("creating a private folder in {}", path.display())]
    PrivateFolder {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Approval(Box<PluginApprovalError>),
}

impl From<PluginApprovalError> for BoundDirError {
    fn from(error: PluginApprovalError) -> Self {
        Self::Approval(Box::new(error))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct NotLinkable(String);

type Identity = Option<(u64, u64)>;

#[cfg(unix)]
fn identity_of(path: &Path) -> Identity {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path)
        .ok()
        .map(|meta| (meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn identity_of(_: &Path) -> Identity {
    None
}

#[cfg(unix)]
fn handle_identity(file: &File) -> Identity {
    use std::os::unix::fs::MetadataExt;
    file.metadata().ok().map(|meta| (meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn handle_identity(_: &File) -> Identity {
    None
}

#[cfg(target_os = "linux")]
fn opened_path(file: &File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()
}

#[cfg(target_os = "macos")]
fn opened_path(file: &File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    let mut buffer = [0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes a NUL-terminated path of at most PATH_MAX
    let status = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) };
    if status == -1 {
        return None;
    }
    let len = buffer.iter().position(|&byte| byte == 0)?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buffer[..len])))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn opened_path(_: &File) -> Option<PathBuf> {
    None
}

enum PinKind {
    File,
    Folder,
}

// Nonblocking open prevents a swapped FIFO from hanging admission.
fn pin(path: &Path, kind: PinKind) -> Result<Identity> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let handle = options
        .open(path)
        .map_err(|source| BoundDirError::CannotRead {
            path: path.to_path_buf(),
            source,
        })?;
    let meta = handle
        .metadata()
        .map_err(|source| BoundDirError::CannotRead {
            path: path.to_path_buf(),
            source,
        })?;
    match kind {
        PinKind::File if !meta.is_file() => {
            return Err(BoundDirError::NotFile {
                path: path.to_path_buf(),
            });
        }
        PinKind::Folder if !meta.is_dir() => {
            return Err(BoundDirError::NotFolder {
                path: path.to_path_buf(),
            });
        }
        _ => {}
    }
    let identity = handle_identity(&handle);
    if let Some(opened) = opened_path(&handle) {
        if opened != path && !named_in_pinned_folder(path, identity) {
            return Err(BoundDirError::PathResolvesElsewhere {
                path: path.to_path_buf(),
                resolved: opened,
            });
        }
    }
    Ok(identity)
}

#[cfg(unix)]
fn named_in_pinned_folder(path: &Path, identity: Identity) -> bool {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt as _;
    let (Some(folder), Some(name), Some(_)) = (path.parent(), path.file_name(), identity) else {
        return false;
    };
    let Ok(dir) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(folder)
    else {
        return false;
    };
    if opened_path(&dir).as_deref() != Some(folder) {
        return false;
    }
    let Ok(name) = std::ffi::CString::new(name.as_bytes()) else {
        return false;
    };
    // SAFETY: `dir` is an open directory and `name` a NUL-terminated name.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return false;
    }
    // SAFETY: `fd` was just opened and is owned by nothing else.
    let named = unsafe { File::from_raw_fd(fd) };
    named.metadata().is_ok_and(|meta| meta.is_file()) && handle_identity(&named) == identity
}

#[cfg(not(unix))]
fn named_in_pinned_folder(_: &Path, _: Identity) -> bool {
    false
}

fn link_roots(file: &Path) -> Vec<PathBuf> {
    let Some(device) = identity_of(file).map(|(device, _)| device) else {
        return vec![std::env::temp_dir()];
    };
    let same_device = |path: &Path| identity_of(path).is_some_and(|(found, _)| found == device);
    let mut roots = vec![std::env::temp_dir()];
    let mut ancestors: Vec<PathBuf> = file.ancestors().skip(1).map(Path::to_path_buf).collect();
    ancestors.reverse();
    roots.extend(ancestors);
    roots.retain(|root| same_device(root));
    roots
}

fn private_folder(root: &Path) -> Result<tempfile::TempDir> {
    let shared = root.join(PRIVATE_ROOT);
    #[cfg(unix)]
    let create = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(&shared)
    };
    #[cfg(not(unix))]
    let create = std::fs::DirBuilder::new().create(&shared);
    match create {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(BoundDirError::CreateDir {
                path: shared,
                source,
            });
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(&shared)?;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        if !(meta.is_dir() && meta.uid() == me) {
            return Err(BoundDirError::NotOwned { path: shared });
        }
    }
    sweep(&shared);
    tempfile::Builder::new()
        .prefix("b-")
        .tempdir_in(&shared)
        .map_err(|source| BoundDirError::PrivateFolder {
            path: shared,
            source,
        })
}

fn sweep(shared: &Path) {
    let Ok(entries) = std::fs::read_dir(shared) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|modified| now.duration_since(modified).is_ok_and(|age| age > STALE));
        if old {
            if let Err(error) = std::fs::remove_dir_all(entry.path()) {
                tracing::debug!(%error, "could not remove a stale plugin bind folder");
            }
        }
    }
}

#[cfg(test)]
#[path = "bound_tests.rs"]
mod tests;
