use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{is_valid_pack_name, InstalledPackPlugin, PackKind};
use crate::pack::archive::PackArchive;
use crate::store::local::file_lock::FileLock;

/// Who installed a pack: the operator through the CLI, or the model through
/// self-configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstalledVia {
    Operator,
    SelfConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomePackInstall {
    pub coordinate: String,
    pub version: String,
    pub digest: String,
    pub kind: PackKind,
    pub assets: String,
    /// The materialized asset directories of the dependencies this install ran,
    /// as `(coordinate, assets)` with `assets` spelled as [`Self::assets`].
    /// Dependencies write no record of their own, so the dependent's record
    /// is where their assets are found. Empty when none.
    #[serde(default)]
    pub dependency_assets: Vec<(String, String)>,
    pub plugins: Vec<InstalledPackPlugin>,
    pub installed_at: String,
    /// The UX plugins the pack declared, kept verbatim so the desktop can
    /// list and gate them without opening the archive. Absent on a record
    /// written before the field existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ux: Vec<crate::pack::PackUxPlugin>,
    /// Absent on records written before provenance was recorded; read through
    /// [`recorded_provenance`].
    #[serde(default)]
    pub installed_via: Option<InstalledVia>,
}

/// Lean `provenanceOf`. `record` is `None` without a record, `Some(None)` for
/// a record without `installed_via`. The CLI was the only writer of legacy
/// assets and plugins records, so those read `Operator`; documents never had
/// one, and a missing record fails closed.
pub fn recorded_provenance(kind: PackKind, record: Option<Option<InstalledVia>>) -> InstalledVia {
    match (record, kind) {
        (Some(Some(via)), _) => via,
        (Some(None), PackKind::Assets | PackKind::Plugins) => InstalledVia::Operator,
        _ => InstalledVia::SelfConfig,
    }
}

/// Lean `recordAfterInstall`: the value the record is written with. An
/// explicit install writes `via`; a dependency install never lowers an
/// `Operator` record.
pub fn record_after_install_via(
    prior: Option<Option<InstalledVia>>,
    kind: PackKind,
    explicit: bool,
    via: InstalledVia,
) -> InstalledVia {
    if !explicit && recorded_provenance(kind, prior) == InstalledVia::Operator {
        InstalledVia::Operator
    } else {
        via
    }
}

/// Writes `record` with the provenance [`record_after_install_via`] decides,
/// replacing any prior record.
pub fn record_after_install(
    home: &Path,
    mut record: HomePackInstall,
    installed_via: InstalledVia,
    explicit: bool,
) -> Result<()> {
    let prior = read_home_install(home, &record.coordinate)?.map(|stored| stored.installed_via);
    record.installed_via = Some(record_after_install_via(
        prior,
        record.kind.clone(),
        explicit,
        installed_via,
    ));
    write_home_install(home, &record)
}

/// Materializes `pack` into the home's asset cache when it ships an asset
/// class (a distributable asset under a top-level snake_case directory),
/// whatever plugins are installed. Returns the cache directory relative to
/// `home`, as [`HomePackInstall::assets`] spells it, with the lease the caller
/// holds until the install record is written; `None` for a pack with no asset
/// class.
pub fn materialize_asset_pack(
    home: &Path,
    pack: &PackArchive,
) -> Result<Option<(String, FileLock)>> {
    let ships_class = pack.manifest().metadata.assets.iter().any(|path| {
        path.split_once('/').is_some_and(|(class, _)| {
            super::asset_path::is_snake_case_name(class) && super::is_distributable_asset_path(path)
        })
    });
    if !ships_class {
        return Ok(None);
    }
    let (root, lease) = super::materialize::cached(home, pack)?;
    let assets = root
        .strip_prefix(home)
        .unwrap_or(root.as_path())
        .to_string_lossy()
        .into_owned();
    Ok(Some((assets, lease)))
}

fn checked_coordinate(coordinate: &str) -> Result<(&str, &str)> {
    let (namespace, name) = crate::pack::registry::split_pack_coordinate(coordinate);
    anyhow::ensure!(
        is_valid_pack_name(namespace) && is_valid_pack_name(name),
        "invalid pack coordinate {coordinate:?}"
    );
    Ok((namespace, name))
}

fn record_path(home: &Path, coordinate: &str) -> Result<PathBuf> {
    let (namespace, name) = checked_coordinate(coordinate)?;
    Ok(home
        .join(crate::store::local::home::PACK_INSTALLS_DIR_NAME)
        .join(namespace)
        .join(format!("{name}.json")))
}

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

pub fn forget_home_install(home: &Path, coordinate: &str) -> Result<()> {
    let path = record_path(home, coordinate)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
    }
}

/// Every readable record under `home`, sorted by coordinate; a record that
/// does not parse (one a newer gents wrote) is skipped with a warning, so
/// one bad install cannot hide the rest from a listing.
pub fn list_readable_home_installs(home: &Path) -> Result<Vec<HomePackInstall>> {
    Ok(list_home_installs_with_skips(home)?.0)
}

/// A HomePackInstall file a listing skipped because it did not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedHomeInstall {
    pub path: PathBuf,
    pub reason: String,
}

/// Every readable record under `home`, sorted by coordinate, and every
/// record file that did not parse; a listing never fails on one bad file.
pub fn list_home_installs_with_skips(
    home: &Path,
) -> Result<(Vec<HomePackInstall>, Vec<SkippedHomeInstall>)> {
    let root = home.join(crate::store::local::home::PACK_INSTALLS_DIR_NAME);
    if !root.is_dir() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut records = Vec::new();
    let mut skipped = Vec::new();
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
            let parsed = std::fs::read(&path)
                .with_context(|| format!("reading {}", path.display()))
                .and_then(|bytes| {
                    serde_json::from_slice::<HomePackInstall>(&bytes)
                        .with_context(|| format!("parsing {}", path.display()))
                });
            match parsed {
                Ok(record) => records.push(record),
                Err(error) => {
                    let reason = format!("{error:#}");
                    tracing::warn!(path = %path.display(), error = %reason, "unreadable pack install record; skipped");
                    skipped.push(SkippedHomeInstall { path, reason });
                }
            }
        }
    }
    records.sort_by(|a, b| a.coordinate.cmp(&b.coordinate));
    Ok((records, skipped))
}

pub fn list_home_installs(home: &Path) -> Result<Vec<HomePackInstall>> {
    let root = home.join(crate::store::local::home::PACK_INSTALLS_DIR_NAME);
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
            dependency_assets: vec![],
            plugins: vec![],
            installed_at: "2026-01-01T00:00:00Z".into(),
            ux: vec![],
            installed_via: None,
        }
    }

    fn read_via(home: &Path, coordinate: &str) -> Option<Option<InstalledVia>> {
        read_home_install(home, coordinate)
            .unwrap()
            .map(|record| record.installed_via)
    }

    #[test]
    fn the_readable_listing_skips_a_record_it_cannot_parse_and_the_strict_one_fails() {
        let home = tempfile::tempdir().unwrap();
        write_home_install(home.path(), &sample("acme/tools")).unwrap();
        write_home_install(home.path(), &sample("zeta/tools")).unwrap();
        let newer = home
            .path()
            .join(crate::store::local::home::PACK_INSTALLS_DIR_NAME)
            .join("acme")
            .join("future.json");
        std::fs::write(&newer, b"{\"coordinate\": 7, \"from\": \"a newer gents\"}").unwrap();

        let readable = list_readable_home_installs(home.path()).unwrap();
        assert_eq!(
            readable
                .iter()
                .map(|r| r.coordinate.clone())
                .collect::<Vec<_>>(),
            vec!["acme/tools".to_owned(), "zeta/tools".to_owned()]
        );
        let strict = list_home_installs(home.path()).unwrap_err();
        assert!(format!("{strict:#}").contains("future.json"), "{strict:#}");
    }

    #[test]
    fn home_install_with_retired_kind_is_skipped() {
        let home = tempfile::tempdir().unwrap();
        write_home_install(home.path(), &sample("fixture/kept")).unwrap();
        let mut json = serde_json::to_value(sample("fixture/retired")).unwrap();
        json["kind"] = "retired_fixture".into();
        let retired = home
            .path()
            .join(crate::store::local::home::PACK_INSTALLS_DIR_NAME)
            .join("fixture")
            .join("retired.json");
        std::fs::write(&retired, serde_json::to_vec(&json).unwrap()).unwrap();

        let (records, skipped) = list_home_installs_with_skips(home.path()).unwrap();
        assert_eq!(
            records
                .iter()
                .map(|r| r.coordinate.as_str())
                .collect::<Vec<_>>(),
            vec!["fixture/kept"]
        );
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        assert_eq!(skipped[0].path, retired);
        assert!(
            skipped[0].reason.contains("unknown variant"),
            "{}",
            skipped[0].reason
        );
        assert_eq!(list_readable_home_installs(home.path()).unwrap(), records);
        let strict = list_home_installs(home.path()).unwrap_err();
        assert!(format!("{strict:#}").contains("retired.json"), "{strict:#}");
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
    fn legacy_record_without_installed_via_reads_operator() {
        let home = tempfile::tempdir().unwrap();
        write_home_install(home.path(), &sample("gents/mailbox")).unwrap();
        let path = record_path(home.path(), "gents/mailbox").unwrap();
        let mut json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        json.as_object_mut().unwrap().remove("installed_via");
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();

        let record = read_home_install(home.path(), "gents/mailbox")
            .unwrap()
            .unwrap();
        assert_eq!(record.installed_via, None);
        assert_eq!(
            recorded_provenance(record.kind, Some(record.installed_via)),
            InstalledVia::Operator
        );
    }

    #[test]
    fn record_without_dependency_assets_reads_empty() {
        let home = tempfile::tempdir().unwrap();
        let mut record = sample("gents/mailbox");
        record.dependency_assets =
            vec![("gents/dep".into(), "packs/.materialized/gents/dep/a".into())];
        write_home_install(home.path(), &record).unwrap();
        let path = record_path(home.path(), "gents/mailbox").unwrap();
        let mut json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        json.as_object_mut().unwrap().remove("dependency_assets");
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();

        let record = read_home_install(home.path(), "gents/mailbox")
            .unwrap()
            .unwrap();
        assert!(record.dependency_assets.is_empty());
    }

    #[test]
    fn missing_documents_record_reads_self_config() {
        let home = tempfile::tempdir().unwrap();
        assert!(read_home_install(home.path(), "gents/docs")
            .unwrap()
            .is_none());
        assert_eq!(
            recorded_provenance(PackKind::Documents, None),
            InstalledVia::SelfConfig
        );
    }

    #[test]
    fn dependency_never_lowers_provenance() {
        let home = tempfile::tempdir().unwrap();
        record_after_install(
            home.path(),
            sample("gents/mailbox"),
            InstalledVia::Operator,
            true,
        )
        .unwrap();
        record_after_install(
            home.path(),
            sample("gents/mailbox"),
            InstalledVia::SelfConfig,
            false,
        )
        .unwrap();
        assert_eq!(
            read_via(home.path(), "gents/mailbox"),
            Some(Some(InstalledVia::Operator))
        );
    }

    #[test]
    fn explicit_self_config_install_lowers_the_record() {
        let home = tempfile::tempdir().unwrap();
        record_after_install(
            home.path(),
            sample("gents/mailbox"),
            InstalledVia::Operator,
            true,
        )
        .unwrap();
        record_after_install(
            home.path(),
            sample("gents/mailbox"),
            InstalledVia::SelfConfig,
            true,
        )
        .unwrap();
        assert_eq!(
            read_via(home.path(), "gents/mailbox"),
            Some(Some(InstalledVia::SelfConfig))
        );
    }

    #[test]
    fn recorded_provenance_is_lean_provenance_of() {
        use InstalledVia::{Operator, SelfConfig};
        for kind in [PackKind::Documents, PackKind::Assets, PackKind::Plugins] {
            for via in [Operator, SelfConfig] {
                assert_eq!(recorded_provenance(kind.clone(), Some(Some(via))), via);
            }
            assert_eq!(recorded_provenance(kind, None), SelfConfig);
        }
        assert_eq!(recorded_provenance(PackKind::Assets, Some(None)), Operator);
        assert_eq!(recorded_provenance(PackKind::Plugins, Some(None)), Operator);
        assert_eq!(
            recorded_provenance(PackKind::Documents, Some(None)),
            SelfConfig
        );
    }

    #[test]
    fn record_after_install_via_is_lean_record_after_install() {
        use InstalledVia::{Operator, SelfConfig};
        for via in [Operator, SelfConfig] {
            for prior in [
                None,
                Some(None),
                Some(Some(Operator)),
                Some(Some(SelfConfig)),
            ] {
                for kind in [PackKind::Documents, PackKind::Assets, PackKind::Plugins] {
                    assert_eq!(
                        record_after_install_via(prior, kind, true, via),
                        via,
                        "an explicit install writes its own provenance"
                    );
                }
            }
        }
        assert_eq!(
            record_after_install_via(Some(Some(Operator)), PackKind::Documents, false, SelfConfig),
            Operator
        );
        assert_eq!(
            record_after_install_via(Some(None), PackKind::Assets, false, SelfConfig),
            Operator,
            "a legacy assets record reads operator"
        );
        assert_eq!(
            record_after_install_via(Some(None), PackKind::Documents, false, SelfConfig),
            SelfConfig
        );
        assert_eq!(
            record_after_install_via(None, PackKind::Plugins, false, SelfConfig),
            SelfConfig
        );
        assert_eq!(
            record_after_install_via(Some(Some(SelfConfig)), PackKind::Assets, false, Operator),
            Operator
        );
    }
}
