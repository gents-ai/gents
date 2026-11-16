#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::time::Duration;

use gents_store::local::home::init_config_path;

/// Off unix there is no device and inode, so only a missing home counts as
/// replaced there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HomeIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    /// Whether the home held its settings file when the watch began: a delete
    /// the running server outraces (it recreates files as they go) can leave
    /// the directory itself in place, so its emptied contents count as gone.
    initialized: bool,
}

impl HomeIdentity {
    #[cfg(unix)]
    fn of(metadata: &std::fs::Metadata, initialized: bool) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            initialized,
        }
    }

    #[cfg(not(unix))]
    fn of(_metadata: &std::fs::Metadata, initialized: bool) -> Self {
        Self { initialized }
    }
}

/// What [`hold_home`] keeps open. Off unix there is no inode to pin, and
/// opening a directory there can be refused.
#[cfg(unix)]
pub(crate) struct HeldHome {
    _directory: std::fs::File,
}
#[cfg(not(unix))]
pub(crate) struct HeldHome;

/// Opens `home`, following symlinks, and records its identity. Keep the
/// handle open while watching: a held directory's inode cannot be reused, and
/// ext4 reuses a freed inode for a directory recreated at the same path.
#[cfg(unix)]
pub(crate) fn hold_home(home: &Path) -> std::io::Result<(HeldHome, HomeIdentity)> {
    let directory = std::fs::File::open(home)?;
    let initialized = init_config_path(home).try_exists()?;
    let identity = HomeIdentity::of(&directory.metadata()?, initialized);
    Ok((
        HeldHome {
            _directory: directory,
        },
        identity,
    ))
}

#[cfg(not(unix))]
pub(crate) fn hold_home(home: &Path) -> std::io::Result<(HeldHome, HomeIdentity)> {
    let initialized = init_config_path(home).try_exists()?;
    Ok((
        HeldHome,
        HomeIdentity::of(&std::fs::metadata(home)?, initialized),
    ))
}

/// Whether `home` is gone, names a different directory than `identity`, or
/// lost the settings file it had.
pub(crate) async fn home_replaced(home: &Path, identity: HomeIdentity) -> bool {
    replaced(tokio::fs::metadata(home).await, identity)
        || (identity.initialized
            && matches!(
                tokio::fs::try_exists(init_config_path(home)).await,
                Ok(false)
            ))
}

/// Only a missing path or a different directory is a replacement. Any other
/// error (EACCES, EIO, ESTALE) keeps the server and its shutdown flush: exit
/// 68 is never restarted.
fn replaced(metadata: std::io::Result<std::fs::Metadata>, identity: HomeIdentity) -> bool {
    match metadata {
        Ok(metadata) => HomeIdentity::of(&metadata, identity.initialized) != identity,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            true
        }
        Err(error) => {
            tracing::debug!(%error, "home identity check failed; treating the home as present");
            false
        }
    }
}

/// Resolves once [`home_replaced`]. A server that outlives its home must exit
/// without its shutdown flush: an old server's shutdown wrote `data/sst` and
/// `data/wal` files into the new store created at the same path, which the new
/// store's key could not read (spike R2b).
pub(crate) async fn home_gone(home: &Path, identity: HomeIdentity) {
    while !home_replaced(home, identity).await {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Exits at once, so no shutdown flush or `Drop` writes into the path.
pub(crate) fn exit_home_gone(home: &Path) -> ! {
    eprintln!(
        "Error: {} was deleted or replaced while the server ran",
        home.display()
    );
    std::process::exit(gents_service::native_service::HOME_GONE_EXIT_CODE)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use tokio::time::timeout;

    #[tokio::test]
    async fn the_home_is_replaced_when_missing_or_a_different_directory() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let (_held, identity) = hold_home(&home).unwrap();
        assert!(!home_replaced(&home, identity).await, "same directory");
        std::fs::rename(&home, temp.path().join("home.old")).unwrap();
        assert!(home_replaced(&home, identity).await, "missing");
        std::fs::create_dir(&home).unwrap();
        #[cfg(unix)]
        assert!(home_replaced(&home, identity).await, "replaced");
        for kind in [
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::Other,
        ] {
            assert!(!replaced(Err(kind.into()), identity), "{kind:?}");
        }
        assert!(replaced(
            Err(std::io::ErrorKind::NotADirectory.into()),
            identity
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_replaced_home_is_gone() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let (_held, identity) = hold_home(&home).unwrap();
        std::fs::rename(&home, temp.path().join("home.old")).unwrap();
        std::fs::create_dir(&home).unwrap();
        timeout(Duration::from_secs(3), home_gone(&home, identity))
            .await
            .expect("noticed");
    }

    #[tokio::test]
    async fn an_emptied_home_whose_directory_survives_is_gone() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let (_held, uninitialized) = hold_home(&home).unwrap();
        std::fs::write(init_config_path(&home), "{}").unwrap();
        let (_held, identity) = hold_home(&home).unwrap();
        assert!(
            !home_replaced(&home, identity).await,
            "initialized and present"
        );
        std::fs::remove_file(init_config_path(&home)).unwrap();
        assert!(home_replaced(&home, identity).await, "settings deleted");
        assert!(
            !home_replaced(&home, uninitialized).await,
            "a home served before its settings existed is watched by directory only"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_home_follows_its_target() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let (_held, identity) = hold_home(&link).unwrap();
        assert_eq!(identity, hold_home(&target).unwrap().1);
        std::fs::remove_dir(&target).unwrap();
        timeout(Duration::from_secs(3), home_gone(&link, identity))
            .await
            .expect("noticed");
    }
}
