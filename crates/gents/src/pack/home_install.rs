use gents_protocol::error_chain::ErrorChain;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum HomeInstallError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Materialize(#[from] super::materialize::MaterializeError),
    #[error("invalid pack coordinate {coordinate:?}")]
    InvalidCoordinate { coordinate: String },
    #[error("pack install record has no parent")]
    NoParent,
    #[error("creating {}", path.display())]
    Create {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("encoding the pack install record")]
    Encode(#[source] serde_json::Error),
    #[error("staging a pack install record in {}", path.display())]
    Stage {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("writing the staged record")]
    WriteStaged(#[source] std::io::Error),
    #[error("syncing the staged record")]
    SyncStaged(#[source] std::io::Error),
    #[error("writing {}", path.display())]
    Persist {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("syncing {}", path.display())]
    SyncDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("reading {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("removing {}", path.display())]
    Remove {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

type Result<T, E = HomeInstallError> = std::result::Result<T, E>;
use serde::{Deserialize, Serialize};

use super::{is_valid_pack_name, InstalledPackPlugin, PackKind};
use crate::file_lock::FileLock;
use crate::pack_archive::PackArchive;

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
    /// Dependencies are graph packs and write no record of their own, so the
    /// dependent's record is where their assets are found. Empty when none.
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
/// one, and a missing record fails closed. A graph pack writes no record and
/// reads as documents do.
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
/// replacing any prior record. A graph pack writes no record.
pub fn record_after_install(
    home: &Path,
    mut record: HomePackInstall,
    installed_via: InstalledVia,
    explicit: bool,
) -> Result<()> {
    if record.kind == PackKind::Graph {
        return Ok(());
    }
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
    let (namespace, name) = crate::pack_registry::split_pack_coordinate(coordinate);
    if !(is_valid_pack_name(namespace) && is_valid_pack_name(name)) {
        return Err(HomeInstallError::InvalidCoordinate {
            coordinate: coordinate.to_owned(),
        });
    }
    Ok((namespace, name))
}

fn record_path(home: &Path, coordinate: &str) -> Result<PathBuf> {
    let (namespace, name) = checked_coordinate(coordinate)?;
    Ok(home
        .join(crate::home::PACK_INSTALLS_DIR_NAME)
        .join(namespace)
        .join(format!("{name}.json")))
}

pub fn write_home_install(home: &Path, record: &HomePackInstall) -> Result<()> {
    let path = record_path(home, &record.coordinate)?;
    let parent = path.parent().ok_or(HomeInstallError::NoParent)?;
    std::fs::create_dir_all(parent).map_err(|source| HomeInstallError::Create {
        path: parent.to_path_buf(),
        source,
    })?;
    let bytes = serde_json::to_vec_pretty(record).map_err(HomeInstallError::Encode)?;
    let mut staged =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| HomeInstallError::Stage {
            path: parent.to_path_buf(),
            source,
        })?;
    std::io::Write::write_all(&mut staged, &bytes).map_err(HomeInstallError::WriteStaged)?;
    staged
        .as_file()
        .sync_all()
        .map_err(HomeInstallError::SyncStaged)?;
    staged
        .persist(&path)
        .map_err(|error| HomeInstallError::Persist {
            path: path.clone(),
            source: error.error,
        })?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|source| HomeInstallError::SyncDir {
            path: parent.to_path_buf(),
            source,
        })?;
    Ok(())
}

fn read_record(path: &Path) -> Result<HomePackInstall> {
    let bytes = std::fs::read(path).map_err(|source| HomeInstallError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| HomeInstallError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

pub fn read_home_install(home: &Path, coordinate: &str) -> Result<Option<HomePackInstall>> {
    let path = record_path(home, coordinate)?;
    match std::fs::read(&path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|source| HomeInstallError::Parse {
                    path: path.clone(),
                    source,
                })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(HomeInstallError::Read { path, source }),
    }
}

pub fn forget_home_install(home: &Path, coordinate: &str) -> Result<()> {
    let path = record_path(home, coordinate)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(HomeInstallError::Remove { path, source }),
    }
}

fn read_dir(path: &Path) -> Result<std::fs::ReadDir> {
    std::fs::read_dir(path).map_err(|source| HomeInstallError::Read {
        path: path.to_path_buf(),
        source,
    })
}

/// Every readable record under `home`, sorted by coordinate; a record that
/// does not parse (one a newer gents wrote) is skipped with a warning, so
/// one bad install cannot hide the rest from a listing.
pub fn list_readable_home_installs(home: &Path) -> Result<Vec<HomePackInstall>> {
    let root = home.join(crate::home::PACK_INSTALLS_DIR_NAME);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for namespace_entry in read_dir(&root)? {
        let namespace_path = namespace_entry?.path();
        if !namespace_path.is_dir() {
            continue;
        }
        for record_entry in read_dir(&namespace_path)? {
            let path = record_entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            match read_record(&path) {
                Ok(record) => records.push(record),
                Err(error) => {
                    tracing::warn!(path = %path.display(), error = %ErrorChain(&error), "unreadable pack install record; skipped")
                }
            }
        }
    }
    records.sort_by(|a, b| a.coordinate.cmp(&b.coordinate));
    Ok(records)
}

pub fn list_home_installs(home: &Path) -> Result<Vec<HomePackInstall>> {
    let root = home.join(crate::home::PACK_INSTALLS_DIR_NAME);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for namespace_entry in read_dir(&root)? {
        let namespace_path = namespace_entry?.path();
        if !namespace_path.is_dir() {
            continue;
        }
        for record_entry in read_dir(&namespace_path)? {
            let path = record_entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            records.push(read_record(&path)?);
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

    fn sample_of(coordinate: &str, kind: PackKind) -> HomePackInstall {
        HomePackInstall {
            kind,
            ..sample(coordinate)
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
            .join(crate::home::PACK_INSTALLS_DIR_NAME)
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
        assert!(
            format!("{}", ErrorChain(&strict)).contains("future.json"),
            "{}",
            ErrorChain(&strict)
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
    fn graph_install_writes_no_home_record() {
        let home = tempfile::tempdir().unwrap();
        record_after_install(
            home.path(),
            sample_of("gents/flow", PackKind::Graph),
            InstalledVia::Operator,
            true,
        )
        .unwrap();
        assert!(read_home_install(home.path(), "gents/flow")
            .unwrap()
            .is_none());
    }

    #[test]
    fn recorded_provenance_is_lean_provenance_of() {
        use InstalledVia::{Operator, SelfConfig};
        for kind in [
            PackKind::Graph,
            PackKind::Documents,
            PackKind::Assets,
            PackKind::Plugins,
        ] {
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
