use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_READ_FILE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct HostFs {
    root: PathBuf,
    base: PathBuf,
}

#[derive(Clone, Copy, Debug)]
pub struct CreateDirectoryOptions {
    pub recursive: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct RemoveOptions {
    pub recursive: bool,
    pub force: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct CopyOptions {
    pub recursive: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostFileMetadata {
    pub is_directory: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    pub created_at_ms: i64,
    pub modified_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostDirectoryEntry {
    pub file_name: String,
    pub is_directory: bool,
    pub is_file: bool,
}

impl HostFs {
    pub fn new_with_base(root: PathBuf, base: Option<PathBuf>) -> io::Result<Self> {
        let root = std::fs::canonicalize(&root).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "canonicalizing host filesystem root {}: {err}",
                    root.display()
                ),
            )
        })?;
        let base = match base {
            Some(base) => {
                let base = std::fs::canonicalize(&base).map_err(|err| {
                    io::Error::new(
                        err.kind(),
                        format!(
                            "canonicalizing host filesystem base {}: {err}",
                            base.display()
                        ),
                    )
                })?;
                if !base.is_dir() || !base.starts_with(&root) {
                    return Err(invalid_input(format!(
                        "host filesystem base {} is outside root {} or is not a directory",
                        base.display(),
                        root.display()
                    )));
                }
                base
            }
            None => root.clone(),
        };
        Ok(Self { root, base })
    }

    pub fn read_file(&self, path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
        let resolved = self.resolve_existing_path(path.as_ref())?;
        let metadata = std::fs::metadata(&resolved)?;
        if metadata.len() > MAX_READ_FILE_BYTES {
            return Err(invalid_input(format!(
                "file is too large to read: limit is {MAX_READ_FILE_BYTES} bytes"
            )));
        }
        std::fs::read(&resolved)
    }

    pub fn write_file(&self, path: impl AsRef<Path>, contents: &[u8]) -> io::Result<()> {
        let resolved = self.resolve_path_allow_create(path.as_ref())?;
        std::fs::write(resolved, contents)
    }

    pub fn create_directory(
        &self,
        path: impl AsRef<Path>,
        options: CreateDirectoryOptions,
    ) -> io::Result<()> {
        let resolved = self.resolve_path_allow_create(path.as_ref())?;
        if options.recursive {
            std::fs::create_dir_all(resolved)
        } else {
            std::fs::create_dir(resolved)
        }
    }

    pub fn get_metadata(&self, path: impl AsRef<Path>) -> io::Result<HostFileMetadata> {
        let candidate = self.absolute_candidate(path.as_ref())?;
        let _resolved = self.resolve_existing_path(candidate.as_path())?;
        let metadata = std::fs::metadata(&candidate)?;
        let symlink_metadata = std::fs::symlink_metadata(&candidate)?;
        Ok(HostFileMetadata {
            is_directory: metadata.is_dir(),
            is_file: metadata.is_file(),
            is_symlink: symlink_metadata.file_type().is_symlink(),
            created_at_ms: metadata.created().ok().map_or(0, system_time_to_unix_ms),
            modified_at_ms: metadata.modified().ok().map_or(0, system_time_to_unix_ms),
        })
    }

    pub fn read_directory(&self, path: impl AsRef<Path>) -> io::Result<Vec<HostDirectoryEntry>> {
        let resolved = self.resolve_existing_path(path.as_ref())?;
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(resolved)? {
            let entry = entry?;
            let Ok(metadata) = std::fs::metadata(entry.path()) else {
                continue;
            };
            entries.push(HostDirectoryEntry {
                file_name: entry.file_name().to_string_lossy().into_owned(),
                is_directory: metadata.is_dir(),
                is_file: metadata.is_file(),
            });
        }
        Ok(entries)
    }

    pub fn validate_watch_path(&self, path: impl AsRef<Path>) -> io::Result<()> {
        let candidate = self.absolute_candidate(path.as_ref())?;
        if candidate.exists() {
            self.resolve_existing_path(candidate.as_path())?;
            return Ok(());
        }

        let Some(parent) = candidate.parent() else {
            return Err(invalid_input(format!(
                "watch path has no parent directory: {}",
                candidate.display()
            )));
        };
        self.resolve_existing_path(parent)?;
        Ok(())
    }

    pub fn remove(&self, path: impl AsRef<Path>, options: RemoveOptions) -> io::Result<()> {
        let candidate = self.absolute_candidate(path.as_ref())?;
        let _resolved = match self.resolve_existing_path(candidate.as_path()) {
            Ok(resolved) => resolved,
            Err(err) if err.kind() == io::ErrorKind::NotFound && options.force => return Ok(()),
            Err(err) => return Err(err),
        };

        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) => {
                if metadata.file_type().is_dir() {
                    if options.recursive {
                        std::fs::remove_dir_all(candidate)
                    } else {
                        std::fs::remove_dir(candidate)
                    }
                } else {
                    std::fs::remove_file(candidate)
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound && options.force => Ok(()),
            Err(err) => Err(err),
        }
    }

    pub fn copy(
        &self,
        source_path: impl AsRef<Path>,
        destination_path: impl AsRef<Path>,
        options: CopyOptions,
    ) -> io::Result<()> {
        let source = self.absolute_candidate(source_path.as_ref())?;
        let resolved_source = self.resolve_existing_path(source.as_path())?;
        let destination = self.resolve_path_allow_create(destination_path.as_ref())?;
        let metadata = std::fs::symlink_metadata(&source)?;
        let file_type = metadata.file_type();

        if file_type.is_dir() {
            if !options.recursive {
                return Err(invalid_input(
                    "fs/copy requires recursive: true when sourcePath is a directory",
                ));
            }
            if destination_is_same_or_descendant_of_source(&resolved_source, &destination)? {
                return Err(invalid_input(
                    "fs/copy cannot copy a directory to itself or one of its descendants",
                ));
            }
            copy_dir_recursive(&source, &destination)?;
            return Ok(());
        }

        if file_type.is_symlink() {
            copy_symlink(&source, &destination)?;
            return Ok(());
        }

        if file_type.is_file() {
            std::fs::copy(source, destination)?;
            return Ok(());
        }

        Err(invalid_input(
            "fs/copy only supports regular files, directories, and symlinks",
        ))
    }

    fn resolve_existing_path(&self, path: &Path) -> io::Result<PathBuf> {
        let candidate = self.absolute_candidate(path)?;
        let resolved = std::fs::canonicalize(&candidate).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("resolving path {}: {err}", candidate.display()),
            )
        })?;
        self.ensure_allowed(resolved)
    }

    fn resolve_path_allow_create(&self, path: &Path) -> io::Result<PathBuf> {
        let candidate = self.absolute_candidate(path)?;
        let mut unresolved_suffix = Vec::<OsString>::new();
        let mut existing_path = candidate.as_path();
        while !existing_path.exists() {
            let Some(file_name) = existing_path.file_name() else {
                break;
            };
            unresolved_suffix.push(file_name.to_os_string());
            let Some(parent) = existing_path.parent() else {
                break;
            };
            existing_path = parent;
        }

        let mut resolved = std::fs::canonicalize(existing_path).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("resolving path {}: {err}", candidate.display()),
            )
        })?;
        for file_name in unresolved_suffix.iter().rev() {
            resolved.push(file_name);
        }
        self.ensure_allowed(resolved)
    }

    fn absolute_candidate(&self, path: &Path) -> io::Result<PathBuf> {
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.base.join(path)
        };
        normalize_for_creation(candidate.as_path())
    }

    fn ensure_allowed(&self, path: PathBuf) -> io::Result<PathBuf> {
        if path.starts_with(&self.root) {
            Ok(path)
        } else {
            Err(invalid_input(format!(
                "path is outside the allowed tool root {}: {}",
                self.root.display(),
                path.display()
            )))
        }
    }
}

fn copy_dir_recursive(source: &Path, target: &Path) -> io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            copy_dir_recursive(&source_path, &target_path)?;
        } else if file_type.is_file() {
            std::fs::copy(&source_path, &target_path)?;
        } else if file_type.is_symlink() {
            copy_symlink(&source_path, &target_path)?;
        }
    }
    Ok(())
}

fn destination_is_same_or_descendant_of_source(
    source: &Path,
    destination: &Path,
) -> io::Result<bool> {
    let source = std::fs::canonicalize(source)?;
    Ok(destination.starts_with(&source))
}

fn copy_symlink(source: &Path, target: &Path) -> io::Result<()> {
    let link_target = std::fs::read_link(source)?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&link_target, target)
    }
    #[cfg(windows)]
    {
        if symlink_points_to_directory(source)? {
            std::os::windows::fs::symlink_dir(&link_target, target)
        } else {
            std::os::windows::fs::symlink_file(&link_target, target)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = link_target;
        let _ = target;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "copying symlinks is unsupported on this platform",
        ))
    }
}

#[cfg(windows)]
fn symlink_points_to_directory(source: &Path) -> io::Result<bool> {
    use std::os::windows::fs::FileTypeExt;

    Ok(std::fs::symlink_metadata(source)?
        .file_type()
        .is_symlink_dir())
}

fn normalize_for_creation(path: &Path) -> io::Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    if normalized.is_absolute() {
        Ok(normalized)
    } else {
        Err(invalid_input(format!(
            "path did not resolve to an absolute path: {}",
            path.display()
        )))
    }
}

fn system_time_to_unix_ms(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
