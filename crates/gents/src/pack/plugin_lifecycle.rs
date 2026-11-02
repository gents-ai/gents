use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;

use anyhow::{Context, Result};

use super::{HomePackInstall, InstalledPackPlugin, PackIdentity, PackKind};
use crate::plugin::{install, store};

/// A local publication and its node notification are driven by one owned task,
/// so cancellation cannot mistake an unconfirmed commit for a refusal.
#[allow(clippy::too_many_arguments)]
pub async fn install<F>(
    home: &Path,
    archive: &crate::pack::archive::PackArchive,
    consent: bool,
    owner: Option<&str>,
    bindings: &BTreeMap<String, String>,
    notify: impl FnOnce(PackIdentity) -> F + Send + 'static,
) -> Result<(HomePackInstall, Vec<store::InstalledPlugin>)>
where
    F: Future<Output = std::result::Result<(), super::PluginStoreChangeError>> + Send + 'static,
{
    anyhow::ensure!(
        matches!(
            archive.manifest().metadata.kind,
            PackKind::Plugins | PackKind::Assets
        ),
        "expected a plugins or assets pack"
    );
    install_owned(home, archive, consent, owner, bindings, false, notify).await
}

#[allow(clippy::too_many_arguments)]
pub async fn install_plugin<F>(
    home: &Path,
    archive: &crate::pack::archive::PackArchive,
    consent: bool,
    notify: impl FnOnce(PackIdentity) -> F + Send + 'static,
) -> Result<(HomePackInstall, Vec<store::InstalledPlugin>)>
where
    F: Future<Output = std::result::Result<(), super::PluginStoreChangeError>> + Send + 'static,
{
    anyhow::ensure!(
        !archive.manifest().metadata.plugins.is_empty(),
        "pack carries no plugins"
    );
    install_owned(home, archive, consent, None, &BTreeMap::new(), true, notify).await
}

#[allow(clippy::too_many_arguments)]
async fn install_owned<F>(
    home: &Path,
    archive: &crate::pack::archive::PackArchive,
    consent: bool,
    owner: Option<&str>,
    bindings: &BTreeMap<String, String>,
    operator: bool,
    notify: impl FnOnce(PackIdentity) -> F + Send + 'static,
) -> Result<(HomePackInstall, Vec<store::InstalledPlugin>)>
where
    F: Future<Output = std::result::Result<(), super::PluginStoreChangeError>> + Send + 'static,
{
    anyhow::ensure!(
        bindings.is_empty() || owner.is_some_and(|owner| !owner.is_empty()),
        "plugin slot bindings require an owner"
    );
    let manifest = archive.manifest().clone();
    let digest = archive.digest().to_owned();
    let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
    let (root, cache_lease) = super::materialize::cached(home, archive)?;
    let relative_root = root.strip_prefix(home)?.to_string_lossy().into_owned();
    let mut assets = BTreeMap::new();
    for path in install::stage_asset_paths(&manifest)? {
        assets.insert(path.to_owned(), archive.asset(path)?.to_vec());
    }
    let home = home.to_owned();
    let owner = owner.unwrap_or_default().to_owned();
    let bindings = bindings.clone();
    tokio::spawn(async move {
        let _cache_lease = cache_lease;
        let stage = install::stage_pack_graph(&home, vec![install::PluginStageInput {
            manifest: manifest.clone(), digest: digest.clone(), assets, bindings,
        }], consent, &owner).await?;
        let plugins = stage.installed.clone();
        let record = HomePackInstall {
            coordinate: coordinate.clone(), version: manifest.version.clone(), digest: digest.clone(),
            kind: manifest.metadata.kind.clone(), assets: relative_root, dependency_assets: Vec::new(),
            plugins: plugins.iter().map(|plugin| InstalledPackPlugin { name: plugin.name.clone(), digest: plugin.digest.clone() }).collect(),
            installed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            ux: manifest.metadata.ux.clone(), installed_via: operator.then_some(super::InstalledVia::Operator),
        };
        let publish_home = home.clone();
        let publish_record = record.clone();
        let previous = tokio::task::spawn_blocking(move || -> Result<_> {
            let previous = super::read_home_install(&publish_home, &publish_record.coordinate)?;
            let result = if operator {
                super::record_after_install(&publish_home, publish_record.clone(), super::InstalledVia::Operator, true)
            } else { super::write_home_install(&publish_home, &publish_record) };
            if let Err(error) = result {
                match &previous {
                    Some(previous) => super::write_home_install(&publish_home, previous)?,
                    None => super::forget_home_install(&publish_home, &publish_record.coordinate)?,
                }
                return Err(error);
            }
            Ok(previous)
        }).await?;
        let previous = match previous {
            Ok(previous) => previous,
            Err(error) => { stage.finish(false).await?; return Err(error).context("publishing the local pack install record"); }
        };
        let notification = if record.plugins.is_empty() { Ok(()) } else {
            notify(PackIdentity::new(&manifest, &digest, record.plugins.clone())).await
        };
        if matches!(&notification, Err(super::PluginStoreChangeError::PinConflict(_) | super::PluginStoreChangeError::Rejected(_))) {
            let restore_home = home.clone();
            let restore_coordinate = coordinate.clone();
            let restored = tokio::task::spawn_blocking(move || match previous {
                Some(previous) => super::write_home_install(&restore_home, &previous),
                None => super::forget_home_install(&restore_home, &restore_coordinate),
            }).await?;
            stage.finish(false).await?;
            restored?;
            return notification.map(|_| (record, plugins)).map_err(Into::into);
        }
        stage.finish(true).await?;
        notification.context("the pack was installed locally, but its node notification was not confirmed; retry the pack operation to reconcile the node record")?;
        Ok((record, plugins))
    }).await.context("owned plugin installation task failed")?
}

/// Releases the plugin store records `coordinate` still owns at the digests its
/// node record lists; records another pack or a newer install took over stay.
pub fn release_plugin_records(
    home: &Path,
    coordinate: &str,
    plugins: &[InstalledPackPlugin],
) -> Result<Vec<InstalledPackPlugin>> {
    let (namespace, _) = crate::pack::registry::split_pack_coordinate(coordinate);
    let mut released = Vec::new();
    for plugin in plugins {
        if store::remove_record_if_owned(home, namespace, &plugin.name, coordinate, &plugin.digest)?
        {
            released.push(plugin.clone());
        }
    }
    Ok(released)
}

pub struct LocalPackRemoval {
    pub record: HomePackInstall,
    pub plugins: Vec<InstalledPackPlugin>,
    pub cache_error: Option<String>,
}

/// Cache cleanup stays under the publication exclusion so a concurrent reinstall cannot
/// republish into the cache while it is being retired.
pub fn remove_local<P>(
    home: &Path,
    coordinate: &str,
    prepare_cache: impl FnOnce(&HomePackInstall) -> Result<P>,
    cleanup_cache: impl FnOnce(P) -> Result<()>,
) -> Result<LocalPackRemoval> {
    let guard = store::try_lock_store(home)?;
    remove_local_locked(&guard, home, coordinate, prepare_cache, cleanup_cache)
}

pub fn remove_local_locked<P>(
    guard: &store::StoreGuard,
    home: &Path,
    coordinate: &str,
    prepare_cache: impl FnOnce(&HomePackInstall) -> Result<P>,
    cleanup_cache: impl FnOnce(P) -> Result<()>,
) -> Result<LocalPackRemoval> {
    guard.for_home(home)?;
    let record = super::read_home_install(home, coordinate)?
        .with_context(|| format!("{coordinate} is not installed"))?;
    let prepared_cache = prepare_cache(&record)?;
    let (namespace, _) = crate::pack::registry::split_pack_coordinate(coordinate);
    let mut previous = Vec::new();
    for plugin in &record.plugins {
        match store::read_record(home, namespace, &plugin.name) {
            Ok(existing) => previous.push(existing),
            Err(error) if store::is_not_found(&error) => {}
            Err(error) => return Err(error),
        }
    }
    let result = (|| {
        let mut removed = Vec::new();
        for plugin in &record.plugins {
            if store::remove_record_if_owned_locked(
                guard,
                home,
                namespace,
                &plugin.name,
                coordinate,
                &plugin.digest,
            )? {
                removed.push(plugin.clone());
            }
        }
        super::forget_home_install(home, coordinate)?;
        Ok(removed)
    })();
    match result {
        Ok(plugins) => {
            let cache_error = cleanup_cache(prepared_cache)
                .err()
                .map(|error| format!("{error:#}"));
            Ok(LocalPackRemoval {
                record,
                plugins,
                cache_error,
            })
        }
        Err(error) => {
            for plugin in previous {
                if let Err(restore) = store::write_record(home, &plugin) {
                    tracing::error!(%coordinate, %restore, "failed to restore plugin after removal error");
                }
            }
            if let Err(restore) = super::write_home_install(home, &record) {
                tracing::error!(%coordinate, %restore, "failed to restore home pin after removal error");
            }
            Err(error)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn plugin_bindings_without_owner_are_refused_before_publication() {
        let (_source, _, archive) = fixture("1.0.0", false);
        let home = tempfile::tempdir().unwrap();
        let error = install(
            home.path(),
            &archive,
            false,
            None,
            &BTreeMap::from([("model".into(), "profile".into())]),
            |_| async { panic!("refused install must not notify") },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("bindings require an owner"));
        assert!(store::list_records(home.path()).unwrap().is_empty());
    }

    #[test]
    fn plugin_staging_excludes_unrelated_pack_assets() {
        let (_source, _, archive) = fixture("1.0.0", false);
        let mut manifest = archive.manifest().clone();
        manifest
            .metadata
            .assets
            .push("models/large-weights.bin".into());
        manifest.schemas.push("schemas/types.graphql".into());
        manifest.metadata.plugins[0].instructions = Some("instructions/plugin.md".into());
        let paths = install::stage_asset_paths(&manifest).unwrap();
        assert!(paths.contains("plugins/native_echo.afb"));
        assert!(paths.contains("instructions/plugin.md"));
        assert!(paths.contains("schemas/types.graphql"));
        assert!(!paths.contains("models/large-weights.bin"));
        assert!(!paths.contains("README.md"));
    }

    pub(crate) fn fixture(
        version: &str,
        authority: bool,
    ) -> (
        tempfile::TempDir,
        std::path::PathBuf,
        crate::pack::archive::PackArchive,
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
        let (bytes, _) = crate::pack::archive::pack_dir(&source).unwrap();
        let artifact = dir.path().join("native_echo.pack");
        std::fs::write(&artifact, &bytes).unwrap();
        let archive = crate::pack::archive::PackArchive::from_bytes(&bytes).unwrap();
        (dir, artifact, archive)
    }

    #[tokio::test]
    async fn plugin_lifecycle_keeps_local_publication_after_notification_failure_or_cancellation() {
        let home = tempfile::tempdir().unwrap();
        let (_dir, _path, archive) = fixture("1.0.0", false);
        let coordinate = "fixture/native_echo_pack";
        let error = install(
            home.path(),
            &archive,
            false,
            None,
            &BTreeMap::new(),
            |_| async {
                Err(super::super::PluginStoreChangeError::Unconfirmed(
                    anyhow::anyhow!("notification failed"),
                ))
            },
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("installed locally"));
        let first = super::super::read_home_install(home.path(), coordinate)
            .unwrap()
            .expect("local pin survives a failed notification");
        assert_eq!(first.version, "1.0.0");
        assert_eq!(first.digest, archive.digest());
        let first_plugin = store::read_record(home.path(), "fixture", "native_echo").unwrap();
        assert_eq!(first_plugin.version, "1.0.0");
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
        assert_eq!(before.version, "1.0.0");
        let plugin_before = store::read_record(home.path(), "fixture", "native_echo").unwrap();
        assert_eq!(plugin_before.version, "1.0.0");
        let (_dir2, _, updated) = fixture("2.0.0", false);
        let updated_digest = updated.digest().to_owned();
        let reached = std::sync::Arc::new(tokio::sync::Notify::new());
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        let completed = std::sync::Arc::new(tokio::sync::Notify::new());
        let home_path = home.path().to_owned();
        let mut caller = tokio::spawn({
            let reached = reached.clone();
            let release = release.clone();
            let completed = completed.clone();
            async move {
                install(
                    &home_path,
                    &updated,
                    false,
                    None,
                    &BTreeMap::new(),
                    move |_| async move {
                        reached.notify_one();
                        release.notified().await;
                        completed.notify_one();
                        Ok(())
                    },
                )
                .await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::select! {
                _ = reached.notified() => {},
                result = &mut caller => panic!("installation ended before notification: {result:?}"),
            }
        }).await.expect("installation did not reach notification");
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        let after_cancel = super::super::read_home_install(home.path(), coordinate)
            .unwrap()
            .unwrap();
        assert_eq!(after_cancel.version, "2.0.0");
        assert_eq!(after_cancel.digest, updated_digest);
        let plugin_after_cancel =
            store::read_record(home.path(), "fixture", "native_echo").unwrap();
        assert_eq!(plugin_after_cancel.version, "2.0.0");
        release.notify_one();
        completed.notified().await;
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
        let _error = install(
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
