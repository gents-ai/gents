//! File-recorded installs of assets and plugins packs.
//!
//! Those two pack kinds write no document and open no node, so `gents pack
//! remove` cannot look them up in `PackInstallation`. Each successful install
//! instead writes one record here, at
//! `<home>/pack-installs/<namespace>/<name>.json`, replaced whole on
//! reinstall or upgrade. `gents pack remove` checks for this record before it
//! ever resolves an owner or opens a node.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{is_valid_pack_name, InstalledPackPlugin, PackKind};

/// One assets or plugins pack install, recorded on the filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomePackInstall {
    pub coordinate: String,
    pub version: String,
    pub digest: String,
    pub kind: PackKind,
    /// The pack's materialized asset directory, relative to the home:
    /// `packs/<name>/<hex>`.
    pub assets: String,
    pub plugins: Vec<InstalledPackPlugin>,
    pub installed_at: String,
}

/// Splits `coordinate` into a namespace and name, refusing anything that
/// would not also pass [`is_valid_pack_name`] on both halves; a coordinate
/// reaches the filesystem as two path components; without this check `../x`
/// or `a/../b` would write or read outside the pack-installs directory.
fn checked_coordinate(coordinate: &str) -> Result<(&str, &str)> {
    let (namespace, name) = crate::pack_registry::split_pack_coordinate(coordinate);
    anyhow::ensure!(
        is_valid_pack_name(namespace) && is_valid_pack_name(name),
        "invalid pack coordinate {coordinate:?}"
    );
    Ok((namespace, name))
}

fn record_path(home: &Path, coordinate: &str) -> Result<PathBuf> {
    let (namespace, name) = checked_coordinate(coordinate)?;
    Ok(home
        .join(crate::home::PACK_INSTALLS_DIR_NAME)
        .join(namespace)
        .join(format!("{name}.json")))
}

/// Writes `record`, replacing any prior record for the same coordinate.
pub fn write_home_install(home: &Path, record: &HomePackInstall) -> Result<()> {
    let path = record_path(home, &record.coordinate)?;
    let parent = path.parent().context("pack install record has no parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let bytes = serde_json::to_vec_pretty(record).context("encoding the pack install record")?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("staging a pack install record in {}", parent.display()))?;
    std::io::Write::write_all(&mut staged, &bytes).context("writing the staged record")?;
    staged
        .as_file()
        .sync_all()
        .context("syncing the staged record")?;
    staged
        .persist(&path)
        .map_err(|error| error.error)
        .with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .with_context(|| format!("syncing {}", parent.display()))?;
    Ok(())
}

/// The file-recorded install for `coordinate`, or `None` if there is none.
pub fn read_home_install(home: &Path, coordinate: &str) -> Result<Option<HomePackInstall>> {
    let path = record_path(home, coordinate)?;
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", path.display()))
            .map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

/// Removes the file-recorded install for `coordinate`, if any.
pub fn forget_home_install(home: &Path, coordinate: &str) -> Result<()> {
    let path = record_path(home, coordinate)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
    }
}

/// Every file-recorded install under `home`, sorted by coordinate.
pub fn list_home_installs(home: &Path) -> Result<Vec<HomePackInstall>> {
    let root = home.join(crate::home::PACK_INSTALLS_DIR_NAME);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for namespace_entry in
        std::fs::read_dir(&root).with_context(|| format!("reading {}", root.display()))?
    {
        let namespace_path = namespace_entry?.path();
        if !namespace_path.is_dir() {
            continue;
        }
        for record_entry in std::fs::read_dir(&namespace_path)
            .with_context(|| format!("reading {}", namespace_path.display()))?
        {
            let path = record_entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let bytes =
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            records.push(
                serde_json::from_slice::<HomePackInstall>(&bytes)
                    .with_context(|| format!("parsing {}", path.display()))?,
            );
        }
    }
    records.sort_by(|a, b| a.coordinate.cmp(&b.coordinate));
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(coordinate: &str) -> HomePackInstall {
        HomePackInstall {
            coordinate: coordinate.to_owned(),
            version: "1.0.0".into(),
            digest: format!("sha256:{}", "a".repeat(64)),
            kind: PackKind::Assets,
            assets: "packs/subject_pack/aaaa".into(),
            plugins: vec![],
            installed_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn round_trips_and_replaces_on_reinstall() {
        let home = tempfile::tempdir().unwrap();
        write_home_install(home.path(), &sample("gents/mailbox")).unwrap();
        assert_eq!(
            read_home_install(home.path(), "gents/mailbox")
                .unwrap()
                .unwrap(),
            sample("gents/mailbox")
        );

        let mut upgraded = sample("gents/mailbox");
        upgraded.version = "1.1.0".into();
        write_home_install(home.path(), &upgraded).unwrap();
        assert_eq!(
            read_home_install(home.path(), "gents/mailbox")
                .unwrap()
                .unwrap(),
            upgraded
        );
    }

    #[test]
    fn list_is_sorted_and_ignores_non_json_files() {
        let home = tempfile::tempdir().unwrap();
        write_home_install(home.path(), &sample("zeta/tools")).unwrap();
        write_home_install(home.path(), &sample("acme/tools")).unwrap();
        let stray = home
            .path()
            .join(crate::home::PACK_INSTALLS_DIR_NAME)
            .join("acme");
        std::fs::write(stray.join("README.md"), b"not a record").unwrap();

        let listed = list_home_installs(home.path()).unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|r| r.coordinate.clone())
                .collect::<Vec<_>>(),
            vec!["acme/tools".to_owned(), "zeta/tools".to_owned()]
        );
    }

    #[test]
    fn an_escaping_coordinate_is_refused() {
        let home = tempfile::tempdir().unwrap();
        for coordinate in ["../x", "a/../b", "../../etc/passwd", "a/b/c"] {
            write_home_install(home.path(), &sample(coordinate))
                .expect_err(&format!("{coordinate:?} must be refused"));
        }
    }

    #[test]
    fn forget_then_read_returns_none() {
        let home = tempfile::tempdir().unwrap();
        write_home_install(home.path(), &sample("gents/mailbox")).unwrap();
        forget_home_install(home.path(), "gents/mailbox").unwrap();
        assert!(read_home_install(home.path(), "gents/mailbox")
            .unwrap()
            .is_none());
        // Forgetting an absent record is not an error.
        forget_home_install(home.path(), "gents/mailbox").unwrap();
    }
}
