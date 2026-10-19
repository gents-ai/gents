use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{is_valid_pack_name, InstalledPackPlugin, PackKind};

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
    pub plugins: Vec<InstalledPackPlugin>,
    pub installed_at: String,
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
