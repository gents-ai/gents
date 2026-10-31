use anyhow::{Context, Result};
use std::collections::BTreeSet;

use super::{asset_path, is_valid_pack_name, PackKind, PackManifest};

pub fn validate_manifest(name: &str, manifest: &PackManifest) -> Result<()> {
    anyhow::ensure!(
        manifest.manifest_version == 1 && manifest.name == name,
        "invalid pack identity/version"
    );
    validate_pack_manifest(manifest)
}

pub fn validate_pack_manifest(manifest: &PackManifest) -> Result<()> {
    anyhow::ensure!(
        manifest.manifest_version == 1,
        "unsupported pack manifest version"
    );
    anyhow::ensure!(
        manifest.metadata.kind == PackKind::Documents || manifest.metadata.dependencies.is_empty(),
        "only document packs support package dependencies; nested dependencies are unsupported"
    );
    for dependency in &manifest.metadata.dependencies {
        anyhow::ensure!(
            !dependency.contains('@'),
            "dependency {dependency:?} must be a coordinate (name or ns/name), not a pinned version"
        );
        let (namespace, name) = dependency
            .split_once('/')
            .unwrap_or((crate::pack::archive::DEFAULT_NAMESPACE, dependency.as_str()));
        anyhow::ensure!(
            is_valid_pack_name(namespace) && is_valid_pack_name(name),
            "dependency {dependency:?} is not a valid pack coordinate"
        );
    }
    anyhow::ensure!(
        !manifest.description.trim().is_empty() && !manifest.metadata.authors.is_empty(),
        "pack needs description and authors"
    );
    anyhow::ensure!(
        is_valid_pack_name(&manifest.name),
        "pack name must be snake_case"
    );
    anyhow::ensure!(
        is_valid_pack_name(&manifest.metadata.namespace),
        "pack namespace must be snake_case: {:?}",
        manifest.metadata.namespace
    );
    let mut unique = BTreeSet::new();
    for path in &manifest.metadata.assets {
        anyhow::ensure!(
            asset_path::is_distributable_asset(path),
            "unsafe/private pack asset: {path}"
        );
        anyhow::ensure!(
            asset_path::has_canonical_asset_spelling(path),
            "non-canonical pack asset spelling: {path}"
        );
        anyhow::ensure!(unique.insert(path), "duplicate asset {path}");
    }
    anyhow::ensure!(
        unique.contains(&"README.md".to_owned()),
        "pack must declare README.md"
    );

    let mut plugin_names = BTreeSet::new();
    for plugin in &manifest.metadata.plugins {
        plugin.validate()?;
        anyhow::ensure!(
            plugin_names.insert(plugin.name.as_str()),
            "pack declares the plugin {:?} twice",
            plugin.name
        );
        anyhow::ensure!(
            unique.contains(&plugin.artifact),
            "pack declares the plugin {:?} but not its artifact {:?} as an asset",
            plugin.name,
            plugin.artifact
        );
        if let Some(instructions) = &plugin.instructions {
            anyhow::ensure!(
                unique.contains(instructions),
                "pack declares the plugin {:?} but not its instructions {instructions:?} as an asset",
                plugin.name
            );
        }
    }
    anyhow::ensure!(
        manifest.metadata.kind != PackKind::Plugins || !manifest.metadata.plugins.is_empty(),
        "a plugins pack must declare at least one plugin"
    );

    let mut ux_names = BTreeSet::new();
    for ux in &manifest.metadata.ux {
        ux.validate(&plugin_names)?;
        anyhow::ensure!(
            ux_names.insert(ux.name.as_str()),
            "pack declares the ux plugin {:?} twice",
            ux.name
        );
        // A UX plugin's files travel as declared assets like a plugin's
        // artifact, so the pack digest covers the code the webview runs.
        for (what, path) in [("entry", &ux.entry), ("css", &ux.css)] {
            if let Some(path) = path {
                anyhow::ensure!(
                    unique.contains(path),
                    "pack declares the ux plugin {:?} but not its {what} {path:?} as an asset",
                    ux.name
                );
            }
        }
    }

    let mut slot_names = BTreeSet::new();
    let mut slot_agents = BTreeSet::new();
    for slot in &manifest.metadata.inference_slots {
        anyhow::ensure!(
            is_valid_pack_name(&slot.name),
            "inference slot name must be snake_case: {:?}",
            slot.name
        );
        anyhow::ensure!(
            slot_names.insert(slot.name.as_str()),
            "pack declares inference slot {:?} twice",
            slot.name
        );
        anyhow::ensure!(
            !slot.description.trim().is_empty(),
            "inference slot {:?} needs a description",
            slot.name
        );
        anyhow::ensure!(
            slot.optional == slot.agents.is_empty(),
            "inference slot {:?} must name at least one agent, unless it is optional, and \
             an optional slot names none",
            slot.name
        );
        let mut local = BTreeSet::new();
        for agent in &slot.agents {
            anyhow::ensure!(
                !agent.trim().is_empty() && local.insert(agent.as_str()),
                "inference slot {:?} repeats or has a blank agent",
                slot.name
            );
            anyhow::ensure!(
                slot_agents.insert(agent.as_str()),
                "agent {agent:?} belongs to more than one inference slot"
            );
        }
    }
    anyhow::ensure!(
        manifest.metadata.kind == PackKind::Documents
            || manifest
                .metadata
                .inference_slots
                .iter()
                .all(|slot| slot.optional),
        "asset and plugins packs can declare only optional inference slots"
    );
    let mut plugin_slots = BTreeSet::new();
    for plugin in &manifest.metadata.plugins {
        if let Some(slot) = &plugin.model_slot {
            anyhow::ensure!(
                slot_names.contains(slot.as_str()),
                "plugin {:?} names the model slot {slot:?}, which the pack does not declare",
                plugin.name
            );
            anyhow::ensure!(
                manifest.metadata.inference_slots.iter().any(|declared| {
                    declared.name == *slot && declared.optional && declared.agents.is_empty()
                }),
                "plugin {:?} model slot {slot:?} must be optional and have no agents",
                plugin.name
            );
            plugin_slots.insert(slot.as_str());
        }
    }
    for slot in manifest
        .metadata
        .inference_slots
        .iter()
        .filter(|slot| slot.optional)
    {
        anyhow::ensure!(
            plugin_slots.contains(slot.name.as_str()),
            "optional inference slot {:?} is used by no plugin",
            slot.name
        );
    }

    match manifest.metadata.kind {
        PackKind::Documents => {
            let config = manifest
                .config
                .as_deref()
                .context("document pack requires a config asset")?;
            anyhow::ensure!(
                manifest.metadata.assets.iter().any(|asset| asset == config),
                "config asset must be declared"
            );
        }
        PackKind::Assets | PackKind::Plugins => {
            anyhow::ensure!(
                manifest.config.is_none(),
                "asset-only pack cannot declare configuration"
            );
            anyhow::ensure!(
                manifest.schemas.is_empty(),
                "asset-only pack cannot declare installed schemas"
            );
        }
    }
    let mut schemas = BTreeSet::new();
    for schema in &manifest.schemas {
        anyhow::ensure!(
            manifest.metadata.assets.contains(schema),
            "schema asset must be declared: {schema}"
        );
        anyhow::ensure!(schemas.insert(schema), "duplicate schema asset: {schema}");
    }
    Ok(())
}
