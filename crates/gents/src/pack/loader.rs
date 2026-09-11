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
    let path = manifest
        .config
        .as_deref()
        .context("pack has no configuration")?;
    let bytes = read_asset(path).with_context(|| format!("reading pack config {path}"))?;
    let value: Value = serde_json::from_slice(&bytes).context("parsing pack config JSON")?;
    decode_pack_config(value, Some(options), environment, &|_, _, reference| {
        let mut prompt = reference.to_owned();
        hydrate_sidecar(&mut prompt, path, manifest, read_asset)?;
        Ok(prompt)
    })
}

/// Decode canonical authoring for both distributed packs and local configuration.
/// Explicit install scope wins over ambient environment. Without install options,
/// the authored principal must supply its owner. Sidecar access stays with the
/// caller's asset/filesystem boundary; sidecar contents are never interpolated.
pub fn decode_pack_config(
    mut value: Value,
    options: Option<&PackInstallOptions>,
    environment: &dyn Fn(&str) -> Option<String>,
    read_sidecar: &dyn Fn(crate::Collection, &str, &str) -> Result<String>,
) -> Result<PackConfig> {
    let authored_owner = if options.is_none() {
        value
            .pointer("/agent_principal/agent_did")
            .and_then(Value::as_str)
            .map(|owner| {
                interpolate::interpolate_with(owner, environment).map_err(|missing| {
                    anyhow::anyhow!(
                        "principal owner references unset variables: {}",
                        missing.join(", ")
                    )
                })
            })
            .transpose()?
    } else {
        None
    };
    let owner = options
        .map(|options| options.agent_did.as_str())
        .or(authored_owner.as_deref())
        .context("configuration requires an explicit principal owner")?;
    anyhow::ensure!(
        !owner.trim().is_empty(),
        "configuration owner DID must not be blank"
    );
    interpolate_values(&mut value, &|name| {
        if name == "GENTS_PACK_AGENT_DID" {
            Some(owner.to_owned())
        } else {
            environment(name)
        }
    })?;
    let root = value
        .as_object_mut()
        .context("pack config must be an object")?;
    bind_owner(
        root.get_mut("agent_principal")
            .context("pack config requires agent_principal")?,
        owner,
        "agent_principal",
    )?;
    // These are document roots, not an unrestricted recursive DID replacement.
    // The canonical serde decoder below rejects unknown collections/fields.
    for collection in crate::Collection::ALL
        .into_iter()
        .filter_map(|collection| collection.dir_name())
        .chain(["graph_intents", "graph_capabilities"])
    {
        if let Some(values) = root.get_mut(collection) {
            if values.is_null() {
                continue;
            }
            let rows = values
                .as_array_mut()
                .with_context(|| format!("{collection} must be an array or null"))?;
            for (index, row) in rows.iter_mut().enumerate() {
                bind_owner(row, owner, &format!("{collection}[{index}]"))?;
            }
        }
    }
    let mut config: PackConfig =
        serde_json::from_value(value).context("decoding canonical pack configuration")?;
    for context in &mut config.contexts {
        if let Some(prompt) = &mut context.system_prompt {
            if prompt.starts_with("./") {
                *prompt =
                    read_sidecar(crate::Collection::AgentContext, &context.context_id, prompt)?;
            }
        }
    }
    for task in &mut config.tasks {
        if task.prompt_template.starts_with("./") {
            task.prompt_template = read_sidecar(
                crate::Collection::Task,
                &task.task_id,
                &task.prompt_template,
            )?;
        }
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
