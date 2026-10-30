use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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

fn file(home: &Path) -> PathBuf {
    home.join(crate::store::local::home::ALLOWED_DIRS_FILE_NAME)
}

fn lock_file(home: &Path) -> PathBuf {
    home.join(crate::store::local::home::ALLOWED_DIRS_LOCK_FILE_NAME)
}

fn lock(home: &Path) -> Result<crate::file_lock::FileLock> {
    std::fs::create_dir_all(home).context("creating the allowed-folders home")?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_file(home))
        .context("opening the allowed-folders lock")?;
    crate::file_lock::FileLock::exclusive(file).context("locking allowed folders")
}
pub fn list(home: &Path) -> Result<Vec<AllowedDir>> {
    let path = file(home);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let mut stored: Stored = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not a valid allowed-folders file", path.display()))?;
    stored.dirs.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(stored.dirs)
}

fn save(home: &Path, dirs: Vec<AllowedDir>) -> Result<()> {
    std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
    let bytes = serde_json::to_vec_pretty(&Stored { dirs })?;
    let path = file(home);
    write_permission_file(home, &path, |file| {
        std::io::Write::write_all(file, &bytes).context("staging the allowed folders")
    })
}

pub fn add(home: &Path, path: &Path, access: BindAccess) -> Result<AllowedDir> {
    put(home, path, access, false)
}

/// An approval may widen a grant, but never downgrade a concurrent grant.
/// The path is the canonical target captured by the question before waiting.
/// All allowed-folder mutations share the same native exclusive lock.
pub(super) fn grant_at_least(home: &Path, path: &Path, access: BindAccess) -> Result<AllowedDir> {
    put(home, path, access, true)
}

fn put(home: &Path, path: &Path, access: BindAccess, preserve_wider: bool) -> Result<AllowedDir> {
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

pub(super) fn prepare_grant(home: &Path, path: &Path, access: BindAccess) -> Result<AllowedDir> {
    let path = path
        .canonicalize()
        .with_context(|| format!("{} does not exist or cannot be read", path.display()))?;
    anyhow::ensure!(
        path.is_dir() || path.is_file(),
        "{} is not a file or folder",
        path.display()
    );
    ensure_scope_path(&path, user_home().as_deref(), home)?;
    Ok(AllowedDir { path, access })
}

#[derive(Debug, thiserror::Error)]
#[error("The requested path changed or is unavailable: {}. Retry the action with its current path.", .0.display())]
pub(super) struct PathChanged(pub PathBuf);

/// Consent records hold canonical targets; a later alias cannot select a
/// different target under an existing grant.
pub(super) fn canonical_target(path: &Path, directory: Option<bool>) -> Result<PathBuf> {
    let current = path
        .canonicalize()
        .map_err(|_| PathChanged(path.to_owned()))?;
    if current != path
        || directory.is_some_and(|expected| {
            if expected {
                !current.is_dir()
            } else {
                !current.is_file()
            }
        })
    {
        return Err(PathChanged(path.to_owned()).into());
    }
    Ok(current)
}

/// A queued answer names the canonical path captured before the wait.
/// Following a replacement symlink would grant access to another target.
pub(super) fn prepare_answered_grant(
    home: &Path,
    path: &Path,
    access: BindAccess,
) -> Result<AllowedDir> {
    let entry = prepare_grant(home, path, access)?;
    if entry.path != path {
        return Err(PathChanged(path.to_owned()).into());
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

pub fn remove(home: &Path, path: &Path) -> Result<bool> {
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

fn ensure_scope_path(path: &Path, user_home: Option<&Path>, gents_home: &Path) -> Result<()> {
    anyhow::ensure!(
        !too_broad(path, user_home, gents_home)
            && !gents_home
                .canonicalize()
                .is_ok_and(|home| path.starts_with(home)),
        "{} is too broad or includes the gents home; allow a specific file or folder",
        path.display()
    );
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
    ) -> Result<Self> {
        Self::from_entries(gents_home, list(gents_home)?, workdir, user_home)
    }

    pub(super) fn from_entries(
        gents_home: &Path,
        mut entries: Vec<AllowedDir>,
        workdir: Option<&Path>,
        user_home: Option<&Path>,
    ) -> Result<Self> {
        for entry in &entries {
            let path = canonical_target(&entry.path, None)
                .with_context(|| format!("allowed path {} is unavailable", entry.path.display()))?;
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
    let failed = |error: anyhow::Error| format!("{error:#}");
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
        Err(error)
            if folder_allowed
                && access == BindAccess::Read
                && error.is::<super::bound::NotLinkable>() =>
        {
            BoundDir::beside(&resolved.target, access).map_err(failed)
        }
        Err(error) if error.is::<super::bound::NotLinkable>() => Err(format!(
            "{error:#}; allow its folder with `gents plugin dirs add {}`",
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
