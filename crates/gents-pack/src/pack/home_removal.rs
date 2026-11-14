use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents_store::config_client::ConfigAccess;
use gents_store::local::file_lock::FileLock;
use serde::Serialize;

use super::materialize::{cache_root, cache_root_disposition, lock_exclusive, CacheRelease};
use super::{HomePackInstall, InstalledPackPlugin};
use crate::plugin::store::StoreGuard;

pub struct PreparedCacheRelease {
    root: PathBuf,
    disposition: CacheRelease,
    _lock: Option<FileLock>,
}

impl PreparedCacheRelease {
    pub fn finish(self) -> Result<CacheRelease> {
        match self.disposition {
            CacheRelease::Removed => {
                std::fs::remove_dir_all(&self.root)
                    .with_context(|| format!("removing {}", self.root.display()))?;
                Ok(CacheRelease::Removed)
            }
            CacheRelease::Retained(reason) => Ok(CacheRelease::Retained(reason)),
        }
    }
}

pub fn prepare_recorded_cache(
    home: &Path,
    coordinate: &str,
    record: &HomePackInstall,
) -> Result<PreparedCacheRelease> {
    anyhow::ensure!(
        record.coordinate == coordinate,
        "pack receipt coordinate mismatch"
    );
    let (namespace, name) = super::registry::split_pack_coordinate(coordinate);
    let expected = cache_root(Path::new(""), namespace, name, &record.digest)?;
    let recorded = Path::new(&record.assets);
    let unscoped = Path::new(gents_store::local::home::PACKS_DIR_NAME)
        .join(name)
        .join(super::archive::digest_hex(&record.digest)?);
    anyhow::ensure!(
        recorded == expected || recorded == unscoped,
        "{coordinate}'s recorded asset path {} does not match {}",
        recorded.display(),
        expected.display()
    );
    let root = home.join(recorded);
    let Some(parent) = root.parent().filter(|parent| parent.is_dir()) else {
        return Ok(PreparedCacheRelease {
            root,
            disposition: CacheRelease::Retained("already absent"),
            _lock: None,
        });
    };
    let lock = lock_exclusive(parent)?;
    let disposition = cache_root_disposition(&root, || {
        if recorded == unscoped {
            let archive = super::store::PackStore::new(home).open(&record.digest)?;
            anyhow::ensure!(
                archive.manifest().metadata.namespace == namespace
                    && archive.manifest().name == name,
                "{coordinate}'s recorded asset digest belongs to a different pack"
            );
            for path in super::declared_paths(archive.manifest()) {
                anyhow::ensure!(
                    std::fs::read(root.join(&path))? == archive.asset(&path)?,
                    "{coordinate}'s recorded asset {path} does not match {}",
                    record.digest
                );
            }
        }
        Ok(())
    })?;
    Ok(PreparedCacheRelease {
        root,
        disposition,
        _lock: Some(lock),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct RetainedAssets {
    pub item: String,
    pub reason: String,
}

/// An assets or plugins pack released from the node home.
#[derive(Debug, Clone, Serialize)]
pub struct HomeRemoval {
    pub record: HomePackInstall,
    pub plugins: Vec<InstalledPackPlugin>,
    pub assets: Vec<String>,
    pub retained: Vec<RetainedAssets>,
    pub plugin_bytes: Vec<String>,
    pub archives: Vec<String>,
}

/// Removes `coordinate`'s home record, the plugin records it owns and its
/// asset cache under `guard`, provided the record still equals `checked`
/// (the record whose plugins the caller found unreferenced under the same
/// guard). Plugin bytes and archives nothing else references are released
/// after; `access` is the node whose installations may still reference an
/// archive.
pub async fn remove_checked(
    guard: StoreGuard,
    home: &Path,
    coordinate: &str,
    checked: HomePackInstall,
    access: Option<&ConfigAccess>,
) -> Result<HomeRemoval> {
    let mut removal =
        remove_checked_keeping_bytes(guard, home, coordinate, checked, access).await?;
    release_removed_plugin_bytes(home, &mut removal)?;
    Ok(removal)
}

/// [`remove_checked`] without releasing the plugin bytes: a caller that must
/// first record the plugins' retirement on the node releases them after,
/// with [`release_removed_plugin_bytes`], so a retirement the node did not
/// record leaves the bytes it still lists.
pub async fn remove_checked_keeping_bytes(
    guard: StoreGuard,
    home: &Path,
    coordinate: &str,
    checked: HomePackInstall,
    access: Option<&ConfigAccess>,
) -> Result<HomeRemoval> {
    let mut retained = Vec::new();
    let mut assets = Vec::new();
    let removal = super::plugin_lifecycle::remove_local_locked(
        &guard,
        home,
        coordinate,
        |record| {
            anyhow::ensure!(
                record == &checked,
                "{coordinate} changed while checking plugin references; retry removal"
            );
            Ok((
                prepare_recorded_cache(home, coordinate, record)?,
                record.assets.clone(),
            ))
        },
        |(prepared, recorded_assets)| {
            match prepared.finish()? {
                CacheRelease::Removed => assets.push(recorded_assets),
                CacheRelease::Retained(reason) => retained.push(RetainedAssets {
                    item: recorded_assets,
                    reason: reason.to_owned(),
                }),
            }
            Ok(())
        },
    )?;
    drop(guard);
    if let Some(reason) = removal.cache_error {
        retained.push(RetainedAssets {
            item: removal.record.assets.clone(),
            reason,
        });
    }
    let archives = release_archives(
        home,
        access,
        &BTreeSet::from([removal.record.digest.clone()]),
    )
    .await
    .context("the local pack was removed, but unreferenced archive cleanup did not finish")?;
    Ok(HomeRemoval {
        record: removal.record,
        plugins: removal.plugins,
        assets,
        retained,
        plugin_bytes: Vec::new(),
        archives,
    })
}

/// Releases the plugin bytes of a [`remove_checked_keeping_bytes`] removal
/// that nothing else references.
pub fn release_removed_plugin_bytes(home: &Path, removal: &mut HomeRemoval) -> Result<()> {
    let plugin_digests = removal
        .record
        .plugins
        .iter()
        .map(|plugin| plugin.digest.clone())
        .collect();
    removal.plugin_bytes = release_plugin_bytes(home, &plugin_digests).context(
        "the local pack was removed, but unreferenced plugin-byte cleanup did not finish",
    )?;
    Ok(())
}

pub fn release_plugin_bytes(home: &Path, digests: &BTreeSet<String>) -> Result<Vec<String>> {
    if digests.is_empty() {
        return Ok(Vec::new());
    }
    crate::plugin::store::release_unreferenced_bytes(home, digests)
}

/// Deletes each stored archive in `digests` that no home record and no
/// installation on `node` references, and returns the digests released.
pub async fn release_archives(
    home: &Path,
    node: Option<&ConfigAccess>,
    digests: &BTreeSet<String>,
) -> Result<Vec<String>> {
    if digests.is_empty() {
        return Ok(Vec::new());
    }
    let referenced = super::referenced_pack_digests(home, node).await?;
    let store = super::store::PackStore::new(home);
    let mut released = Vec::new();
    for digest in digests {
        if referenced.contains(digest) {
            continue;
        }
        if store.release(digest)? {
            released.push(digest.clone());
        }
    }
    released.sort();
    Ok(released)
}

/// What a removed documents pack released from the node home.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DocumentsRelease {
    pub plugins: Vec<InstalledPackPlugin>,
    pub dependency_plugins: Vec<Vec<InstalledPackPlugin>>,
    pub plugin_bytes: Vec<String>,
    pub archives: Vec<String>,
}

/// After `report`'s removal transaction retired its plugins (Lean
/// `Configuration.pluginRetirementAllowed`), forgets its home record and
/// releases its plugin records and the plugin bytes and archives nothing else
/// references.
pub async fn release_documents_pack(
    home: &Path,
    access: Option<&ConfigAccess>,
    report: &super::RemoveReport,
) -> Result<DocumentsRelease> {
    super::forget_home_install(home, &report.pack)?;
    let mut candidates = BTreeSet::new();
    let mut release = |removed: &super::RemoveReport| {
        candidates.extend(
            removed
                .documents
                .plugins
                .iter()
                .map(|plugin| plugin.digest.clone()),
        );
        super::plugin_lifecycle::release_plugin_records(
            home,
            &removed.pack,
            &removed.documents.plugins,
        )
    };
    let plugins = release(report)?;
    let dependency_plugins = report
        .dependencies
        .iter()
        .map(&mut release)
        .collect::<Result<Vec<_>>>()?;
    let plugin_bytes = release_plugin_bytes(home, &candidates)?;
    let digests = std::iter::once(report)
        .chain(&report.dependencies)
        .flat_map(|removed| removed.digests.iter().cloned())
        .collect();
    let archives = release_archives(home, access, &digests).await?;
    Ok(DocumentsRelease {
        plugins,
        dependency_plugins,
        plugin_bytes,
        archives,
    })
}
