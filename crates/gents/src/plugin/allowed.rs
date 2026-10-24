use std::path::{Path, PathBuf};

use gents_protocol::error_chain::ErrorChain;
use serde::{Deserialize, Serialize};

use super::approval::PluginApprovalError;
use super::bound::BoundDirError;
use super::BoundDir;
use crate::pack::BindAccess;

#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowedDir {
    pub path: PathBuf,
    pub access: BindAccess,
}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    dirs: Vec<AllowedDir>,
}

#[derive(Debug, thiserror::Error)]
pub enum PluginAllowedError {
    #[error("creating the allowed-folders home")]
    CreateHome(#[source] std::io::Error),
    #[error("opening the allowed-folders lock")]
    OpenLock(#[source] std::io::Error),
    #[error("locking allowed folders")]
    Lock(#[source] std::io::Error),
    #[error("reading {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{} is not a valid allowed-folders file", path.display())]
    Malformed {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("creating {}", path.display())]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Encode(#[from] serde_json::Error),
    #[error("staging the allowed folders")]
    Stage(#[source] std::io::Error),
    #[error("{} does not exist or cannot be read", path.display())]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{} is not a file or folder", path.display())]
    NotFileOrFolder { path: PathBuf },
    #[error("allowed path {} is unavailable", path.display())]
    Unavailable {
        path: PathBuf,
        #[source]
        source: Box<PluginAllowedError>,
    },
    #[error("{} is too broad or includes the gents home; allow a specific file or folder", path.display())]
    TooBroad { path: PathBuf },
    #[error("The requested path changed or is unavailable: {}. Retry the action with its current path.", path.display())]
    PathChanged { path: PathBuf },
    #[error(transparent)]
    Approval(Box<PluginApprovalError>),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Runtime permission files cannot be shared with a plugin.")]
    Protected,
    #[error("Command isolation for runtime permissions is unavailable. Use native file tools or a runtime with supported command isolation.")]
    CommandIsolationUnavailable,
    #[error("The runtime permission home changed. Retry the command.")]
    PermissionHomeChanged,
    #[error("A runtime permission file has a hard-link alias. Remove the alias outside this session, then retry the command.")]
    PermissionFileAliased,
    #[error("creating permission staging directory")]
    StagingDir(#[source] std::io::Error),
    #[error("staging runtime permissions")]
    StageFile(#[source] std::io::Error),
    #[error("flushing runtime permissions")]
    Flush(#[source] std::io::Error),
    #[error("publishing runtime permissions")]
    Publish(#[source] std::io::Error),
    #[error("resolving the permission home")]
    ResolveHome(#[source] std::io::Error),
    #[error("resolving command permission state")]
    ResolveCommandPermissionState(#[source] std::io::Error),
    #[error("permission target is outside its runtime home")]
    OutsideHome,
    #[error("permission target has no parent")]
    NoParent,
    #[error("permission target has no name")]
    NoName,
    #[error("permission staging directory or file changed before publication")]
    StagingChanged,
    #[error(
        "The shared folder contains protected runtime permissions. Choose another file or folder."
    )]
    MountProtected,
}

impl From<PluginApprovalError> for PluginAllowedError {
    fn from(error: PluginApprovalError) -> Self {
        Self::Approval(Box::new(error))
    }
}

fn file(home: &Path) -> PathBuf {
    home.join(crate::home::ALLOWED_DIRS_FILE_NAME)
}

fn lock_file(home: &Path) -> PathBuf {
    home.join(crate::home::ALLOWED_DIRS_LOCK_FILE_NAME)
}

fn lock(home: &Path) -> Result<crate::file_lock::FileLock, PluginAllowedError> {
    std::fs::create_dir_all(home).map_err(PluginAllowedError::CreateHome)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_file(home))
        .map_err(PluginAllowedError::OpenLock)?;
    crate::file_lock::FileLock::exclusive(file).map_err(PluginAllowedError::Lock)
}

pub fn list(home: &Path) -> Result<Vec<AllowedDir>, PluginAllowedError> {
    let path = file(home);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(PluginAllowedError::Read { path, source }),
    };
    let mut stored: Stored =
        serde_json::from_slice(&bytes).map_err(|source| PluginAllowedError::Malformed {
            path: path.clone(),
            source,
        })?;
    stored.dirs.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(stored.dirs)
}

fn save(home: &Path, dirs: Vec<AllowedDir>) -> Result<(), PluginAllowedError> {
    std::fs::create_dir_all(home).map_err(|source| PluginAllowedError::CreateDir {
        path: home.to_path_buf(),
        source,
    })?;
    let bytes = serde_json::to_vec_pretty(&Stored { dirs })?;
    let path = file(home);
    write_permission_file(home, &path, |file| {
        std::io::Write::write_all(file, &bytes).map_err(PluginAllowedError::Stage)
    })
}

pub fn add(home: &Path, path: &Path, access: BindAccess) -> Result<AllowedDir, PluginAllowedError> {
    put(home, path, access, false)
}

/// An approval may widen a grant, but never downgrade a concurrent grant.
/// The path is the canonical target captured by the question before waiting.
/// All allowed-folder mutations share the same native exclusive lock.
pub(super) fn grant_at_least(
    home: &Path,
    path: &Path,
    access: BindAccess,
) -> Result<AllowedDir, PluginAllowedError> {
    put(home, path, access, true)
}

fn put(
    home: &Path,
    path: &Path,
    access: BindAccess,
    preserve_wider: bool,
) -> Result<AllowedDir, PluginAllowedError> {
    let entry = if preserve_wider {
        prepare_answered_grant(home, path, access)?
    } else {
        prepare_grant(home, path, access)?
    };
    let _lock = lock(home)?;
    let mut dirs = list(home)?;
    let entry = put_entry(&mut dirs, entry, preserve_wider);
    save(home, dirs)?;
    Ok(entry)
}

pub(super) fn prepare_grant(
    home: &Path,
    path: &Path,
    access: BindAccess,
) -> Result<AllowedDir, PluginAllowedError> {
    let path = path
        .canonicalize()
        .map_err(|source| PluginAllowedError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;
    if !(path.is_dir() || path.is_file()) {
        return Err(PluginAllowedError::NotFileOrFolder { path });
    }
    ensure_scope_path(&path, user_home().as_deref(), home)?;
    Ok(AllowedDir { path, access })
}

/// Consent records hold canonical targets; a later alias cannot select a
/// different target under an existing grant.
pub(super) fn canonical_target(
    path: &Path,
    directory: Option<bool>,
) -> Result<PathBuf, PluginAllowedError> {
    let current = path
        .canonicalize()
        .map_err(|_| PluginAllowedError::PathChanged {
            path: path.to_owned(),
        })?;
    if current != path
        || directory.is_some_and(|expected| {
            if expected {
                !current.is_dir()
            } else {
                !current.is_file()
            }
        })
    {
        return Err(PluginAllowedError::PathChanged {
            path: path.to_owned(),
        });
    }
    Ok(current)
}

/// A queued answer names the canonical path captured before the wait.
/// Following a replacement symlink would grant access to another target.
pub(super) fn prepare_answered_grant(
    home: &Path,
    path: &Path,
    access: BindAccess,
) -> Result<AllowedDir, PluginAllowedError> {
    let entry = prepare_grant(home, path, access)?;
    if entry.path != path {
        return Err(PluginAllowedError::PathChanged {
            path: path.to_owned(),
        });
    }
    Ok(entry)
}

pub(super) fn put_entry(
    dirs: &mut Vec<AllowedDir>,
    mut entry: AllowedDir,
    preserve_wider: bool,
) -> AllowedDir {
    if preserve_wider {
        entry.access = dirs
            .iter()
            .filter(|existing| existing.path == entry.path)
            .fold(entry.access, |held, existing| held.max(existing.access));
    }
    dirs.retain(|existing| existing.path != entry.path);
    dirs.push(entry.clone());
    entry
}

pub fn remove(home: &Path, path: &Path) -> Result<bool, PluginAllowedError> {
    let canonical = path.canonicalize().ok();
    let _lock = lock(home)?;
    let mut dirs = list(home)?;
    let before = dirs.len();
    dirs.retain(|entry| entry.path != path && Some(&entry.path) != canonical.as_ref());
    let removed = dirs.len() != before;
    if removed {
        save(home, dirs)?;
    }
    Ok(removed)
}

pub fn user_home() -> Option<PathBuf> {
    dirs::home_dir().and_then(|home| home.canonicalize().ok())
}

#[derive(Clone, Debug)]
pub struct Resolved {
    pub target: PathBuf,
    pub is_dir: bool,
}

impl Resolved {
    pub fn folder(&self) -> &Path {
        if self.is_dir {
            &self.target
        } else {
            self.target.parent().unwrap_or(&self.target)
        }
    }
}

pub fn resolve(
    requested: &str,
    workdir: Option<&Path>,
    user_home: Option<&Path>,
    gents_home: &Path,
) -> Result<Resolved, String> {
    let requested = requested.trim();
    let path = if requested == "~" || requested.starts_with("~/") {
        let home = user_home.ok_or("this system has no home folder to expand ~ against")?;
        home.join(requested.trim_start_matches('~').trim_start_matches('/'))
    } else if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        workdir
            .ok_or_else(|| format!("{requested} is not an absolute path; give the full path"))?
            .join(requested)
    };
    let target = path
        .canonicalize()
        .map_err(|_| format!("{requested} does not exist or cannot be read"))?;
    if let Ok(home) = gents_home.canonicalize() {
        if target.starts_with(&home) || home.starts_with(&target) {
            return Err(format!("{requested} is the gents home or contains it"));
        }
    }
    Ok(Resolved {
        is_dir: target.is_dir(),
        target,
    })
}

fn too_broad(dir: &Path, user_home: Option<&Path>, gents_home: &Path) -> bool {
    dir.parent().is_none()
        || user_home.is_some_and(|home| home.starts_with(dir))
        || gents_home
            .canonicalize()
            .is_ok_and(|home| home.starts_with(dir))
}

fn ensure_scope_path(
    path: &Path,
    user_home: Option<&Path>,
    gents_home: &Path,
) -> Result<(), PluginAllowedError> {
    if too_broad(path, user_home, gents_home)
        || gents_home
            .canonicalize()
            .is_ok_and(|home| path.starts_with(home))
    {
        return Err(PluginAllowedError::TooBroad {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Scope {
    entries: Vec<AllowedDir>,
}

impl Scope {
    pub fn load(
        gents_home: &Path,
        workdir: Option<&Path>,
        user_home: Option<&Path>,
    ) -> Result<Self, PluginAllowedError> {
        Self::from_entries(gents_home, list(gents_home)?, workdir, user_home)
    }

    pub(super) fn from_entries(
        gents_home: &Path,
        mut entries: Vec<AllowedDir>,
        workdir: Option<&Path>,
        user_home: Option<&Path>,
    ) -> Result<Self, PluginAllowedError> {
        for entry in &entries {
            let path = canonical_target(&entry.path, None).map_err(|source| {
                PluginAllowedError::Unavailable {
                    path: entry.path.clone(),
                    source: Box::new(source),
                }
            })?;
            ensure_scope_path(&path, user_home, gents_home)?;
        }
        let working = workdir
            .and_then(|dir| dir.canonicalize().ok())
            .filter(|dir| !too_broad(dir, user_home, gents_home));
        if let Some(workdir) = working {
            entries.push(AllowedDir {
                path: workdir,
                access: BindAccess::Read,
            });
        }
        Ok(Self { entries })
    }

    pub fn granted(&self, target: &Path) -> Option<BindAccess> {
        self.entries
            .iter()
            .filter(|entry| {
                canonical_target(&entry.path, None).is_ok_and(|root| target.starts_with(root))
            })
            .map(|entry| entry.access)
            .max()
    }
}

pub fn bind(
    resolved: &Resolved,
    access: BindAccess,
    folder_allowed: bool,
) -> Result<BoundDir, String> {
    let failed = |error: BoundDirError| ErrorChain(&error).to_string();
    if resolved.is_dir {
        return BoundDir::folder(&resolved.target, access).map_err(failed);
    }
    if !resolved.target.is_file() {
        return Err(format!(
            "{} is not a file or folder",
            resolved.target.display()
        ));
    }
    match BoundDir::for_file(&resolved.target, access) {
        Err(BoundDirError::NotLinkable(_)) if folder_allowed && access == BindAccess::Read => {
            BoundDir::beside(&resolved.target, access).map_err(failed)
        }
        Err(error @ BoundDirError::NotLinkable(_)) => Err(format!(
            "{}; allow its folder with `gents plugin dirs add {}`",
            ErrorChain(&error),
            resolved.folder().display()
        )),
        other => other.map_err(failed),
    }
}

mod protected;
pub(crate) use protected::ensure_mount_safe;
pub(crate) use protected::write_permission_file;
pub(crate) use protected::CommandProtection;
pub use protected::{is_protected, protect};

#[cfg(test)]
#[path = "allowed_tests.rs"]
mod tests;
