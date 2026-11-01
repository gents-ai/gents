use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Result};

pub(super) fn validate_directory(dir: &Path) -> Result<()> {
    ensure!(dir.is_absolute(), "receipt directory must be absolute");
    ensure!(
        !std::fs::symlink_metadata(dir)?.file_type().is_symlink(),
        "receipt directory is a symlink"
    );
    let metadata = std::fs::metadata(dir)?;
    ensure!(metadata.is_dir(), "receipt destination is not a directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "receipt directory permits group/other access"
        );
        // Owner-only mode is not enough: a 0700 directory owned by another
        // user is still writable by that user, so the receipt would not be
        // private to this process.
        ensure!(
            owner_is_effective_user(metadata.uid()),
            "receipt directory is not owned by the effective user"
        );
    }
    #[cfg(not(unix))]
    anyhow::bail!("private receipt directory validation requires Unix permissions");
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    ensure!(
        !dir.canonicalize()?.starts_with(checkout),
        "receipt directory must be outside checkout"
    );
    Ok(())
}

/// The private artifact directory must belong to the user running the export.
#[cfg(unix)]
fn owner_is_effective_user(uid: u32) -> bool {
    uid == unsafe { libc::geteuid() }
}

pub(super) fn publish_receipt(dir: &Path, scenario: &str, bytes: &[u8]) -> Result<PathBuf> {
    validate_directory(dir)?;
    validate_scenario(scenario)?;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    let path = dir.join(format!("{scenario}.json"));
    file.persist_noclobber(&path).map_err(|error| error.error)?;
    std::fs::File::open(dir)?.sync_all()?;
    Ok(path)
}

pub(super) fn validate_scenario(scenario: &str) -> Result<()> {
    ensure!(
        !scenario.is_empty()
            && scenario.len() <= 80
            && scenario
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
        "receipt scenario must be a short filename stem"
    );
    Ok(())
}
