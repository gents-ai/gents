use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use crypto::Key;
use identity::RawIdentity;

use super::IdentityError;

type Result<T, E = IdentityError> = std::result::Result<T, E>;

/// Refused, never repaired: a key others could read may already be exposed.
/// Typed so hosts can route the refusal instead of matching its message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("identity key {} has insecure permissions {mode:o}; remove group/other access before loading it", path.display())]
pub struct InsecureKeyPermissions {
    pub path: PathBuf,
    pub mode: u32,
}

/// Loads an existing file-backed Ed25519 identity without creating a key.
/// The opened inode must be a regular file inaccessible to group and other
/// users; a symlink is never followed.
pub fn load_file_identity(path: &Path) -> Result<RawIdentity> {
    read_existing(path)?.ok_or_else(|| IdentityError::KeyMissing {
        path: path.to_path_buf(),
    })
}

/// Creates the identity only when no key exists. Concurrent creators all
/// adopt the same validated winning identity; a published key is never replaced.
pub fn load_or_create_file_identity(path: &Path) -> Result<RawIdentity> {
    if let Some(identity) = read_existing(path)? {
        return Ok(identity);
    }

    let private_key = crypto::generate_ed25519()?;
    let bytes = private_key.raw();
    match publish_private_key_file(path, bytes) {
        Ok(true) => RawIdentity::from_bytes(crypto::KeyType::Ed25519, bytes).map_err(|source| {
            IdentityError::ConstructIdentity {
                path: path.to_path_buf(),
                source,
            }
        }),
        Ok(false) => load_file_identity(path),
        Err(error) => Err(error),
    }
}

/// Never replaces an existing file: `Ok(false)` reports another writer won.
pub(crate) fn publish_private_key_file(path: &Path, bytes: &[u8]) -> Result<bool> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        create_private_parent(parent)?;
    }
    let mut staged = tempfile::NamedTempFile::new_in(parent.unwrap_or_else(|| Path::new(".")))
        .map_err(|source| IdentityError::StageKey {
            path: path.to_path_buf(),
            source,
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        staged
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| IdentityError::SecureStagedKey {
                path: path.to_path_buf(),
                source,
            })?;
    }
    staged
        .write_all(bytes)
        .map_err(|source| IdentityError::WriteStagedKey {
            path: path.to_path_buf(),
            source,
        })?;
    staged
        .as_file()
        .sync_all()
        .map_err(|source| IdentityError::SyncStagedKey {
            path: path.to_path_buf(),
            source,
        })?;

    match staged.persist_noclobber(path) {
        Ok(_) => {
            #[cfg(unix)]
            fs::File::open(parent.unwrap_or_else(|| Path::new(".")))?.sync_all()?;
            Ok(true)
        }
        Err(error) if error.error.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(IdentityError::PersistKey {
            path: path.to_path_buf(),
            source: error.error,
        }),
    }
}

fn read_existing(path: &Path) -> Result<Option<RawIdentity>> {
    let Some(bytes) = read_private_key_file(path)? else {
        return Ok(None);
    };
    RawIdentity::from_bytes(crypto::KeyType::Ed25519, &bytes)
        .map(Some)
        .map_err(|source| IdentityError::LoadIdentity {
            path: path.to_path_buf(),
            source,
        })
}

/// Reads an owner-only key file without following a symlink; `None` when
/// absent. Group/other access is refused as [`InsecureKeyPermissions`].
pub(crate) fn read_private_key_file(path: &Path) -> Result<Option<Vec<u8>>> {
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
        Err(source) => {
            return Err(IdentityError::OpenKey {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    validate_opened_key(&file, path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| IdentityError::ReadKey {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(Some(bytes))
}

fn validate_opened_key(file: &File, path: &Path) -> Result<()> {
    let metadata = file
        .metadata()
        .map_err(|source| IdentityError::InspectKey {
            path: path.to_path_buf(),
            source,
        })?;
    if !metadata.file_type().is_file() {
        return Err(IdentityError::KeyNotRegular {
            path: path.to_path_buf(),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(IdentityError::InsecureKey(InsecureKeyPermissions {
                path: path.to_path_buf(),
                mode: mode & 0o777,
            }));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_parent(parent: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let missing: Vec<_> = parent
        .ancestors()
        .take_while(|path| !path.as_os_str().is_empty() && !path.exists())
        .map(Path::to_path_buf)
        .collect();
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|source| IdentityError::CreateKeyDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
    for created in missing {
        let containing = created
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::File::open(containing)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn create_private_parent(parent: &Path) -> Result<()> {
    fs::create_dir_all(parent).map_err(|source| IdentityError::CreateKeyDirectory {
        path: parent.to_path_buf(),
        source,
    })
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
        assert!(error.to_string().contains("insecure permissions"));
        assert!(matches!(
            &error,
            IdentityError::InsecureKey(key)
                if *key == InsecureKeyPermissions {
                    path: target.clone(),
                    mode: 0o644
                }
        ));
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
        let created = unsafe { libc::mkfifo(name.as_ptr(), 0o600) };
        assert_eq!(created, 0, "{}", std::io::Error::last_os_error());
        assert!(load_file_identity(&fifo).is_err());
        assert!(load_or_create_file_identity(&fifo).is_err());
    }
}
