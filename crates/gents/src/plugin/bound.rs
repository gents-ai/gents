//! One directory, or one file alone, admitted for one plugin call.
//!
//! Binding is authority for exactly one call and is never recorded in an
//! install's `granted` manifold. A single file is exposed alone through a
//! private folder holding a hard link to it (WASI preopens directories, not
//! files); the folder lives on the file's own filesystem.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

use crate::pack::BindAccess;

/// The folder, inside a candidate root, that holds the per-call private folders.
const PRIVATE_ROOT: &str = ".gents-bind";

/// Age after which a private folder is a leftover of a call that died.
const STALE: Duration = Duration::from_secs(24 * 60 * 60);

/// A single directory, or one file, admitted for one plugin call, never wider
/// than the ceiling [`Self::new`]'s `within` names or what
/// [`super::allowed::bind`] was granted. Operator call sites (`gents plugin
/// run --bind-dir`, a `gents pack test` case's own `bind` field, a scenario
/// `prepare` step) name the directory themselves. A path that comes from data
/// (a graph node's source document, a model's tool arguments) is bound only
/// through [`super::allowed::bind`]: it must resolve inside the working folder
/// or an allowed folder, or be approved by the operator for that one call.
#[derive(Debug, Clone)]
pub struct BoundDir {
    dir: PathBuf,
    target: PathBuf,
    /// The canonical file or folder the caller named, whatever `target` is.
    original: PathBuf,
    access: BindAccess,
    /// Keeps the private folder of a single-file binding alive.
    _private: Option<Arc<tempfile::TempDir>>,
}

impl BoundDir {
    /// Canonicalizes `requested` and requires it to be a directory. The
    /// operator named it, so it permits whatever access the plugin declares.
    /// A `within` prefix mismatch (through `..` or a symlink) is refused here,
    /// before any plugin runs, rather than left to WASI's preopen resolution;
    /// `within` is canonicalized independently so a host-specific symlink in
    /// its own path resolves the same way on both sides.
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

    /// Opens the directory once and reads its path back from that handle,
    /// so a component swapped since `dir` was resolved is refused here.
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

    /// Exposes the canonical regular file `file` alone through a private
    /// folder holding a hard link to it. The file is opened once; its path
    /// read back from that handle and the link's inode are both checked
    /// against it, so a path swapped between the scope check and here is
    /// refused.
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

    /// Refuses when the directory or target no longer canonicalizes to the
    /// path it was validated as. Run immediately before the preopen; a swap
    /// between this check and the guest's first read remains possible for a
    /// folder and is only contained by WASI's own preopen resolution.
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

    /// The canonical file or folder the caller named; `target` of a single
    /// file is a link in a private folder that is gone when the call ends.
    pub fn original(&self) -> &Path {
        &self.original
    }

    pub fn access(&self) -> BindAccess {
        self.access
    }
}

/// A file no hard link could be made to (no writable ancestor on its
/// filesystem, read-only mount), as opposed to a path that changed under
/// the call.
#[derive(Debug)]
pub(crate) struct NotLinkable(String);

impl std::fmt::Display for NotLinkable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotLinkable {}

/// The device and inode of an open file or folder; `None` where the platform
/// has neither.
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

/// The path the kernel reports for `file`'s open handle.
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
    // bytes into the buffer, which is exactly that long.
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

/// Opens `path` once and requires the handle's own path to be the canonical
/// path checked against the scope. Opening a FIFO for reading blocks until a
/// writer appears, so on unix the open is nonblocking.
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

/// Whether `path`'s folder, itself pinned, names the regular file `identity`
/// under `path`'s file name without following a symlink. macOS's F_GETPATH
/// reports any one of a hard-linked file's names, not the one it was opened
/// by, so a file with a second link is checked through its folder instead.
#[cfg(unix)]
fn named_in_pinned_folder(path: &Path, identity: Identity) -> bool {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt as _;
    let (Some(folder), Some(name), Some(_)) = (path.parent(), path.file_name(), identity) else {
        return false;
    };
    // O_NONBLOCK keeps a FIFO swapped into the folder from parking this check.
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

/// Folders a link to `file` can be made under, best first: the system temp
/// folder when it is on the file's filesystem, then its ancestors (a link
/// cannot cross filesystems).
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

/// A fresh private folder under `root`'s `.gents-bind` folder, which must be
/// ours alone; stale siblings are removed.
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

/// Removes private folders older than [`STALE`]; a failure leaves the
/// leftover for the next sweep.
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
