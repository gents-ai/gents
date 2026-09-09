use anyhow::{Context, Result};
use serde_json::Value;

use super::{asset_path, interpolate, validate_pack_manifest, PackInstallOptions, PackManifest};
use crate::document_config::PackConfig;

/// Decode the same canonical authored bundle for document and graph packs.
/// Callers provide asset bytes and environment lookup; distribution admission,
/// interpolation, sidecars and installation scope have this single owner.
/// Reference/resource validation and persistence remain the installer's job.
pub fn load_pack_config(
    manifest: &PackManifest,
    options: &PackInstallOptions,
    read_asset: &dyn Fn(&str) -> Result<Vec<u8>>,
    environment: &dyn Fn(&str) -> Option<String>,
) -> Result<PackConfig> {
    validate_pack_manifest(manifest)?;
    anyhow::ensure!(
        !options.agent_did.trim().is_empty(),
        "installation owner DID must not be blank"
    );
    let path = manifest
        .config
        .as_deref()
        .context("pack has no configuration")?;
    let bytes = read_asset(path).with_context(|| format!("reading pack config {path}"))?;
    let mut value: Value = serde_json::from_slice(&bytes).context("parsing pack config JSON")?;
    interpolate_values(&mut value, environment)?;
    let root = value
        .as_object_mut()
        .context("pack config must be an object")?;
    bind_owner(
        root.get_mut("agent_principal")
            .context("pack config requires agent_principal")?,
        &options.agent_did,
        "agent_principal",
    )?;
    // These are document roots, not an unrestricted recursive DID replacement.
    // The canonical serde decoder below rejects unknown collections/fields.
    for collection in [
        "agent_behaviors",
        "contexts",
        "compactions",
        "tools",
        "subagent_targets",
        "skills",
        "datastore_tool_surfaces",
        "chain_key_bindings",
        "eth_tools",
        "inference_backends",
        "inference_profiles",
        "inference_sampling",
        "inference_execution",
        "inference_retry_policies",
        "tool_service_registries",
        "projection_acp_bindings",
        "tasks",
        "triggers",
        "schedules",
        "event_sources",
        "callbacks",
        "callback_bindings",
        "callback_modules",
        "repository_placements",
        "graphs",
        "graph_intents",
        "graph_capabilities",
    ] {
        if let Some(values) = root.get_mut(collection) {
            if values.is_null() {
                continue;
            }
            let rows = values
                .as_array_mut()
                .with_context(|| format!("{collection} must be an array or null"))?;
            for (index, row) in rows.iter_mut().enumerate() {
                bind_owner(row, &options.agent_did, &format!("{collection}[{index}]"))?;
            }
        }
    }
    let mut config: PackConfig =
        serde_json::from_value(value).context("decoding canonical pack configuration")?;
    for context in &mut config.contexts {
        if let Some(prompt) = &mut context.system_prompt {
            hydrate_sidecar(prompt, path, manifest, read_asset)?;
        }
    }
    for task in &mut config.tasks {
        hydrate_sidecar(&mut task.prompt_template, path, manifest, read_asset)?;
    }
    Ok(config)
}

fn bind_owner(value: &mut Value, owner: &str, location: &str) -> Result<()> {
    let object = value
        .as_object_mut()
        .with_context(|| format!("{location} must be a document object"))?;
    match object.get("agent_did") {
        None => {
            object.insert("agent_did".into(), owner.into());
        }
        Some(value) => anyhow::ensure!(
            value.as_str() == Some(owner),
            "{location} owner does not match installation scope"
        ),
    }
    Ok(())
}

fn interpolate_values(
    value: &mut Value,
    environment: &dyn Fn(&str) -> Option<String>,
) -> Result<()> {
    match value {
        Value::String(text) => {
            *text = interpolate::interpolate_with(text, environment).map_err(|missing| {
                anyhow::anyhow!(
                    "pack config references unset environment variables: {}",
                    missing.join(", ")
                )
            })?;
        }
        Value::Array(values) => {
            for value in values {
                interpolate_values(value, environment)?;
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                interpolate_values(value, environment)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn hydrate_sidecar(
    prompt: &mut String,
    config_path: &str,
    manifest: &PackManifest,
    read_asset: &dyn Fn(&str) -> Result<Vec<u8>>,
) -> Result<()> {
    let Some(relative) = prompt.strip_prefix("./") else {
        return Ok(());
    };
    anyhow::ensure!(
        asset_path::is_distributable_asset(relative)
            && asset_path::has_canonical_asset_spelling(relative),
        "unsafe/non-canonical pack sidecar path: {relative}"
    );
    let path = match config_path.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/{relative}"),
        None => relative.to_owned(),
    };
    anyhow::ensure!(
        manifest.metadata.assets.contains(&path),
        "undeclared pack sidecar: {path}"
    );
    let bytes = read_asset(&path).with_context(|| format!("reading pack sidecar {path}"))?;
    *prompt =
        String::from_utf8(bytes).with_context(|| format!("pack sidecar {path} is not UTF-8"))?;
    Ok(())
}

#[cfg(test)]
mod tests;
