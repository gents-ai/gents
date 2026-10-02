use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

/// Eval snapshots may contain private prompts and documents, so replacements
/// keep the staging file's owner-only permissions. The parent must already
/// exist: a run removed while its loop is active must stay removed.
pub(super) fn write_json_atomically(path: &Path, value: &impl Serialize) -> Result<()> {
    let dir = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    let mut staged = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("staging {}", path.display()))?;
    let encoded =
        serde_json::to_vec_pretty(value).with_context(|| format!("encoding {}", path.display()))?;
    staged
        .write_all(&encoded)
        .with_context(|| format!("writing {}", path.display()))?;
    staged
        .persist(path)
        .with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_failed_encoding_preserves_the_previous_snapshot_and_cleans_up_staging() {
        struct Invalid;
        impl Serialize for Invalid {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("encoding failed"))
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("definition.json");
        std::fs::write(&path, b"previous").unwrap();
        assert!(write_json_atomically(&path, &Invalid).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"previous");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_snapshot_does_not_recreate_a_removed_run_directory() {
        let dir = tempfile::tempdir().unwrap();
        let removed = dir.path().join("removed");
        assert!(write_json_atomically(&removed.join("progress.json"), &json!({})).is_err());
        assert!(!removed.exists());
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_replacements_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("definition.json");
        std::fs::write(&path, b"previous").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_json_atomically(&path, &json!({})).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
