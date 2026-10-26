use gents_protocol::error_chain::ErrorChain;
use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;

use super::{CallbackError, HomePackInstall, InstalledPackPlugin, PackIdentity, PackKind};
use crate::plugin::{install, store};

#[derive(Debug, thiserror::Error)]
pub enum PluginLifecycleError {
    #[error("expected a plugins or assets pack")]
    NotPluginsPack,
    #[error("plugin slot bindings require an owner")]
    BindingsNeedOwner,
    #[error("publishing the local pack install record")]
    PublishRecord(#[source] super::home_install::HomeInstallError),
    #[error("local pack install did not publish its record")]
    NotPublished,
    #[error("the pack was installed locally, but its node notification was not confirmed; retry the pack operation to reconcile the node record")]
    NotifyUnconfirmed(#[source] CallbackError),
    #[error("{coordinate} is not installed")]
    NotInstalled { coordinate: String },
    #[error(transparent)]
    PrepareCache(CallbackError),
    #[error(transparent)]
    Install(#[from] install::PluginInstallError),
    #[error(transparent)]
    Store(#[from] store::PluginStoreError),
    #[error(transparent)]
    HomeInstall(#[from] super::home_install::HomeInstallError),
    #[error(transparent)]
    Materialize(#[from] super::materialize::MaterializeError),
    #[error(transparent)]
    StripPrefix(#[from] std::path::StripPrefixError),
}

type Result<T, E = PluginLifecycleError> = std::result::Result<T, E>;

/// Model-facing callers must pass false; model input never constitutes operator consent.
/// Local publication is durable before node notification: a failed or cancelled notification
/// cannot roll back a transaction that may already have committed without returning a receipt.
#[allow(clippy::too_many_arguments)]
pub async fn install<F, N>(
    home: &Path,
    archive: &crate::pack_archive::PackArchive,
    consent: bool,
    owner: Option<&str>,
    bindings: &BTreeMap<String, String>,
    notify: impl FnOnce(PackIdentity) -> F,
) -> Result<(HomePackInstall, Vec<store::InstalledPlugin>)>
where
    F: Future<Output = std::result::Result<(), N>>,
    N: Into<CallbackError>,
{
    let manifest = archive.manifest();
    if !matches!(manifest.metadata.kind, PackKind::Plugins | PackKind::Assets) {
        return Err(PluginLifecycleError::NotPluginsPack);
    }
    let coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
    let (root, _cache_lease) = super::materialize::cached(home, archive)?;
    let install_owner = if bindings.is_empty() {
        None
    } else {
        Some(owner.ok_or(PluginLifecycleError::BindingsNeedOwner)?)
    };
    let mut published_record = None;
    let installed = install::install_pack_plugins_with_bindings(
        home,
        manifest,
        archive.digest(),
        |path| archive.asset(path),
        consent,
        install_owner,
        bindings,
        |plugins| {
            let previous = super::read_home_install(home, &coordinate)?;
            let record = HomePackInstall {
                coordinate: coordinate.clone(),
                version: manifest.version.clone(),
                digest: archive.digest().to_owned(),
                kind: manifest.metadata.kind.clone(),
                assets: root.strip_prefix(home)?.to_string_lossy().into_owned(),
                dependency_assets: Vec::new(),
                plugins: plugins
                    .iter()
                    .map(|plugin| InstalledPackPlugin {
                        name: plugin.name.clone(),
                        digest: plugin.digest.clone(),
                    })
                    .collect(),
                installed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                ux: manifest.metadata.ux.clone(),
                installed_via: None,
            };
            if let Err(error) = super::write_home_install(home, &record) {
                let restored = match previous {
                    Some(previous) => super::write_home_install(home, &previous),
                    None => super::forget_home_install(home, &coordinate),
                };
                if let Err(restore_error) = restored {
                    tracing::error!(coordinate = %coordinate, error = %restore_error, "failed to restore pack install record after local publication error");
                }
                return Err(PluginLifecycleError::PublishRecord(error));
            }
            published_record = Some(record);
            Ok::<_, PluginLifecycleError>(())
        },
    )?;
    let record = published_record.ok_or(PluginLifecycleError::NotPublished)?;
    if !record.plugins.is_empty() {
        if let Err(error) = notify(PackIdentity::new(
            manifest,
            archive.digest(),
            record.plugins.clone(),
        ))
        .await
        {
            return Err(PluginLifecycleError::NotifyUnconfirmed(error.into()));
        }
    }
    Ok((record, installed))
}

/// Releases the plugin store records `coordinate` still owns at the digests its
/// node record lists; records another pack or a newer install took over stay.
pub fn release_plugin_records(
    home: &Path,
    coordinate: &str,
    plugins: &[InstalledPackPlugin],
) -> Result<Vec<InstalledPackPlugin>> {
    let (namespace, _) = crate::pack_registry::split_pack_coordinate(coordinate);
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
pub fn remove_local<P, E, C>(
    home: &Path,
    coordinate: &str,
    prepare_cache: impl FnOnce(&HomePackInstall) -> std::result::Result<P, E>,
    cleanup_cache: impl FnOnce(P) -> std::result::Result<(), C>,
) -> Result<LocalPackRemoval>
where
    E: Into<CallbackError>,
    C: Into<CallbackError>,
{
    let guard = store::try_lock_store(home)?;
    let record = super::read_home_install(home, coordinate)?.ok_or_else(|| {
        PluginLifecycleError::NotInstalled {
            coordinate: coordinate.to_owned(),
        }
    })?;
    let prepared_cache =
        prepare_cache(&record).map_err(|error| PluginLifecycleError::PrepareCache(error.into()))?;
    let (namespace, _) = crate::pack_registry::split_pack_coordinate(coordinate);
    let mut previous = Vec::new();
    for plugin in &record.plugins {
        match store::read_record(home, namespace, &plugin.name) {
            Ok(existing) => previous.push(existing),
            Err(error) if store::is_not_found(&error) => {}
            Err(error) => return Err(error.into()),
        }
    }
    let result = (|| -> Result<Vec<InstalledPackPlugin>> {
        let mut removed = Vec::new();
        for plugin in &record.plugins {
            if store::remove_record_if_owned_locked(
                &guard,
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
            let cache_error = cleanup_cache(prepared_cache).err().map(|error| {
                let error: CallbackError = error.into();
                format!("{}", ErrorChain(&*error))
            });
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

    type TestError = Box<dyn std::error::Error + Send + Sync>;

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
            |_| async { Err::<(), TestError>("notification failed".into()) },
        )
        .await
        .unwrap_err();
        assert!(format!("{}", ErrorChain(&error)).contains("installed locally"));
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
            |_| async { Ok::<_, TestError>(()) },
        )
        .await
        .unwrap();
        assert_eq!(before.version, "1.0.0");
        let plugin_before = store::read_record(home.path(), "fixture", "native_echo").unwrap();
        assert_eq!(plugin_before.version, "1.0.0");
        let (_dir2, _, updated) = fixture("2.0.0", false);
        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            install(home.path(), &updated, false, None, &BTreeMap::new(), |_| {
                std::future::pending::<std::result::Result<(), TestError>>()
            }),
        )
        .await;
        assert!(cancelled.is_err());
        let after_cancel = super::super::read_home_install(home.path(), coordinate)
            .unwrap()
            .unwrap();
        assert_eq!(after_cancel.version, "2.0.0");
        assert_eq!(after_cancel.digest, updated.digest());
        let plugin_after_cancel =
            store::read_record(home.path(), "fixture", "native_echo").unwrap();
        assert_eq!(plugin_after_cancel.version, "2.0.0");
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
            |_| async { Ok::<_, TestError>(()) }
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
            |_| async { Ok::<_, TestError>(()) },
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
            |_| async { Ok::<_, TestError>(()) }
        )
        .await
        .is_err());
        assert_eq!(
            store::read_record(home.path(), "fixture", "native_echo").unwrap(),
            record
        );
    }
}
