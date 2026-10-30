use super::*;

/// Ensure every component of `parent` exists as a plain directory.
///
/// Deliberately not `create_dir_all`, which walks the path through symlinks:
/// existing directories are accepted with their own mode (never chmodmed),
/// symlinked and non-directory components are rejected, and missing components
/// are created `0700` from creation and verified with `symlink_metadata`.
pub(super) fn ensure_socket_parent(parent: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(SOCKET_PARENT_DIR_MODE);
    let mut current = PathBuf::new();
    for component in parent.components() {
        use std::path::Component;
        match component {
            Component::Prefix(_) | Component::RootDir => {
                current.push(component.as_os_str());
                continue;
            }
            _ => current.push(component.as_os_str()),
        }
        // Lstat: never follows a symlink component.
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                let file_type = metadata.file_type();
                if file_type.is_symlink() {
                    bail!(
                        "refusing the grok shim socket parent {}: {} is a symlink",
                        parent.display(),
                        current.display()
                    );
                }
                if !file_type.is_dir() {
                    bail!(
                        "refusing the grok shim socket parent {}: {} exists and is not a directory",
                        parent.display(),
                        current.display()
                    );
                }
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "inspecting the grok shim socket parent component {}",
                        current.display()
                    )
                });
            }
        }
        builder.create(&current).with_context(|| {
            format!("creating the grok shim socket parent {}", current.display())
        })?;
        let created = std::fs::symlink_metadata(&current).with_context(|| {
            format!(
                "verifying the newly created grok shim socket parent {}",
                current.display()
            )
        })?;
        if created.file_type().is_symlink() || !created.file_type().is_dir() {
            bail!(
                "the grok shim socket parent component {} was created but is not a plain directory",
                current.display()
            );
        }
    }
    Ok(())
}

/// Remove a stale socket left behind by a crashed leader; only called under
/// the exclusive lock, so a live leader's socket is never touched. Anything
/// that is not a plain socket file is refused.
pub(super) fn remove_stale_socket(socket_path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(socket_path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                bail!(
                    "refusing to replace the symlink at {} with a grok shim leader socket",
                    socket_path.display()
                );
            }
            if !file_type.is_socket() {
                bail!(
                    "{} exists and is not a unix socket; refusing to replace it",
                    socket_path.display()
                );
            }
            std::fs::remove_file(socket_path).with_context(|| {
                format!(
                    "removing the stale grok shim socket {}",
                    socket_path.display()
                )
            })?;
            tracing::debug!(
                target: LOG_TARGET,
                socket = %socket_path.display(),
                "removed a stale grok shim leader socket under the exclusive leader lock"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "inspecting the grok shim socket path {}",
                    socket_path.display()
                )
            });
        }
    }
    Ok(())
}

/// Publish the leader socket atomically: bind inside a private `0700` short
/// same-device staging ancestor (so binding never depends on the published
/// path length), force the socket `0600` while it is still unreachable, then
/// `rename(2)` onto the published path.
pub(super) fn publish_listener(socket_path: &Path) -> Result<UnixListener> {
    let parent = socket_path.parent().ok_or_else(|| {
        anyhow!(
            "the grok shim leader socket path {} has no parent directory",
            socket_path.display()
        )
    })?;
    ensure_socket_parent(parent).with_context(|| {
        format!(
            "creating the grok shim leader socket parent {}",
            parent.display()
        )
    })?;
    let staging = StagingDir::create(parent)?;
    let result = bind_and_publish(&staging.path, socket_path);
    staging.cleanup();
    result
}

fn bind_and_publish(staging_dir: &Path, socket_path: &Path) -> Result<UnixListener> {
    let staged_socket = staging_dir.join(STAGED_SOCKET_NAME);
    let listener = UnixListener::bind(&staged_socket).with_context(|| {
        format!(
            "binding the grok shim socket inside the staging ancestor {}",
            staging_dir.display()
        )
    })?;
    if let Err(error) = force_socket_mode(&staged_socket) {
        let _ = std::fs::remove_file(&staged_socket);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&staged_socket, socket_path) {
        let _ = std::fs::remove_file(&staged_socket);
        return Err(error).with_context(|| {
            format!(
                "publishing the grok shim socket at {}",
                socket_path.display()
            )
        });
    }
    Ok(listener)
}

fn force_socket_mode(socket: &Path) -> Result<()> {
    std::fs::set_permissions(socket, Permissions::from_mode(PRIVATE_FILE_MODE)).with_context(
        || {
            format!(
                "forcing mode 0600 on the grok shim socket {}",
                socket.display()
            )
        },
    )?;
    let mode = std::fs::metadata(socket)
        .with_context(|| {
            format!(
                "reading the mode of the grok shim socket {}",
                socket.display()
            )
        })?
        .permissions()
        .mode();
    if mode & 0o777 != PRIVATE_FILE_MODE {
        bail!(
            "the grok shim socket {} is mode {:o} instead of 0600",
            socket.display(),
            mode & 0o777
        );
    }
    Ok(())
}

/// The private short same-device staging ancestor for one publication.
pub(super) struct StagingDir {
    pub(super) path: PathBuf,
}

impl StagingDir {
    /// Same-device ancestors, shortest first, so `rename(2)` stays on one
    /// filesystem and the bind path stays short near the `sun_path` limit.
    pub(super) fn create(final_parent: &Path) -> Result<Self> {
        let mut last_error: Option<std::io::Error> = None;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(STAGING_DIR_MODE);
        for ancestor in staging_candidate_ancestors(final_parent) {
            for _ in 0..STAGING_ATTEMPTS {
                let candidate = ancestor.join(staging_dir_name());
                match builder.create(&candidate) {
                    Ok(()) => {
                        let mode = std::fs::metadata(&candidate)
                            .with_context(|| {
                                format!(
                                    "inspecting the grok shim staging ancestor {}",
                                    candidate.display()
                                )
                            })?
                            .permissions()
                            .mode();
                        if mode & 0o777 != STAGING_DIR_MODE {
                            let _ = std::fs::remove_dir(&candidate);
                            bail!(
                                "the grok shim staging ancestor {} is mode {:o} instead of 0700",
                                candidate.display(),
                                mode & 0o777
                            );
                        }
                        tracing::debug!(
                            target: LOG_TARGET,
                            staging = %candidate.display(),
                            "created a private staging ancestor for the grok shim socket"
                        );
                        return Ok(Self { path: candidate });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => {
                        last_error = Some(error);
                        break;
                    }
                }
            }
        }
        bail!(
            "no writable same-device staging ancestor for {}; last error: {}",
            final_parent.display(),
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "none".to_string())
        )
    }

    pub(super) fn cleanup(self) {
        if let Err(error) = std::fs::remove_dir(&self.path) {
            tracing::warn!(
                target: LOG_TARGET,
                %error,
                staging = %self.path.display(),
                "failed to remove the grok shim staging ancestor"
            );
        }
    }
}

/// Same-device non-root ancestors of `final_parent`, shortest first. `/` is
/// never a candidate (a privileged process must not publish shim directories
/// at the filesystem root); cross-device ancestors cannot `rename(2)` onto the
/// final path.
fn staging_candidate_ancestors(final_parent: &Path) -> Vec<PathBuf> {
    let parent_dev = match std::fs::metadata(final_parent) {
        Ok(metadata) => metadata.dev(),
        Err(_) => return Vec::new(),
    };
    let mut candidates = Vec::new();
    for ancestor in final_parent.ancestors() {
        if ancestor == Path::new("/") {
            continue;
        }
        if let Ok(metadata) = std::fs::metadata(ancestor) {
            if metadata.dev() == parent_dev {
                candidates.push(ancestor.to_path_buf());
            }
        }
    }
    candidates.sort_by_key(|path| path.as_os_str().len());
    candidates
}

fn staging_dir_name() -> String {
    format!(
        "{STAGING_DIR_PREFIX}{}",
        &Uuid::new_v4().simple().to_string()[..8]
    )
}
