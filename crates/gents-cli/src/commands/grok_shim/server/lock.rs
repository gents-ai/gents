use super::socket::ensure_socket_parent;
use super::*;

/// The exclusive sibling leader lock at `socket_path.with_extension("lock")`:
/// opened `O_NOFOLLOW`, forced `0600`, locked nonblocking-exclusive with
/// `File::try_lock` (`flock(2)` `LOCK_EX | LOCK_NB` on Unix). The open `File`
/// is the guard; the holder PID is written for diagnostics.
pub(super) struct LeaderLock {
    path: PathBuf,
    file: File,
}

impl LeaderLock {
    pub(super) fn acquire(socket_path: &Path) -> Result<Self> {
        let path = socket_path.with_extension("lock");
        if let Some(parent) = path.parent() {
            ensure_socket_parent(parent).with_context(|| {
                format!(
                    "creating the grok shim leader lock parent {}",
                    parent.display()
                )
            })?;
        }
        for _ in 0..LOCK_ACQUIRE_ATTEMPTS {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(PRIVATE_FILE_MODE)
                .custom_flags(O_NOFOLLOW)
                .open(&path)
                .with_context(|| format!("opening the grok shim leader lock {}", path.display()))?;
            if let Err(error) = file.try_lock() {
                if matches!(error, std::fs::TryLockError::WouldBlock) {
                    let holder = read_holder_pid(&path);
                    return Err(anyhow!(
                        "another grok leader already holds {} (last recorded holder pid: {})",
                        path.display(),
                        holder
                            .map(|pid| pid.to_string())
                            .unwrap_or_else(|| "unknown".to_string()),
                    ));
                }
                return Err(anyhow!(
                    "locking the grok shim leader lock {} failed: {error}",
                    path.display()
                ));
            }
            if file_identity(&file) != path_identity(&path) {
                drop(file);
                continue;
            }
            force_private_mode(&file, &path)?;
            let mut file = file;
            write_holder_pid(&mut file)
                .with_context(|| format!("recording the leader pid in {}", path.display()))?;
            // Revalidate after every mutation of the descriptor. A replaced
            // pathname never becomes the published election point.
            if file_identity(&file) != path_identity(&path) {
                drop(file);
                continue;
            }
            return Ok(Self { path, file });
        }
        bail!(
            "the grok shim leader lock {} changed inode during acquisition",
            path.display()
        )
    }

    /// Release by dropping the descriptor while retaining the stable private
    /// inode: removing a flock file before (or while) dropping the descriptor
    /// lets a concurrent opener race past the exclusive lock.
    pub(super) fn release(self) {
        tracing::debug!(target: LOG_TARGET, lock = %self.path.display(), "released the grok shim leader lock");
        drop(self.file);
    }
}

/// Force `0600` on the open lock file and verify it through the descriptor:
/// `open(2)`'s mode is masked by the umask (and ignored for existing files),
/// and descriptor-based verification fails closed if the path was swapped.
fn force_private_mode(file: &File, path: &Path) -> Result<()> {
    let current = file
        .metadata()
        .with_context(|| {
            format!(
                "reading the mode of the grok shim leader lock {}",
                path.display()
            )
        })?
        .permissions()
        .mode();
    if current & 0o777 == PRIVATE_FILE_MODE {
        return Ok(());
    }
    file.set_permissions(Permissions::from_mode(PRIVATE_FILE_MODE))
        .with_context(|| {
            format!(
                "forcing mode 0600 on the grok shim leader lock {}",
                path.display()
            )
        })?;
    let forced = file
        .metadata()
        .with_context(|| {
            format!(
                "re-reading the mode of the grok shim leader lock {}",
                path.display()
            )
        })?
        .permissions()
        .mode();
    if forced & 0o777 != PRIVATE_FILE_MODE {
        bail!(
            "the grok shim leader lock {} is mode {:o} instead of 0600",
            path.display(),
            forced & 0o777
        );
    }
    Ok(())
}

fn file_identity(file: &File) -> Option<(u64, u64)> {
    let metadata = file.metadata().ok()?;
    Some((metadata.dev(), metadata.ino()))
}

fn path_identity(path: &Path) -> Option<(u64, u64)> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return None;
    }
    Some((metadata.dev(), metadata.ino()))
}

fn read_holder_pid(lock_path: &Path) -> Option<u32> {
    std::fs::read_to_string(lock_path).ok()?.trim().parse().ok()
}

fn write_holder_pid(file: &mut File) -> Result<()> {
    file.set_len(0)
        .with_context(|| "truncating the grok shim leader lock".to_string())?;
    file.write_all(format!("{}\n", process::id()).as_bytes())
        .with_context(|| "writing the leader pid into the lock file".to_string())?;
    Ok(())
}
