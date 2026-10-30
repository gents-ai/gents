use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::manifest::ScenarioManifest;
use super::validate::{
    validate_manifest, validate_prompt_tool_contracts, validate_task_goal_declarations,
};
use crate::desired_state::interpolate::interpolate_with;
use gents::graphql::validate_collection_identifier;

pub(super) fn validate_source_pack_path(path: &Path) -> Result<()> {
    anyhow::ensure!(
        !path.components().any(|component| matches!(
            component,
            std::path::Component::CurDir | std::path::Component::ParentDir
        )),
        "source pack path must be lexically normalized"
    );
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("source pack path needs a UTF-8 directory name")?;
    anyhow::ensure!(
        gents::pack::is_valid_pack_name(name),
        "source pack directory name must be snake_case"
    );
    Ok(())
}

pub(super) fn read_distribution_manifest(root: &Path) -> Result<gents::pack::PackManifest> {
    let path = root.join("manifest.json");
    serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?,
    )
    .with_context(|| format!("parsing {}", path.display()))
}

pub(super) async fn resolve_scenario_dir(
    target: &str,
    registry: Option<&str>,
) -> Result<(
    PathBuf,
    Option<gents::file_lock::FileLock>,
    gents::pack::PackManifest,
)> {
    let direct = PathBuf::from(target);
    if direct.join("experiment.json").is_file() {
        validate_source_pack_path(&direct)?;
        let distribution = read_distribution_manifest(&direct)?;
        return Ok((direct, None, distribution));
    }
    let home = crate::home_state::resolve_home_dir(None);
    let pack = super::super::resolve_pack_source(target, registry, &home).await?;
    anyhow::ensure!(
        gents::pack::declared_paths(pack.manifest())
            .iter()
            .any(|path| path == "experiment.json"),
        "pack {target} has no scenario (experiment.json)"
    );
    let (root, lease) = super::super::materialize_cached_pack(&home, &pack)?;
    Ok((root, Some(lease), pack.manifest().clone()))
}

pub(super) fn load_manifest(
    pack: &Path,
    distribution: &gents::pack::PackManifest,
) -> Result<ScenarioManifest> {
    load_manifest_with(pack, distribution, &|name| std::env::var(name).ok())
}

pub(super) fn load_manifest_with(
    pack: &Path,
    distribution: &gents::pack::PackManifest,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<ScenarioManifest> {
    let path = pack.join("experiment.json");
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let expanded = interpolate_with(&raw, lookup).map_err(|missing| {
        anyhow::anyhow!(
            "{} references unset environment variable(s): {}",
            path.display(),
            missing.join(", ")
        )
    })?;
    let mut manifest: ScenarioManifest =
        serde_json::from_str(&expanded).with_context(|| format!("parsing {}", path.display()))?;
    anyhow::ensure!(
        distribution.name == manifest.name,
        "scenario name must match the distribution manifest"
    );
    manifest.dependencies = distribution.metadata.dependencies.clone();
    manifest.plugins = distribution.metadata.plugins.clone();
    manifest.config = Some(load_pack_config_with(pack, lookup)?);
    validate_manifest(&manifest).with_context(|| format!("validating {}", path.display()))?;
    validate_expected_trigger_correlations(&manifest)
        .with_context(|| format!("validating trigger correlations in {}", path.display()))?;
    validate_prompt_tool_contracts(pack, &manifest)
        .with_context(|| format!("validating prompt/tool contracts in {}", path.display()))?;
    validate_task_goal_declarations(&manifest)
        .with_context(|| format!("validating task goal declarations in {}", path.display()))?;
    Ok(manifest)
}

fn load_pack_config_with(
    pack: &Path,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<gents::documents::PackConfig> {
    const VALIDATION_OWNER: &str = "did:key:zScenarioPackValidationOwner";
    let path = pack.join("pack_config.json");
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading canonical pack config {}", path.display()))?;
    let value = serde_json::from_str(&raw)
        .with_context(|| format!("parsing canonical pack config {}", path.display()))?;
    gents::pack::decode_pack_config(
        value,
        Some(&gents::pack::PackInstallOptions {
            node_did: VALIDATION_OWNER.into(),
        }),
        lookup,
        &|_, _, reference| read_pack_sidecar(pack, reference),
    )
    .and_then(|config| {
        gents::pack::ensure_pack_leaves_default_unselected(&config)?;
        Ok(config)
    })
    .with_context(|| format!("decoding canonical pack config {}", path.display()))
}

pub(super) fn read_pack_sidecar(pack: &Path, reference: &str) -> Result<String> {
    let relative = reference
        .strip_prefix("./")
        .context("pack sidecar path must start with ./")?;
    anyhow::ensure!(
        gents::pack::is_distributable_asset_path(relative),
        "pack sidecar path is not a pack asset path: {reference}"
    );
    std::fs::read_to_string(pack.join(relative))
        .with_context(|| format!("reading pack sidecar {relative}"))
}

pub(super) fn scenario_config(
    manifest: &ScenarioManifest,
) -> Result<&gents::documents::PackConfig> {
    manifest
        .config
        .as_ref()
        .context("scenario canonical configuration was not loaded")
}

pub(super) fn trigger_source_collections(
    manifest: &ScenarioManifest,
    trigger_ids: &[String],
) -> Result<Vec<String>> {
    let config = scenario_config(manifest)?;
    let mut collections = std::collections::BTreeSet::new();
    for trigger_id in trigger_ids {
        let source = scenario_event_source(config, trigger_id)?;
        let source_collection = source.source_collection.as_str();
        validate_collection_identifier(source_collection)?;
        collections.insert(source_collection.to_string());
    }
    Ok(collections.into_iter().collect())
}

fn scenario_event_source<'a>(
    config: &'a gents::documents::PackConfig,
    trigger_id: &str,
) -> Result<&'a gents::documents::EventSource> {
    let trigger = config
        .triggers
        .iter()
        .find(|trigger| trigger.trigger_id == trigger_id)
        .with_context(|| format!("missing Trigger {trigger_id}"))?;
    let event_source_id = match &trigger.source {
        gents::documents::TriggerSource::Event { event_source_id } => event_source_id,
        gents::documents::TriggerSource::Schedule { .. } => {
            bail!("scenario Trigger {trigger_id} must use an event source")
        }
    };
    config
        .event_sources
        .iter()
        .find(|source| source.event_source_id == *event_source_id)
        .with_context(|| format!("missing EventSource {event_source_id}"))
}

fn validate_expected_trigger_correlations(manifest: &ScenarioManifest) -> Result<()> {
    let config = scenario_config(manifest)?;
    for trigger_id in &manifest.expect.trigger_ids {
        let source = scenario_event_source(config, trigger_id)?;
        if source
            .correlation_field
            .as_deref()
            .is_none_or(|field| field.trim().is_empty())
        {
            bail!(
                "expect.trigger_ids entry {trigger_id} uses EventSource {} without a \
                 correlation_field, so its requests can never match the scenario run",
                source.event_source_id
            );
        }
    }
    Ok(())
}
