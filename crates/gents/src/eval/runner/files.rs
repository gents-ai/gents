use std::io::Write;
use std::path::Path;

use serde::Serialize;

use crate::eval::documents::EvalError;

type Result<T, E = EvalError> = std::result::Result<T, E>;

/// Eval snapshots may contain private prompts and documents, so replacements
/// keep the staging file's owner-only permissions. The parent must already
/// exist: a run removed while its loop is active must stay removed.
pub(super) fn write_json_atomically(path: &Path, value: &impl Serialize) -> Result<()> {
    let dir = path.parent().ok_or_else(|| EvalError::NoParentDirectory {
        path: path.to_path_buf(),
    })?;
    let mut staged =
        tempfile::NamedTempFile::new_in(dir).map_err(|source| EvalError::StageSnapshot {
            path: path.to_path_buf(),
            source,
        })?;
    let encoded = serde_json::to_vec_pretty(value).map_err(|source| EvalError::EncodeSnapshot {
        path: path.to_path_buf(),
        source,
    })?;
    staged
        .write_all(&encoded)
        .map_err(|source| EvalError::WriteFile {
            path: path.to_path_buf(),
            source,
        })?;
    staged
        .persist(path)
        .map_err(|source| EvalError::ReplaceSnapshot {
            path: path.to_path_buf(),
            source,
        })?;
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
            fn serialize<S>(&self, _: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
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
