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
