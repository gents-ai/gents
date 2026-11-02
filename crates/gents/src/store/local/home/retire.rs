use std::fs;
use std::path::{Path, PathBuf};

use super::{init_config_path, HomeError, Result, RUNTIME_HOME_ENTRIES};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HomeEntries {
    pub owned: Vec<PathBuf>,
    pub retained: Vec<PathBuf>,
}

pub fn home_entries(home_dir: &Path, exclude: &[PathBuf]) -> Result<HomeEntries> {
    let mut entries = HomeEntries::default();
    let listing = match fs::read_dir(home_dir) {
        Ok(listing) => listing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(source) => {
            return Err(HomeError::ListHome {
                home: home_dir.to_path_buf(),
                source,
            })
        }
    };
    let mut candidates = Vec::new();
    for entry in listing {
        let entry = entry.map_err(|source| HomeError::ListHome {
            home: home_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let owned_name = entry
            .file_name()
            .to_str()
            .is_some_and(|name| RUNTIME_HOME_ENTRIES.contains(&name));
        if owned_name {
            candidates.push(path);
        } else {
            entries.retained.push(path);
        }
    }
    let mut references: Vec<PathBuf> = exclude.to_vec();
    for retained in &entries.retained {
        references.extend(child_home_references(retained));
    }
    for candidate in candidates {
        if references
            .iter()
            .any(|reference| reference.starts_with(&candidate))
        {
            entries.retained.push(candidate);
        } else {
            entries.owned.push(candidate);
        }
    }
    entries.owned.sort();
    entries.retained.sort();
    Ok(entries)
}

fn child_home_references(directory: &Path) -> Vec<PathBuf> {
    let Ok(bytes) = fs::read(init_config_path(directory)) else {
        return Vec::new();
    };
    let Ok(record) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Vec::new();
    };
    ["home", "key_path", "tool_root"]
        .iter()
        .filter_map(|field| record.get(field).and_then(serde_json::Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            let path = PathBuf::from(value);
            fs::canonicalize(&path).unwrap_or(path)
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
pub enum RetireDisposition<'a> {
    Archive { backup: &'a Path },
    Delete,
}

#[derive(Debug, Clone, Copy)]
pub struct RetireGroup<'a> {
    pub name: &'a str,
    pub entries: &'a [PathBuf],
}

/// Fails unless every present entry can be renamed into `backup`: renames do
/// not cross filesystems, and discovering that after the owning service was
/// stopped would leave a half-retired home.
pub fn archive_preflight(groups: &[RetireGroup<'_>], backup: &Path) -> Result<()> {
    archive_preflight_with(groups, backup, device_of)
}

#[cfg(unix)]
fn device_of(path: &Path) -> std::io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path).map(|metadata| metadata.dev())
}

#[cfg(not(unix))]
fn device_of(_path: &Path) -> std::io::Result<u64> {
    Ok(0)
}

pub(super) fn archive_preflight_with(
    groups: &[RetireGroup<'_>],
    backup: &Path,
    device: impl Fn(&Path) -> std::io::Result<u64>,
) -> Result<()> {
    let parent = backup
        .parent()
        .ok_or_else(|| HomeError::BackupWithoutParent {
            backup: backup.to_path_buf(),
        })?;
    let target = device(parent).map_err(|source| HomeError::Inspect {
        path: parent.to_path_buf(),
        source,
    })?;
    for entry in groups.iter().flat_map(|group| group.entries.iter()) {
        match device(entry) {
            Ok(source) if source == target => {}
            Ok(_) => {
                return Err(HomeError::CrossFilesystem {
                    entry: entry.clone(),
                    parent: parent.to_path_buf(),
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(HomeError::Inspect {
                    path: entry.clone(),
                    source,
                })
            }
        }
    }
    Ok(())
}

pub fn retire_entries(
    groups: &[RetireGroup<'_>],
    disposition: RetireDisposition<'_>,
) -> Result<Vec<PathBuf>> {
    retire_entries_with(groups, disposition, |path| {
        fs::symlink_metadata(path).map(|metadata| metadata.is_dir())
    })
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(path)
    }
    #[cfg(not(unix))]
    {
        fs::DirBuilder::new().create(path)
    }
}

pub(super) fn retire_entries_with(
    groups: &[RetireGroup<'_>],
    disposition: RetireDisposition<'_>,
    inspect: impl Fn(&Path) -> std::io::Result<bool>,
) -> Result<Vec<PathBuf>> {
    let present = |entry: &Path| -> Result<Option<bool>> {
        match inspect(entry) {
            Ok(is_dir) => Ok(Some(is_dir)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(HomeError::Inspect {
                path: entry.to_path_buf(),
                source,
            }),
        }
    };
    match disposition {
        RetireDisposition::Archive { backup } => {
            private_dir(backup).map_err(|source| HomeError::CreateBackup {
                path: backup.to_path_buf(),
                source,
            })?;
            let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
            let result = (|| -> Result<()> {
                let mut created: Vec<&str> = Vec::new();
                for group in groups {
                    let target_dir = backup.join(group.name);
                    if !created.contains(&group.name) {
                        private_dir(&target_dir).map_err(|source| HomeError::CreateDirectory {
                            path: target_dir.clone(),
                            source,
                        })?;
                        created.push(group.name);
                    }
                    for entry in group.entries {
                        if present(entry)?.is_none() {
                            continue;
                        }
                        let name = entry.file_name().ok_or_else(|| HomeError::NoFileName {
                            path: entry.clone(),
                        })?;
                        let target = target_dir.join(name);
                        fs::rename(entry, &target).map_err(|source| HomeError::Move {
                            from: entry.clone(),
                            to: target.clone(),
                            source,
                        })?;
                        moved.push((entry.clone(), target));
                    }
                }
                Ok(())
            })();
            match result {
                Ok(()) => Ok(moved.into_iter().map(|(source, _)| source).collect()),
                Err(error) => {
                    let mut unrestored = Vec::new();
                    for (source, target) in moved.iter().rev() {
                        if fs::rename(target, source).is_err() {
                            unrestored.push(target.display().to_string());
                        }
                    }
                    if unrestored.is_empty() {
                        for group in groups {
                            let _ = fs::remove_dir(backup.join(group.name));
                        }
                        let _ = fs::remove_dir(backup);
                        Err(HomeError::ArchiveRolledBack {
                            source: Box::new(error),
                        })
                    } else {
                        Err(HomeError::ArchiveUnrestored {
                            entries: unrestored.join(", "),
                            source: Box::new(error),
                        })
                    }
                }
            }
        }
        RetireDisposition::Delete => {
            let mut removed: Vec<PathBuf> = Vec::new();
            let done = |removed: &[PathBuf]| {
                removed
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            for entry in groups.iter().flat_map(|group| group.entries.iter()) {
                let is_dir = match present(entry) {
                    Ok(Some(is_dir)) => is_dir,
                    Ok(None) => continue,
                    Err(error) => {
                        return Err(HomeError::DeleteStopped {
                            removed: done(&removed),
                            source: Box::new(error),
                        })
                    }
                };
                let outcome = if is_dir {
                    fs::remove_dir_all(entry)
                } else {
                    fs::remove_file(entry)
                };
                if let Err(source) = outcome {
                    return Err(HomeError::DeleteFailed {
                        path: entry.clone(),
                        removed: done(&removed),
                        source,
                    });
                }
                removed.push(entry.clone());
            }
            Ok(removed)
        }
    }
}
