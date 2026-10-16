use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

use crate::pack::BindAccess;

const PRIVATE_ROOT: &str = ".gents-bind";

const STALE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone)]
pub struct BoundDir {
    dir: PathBuf,
    target: PathBuf,
    original: PathBuf,
    access: BindAccess,
    _private: Option<Arc<tempfile::TempDir>>,
}

impl BoundDir {
    pub fn new(requested: &Path, within: Option<&Path>) -> Result<Self> {
        let canonical = requested
            .canonicalize()
            .with_context(|| format!("{} does not exist or cannot be read", requested.display()))?;
        anyhow::ensure!(
            canonical.is_dir(),
            "{} is not a directory",
            canonical.display()
        );
        if let Some(within) = within {
            let within = within.canonicalize().with_context(|| {
                format!("{} does not exist or cannot be read", within.display())
            })?;
            anyhow::ensure!(
                canonical.starts_with(&within),
                "{} is outside {}, the only directory this call may bind",
                canonical.display(),
                within.display()
            );
        }
        Ok(Self {
            target: canonical.clone(),
            original: canonical.clone(),
            dir: canonical,
            access: BindAccess::ReadWrite,
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
            _private: None,
        })
    }

    pub(crate) fn beside(file: &Path, access: BindAccess) -> Result<Self> {
        let dir = file
            .parent()
            .with_context(|| format!("{} has no folder", file.display()))?;
        let mut bound = Self::folder(dir, access)?;
        bound.target = file.to_path_buf();
        bound.original = file.to_path_buf();
        Ok(bound)
    }

    pub(crate) fn for_file(file: &Path, access: BindAccess) -> Result<Self> {
        let name = file
            .file_name()
            .with_context(|| format!("{} has no file name", file.display()))?;
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
                    anyhow::ensure!(
                        identity == identity_of(&link),
                        "{} changed while it was being shared",
                        file.display()
                    );
                    return Ok(Self {
                        dir,
                        target: link,
                        original: file.to_path_buf(),
                        access,
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
        for path in [&self.dir, &self.target] {
            let now = path.canonicalize().with_context(|| {
                format!("{} no longer exists or cannot be read", path.display())
            })?;
            anyhow::ensure!(
                &now == path,
                "{} now resolves to {}; the bound path changed after it was validated",
                path.display(),
                now.display()
            );
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

#[derive(Debug)]
pub(crate) struct NotLinkable(String);

impl std::fmt::Display for NotLinkable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotLinkable {}

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
        .with_context(|| format!("{} cannot be read", path.display()))?;
    let meta = handle
        .metadata()
        .with_context(|| format!("{} cannot be read", path.display()))?;
    match kind {
        PinKind::File => anyhow::ensure!(meta.is_file(), "{} is not a file", path.display()),
        PinKind::Folder => anyhow::ensure!(meta.is_dir(), "{} is not a folder", path.display()),
    }
    let identity = handle_identity(&handle);
    if let Some(opened) = opened_path(&handle) {
        anyhow::ensure!(
            opened == path || named_in_pinned_folder(path, identity),
            "{} now resolves to {}; the path changed after it was validated",
            path.display(),
            opened.display()
        );
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
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    match builder.create(&shared) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error).with_context(|| format!("creating {}", shared.display()));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(&shared)?;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        anyhow::ensure!(
            meta.is_dir() && meta.uid() == me,
            "{} is not owned by this user",
            shared.display()
        );
    }
    sweep(&shared);
    tempfile::Builder::new()
        .prefix("b-")
        .tempdir_in(&shared)
        .with_context(|| format!("creating a private folder in {}", shared.display()))
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
