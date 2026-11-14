use std::path::Path;

use anyhow::{ensure, Context, Result};
use rapidhash::RapidHashMap;

/// WorkspaceWrite cannot use inherited system temporary paths outside its write
/// boundary. Keep temporary files within the existing canonical sandbox root;
/// a pre-existing redirected directory is not authority to write elsewhere.
pub(super) fn prepare(root: &Path, environment: &mut RapidHashMap<String, String>) -> Result<()> {
    let root = root
        .canonicalize()
        .context("resolving command write root")?;
    let directory = root.join(".gents-tmp");
    std::fs::create_dir_all(&directory)
        .context("preparing workspace command temporary directory")?;
    let directory = directory
        .canonicalize()
        .context("resolving command temporary directory")?;
    ensure!(
        directory.starts_with(&root),
        "command temporary directory escapes the workspace write root"
    );
    for key in ["TMPDIR", "TMP", "TEMP"] {
        environment.insert(key.into(), directory.to_string_lossy().into_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_temporary_variables_share_an_existing_in_root_directory() {
        let root = tempfile::tempdir().unwrap();
        let mut environment = RapidHashMap::default();
        environment.insert("TMPDIR".into(), "/outside/system-temp".into());
        environment.insert("HOME".into(), "/unchanged/home".into());
        prepare(root.path(), &mut environment).unwrap();
        let path = Path::new(&environment["TMPDIR"]);
        assert!(path.is_dir());
        assert!(path.starts_with(root.path().canonicalize().unwrap()));
        assert_eq!(environment["TMP"], environment["TMPDIR"]);
        assert_eq!(environment["TEMP"], environment["TMPDIR"]);
        assert_eq!(environment["HOME"], "/unchanged/home");
        std::fs::write(path.join("profile"), "fixture").unwrap();
        prepare(root.path(), &mut environment).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn redirected_temporary_directory_is_rejected_without_environment_changes() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join(".gents-tmp")).unwrap();
        let mut environment = RapidHashMap::default();
        environment.insert("TMPDIR".into(), "unchanged".into());
        assert!(prepare(root.path(), &mut environment).is_err());
        assert_eq!(environment["TMPDIR"], "unchanged");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
