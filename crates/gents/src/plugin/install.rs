use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};

use super::authority::describe_plugin_authority;
use super::model_calls::ModelBinding;
use super::store::{self, InstalledPlugin};
use super::{Manifold, PluginRunner};
use crate::pack::{PackManifest, PackPlugin};

#[allow(clippy::too_many_arguments)]
pub fn install_from_pack(
    home: &Path,
    pack_namespace: &str,
    pack_coordinate: &str,
    pack_version: &str,
    pack_digest: &str,
    plugin: &PackPlugin,
    artifact_bytes: &[u8],
    instructions: Option<String>,
    data_unique_keys: BTreeMap<String, Vec<Vec<String>>>,
    data_collection_fields: BTreeMap<String, Vec<String>>,
    consent: bool,
) -> Result<InstalledPlugin> {
    let lock = store::try_lock_store(home)?;
    install_from_pack_locked(
        &lock,
        home,
        pack_namespace,
        pack_coordinate,
        pack_version,
        pack_digest,
        plugin,
        artifact_bytes,
        instructions,
        data_unique_keys,
        data_collection_fields,
        consent,
    )
}

#[allow(clippy::too_many_arguments)]
fn install_from_pack_locked(
    lock: &store::StoreGuard,
    home: &Path,
    pack_namespace: &str,
    pack_coordinate: &str,
    pack_version: &str,
    pack_digest: &str,
    plugin: &PackPlugin,
    artifact_bytes: &[u8],
    instructions: Option<String>,
    data_unique_keys: BTreeMap<String, Vec<Vec<String>>>,
    data_collection_fields: BTreeMap<String, Vec<String>>,
    consent: bool,
) -> Result<InstalledPlugin> {
    // The pack lifecycle already holds this lock across its transaction window; flock does not re-enter.
    lock.for_home(home)?;
    store::check_plugin_ownership(home, pack_namespace, &plugin.name, pack_coordinate)?;
    let granted = store::grant_on_install(home, pack_namespace, plugin, consent)?;
    let model_binding = match store::read_record(home, pack_namespace, &plugin.name) {
        Ok(previous) => Some(previous)
            .filter(|previous| previous.declaration.model_slot == plugin.model_slot)
            .and_then(|previous| previous.model_binding),
        Err(error) if is_not_found(&error) => None,
        Err(error) => {
            tracing::warn!(
                plugin = %plugin.name,
                error = %format!("{error:#}"),
                "the previous plugin record is unreadable; its model binding is not carried over"
            );
            None
        }
    };
    PluginRunner::compile(artifact_bytes, plugin)
        .with_context(|| format!("admitting pack plugin {}", plugin.name))?;
    let effective = granted.clone().unwrap_or_else(Manifold::sealed);
    if let Some(description) = describe_plugin_authority(plugin, &effective) {
        tracing::info!(
            plugin = %plugin.name,
            namespace = pack_namespace,
            authority = %description,
            "installed pack plugin",
        );
    }
    let digest_hex = store::content_digest_hex(artifact_bytes);
    store::store_bytes(home, &digest_hex, artifact_bytes)?;
    let record = InstalledPlugin {
        namespace: pack_namespace.to_owned(),
        name: plugin.name.clone(),
        version: pack_version.to_owned(),
        digest: format!("sha256:{digest_hex}"),
        language: plugin.language.clone(),
        declaration: plugin.clone(),
        granted,
        instructions,
        owner_pack_coordinate: Some(pack_coordinate.to_owned()),
        owner_pack_digest: Some(pack_digest.to_owned()),
        model_binding,
        data_unique_keys,
        data_collection_fields,
    };
    store::write_record(home, &record)?;
    Ok(record)
}

pub fn install_pack_plugins<'a>(
    home: &Path,
    manifest: &PackManifest,
    pack_digest: &str,
    asset: impl Fn(&str) -> Result<&'a [u8]>,
    consent: bool,
) -> Result<Vec<InstalledPlugin>> {
    install_pack_plugins_with_bindings(
        home,
        manifest,
        pack_digest,
        asset,
        consent,
        None,
        &BTreeMap::new(),
        |_| Ok(()),
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "install inputs and policies supplied by each install entry point"
)]
pub fn install_pack_plugins_with_bindings<'a>(
    home: &Path,
    manifest: &PackManifest,
    pack_digest: &str,
    asset: impl Fn(&str) -> Result<&'a [u8]>,
    consent: bool,
    owner: Option<&str>,
    bindings: &BTreeMap<String, String>,
    publish_local: impl FnOnce(&[InstalledPlugin]) -> Result<()>,
) -> Result<Vec<InstalledPlugin>> {
    let lock = store::try_lock_store(home)?;
    store::list_records(home)?;
    let previous = snapshot_pack_plugin_records(home, manifest)?;
    let result = (|| {
        let installed =
            install_pack_plugins_locked(&lock, home, manifest, pack_digest, asset, consent)?;
        if !bindings.is_empty() {
            bind_plugin_slots_locked(
                &lock,
                home,
                manifest,
                owner.context("plugin slot bindings require an owner")?,
                bindings,
            )?;
        }
        publish_local(&installed)?;
        Ok(installed)
    })();
    if let Err(error) = result {
        rollback_pack_plugin_records(home, &previous).with_context(|| {
            format!("restoring plugin records after installation failed: {error:#}")
        })?;
        return Err(error);
    }
    result
}

fn install_pack_plugins_locked<'a>(
    lock: &store::StoreGuard,
    home: &Path,
    manifest: &PackManifest,
    pack_digest: &str,
    asset: impl Fn(&str) -> Result<&'a [u8]>,
    consent: bool,
) -> Result<Vec<InstalledPlugin>> {
    lock.for_home(home)?;
    let pack_coordinate = format!("{}/{}", manifest.metadata.namespace, manifest.name);
    manifest
        .metadata
        .plugins
        .iter()
        .map(|plugin| {
            let instructions = plugin
                .instructions
                .as_deref()
                .map(|path| crate::pack::tool_instructions(&plugin.name, asset(path)?))
                .transpose()?;
            let (data_unique_keys, data_collection_fields) =
                crate::pack::plugin_data_unique_keys(manifest, plugin, &asset)?;
            install_from_pack_locked(
                lock,
                home,
                &manifest.metadata.namespace,
                &pack_coordinate,
                &manifest.version,
                pack_digest,
                plugin,
                asset(&plugin.artifact)?,
                instructions,
                data_unique_keys,
                data_collection_fields,
                consent,
            )
        })
        .collect()
}

pub fn bind_plugin_slots(
    home: &Path,
    manifest: &PackManifest,
    owner: &str,
    bindings: &BTreeMap<String, String>,
) -> Result<Vec<String>> {
    let lock = store::try_lock_store(home)?;
    bind_plugin_slots_locked(&lock, home, manifest, owner, bindings)
}

fn bind_plugin_slots_locked(
    lock: &store::StoreGuard,
    home: &Path,
    manifest: &PackManifest,
    owner: &str,
    bindings: &BTreeMap<String, String>,
) -> Result<Vec<String>> {
    lock.for_home(home)?;
    let mut bound = Vec::new();
    for plugin in &manifest.metadata.plugins {
        let Some(profile_id) = plugin
            .model_slot
            .as_ref()
            .and_then(|slot| bindings.get(slot))
        else {
            continue;
        };
        let coordinate = format!("{}/{}", manifest.metadata.namespace, plugin.name);
        set_model_binding_locked(
            lock,
            home,
            &coordinate,
            Some(ModelBinding {
                node_did: owner.to_owned(),
                profile_id: profile_id.clone(),
            }),
        )?;
        bound.push(coordinate);
    }
    Ok(bound)
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
    })
}

pub fn set_model_binding(
    home: &Path,
    coordinate: &str,
    binding: Option<ModelBinding>,
) -> Result<InstalledPlugin> {
    let lock = store::try_lock_store(home)?;
    set_model_binding_locked(&lock, home, coordinate, binding)
}

fn set_model_binding_locked(
    lock: &store::StoreGuard,
    home: &Path,
    coordinate: &str,
    binding: Option<ModelBinding>,
) -> Result<InstalledPlugin> {
    lock.for_home(home)?;
    let (namespace, name) = store::parse_coordinate(coordinate)?;
    let mut record = store::read_record(home, namespace, name)
        .with_context(|| format!("plugin {coordinate} is not installed"))?;
    anyhow::ensure!(
        record.declaration.model_slot.is_some(),
        "plugin {coordinate} does not call a model, so it has no slot to bind"
    );
    record.model_binding = binding;
    store::write_record(home, &record)?;
    Ok(record)
}

fn snapshot_pack_plugin_records(
    home: &Path,
    manifest: &PackManifest,
) -> Result<Vec<(String, String, Option<InstalledPlugin>)>> {
    manifest
        .metadata
        .plugins
        .iter()
        .map(|plugin| {
            let previous =
                match store::read_record(home, &manifest.metadata.namespace, &plugin.name) {
                    Ok(record) => Some(record),
                    Err(error) if store::is_not_found(&error) => None,
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!(
                                "reading existing plugin {}/{} before installation",
                                manifest.metadata.namespace, plugin.name
                            )
                        })
                    }
                };
            Ok((
                manifest.metadata.namespace.clone(),
                plugin.name.clone(),
                previous,
            ))
        })
        .collect()
}

fn rollback_pack_plugin_records(
    home: &Path,
    previous: &[(String, String, Option<InstalledPlugin>)],
) -> Result<()> {
    for (namespace, name, record) in previous {
        match record {
            Some(record) => store::write_record(home, record),
            None => match store::read_record(home, namespace, name) {
                Ok(_) => store::remove_record(home, namespace, name).map(|_| ()),
                Err(error) if store::is_not_found(&error) => Ok(()),
                Err(error) => Err(error),
            },
        }
        .with_context(|| format!("restoring plugin {namespace}/{name}"))?;
    }
    Ok(())
}

struct StagedPluginRecords {
    home: std::path::PathBuf,
    _lock: store::StoreGuard,
    previous: Vec<(String, String, Option<InstalledPlugin>)>,
}

pub(crate) struct StagedPluginInstall {
    records: Option<StagedPluginRecords>,
    cleanup: crate::runtime::drop_cleanup::DropCleanup,
    pub(crate) installed: Vec<InstalledPlugin>,
}

impl StagedPluginInstall {
    pub(crate) async fn finish(mut self, committed: bool) -> Result<()> {
        let records = self.records.take();
        tokio::task::spawn_blocking(move || {
            if let Some(records) = records {
                if !committed {
                    rollback_pack_plugin_records(&records.home, &records.previous)?;
                }
            }
            Ok(())
        })
        .await?
    }
}

impl Drop for StagedPluginInstall {
    fn drop(&mut self) {
        if let Some(records) = self.records.take() {
            self.cleanup.spawn(async move {
                let _ = tokio::task::spawn_blocking(move || {
                    if let Err(error) = rollback_pack_plugin_records(&records.home, &records.previous) {
                        tracing::error!(%error, "failed to restore plugin records after cancellation");
                    }
                })
                .await;
            });
        }
    }
}

pub(crate) fn stage_asset_paths(
    manifest: &PackManifest,
) -> Result<std::collections::BTreeSet<&str>> {
    let mut paths = crate::pack::sdl_paths(manifest);
    for plugin in &manifest.metadata.plugins {
        paths.insert(plugin.artifact.as_str());
        paths.extend(plugin.instructions.as_deref());
    }
    Ok(paths)
}

#[derive(Clone)]
pub(crate) struct PluginStageInput {
    pub(crate) manifest: PackManifest,
    pub(crate) digest: String,
    pub(crate) assets: BTreeMap<String, Vec<u8>>,
    pub(crate) bindings: BTreeMap<String, String>,
}

pub(crate) async fn stage_pack_graph(
    home: &Path,
    inputs: Vec<PluginStageInput>,
    consent: bool,
    owner: &str,
) -> Result<StagedPluginInstall> {
    anyhow::ensure!(
        !owner.is_empty() || inputs.iter().all(|input| input.bindings.is_empty()),
        "plugin slot bindings require an owner"
    );
    let cleanup = crate::runtime::drop_cleanup::DropCleanup::current();
    let home = home.to_owned();
    let owner = owner.to_owned();
    tokio::task::spawn_blocking(move || {
        let lock = store::try_lock_store(&home)?;
        store::list_records(&home)?;
        let mut previous = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for input in &inputs {
            for record in snapshot_pack_plugin_records(&home, &input.manifest)? {
                anyhow::ensure!(
                    seen.insert((record.0.clone(), record.1.clone())),
                    "pack graph names plugin {}/{} more than once",
                    record.0,
                    record.1
                );
                previous.push(record);
            }
        }
        let result = (|| {
            let mut installed = Vec::new();
            for input in inputs {
                installed.extend(install_pack_plugins_locked(
                    &lock,
                    &home,
                    &input.manifest,
                    &input.digest,
                    |path| {
                        input
                            .assets
                            .get(path)
                            .map(Vec::as_slice)
                            .with_context(|| format!("missing pack asset {path}"))
                    },
                    consent,
                )?);
                if !input.bindings.is_empty() {
                    bind_plugin_slots_locked(
                        &lock,
                        &home,
                        &input.manifest,
                        &owner,
                        &input.bindings,
                    )?;
                }
            }
            Ok(installed)
        })();
        match result {
            Ok(installed) => Ok(StagedPluginInstall {
                records: Some(StagedPluginRecords {
                    home,
                    _lock: lock,
                    previous,
                }),
                cleanup,
                installed,
            }),
            Err(error) => {
                rollback_pack_plugin_records(&home, &previous).with_context(|| {
                    format!("restoring plugin records after installation failed: {error:#}")
                })?;
                Err(error)
            }
        }
    })
    .await?
}
