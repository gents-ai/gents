use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use same_file::Handle;

use super::PluginAllowedError;

type Result<T, E = PluginAllowedError> = std::result::Result<T, E>;

static PROTECTED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn reserved_paths(home: &Path) -> [PathBuf; 7] {
    [
        super::file(home),
        super::lock_file(home),
        crate::plugin::approval::lock_file(home),
        home.join(crate::plugin::approval::workspace::FILE),
        home.join(crate::plugin::approval::workspace::LOCK),
        home.join(crate::home::PLUGIN_APPROVALS_DIR_NAME),
        home.join(crate::home::PERMISSION_STAGING_DIR_NAME),
    ]
}

pub fn protect(home: &Path) {
    let home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let paths = reserved_paths(&home);
    if let Ok(mut protected) = PROTECTED.lock() {
        for path in paths {
            if !protected.contains(&path) {
                protected.push(path);
            }
        }
    }
}

pub(crate) struct CommandProtection {
    home: PathBuf,
    #[cfg(target_os = "macos")]
    paths: Vec<PathBuf>,
}

impl CommandProtection {
    pub(crate) fn home(&self) -> &Path {
        &self.home
    }

    pub(crate) fn capture(home: &Path, supported: bool) -> Result<Self, PluginAllowedError> {
        use gents_loop::tool_policy::operator_paths::command_guard_allows;

        if !command_guard_allows(true, supported, true, true) {
            return Err(PluginAllowedError::CommandIsolationUnavailable);
        }
        let canonical = home
            .canonicalize()
            .map_err(PluginAllowedError::ResolveCommandPermissionState)?;
        if canonical != home {
            return Err(PluginAllowedError::PermissionHomeChanged);
        }
        let paths = reserved_paths(home).to_vec();
        let links = visit_objects(&paths, true, single_linked)?;
        if !command_guard_allows(
            true,
            supported,
            true,
            links.into_iter().all(|single| single),
        ) {
            return Err(PluginAllowedError::PermissionFileAliased);
        }
        Ok(Self {
            home: canonical,
            #[cfg(target_os = "macos")]
            paths,
        })
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn ancestors(&self) -> std::collections::BTreeSet<PathBuf> {
        self.paths
            .iter()
            .flat_map(|path| path.ancestors().skip(1).map(Path::to_owned))
            .collect()
    }

    pub(crate) fn revalidate(&self) -> Result<(), PluginAllowedError> {
        Self::capture(&self.home, true).map(|_| ())
    }
}

fn single_linked(path: &Path) -> io::Result<bool> {
    let file = open_regular_file(path, true)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let links = file.metadata()?.nlink();
        if links == 0 {
            return Err(io::Error::other(
                "Runtime permission state changed during inspection. Retry the action.",
            ));
        }
        Ok(links == 1)
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(false)
    }
}

/// Temporary permission data shares the reserved boundary of its final file.
/// Retained handles detect directory or object replacement before publication;
/// namespace changes after the last inspection remain a host filesystem premise.
pub(crate) fn write_permission_file<E>(
    home: &Path,
    target: &Path,
    write: impl FnOnce(&mut fs::File) -> Result<(), E>,
) -> Result<(), E>
where
    E: From<PluginAllowedError> + From<io::Error>,
{
    let home = home
        .canonicalize()
        .map_err(PluginAllowedError::ResolveHome)?;
    let parent = target.parent().ok_or(PluginAllowedError::NoParent)?;
    if parent.canonicalize()? != home {
        return Err(PluginAllowedError::OutsideHome.into());
    }
    let target = home.join(target.file_name().ok_or(PluginAllowedError::NoName)?);
    let directory = home.join(crate::home::PERMISSION_STAGING_DIR_NAME);
    #[cfg(unix)]
    let create = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(&directory)
    };
    #[cfg(not(unix))]
    let create = fs::DirBuilder::new().create(&directory);
    match create {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(PluginAllowedError::StagingDir(error).into()),
    }
    super::canonical_target(&directory, Some(true))?;
    let directory_identity = Handle::from_path(&directory)?;
    protect(&home);
    let mut staged =
        tempfile::NamedTempFile::new_in(&directory).map_err(PluginAllowedError::StageFile)?;
    let file_identity = Handle::from_file(staged.as_file().try_clone()?)?;
    validate_staging(&directory, &directory_identity, &staged, &file_identity)?;
    write(staged.as_file_mut())?;
    staged
        .as_file()
        .sync_all()
        .map_err(PluginAllowedError::Flush)?;
    validate_staging(&directory, &directory_identity, &staged, &file_identity)?;
    staged
        .persist(&target)
        .map_err(|error| PluginAllowedError::Publish(error.error))?;
    Ok(())
}

fn validate_staging(
    directory: &Path,
    directory_identity: &Handle,
    staged: &tempfile::NamedTempFile,
    file_identity: &Handle,
) -> Result<(), PluginAllowedError> {
    super::canonical_target(directory, Some(true))?;
    super::canonical_target(staged.path(), Some(false))?;
    if Handle::from_path(directory)? != *directory_identity
        || regular_file(staged.path())? != *file_identity
    {
        return Err(PluginAllowedError::StagingChanged);
    }
    Ok(())
}

/// Missing targets have already had their existing ancestors canonicalized
/// by the path owner. Object comparisons retain both handles until the
/// comparison finishes, including on filesystems that reuse closed file IDs.
pub fn is_protected(path: &Path) -> bool {
    let Ok(paths) = PROTECTED.lock().map(|paths| paths.clone()) else {
        return true;
    };
    inspect(&paths, path).unwrap_or(true)
}

/// WASI preopens reject links outside their root. Inspect physical descendants
/// without following links; an in-root link's target is visited at its own name.
/// Namespace changes after the final inspection remain a host filesystem premise.
pub(crate) fn ensure_mount_safe(root: &Path) -> Result<(), PluginAllowedError> {
    let paths = PROTECTED
        .lock()
        .map_err(|_| io::Error::other("operator path guard is unavailable"))?
        .clone();
    if mount_contains_protected(&paths, root)? {
        return Err(PluginAllowedError::MountProtected);
    }
    Ok(())
}

pub(super) fn mount_contains_protected(paths: &[PathBuf], root: &Path) -> io::Result<bool> {
    let root = root.canonicalize()?;
    if paths
        .iter()
        .any(|guarded| root.starts_with(guarded) || guarded.starts_with(&root))
    {
        return Ok(true);
    }
    let objects = protected_objects(paths)?;
    if objects.is_empty() {
        return Ok(false);
    }
    let mut directories = vec![root.clone()];
    while let Some(directory) = directories.pop() {
        if directory.canonicalize()? != directory || !directory.starts_with(&root) {
            return Err(io::Error::other(
                "the shared folder changed during inspection",
            ));
        }
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                directories.push(entry.path());
            } else if kind.is_file() {
                let file = regular_file(&entry.path())?;
                if objects.contains(&file) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

pub(super) fn inspect(paths: &[PathBuf], path: &Path) -> io::Result<bool> {
    if paths.iter().any(|guarded| path.starts_with(guarded)) {
        return Ok(true);
    }
    let canonical = match path.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if paths.iter().any(|guarded| canonical.starts_with(guarded)) {
        return Ok(true);
    }
    if fs::metadata(&canonical)?.is_dir() {
        return Ok(false);
    }
    let target = regular_file(&canonical)?;
    Ok(protected_objects(paths)?.contains(&target))
}

fn protected_objects(paths: &[PathBuf]) -> io::Result<Vec<Handle>> {
    visit_objects(paths, false, regular_file)
}

fn visit_objects<T>(
    paths: &[PathBuf],
    canonical_only: bool,
    inspect: impl Fn(&Path) -> io::Result<T>,
) -> io::Result<Vec<T>> {
    let mut objects = Vec::new();
    let mut remaining = paths.to_vec();
    while let Some(guarded) = remaining.pop() {
        let metadata = match fs::symlink_metadata(&guarded) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if canonical_only {
            if metadata.is_symlink() {
                return Err(io::Error::other(
                    "runtime permission state contains a symbolic link",
                ));
            }
            let canonical = match guarded.canonicalize() {
                Ok(path) => path,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if canonical != guarded {
                return Err(io::Error::other(
                    "runtime permission state changed during inspection",
                ));
            }
        }
        if metadata.is_dir() {
            if let Some(entries) = optional_directory(&guarded)? {
                for entry in entries {
                    remaining.push(entry?.path());
                }
            }
        } else {
            if metadata.is_symlink() && fs::metadata(&guarded)?.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "operator state contains a directory link",
                ));
            }
            match inspect(&guarded) {
                Ok(current) => objects.push(current),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(objects)
}

pub(super) fn optional_directory(path: &Path) -> io::Result<Option<fs::ReadDir>> {
    match fs::read_dir(path) {
        Ok(entries) => Ok(Some(entries)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn regular_file(path: &Path) -> io::Result<Handle> {
    Handle::from_file(open_regular_file(path, false)?)
}

fn open_regular_file(path: &Path, _no_follow: bool) -> io::Result<fs::File> {
    if !fs::metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "operator file identity is unavailable",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | if _no_follow { libc::O_NOFOLLOW } else { 0 });
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            options.read(false).write(true).open(path)?
        }
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "operator file changed during inspection",
        ));
    }
    Ok(file)
}
