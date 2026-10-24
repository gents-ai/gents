use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use super::super::error::EvalInitError;
use super::{is_generated_eval_pack, replaceable_out, resolve_through_existing_ancestor};

pub(super) fn sibling_name(out: &Path, suffix: &str) -> Result<OsString, EvalInitError> {
    let name = out.file_name().ok_or(EvalInitError::OutputUnnamed)?;
    let mut sibling = OsString::from(".");
    sibling.push(name);
    sibling.push(suffix);
    Ok(sibling)
}

/// The lock file remains named after release: unlinking it would let two
/// writers lock different inodes for the same destination.
pub(super) fn lock_output(out: &Path) -> Result<File, EvalInitError> {
    let parent = out
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let lock = parent.join(sibling_name(out, ".eval-init.lock")?);
    std::fs::create_dir_all(parent).map_err(|source| EvalInitError::CreateDir {
        path: parent.to_path_buf(),
        source,
    })?;
    if lock
        .symlink_metadata()
        .is_ok_and(|metadata| !metadata.is_file())
    {
        return Err(EvalInitError::LockNotRegular { path: lock });
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(&lock)
        .map_err(|source| EvalInitError::OpenLock {
            path: lock.clone(),
            source,
        })?;
    file.try_lock().map_err(|source| EvalInitError::LockBusy {
        out: out.to_path_buf(),
        source,
    })?;
    Ok(file)
}

pub(crate) fn recover_output(out: &Path, gents_home: &Path) -> Result<(), EvalInitError> {
    if out.symlink_metadata().is_ok() {
        return Ok(());
    }
    let parent = out
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.try_exists()? {
        return Ok(());
    }
    let _lock = lock_output(out)?;
    recover_locked(out, gents_home)
}

/// Recovery only renames one validated generated pack. Staged packs and
/// unrecognized siblings remain untouched; several backups require an
/// operator choice instead of guessing which generation to restore.
pub(super) fn recover_locked(out: &Path, gents_home: &Path) -> Result<(), EvalInitError> {
    match out.symlink_metadata() {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(EvalInitError::InspectOutputBeforeRecovery(error)),
    }
    let destination = resolve_through_existing_ancestor(out)?;
    let user_home = std::env::home_dir().ok_or(EvalInitError::RecoveryUserHome)?;
    let cwd = std::env::current_dir().map_err(EvalInitError::WorkingDirectory)?;
    for protected in [user_home.as_path(), cwd.as_path(), gents_home] {
        let protected = resolve_through_existing_ancestor(protected)?;
        if protected.starts_with(&destination) {
            return Err(EvalInitError::OutProtected {
                out: out.to_path_buf(),
            });
        }
    }
    let parent = destination
        .parent()
        .ok_or(EvalInitError::OutputWithoutParent)?;
    let prefix = sibling_name(out, ".replaced-")?;
    let mut backups: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir()
            || !entry
                .file_name()
                .as_encoded_bytes()
                .starts_with(prefix.as_encoded_bytes())
        {
            continue;
        }
        let pack = entry.path().join("pack");
        if !pack
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_dir())
            || !is_generated_eval_pack(&pack)
        {
            continue;
        }
        replaceable_out(&pack, Some(&user_home), gents_home, &cwd)?;
        backups.push(pack);
    }
    if backups.len() > 1 {
        return Err(EvalInitError::SeveralBackups {
            out: out.to_path_buf(),
            backups,
        });
    }
    if let Some(backup) = backups.pop() {
        std::fs::rename(&backup, out).map_err(|source| EvalInitError::RestoreInterrupted {
            out: out.to_path_buf(),
            backup: backup.clone(),
            source,
        })?;
        let holder = backup.parent().ok_or(EvalInitError::BackupWithoutHolder)?;
        if let Err(error) = std::fs::remove_dir(holder) {
            tracing::warn!(path = %holder.display(), %error, "recovered eval pack holder remains");
        }
    }
    Ok(())
}
