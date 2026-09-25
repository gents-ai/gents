use anyhow::{Context, Result};
use serde_json::Value;

use super::{asset_path, interpolate, validate_pack_manifest, PackInstallOptions, PackManifest};
use crate::document_config::{EvalCase, PackConfig};

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
    let mut config = decode_pack_config(value, Some(options), environment, &|_, _, reference| {
        let mut prompt = reference.to_owned();
        hydrate_sidecar(&mut prompt, path, manifest, read_asset)?;
        Ok(prompt)
    })?;
    for capability in &mut config.graph_capabilities {
        pin_pack_plugin(manifest, read_asset, capability)?;
    }
    super::inference::validate_pack_inference_authoring(manifest, &config)?;
    Ok(config)
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
    hydrate_eval_cases(root, read_sidecar)?;
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
    for skill in &mut config.skills {
        if let Some(instructions) = &mut skill.instructions {
            if instructions.starts_with("./") {
                *instructions =
                    read_sidecar(crate::Collection::Skill, &skill.skill_id, instructions)?;
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

/// A plugin node that names one of the pack's own plugins by `name` runs that
/// artifact: the name is qualified with the pack's namespace, as installing the
/// pack records it, and pinned to the artifact's digest. A plugin from outside
/// the pack is named `namespace/name` and pinned by the author.
fn pin_pack_plugin(
    manifest: &PackManifest,
    read_asset: &dyn Fn(&str) -> Result<Vec<u8>>,
    capability: &mut crate::graph_pipeline::StageCapability,
) -> Result<()> {
    let crate::graph_pipeline::StageTarget::Plugin { plugin, digest, .. } = &mut capability.target
    else {
        return Ok(());
    };
    if plugin.contains('/') {
        return Ok(());
    }
    let declared = manifest
        .metadata
        .plugins
        .iter()
        .find(|declared| declared.name == *plugin)
        .with_context(|| {
            format!(
                "graph capability {} runs plugin {plugin:?}, which this pack does not declare",
                capability.capability_id
            )
        })?;
    let bytes = read_asset(&declared.artifact)
        .with_context(|| format!("reading plugin artifact {}", declared.artifact))?;
    let pinned = format!(
        "sha256:{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(&bytes)
    );
    if let Some(authored) = digest.as_deref() {
        anyhow::ensure!(
            authored == pinned,
            "graph capability {} pins plugin {plugin:?} to {authored}, but the pack ships {pinned}",
            capability.capability_id
        );
    }
    *digest = Some(pinned);
    *plugin = format!("{}/{}", manifest.metadata.namespace, plugin);
    Ok(())
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

/// A case of an eval definition may be authored as a `./` sidecar path: the
/// file holds one `EvalCase`, read through the caller's sidecar boundary after
/// interpolation, so its prompts stay literal like every other sidecar. The
/// installed definition carries the cases inline; the paths never reach it.
fn hydrate_eval_cases(
    root: &mut serde_json::Map<String, Value>,
    read_sidecar: &dyn Fn(crate::Collection, &str, &str) -> Result<String>,
) -> Result<()> {
    let Some(definitions) = root
        .get_mut("eval_definitions")
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    for definition in definitions {
        let definition_id = definition
            .get("definition_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let Some(cases) = definition.get_mut("cases").and_then(Value::as_array_mut) else {
            continue;
        };
        for case in cases {
            let Some(reference) = case.as_str().map(str::to_owned) else {
                continue;
            };
            anyhow::ensure!(
                reference.starts_with("./"),
                "eval definition {definition_id:?} case {reference:?} must be an inline case or a ./ sidecar path"
            );
            let text = read_sidecar(
                crate::Collection::EvalDefinition,
                &definition_id,
                &reference,
            )?;
            let parsed: EvalCase = serde_json::from_str(&text).with_context(|| {
                format!("eval definition {definition_id:?} case sidecar {reference} is not one EvalCase")
            })?;
            *case = serde_json::to_value(parsed)?;
        }
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
