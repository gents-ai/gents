use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use crypto::Key;
use identity::RawIdentity;

/// An existing identity key that group or other users can access. It is
/// refused, never repaired: a key that was readable by others may already
/// be exposed, so loading it (or tightening its mode in place) would keep
/// using a possibly compromised identity. Typed so hosts can route the
/// refusal (for example to a fresh start) instead of matching its message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("identity key {} has insecure permissions {mode:o}; remove group/other access before loading it", path.display())]
pub struct InsecureKeyPermissions {
    pub path: PathBuf,
    pub mode: u32,
}

/// Loads an existing file-backed Ed25519 identity without creating a key or
/// directory. On Unix the opened inode must be a regular file inaccessible to
/// group and other users; a symlink is never followed.
pub fn load_file_identity(path: &Path) -> Result<RawIdentity> {
    read_existing(path)?
        .ok_or_else(|| anyhow::anyhow!("identity key does not exist at {}", path.display()))
}

/// Creates a private file-backed Ed25519 identity only when no key exists.
/// The key is published complete and without replacing a concurrent winner;
/// concurrent creators all adopt the same validated winning identity.
pub fn load_or_create_file_identity(path: &Path) -> Result<RawIdentity> {
    if let Some(identity) = read_existing(path)? {
        return Ok(identity);
    }

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        create_private_parent(parent)?;
    }

    let private_key = crypto::generate_ed25519().map_err(anyhow::Error::from)?;
    let bytes = private_key.raw();
    let mut staged = tempfile::NamedTempFile::new_in(parent.unwrap_or_else(|| Path::new(".")))
        .with_context(|| format!("staging identity key for {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        staged
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .with_context(|| format!("securing staged identity key for {}", path.display()))?;
    }
    staged
        .write_all(&bytes)
        .with_context(|| format!("writing staged identity key for {}", path.display()))?;
    staged
        .as_file()
        .sync_all()
        .with_context(|| format!("syncing staged identity key for {}", path.display()))?;

    match staged.persist_noclobber(path) {
        Ok(_) => RawIdentity::from_bytes(crypto::KeyType::Ed25519, &bytes)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("constructing identity from {}", path.display())),
        Err(error) if error.error.kind() == ErrorKind::AlreadyExists => load_file_identity(path),
        Err(error) => Err(error.error)
            .with_context(|| format!("persisting identity key to {}", path.display())),
    }
}

fn read_existing(path: &Path) -> Result<Option<RawIdentity>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("opening identity key {}", path.display()))
        }
    };
    validate_opened_key(&file, path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("reading identity key {}", path.display()))?;
    RawIdentity::from_bytes(crypto::KeyType::Ed25519, &bytes)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("loading identity from {}", path.display()))
        .map(Some)
}

fn validate_opened_key(file: &File, path: &Path) -> Result<()> {
    let metadata = file
        .metadata()
        .with_context(|| format!("inspecting identity key {}", path.display()))?;
    if !metadata.file_type().is_file() {
        bail!("identity key {} is not a regular file", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(InsecureKeyPermissions {
                path: path.to_path_buf(),
                mode: mode & 0o777,
            }
            .into());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_parent(parent: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .with_context(|| format!("creating key directory {}", parent.display()))
}

#[cfg(not(unix))]
fn create_private_parent(parent: &Path) -> Result<()> {
    fs::create_dir_all(parent)
        .with_context(|| format!("creating key directory {}", parent.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use identity::Identity as _;

    #[test]
    fn missing_load_has_no_filesystem_effects() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("missing");
        let path = parent.join("key");
        assert!(load_file_identity(&path).is_err());
        assert!(!parent.exists());
        assert!(!path.exists());
    }

    #[test]
    fn concurrent_creators_and_reloads_keep_one_did() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("private").join("nested").join("agent.key");
        let barrier = std::sync::Barrier::new(12);
        let dids = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..12)
                .map(|_| {
                    let barrier = &barrier;
                    let path = &path;
                    scope.spawn(move || {
                        barrier.wait();
                        load_or_create_file_identity(path)
                            .unwrap()
                            .did()
                            .unwrap()
                            .to_string()
                    })
                })
                .collect();
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(dids.iter().all(|did| did == &dids[0]));
        assert_eq!(
            load_file_identity(&path)
                .unwrap()
                .did()
                .unwrap()
                .to_string(),
            dids[0]
        );
        assert_eq!(
            load_or_create_file_identity(&path)
                .unwrap()
                .did()
                .unwrap()
                .to_string(),
            dids[0]
        );
    }

    #[cfg(unix)]
    #[test]
    fn new_directories_and_file_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = first.join("second");
        let path = second.join("agent.key");
        load_or_create_file_identity(&path).unwrap();
        for dir in [&first, &second] {
            assert_eq!(
                fs::metadata(dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_and_insecure_existing_key_are_rejected_without_rotation() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target.key");
        let link = root.path().join("link.key");
        let identity = load_or_create_file_identity(&target).unwrap();
        let did = identity.did().unwrap().to_string();
        symlink(&target, &link).unwrap();
        assert!(load_file_identity(&link).is_err());
        assert!(load_or_create_file_identity(&link).is_err());
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());

        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        let error = load_or_create_file_identity(&target).unwrap_err();
        assert!(format!("{error:#}").contains("insecure permissions"));
        assert_eq!(
            error.downcast_ref::<InsecureKeyPermissions>(),
            Some(&InsecureKeyPermissions {
                path: target.clone(),
                mode: 0o644
            })
        );
        assert!(load_file_identity(&target).is_err());
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o644
        );
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            load_file_identity(&target)
                .unwrap()
                .did()
                .unwrap()
                .to_string(),
            did
        );
    }

    #[cfg(unix)]
    #[test]
    fn directory_and_fifo_are_rejected_as_keys() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        assert!(load_file_identity(root.path()).is_err());

        let fifo = root.path().join("fifo.key");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // The pathname is NUL-terminated and remains alive for this call.
        let created = unsafe { libc::mkfifo(name.as_ptr(), 0o600) };
        assert_eq!(created, 0, "{}", std::io::Error::last_os_error());
        assert!(load_file_identity(&fifo).is_err());
        assert!(load_or_create_file_identity(&fifo).is_err());
    }
}
