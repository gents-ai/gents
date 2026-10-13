use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;

use anyhow::{Context, Result};

use super::{HomePackInstall, InstalledPackPlugin, PackIdentity, PackKind};
use crate::plugin::{install, store};

struct Rollback<'a> {
    home: &'a Path,
    coordinate: String,
    previous: Option<HomePackInstall>,
    plugins: Vec<(String, String, Option<store::InstalledPlugin>)>,
    committed: bool,
}

impl Drop for Rollback<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        install::rollback_pack_plugin_records(self.home, &self.plugins);
        let restored = match &self.previous {
            Some(previous) => super::write_home_install(self.home, previous),
            None => super::forget_home_install(self.home, &self.coordinate),
        };
        if let Err(error) = restored {
            tracing::error!(coordinate = %self.coordinate, %error, "failed to restore pack install record");
        }
    }
}

/// The caller supplies operator consent and the existing node notification owner.
/// Model-facing callers must pass false; a statement in model input is never consent.
///
/// Lifecycle mutations (plugin records and the home install record) are
/// serialized under the plugin store's exclusive lock, so a concurrent
/// install's published state cannot be erased by this operation's rollback:
/// a snapshot is only taken once this operation holds the lock, and a
/// competing install waits until the lock is released.
///
/// The notification (node record) is written through the existing durable
/// transaction owner, which supports commit followed by lost receipt. A
/// notification error is therefore ambiguous, not proof of rollback: the
/// home install record is only restored when the transaction owner reports
/// the write durably failed (`Err` from a non-retryable failure), and an
/// ambiguous outcome is left to that owner's idempotent retry/recovery
/// instead of reverting local state under a possibly-committed node record.
#[allow(clippy::too_many_arguments)]
pub async fn install<F: Future<Output = Result<()>>>(
    home: &Path,
    archive: &crate::pack_archive::PackArchive,
    consent: bool,
    owner: Option<&str>,
    bindings: &BTreeMap<String, String>,
    notify: impl FnOnce(PackIdentity) -> F,
) -> Result<(HomePackInstall, Vec<store::InstalledPlugin>)> {
    let manifest = archive.manifest();
    anyhow::ensure!(
        matches!(manifest.metadata.kind, PackKind::Plugins | PackKind::Assets),
        "expected a plugins or assets pack"
    );
    let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
    let (root, _cache_lease) = super::materialize::cached(home, archive)?;
    // A malformed record is not absence and must never become a rollback snapshot.
    store::list_records(home)?;
    // The store lock serializes every plugin-record and home-record mutation
    // of this home, including release_unreferenced_bytes. It is held across
    // the awaitable notification so a competing install cannot publish and
    // then be erased by this operation's rollback.
    let lock = store::lock_store(home, true)?;
    let previous = super::read_home_install(home, &coordinate)?;
    for plugin in &manifest.metadata.plugins {
        store::check_plugin_ownership(
            home,
            &manifest.metadata.namespace,
            &plugin.name,
            &coordinate,
        )?;
        store::grant_on_install(home, &manifest.metadata.namespace, plugin, consent)?;
    }
    let mut rollback = Rollback {
        home,
        coordinate: coordinate.clone(),
        previous,
        plugins: install::snapshot_pack_plugin_records(home, manifest),
        committed: false,
    };
    let installed = install::install_pack_plugins(
        home,
        manifest,
        archive.digest(),
        |path| archive.asset(path),
        consent,
    )?;
    let record = HomePackInstall {
        coordinate,
        version: manifest.version.clone(),
        digest: archive.digest().to_owned(),
        kind: manifest.metadata.kind.clone(),
        assets: root.strip_prefix(home)?.to_string_lossy().into_owned(),
        plugins: installed
            .iter()
            .map(|plugin| InstalledPackPlugin {
                name: plugin.name.clone(),
                digest: plugin.digest.clone(),
            })
            .collect(),
        installed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    let result: anyhow::Result<()> = async {
        if !bindings.is_empty() {
            install::bind_plugin_slots(
                home,
                manifest,
                owner.context("plugin slot bindings require an owner")?,
                bindings,
            )?;
        }
        super::write_home_install(home, &record)?;
        if !record.plugins.is_empty() {
            notify(PackIdentity::new(
                manifest,
                archive.digest(),
                record.plugins.clone(),
            ))
            .await?;
        }
        Ok(())
    }
    .await;
    result?;
    rollback.committed = true;
    drop(lock);
    Ok((record, installed))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn fixture(
        version: &str,
        authority: bool,
    ) -> (
        tempfile::TempDir,
        std::path::PathBuf,
        crate::pack_archive::PackArchive,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir_all(source.join("plugins")).unwrap();
        let wat = crate::plugin::tests::constant_output_wat(br#"{"native_pack_echo":"ok"}"#);
        let afb = crate::plugin::tests::build_plugin_afb(&wat);
        std::fs::write(source.join("plugins/native_echo.afb"), afb).unwrap();
        let mut plugin = json!({"name":"native_echo","description":"Deterministic sealed native pack fixture","artifact":"plugins/native_echo.afb","language":"rust","input_schema":{"type":"object","properties":{},"additionalProperties":false}});
        if authority {
            plugin["manifold"] = json!({"fs":"None","net":{"OutboundHttp":["https://example.invalid"]},"env":"None","crypto":false,"child_process":false});
        }
        let manifest = json!({"manifest_version":1,"name":"native_echo_pack","namespace":"fixture","version":version,"description":"Offline native pack lifecycle fixture","authors":["gents-ai"],"kind":"plugins","assets":["README.md","plugins/native_echo.afb"],"plugins":[plugin]});
        std::fs::write(
            source.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::write(
            source.join("README.md"),
            b"Offline deterministic plugin fixture.\n",
        )
        .unwrap();
        let (bytes, _) = crate::pack_archive::pack_dir(&source).unwrap();
        let artifact = dir.path().join("native_echo.pack");
        std::fs::write(&artifact, &bytes).unwrap();
        let archive = crate::pack_archive::PackArchive::from_bytes(&bytes).unwrap();
        (dir, artifact, archive)
    }

    #[tokio::test]
    async fn plugin_lifecycle_rolls_back_notification_failure_and_cancellation() {
        let home = tempfile::tempdir().unwrap();
        let (_dir, _path, archive) = fixture("1.0.0", false);
        let coordinate = "fixture/native_echo_pack";
        install(
            home.path(),
            &archive,
            false,
            None,
            &BTreeMap::new(),
            |_| async { anyhow::bail!("notification failed") },
        )
        .await
        .unwrap_err();
        assert!(super::super::read_home_install(home.path(), coordinate)
            .unwrap()
            .is_none());
        assert!(store::list_records(home.path()).unwrap().is_empty());
        let (before, _) = install(
            home.path(),
            &archive,
            false,
            None,
            &BTreeMap::new(),
            |_| async { Ok(()) },
        )
        .await
        .unwrap();
        let plugin_before = store::read_record(home.path(), "fixture", "native_echo").unwrap();
        let (_dir2, _, updated) = fixture("2.0.0", false);
        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            install(home.path(), &updated, false, None, &BTreeMap::new(), |_| {
                std::future::pending::<Result<()>>()
            }),
        )
        .await;
        assert!(cancelled.is_err());
        assert_eq!(
            super::super::read_home_install(home.path(), coordinate)
                .unwrap()
                .unwrap(),
            before
        );
        assert_eq!(
            store::read_record(home.path(), "fixture", "native_echo").unwrap(),
            plugin_before
        );
    }

    #[tokio::test]
    async fn plugin_lifecycle_refuses_authority_and_cross_pack_ownership() {
        let home = tempfile::tempdir().unwrap();
        let (_dir, _, protected) = fixture("1.0.0", true);
        assert!(install(
            home.path(),
            &protected,
            false,
            None,
            &BTreeMap::new(),
            |_| async { Ok(()) }
        )
        .await
        .is_err());
        assert!(store::list_records(home.path()).unwrap().is_empty());
        let (_dir2, _, sealed) = fixture("1.0.0", false);
        install(
            home.path(),
            &sealed,
            false,
            None,
            &BTreeMap::new(),
            |_| async { Ok(()) },
        )
        .await
        .unwrap();
        let mut record = store::read_record(home.path(), "fixture", "native_echo").unwrap();
        record.owner_pack_coordinate = Some("fixture/other".into());
        store::write_record(home.path(), &record).unwrap();
        assert!(install(
            home.path(),
            &sealed,
            false,
            None,
            &BTreeMap::new(),
            |_| async { Ok(()) }
        )
        .await
        .is_err());
        assert_eq!(
            store::read_record(home.path(), "fixture", "native_echo").unwrap(),
            record
        );
    }
}
