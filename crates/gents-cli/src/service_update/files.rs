use super::*;
use sha2::{Digest as _, Sha256};
use std::fs::{self, File};
use std::io::Read as _;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

pub(super) fn lock(path: &Path) -> Result<File> {
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    file.try_lock()
        .context("another updater owns this operation")?;
    Ok(file)
}
pub(super) fn submission_lock(path: &Path) -> Result<File> {
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    file.lock()?;
    Ok(file)
}
pub(super) fn read_json<T>(path: &Path) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_slice(&fs::read(path)?).with_context(|| format!("reading {}", path.display()))
}
pub(super) async fn read_json_async<T>(path: &Path) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_slice(&tokio::fs::read(path).await?)
        .with_context(|| format!("reading {}", path.display()))
}
pub(super) fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    crate::native_service::atomic_write(path, &serde_json::to_vec_pretty(value)?)?;
    File::open(path.parent().context("receipt directory")?)?.sync_all()?;
    Ok(())
}
pub(super) fn digest(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
pub(super) fn private_dir(path: &Path) -> Result<()> {
    fs::DirBuilder::new().recursive(true).create(path)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
pub(super) fn copy_executable(source: &Path, target: &Path) -> Result<()> {
    fs::copy(source, target)?;
    #[cfg(unix)]
    fs::set_permissions(target, fs::Permissions::from_mode(0o700))?;
    File::open(target)?.sync_all()?;
    Ok(())
}

/// Copies the existing owned home inventory only, while the caller holds the
/// real store lock. Symlinks are retained as links, never followed into other
/// homes; key permissions remain unchanged. Runtime locks and updater receipts
/// are excluded by the existing home inventory owner.
pub(super) fn cold_backup(plan: &Plan, directory: &Path) -> Result<()> {
    if directory.join("backup").is_dir() {
        return Ok(());
    }
    let temp = tempfile::Builder::new()
        .prefix("backup-")
        .tempdir_in(directory)?;
    for path in backup_inventory(&plan.home)? {
        // The claimed store may live on another volume via a data-directory
        // symlink. Back up its bytes, not only the pointer to the live store.
        let source = if path == gents::home::default_data_dir(&plan.home) {
            fs::canonicalize(&path)?
        } else {
            path.clone()
        };
        copy_tree(
            &source,
            &temp
                .path()
                .join(path.file_name().context("owned filename")?),
        )?;
    }
    fs::write(
        temp.path().join("service-definition"),
        &plan.prior_definition,
    )?;
    write_json(&temp.path().join("network.json"), &plan.prior_network)?;
    File::open(temp.path())?.sync_all()?;
    fs::rename(temp.keep(), directory.join("backup"))?;
    File::open(directory)?.sync_all()?;
    Ok(())
}
pub(super) fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(fs::read_link(source)?, target)?;
        #[cfg(not(unix))]
        anyhow::bail!("managed backups require macOS or Linux");
    } else if meta.is_dir() {
        fs::create_dir(target)?;
        fs::set_permissions(target, meta.permissions())?;
        for entry in fs::read_dir(source)? {
            let path = entry?.path();
            copy_tree(
                &path,
                &target.join(path.file_name().context("backup entry")?),
            )?;
        }
        File::open(target)?.sync_all()?;
    } else if meta.is_file() {
        fs::copy(source, target)?;
        fs::set_permissions(target, meta.permissions())?;
        File::open(target)?.sync_all()?;
    } else {
        anyhow::bail!("cannot back up special file {}", source.display());
    }
    Ok(())
}

/// Reject shell scripts and foreign executable formats before any candidate
/// code runs. The authenticated digest establishes provenance; the executable's
/// read-only probe additionally checks its architecture and handoff protocol.
pub(super) fn validate_native_executable(path: &Path) -> Result<()> {
    let mut header = [0u8; 20];
    File::open(path)?.read_exact(&mut header)?;
    let native = match std::env::consts::OS {
        "macos" => matches!(
            &header[..4],
            [0xcf, 0xfa, 0xed, 0xfe] | [0xca, 0xfe, 0xba, 0xbe] | [0xca, 0xfe, 0xba, 0xbf]
        ),
        "linux" => {
            &header[..4] == b"\x7fELF"
                && header[4] == 2
                && header[5] == 1
                && match std::env::consts::ARCH {
                    "aarch64" => u16::from_le_bytes([header[18], header[19]]) == 183,
                    "x86_64" => u16::from_le_bytes([header[18], header[19]]) == 62,
                    _ => false,
                }
        }
        _ => false,
    };
    anyhow::ensure!(
        native,
        "candidate is not a native Gents executable for this platform"
    );
    Ok(())
}

pub(super) fn backup_inventory(home: &Path) -> Result<Vec<PathBuf>> {
    let owned = gents::home::home_entries(home, &[])?.owned;
    let data = gents::home::default_data_dir(home);
    anyhow::ensure!(data.is_dir() && owned.contains(&data), "managed cold backup requires an exclusively owned default data directory; use external maintenance for shared or missing stores");
    Ok(owned)
}

/// Native file notifications avoid polling shared receipts while the runtime
/// and independently supervised helper exchange acknowledgements.
pub(super) struct Changes {
    _watcher: notify::RecommendedWatcher,
    receive: tokio::sync::mpsc::Receiver<notify::Result<notify::Event>>,
}
impl Changes {
    pub(super) async fn watch(path: &Path) -> Result<Self> {
        Self::watch_mode(path, false).await
    }
    pub(super) async fn operations(path: &Path) -> Result<Self> {
        Self::watch_mode(path, true).await
    }
    async fn watch_mode(path: &Path, operations: bool) -> Result<Self> {
        use notify::Watcher as _;
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || -> Result<Self> {
            let (send, receive) = tokio::sync::mpsc::channel(1);
            let mut watcher =
                notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                    if event
                        .as_ref()
                        .is_ok_and(|event| matches!(event.kind, notify::EventKind::Access(_)))
                    {
                        return;
                    }
                    let _ = send.try_send(event);
                })?;
            watcher.watch(&path, notify::RecursiveMode::NonRecursive)?;
            if operations {
                for entry in fs::read_dir(&path)? {
                    let entry = entry?;
                    if entry
                        .file_name()
                        .to_str()
                        .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
                        && entry.path().is_dir()
                    {
                        watcher.watch(&entry.path(), notify::RecursiveMode::NonRecursive)?;
                    }
                }
            }
            Ok(Self {
                _watcher: watcher,
                receive,
            })
        })
        .await?
    }
    pub(super) async fn changed(&mut self) -> Result<()> {
        self.receive
            .recv()
            .await
            .context("operation file watcher stopped")??;
        Ok(())
    }
}

pub(super) fn check_backup_capacity(home: &Path, destination: &Path) -> Result<()> {
    fn size(path: &Path) -> Result<u64> {
        let meta = fs::symlink_metadata(path)?;
        if meta.is_dir() {
            fs::read_dir(path)?.try_fold(0u64, |sum, entry| {
                sum.checked_add(size(&entry?.path())?)
                    .context("backup size overflow")
            })
        } else if meta.is_file() || meta.file_type().is_symlink() {
            Ok(meta.len())
        } else {
            anyhow::bail!("cannot back up special file {}", path.display())
        }
    }
    let mut bytes = 0u64;
    for path in backup_inventory(home)? {
        let source = if path == gents::home::default_data_dir(home) {
            fs::canonicalize(path)?
        } else {
            path
        };
        bytes = bytes
            .checked_add(size(&source)?)
            .context("backup size overflow")?;
    }
    #[cfg(not(unix))]
    let _ = (bytes, destination);
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let path = std::ffi::CString::new(destination.as_os_str().as_bytes())?;
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: the path is NUL terminated and statvfs initializes stats on success.
        anyhow::ensure!(
            unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } == 0,
            "backup capacity check failed: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: statvfs succeeded and initialized stats.
        let stats = unsafe { stats.assume_init() };
        let available = u128::from(stats.f_bavail) * u128::from(stats.f_frsize);
        let required = u128::from(bytes) * 2 + 64 * 1024 * 1024;
        anyhow::ensure!(available >= required, "insufficient space for cold backup: need {required} bytes, available {available}; runtime unchanged");
    }
    Ok(())
}
