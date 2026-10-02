//! Installing a pack's plugins into the home's plugin store.
//!
//! Lives in the runtime crate, not `gents-cli`, so a graph install driven
//! by `self_config` can install a pack's plugins itself without going
//! through the CLI. `gents-cli`'s `commands::pack` module keeps a thin
//! caller that forwards here.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use super::authority::describe_plugin_authority;
use super::model_calls::ModelBinding;
use super::store::{self, InstalledPlugin};
use super::{Manifold, PluginRunner};
use crate::pack::{PackManifest, PackPlugin};

/// Installs one plugin a pack carries into the same content-addressed
/// store `gents plugin install` uses, so a plugin that arrived inside a
/// pack runs by name (`gents plugin run <name>`) just like one installed on
/// its own.
///
/// A plugin declared inline in a pack manifest carries neither a namespace
/// nor a version of its own, so it takes its pack's: two packs from
/// different namespaces may each carry a `format_check`, and recording both
/// under one default namespace would have the second silently replace the
/// first.
///
/// `pack_coordinate` (`{namespace}/{name}` of the pack this plugin ships in)
/// is refused when the record already belongs to a different pack: see
/// [`store::check_plugin_ownership`].
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
    consent: bool,
) -> Result<InstalledPlugin> {
    store::check_plugin_ownership(home, pack_namespace, &plugin.name, pack_coordinate)?;
    let granted = store::grant_on_install(home, pack_namespace, plugin, consent)?;
    // A reinstall or upgrade keeps the operator's binding while the plugin
    // still names the same slot.
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
    let digest_hex = format!("{:x}", Sha256::digest(artifact_bytes));
    // Shared against `release_unreferenced_bytes`'s exclusive lock: bytes and
    // the record that points at them are written before a concurrent
    // `gents pack remove` can decide those bytes are unreferenced (store.rs's
    // own doc).
    let _lock = store::lock_store(home, false)?;
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
    };
    store::write_record(home, &record)?;
    Ok(record)
}

/// Admits and stores every plugin a pack ships in `home`'s plugin store.
/// `pack_digest` is the pack's own content digest, recorded on each
/// plugin's record for operator visibility (see
/// [`InstalledPlugin::owner_pack_digest`]).
pub fn install_pack_plugins<'a>(
    home: &Path,
    manifest: &PackManifest,
    pack_digest: &str,
    asset: impl Fn(&str) -> Result<&'a [u8]>,
    consent: bool,
) -> Result<Vec<InstalledPlugin>> {
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
            install_from_pack(
                home,
                &manifest.metadata.namespace,
                &pack_coordinate,
                &manifest.version,
                pack_digest,
                plugin,
                asset(&plugin.artifact)?,
                instructions,
                consent,
            )
        })
        .collect()
}

/// Points each plugin of `manifest` that names a model slot at the profile
/// `bindings` selects for that slot, for `owner`. A slot with no entry keeps
/// whatever it was bound to (nothing, on a first install). Returns the
/// coordinates of the plugins it bound.
pub fn bind_plugin_slots(
    home: &Path,
    manifest: &PackManifest,
    owner: &str,
    bindings: &BTreeMap<String, String>,
) -> Result<Vec<String>> {
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
        set_model_binding(
            home,
            &coordinate,
            Some(ModelBinding {
                agent_did: owner.to_owned(),
                profile_id: profile_id.clone(),
            }),
        )?;
        bound.push(coordinate);
    }
    Ok(bound)
}

/// Whether `error` is the plain absence of a file, a first install.
fn is_not_found(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
    })
}

/// Binds (`Some`) or unbinds (`None`) the model slot of the installed plugin
/// `coordinate`. Refused when the plugin names no slot.
pub fn set_model_binding(
    home: &Path,
    coordinate: &str,
    binding: Option<ModelBinding>,
) -> Result<InstalledPlugin> {
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

/// Every plugin `manifest` would install and its plugin-store record before
/// any write, so a failure later in the same pack install can restore
/// exactly what was there (or remove what was not) instead of leaving an
/// orphaned plugin record behind. `None` means the name was not installed.
pub fn snapshot_pack_plugin_records(
    home: &Path,
    manifest: &PackManifest,
) -> Vec<(String, String, Option<InstalledPlugin>)> {
    manifest
        .metadata
        .plugins
        .iter()
        .map(|plugin| {
            let previous =
                store::read_record(home, &manifest.metadata.namespace, &plugin.name).ok();
            (
                manifest.metadata.namespace.clone(),
                plugin.name.clone(),
                previous,
            )
        })
        .collect()
}

/// Restores each `(namespace, name)` plugin record to what
/// [`snapshot_pack_plugin_records`] observed before the install that must
/// now be undone: the previous record is put back, or removed if there was
/// none. Best-effort and never fails the caller: a restore that cannot
/// complete is logged loudly rather than masking the original error that
/// triggered the rollback.
pub fn rollback_pack_plugin_records(
    home: &Path,
    previous: &[(String, String, Option<InstalledPlugin>)],
) {
    for (namespace, name, record) in previous {
        let result = match record {
            Some(record) => store::write_record(home, record),
            None => match store::read_record(home, namespace, name) {
                Ok(_) => store::remove_record(home, namespace, name).map(|_| ()),
                // Never written by this install (it failed before reaching
                // this plugin, or this plugin failed itself): nothing to undo.
                Err(_) => Ok(()),
            },
        };
        if let Err(error) = result {
            tracing::error!(
                namespace,
                name,
                error = %error,
                "failed to roll back a plugin record after a failed pack install",
            );
        }
    }
}
