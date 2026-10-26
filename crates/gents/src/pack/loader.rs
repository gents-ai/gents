use serde_json::Value;

use super::{
    asset_path, interpolate, validate_pack_manifest, PackError, PackInstallOptions, PackManifest,
};
use crate::document_config::{EvalCase, PackConfig};

#[derive(Debug, thiserror::Error)]
pub enum LoaderError {
    #[error("pack has no configuration")]
    ConfigMissing,
    #[error("parsing pack config JSON")]
    ConfigJson(#[source] serde_json::Error),
    #[error("node owner references unset variables: {missing}")]
    NodeOwnerUnset { missing: String },
    #[error("configuration requires an explicit node owner")]
    OwnerMissing,
    #[error("configuration owner DID must not be blank")]
    OwnerBlank,
    #[error("pack config references unset environment variables: {missing}")]
    ConfigUnset { missing: String },
    #[error("pack config must be an object")]
    ConfigNotObject,
    #[error("pack config requires node")]
    NodeMissing,
    #[error("{collection} must be an array or null")]
    CollectionNotArray { collection: String },
    #[error("decoding canonical pack configuration")]
    Decode(#[source] serde_json::Error),
    #[error("{location} must be a document object")]
    NotDocumentObject { location: String },
    #[error("{location} owner does not match installation scope")]
    OwnerMismatch { location: String },
    #[error("entry {entry:?} input_schema")]
    EntryInputSchema {
        entry: String,
        #[source]
        source: Box<crate::graph_pipeline::GraphPipelineError>,
    },
    #[error("eval definition {definition_id:?} case {reference:?} must be an inline case or a ./ sidecar path")]
    EvalCaseReference {
        definition_id: String,
        reference: String,
    },
    #[error("eval definition {definition_id:?} case sidecar {reference} is not one EvalCase")]
    EvalCaseSidecar {
        definition_id: String,
        reference: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("unsafe/non-canonical pack sidecar path: {relative}")]
    SidecarPath { relative: String },
    #[error("undeclared pack sidecar: {path}")]
    SidecarUndeclared { path: String },
    #[error("pack sidecar {path} is not UTF-8")]
    SidecarNotUtf8 {
        path: String,
        #[source]
        source: std::string::FromUtf8Error,
    },
    #[error("{what} runs plugin {plugin:?}, which this pack does not declare")]
    PluginUndeclared { what: String, plugin: String },
    #[error("{what} pins plugin {plugin:?} to {authored}, but the pack ships {pinned}")]
    PluginDigestMismatch {
        what: String,
        plugin: String,
        authored: String,
        pinned: String,
    },
    #[error(transparent)]
    Pack(#[from] PackError),
    #[error(transparent)]
    Inference(#[from] super::inference::InferenceError),
    #[error("reading pack config {path}")]
    ReadConfig {
        path: String,
        #[source]
        source: CallbackError,
    },
    #[error("reading plugin artifact {artifact}")]
    ReadPluginArtifact {
        artifact: String,
        #[source]
        source: CallbackError,
    },
    #[error("reading pack sidecar {path}")]
    ReadSidecar {
        path: String,
        #[source]
        source: CallbackError,
    },
    #[error(transparent)]
    Sidecar(CallbackError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// The error a caller-supplied asset or sidecar reader returned; the loader
/// only adds which file it was reading.
pub type CallbackError = Box<dyn std::error::Error + Send + Sync>;

type Result<T, E = LoaderError> = std::result::Result<T, E>;

pub fn load_pack_config<E>(
    manifest: &PackManifest,
    options: &PackInstallOptions,
    read_asset: &dyn Fn(&str) -> std::result::Result<Vec<u8>, E>,
    environment: &dyn Fn(&str) -> Option<String>,
) -> Result<PackConfig>
where
    E: Into<CallbackError>,
{
    validate_pack_manifest(manifest)?;
    let path = manifest
        .config
        .as_deref()
        .ok_or(LoaderError::ConfigMissing)?;
    let bytes = read_asset(path).map_err(|source| LoaderError::ReadConfig {
        path: path.to_owned(),
        source: source.into(),
    })?;
    let value: Value = serde_json::from_slice(&bytes).map_err(LoaderError::ConfigJson)?;
    let mut config = decode_pack_config(value, Some(options), environment, &|_, _, reference| {
        let mut prompt = reference.to_owned();
        hydrate_sidecar(&mut prompt, path, manifest, read_asset)?;
        Ok::<_, LoaderError>(prompt)
    })?;
    ensure_pack_leaves_default_unselected(&config)?;
    pin_pack_plugins(manifest, read_asset, &mut config)?;
    super::inference::validate_pack_inference_authoring(manifest, &config)?;
    Ok(config)
}

pub fn ensure_pack_leaves_default_unselected(config: &PackConfig) -> Result<(), PackError> {
    if let Some(default) = &config.node.default_agent_id {
        return Err(PackError::DefaultAgentSet {
            default: default.clone(),
        });
    }
    Ok(())
}

pub fn pin_pack_plugins<E>(
    manifest: &PackManifest,
    read_asset: &dyn Fn(&str) -> std::result::Result<Vec<u8>, E>,
    config: &mut PackConfig,
) -> Result<()>
where
    E: Into<CallbackError>,
{
    for capability in &mut config.graph_capabilities {
        pin_pack_plugin(manifest, read_asset, capability)?;
    }
    for callback in &mut config.callbacks {
        pin_callback_plugin(manifest, read_asset, callback)?;
    }
    for intent in &mut config.graph_intents {
        for entry in &mut intent.entries {
            pin_entry_prepare_plugin(manifest, read_asset, entry)?;
        }
    }
    Ok(())
}

pub fn decode_pack_config<S>(
    mut value: Value,
    options: Option<&PackInstallOptions>,
    environment: &dyn Fn(&str) -> Option<String>,
    read_sidecar: &dyn Fn(crate::Collection, &str, &str) -> std::result::Result<String, S>,
) -> Result<PackConfig>
where
    S: Into<CallbackError>,
{
    let authored_owner = if options.is_none() {
        value
            .pointer("/node/node_did")
            .and_then(Value::as_str)
            .map(|owner| {
                interpolate::interpolate_with(owner, environment).map_err(|missing| {
                    LoaderError::NodeOwnerUnset {
                        missing: missing.join(", "),
                    }
                })
            })
            .transpose()?
    } else {
        None
    };
    let owner = options
        .map(|options| options.node_did.as_str())
        .or(authored_owner.as_deref())
        .ok_or(LoaderError::OwnerMissing)?;
    if owner.trim().is_empty() {
        return Err(LoaderError::OwnerBlank);
    }
    interpolate_values(&mut value, &|name| {
        if name == "GENTS_PACK_NODE_DID" {
            Some(owner.to_owned())
        } else {
            environment(name)
        }
    })?;
    let root = value.as_object_mut().ok_or(LoaderError::ConfigNotObject)?;
    bind_owner(
        root.get_mut("node").ok_or(LoaderError::NodeMissing)?,
        owner,
        "node",
    )?;
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
                .ok_or_else(|| LoaderError::CollectionNotArray {
                    collection: collection.to_owned(),
                })?;
            for (index, row) in rows.iter_mut().enumerate() {
                bind_owner(row, owner, &format!("{collection}[{index}]"))?;
            }
        }
    }
    hydrate_eval_cases(root, read_sidecar)?;
    let mut config: PackConfig = serde_json::from_value(value).map_err(LoaderError::Decode)?;
    for context in &mut config.contexts {
        if let Some(prompt) = &mut context.system_prompt {
            if prompt.starts_with("./") {
                *prompt =
                    read_sidecar(crate::Collection::AgentContext, &context.context_id, prompt)
                        .map_err(sidecar)?;
            }
        }
    }
    for skill in &mut config.skills {
        if let Some(instructions) = &mut skill.instructions {
            if instructions.starts_with("./") {
                *instructions =
                    read_sidecar(crate::Collection::Skill, &skill.skill_id, instructions)
                        .map_err(sidecar)?;
            }
        }
    }
    for task in &mut config.tasks {
        if task.prompt_template.starts_with("./") {
            task.prompt_template = read_sidecar(
                crate::Collection::Task,
                &task.task_id,
                &task.prompt_template,
            )
            .map_err(sidecar)?;
        }
    }
    Ok(config)
}

fn pin_pack_plugin<E>(
    manifest: &PackManifest,
    read_asset: &dyn Fn(&str) -> std::result::Result<Vec<u8>, E>,
    capability: &mut crate::graph_pipeline::StageCapability,
) -> Result<()>
where
    E: Into<CallbackError>,
{
    let crate::graph_pipeline::StageTarget::Plugin { plugin, digest, .. } = &mut capability.target
    else {
        return Ok(());
    };
    let what = format!("graph capability {}", capability.capability_id);
    if let Some((qualified, pinned)) =
        own_plugin(manifest, read_asset, plugin, digest.as_deref(), &what)?
    {
        *digest = Some(pinned);
        *plugin = qualified;
    }
    Ok(())
}

fn pin_callback_plugin<E>(
    manifest: &PackManifest,
    read_asset: &dyn Fn(&str) -> std::result::Result<Vec<u8>, E>,
    callback: &mut crate::document_config::Callback,
) -> Result<()>
where
    E: Into<CallbackError>,
{
    let crate::document_config::CallbackHandler::Plugin { plugin, digest, .. } =
        &mut callback.handler
    else {
        return Ok(());
    };
    let what = format!("callback {}", callback.callback_id);
    let authored = Some(digest.as_str()).filter(|digest| !digest.is_empty());
    if let Some((qualified, pinned)) = own_plugin(manifest, read_asset, plugin, authored, &what)? {
        *digest = pinned;
        *plugin = qualified;
    }
    Ok(())
}

fn pin_entry_prepare_plugin<E>(
    manifest: &PackManifest,
    read_asset: &dyn Fn(&str) -> std::result::Result<Vec<u8>, E>,
    entry: &mut crate::graph_pipeline::EntryBinding,
) -> Result<()>
where
    E: Into<CallbackError>,
{
    if let Some(schema) = &entry.input_schema {
        crate::graph_pipeline::validate_input_schema(schema).map_err(|source| {
            LoaderError::EntryInputSchema {
                entry: entry.name.clone(),
                source: Box::new(source),
            }
        })?;
    }
    let Some(prepare) = &mut entry.prepare else {
        return Ok(());
    };
    let what = format!("entry {:?} prepare", entry.name);
    if let Some((qualified, pinned)) = own_plugin(
        manifest,
        read_asset,
        &prepare.plugin,
        prepare.digest.as_deref(),
        &what,
    )? {
        prepare.digest = Some(pinned);
        prepare.plugin = qualified;
    }
    Ok(())
}

fn own_plugin<E>(
    manifest: &PackManifest,
    read_asset: &dyn Fn(&str) -> std::result::Result<Vec<u8>, E>,
    plugin: &str,
    authored: Option<&str>,
    what: &str,
) -> Result<Option<(String, String)>>
where
    E: Into<CallbackError>,
{
    if plugin.contains('/') {
        return Ok(None);
    }
    let declared = manifest
        .metadata
        .plugins
        .iter()
        .find(|declared| declared.name == plugin)
        .ok_or_else(|| LoaderError::PluginUndeclared {
            what: what.to_owned(),
            plugin: plugin.to_owned(),
        })?;
    let bytes =
        read_asset(&declared.artifact).map_err(|source| LoaderError::ReadPluginArtifact {
            artifact: declared.artifact.clone(),
            source: source.into(),
        })?;
    let pinned = format!(
        "sha256:{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(&bytes)
    );
    if let Some(authored) = authored {
        if authored != pinned {
            return Err(LoaderError::PluginDigestMismatch {
                what: what.to_owned(),
                plugin: plugin.to_owned(),
                authored: authored.to_owned(),
                pinned,
            });
        }
    }
    Ok(Some((
        format!("{}/{}", manifest.metadata.namespace, plugin),
        pinned,
    )))
}

fn sidecar<S>(error: S) -> LoaderError
where
    S: Into<CallbackError>,
{
    LoaderError::Sidecar(error.into())
}

fn bind_owner(value: &mut Value, owner: &str, location: &str) -> Result<()> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| LoaderError::NotDocumentObject {
            location: location.to_owned(),
        })?;
    match object.get("node_did") {
        None => {
            object.insert("node_did".into(), owner.into());
        }
        Some(value) => {
            if value.as_str() != Some(owner) {
                return Err(LoaderError::OwnerMismatch {
                    location: location.to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn hydrate_eval_cases<S>(
    root: &mut serde_json::Map<String, Value>,
    read_sidecar: &dyn Fn(crate::Collection, &str, &str) -> std::result::Result<String, S>,
) -> Result<()>
where
    S: Into<CallbackError>,
{
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
            if !reference.starts_with("./") {
                return Err(LoaderError::EvalCaseReference {
                    definition_id: definition_id.clone(),
                    reference,
                });
            }
            let text = read_sidecar(
                crate::Collection::EvalDefinition,
                &definition_id,
                &reference,
            )
            .map_err(sidecar)?;
            let parsed: EvalCase =
                serde_json::from_str(&text).map_err(|source| LoaderError::EvalCaseSidecar {
                    definition_id: definition_id.clone(),
                    reference: reference.clone(),
                    source,
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
                LoaderError::ConfigUnset {
                    missing: missing.join(", "),
                }
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

fn hydrate_sidecar<E>(
    prompt: &mut String,
    config_path: &str,
    manifest: &PackManifest,
    read_asset: &dyn Fn(&str) -> std::result::Result<Vec<u8>, E>,
) -> Result<()>
where
    E: Into<CallbackError>,
{
    let Some(relative) = prompt.strip_prefix("./") else {
        return Ok(());
    };
    if !(asset_path::is_distributable_asset(relative)
        && asset_path::has_canonical_asset_spelling(relative))
    {
        return Err(LoaderError::SidecarPath {
            relative: relative.to_owned(),
        });
    }
    let path = match config_path.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/{relative}"),
        None => relative.to_owned(),
    };
    if !manifest.metadata.assets.contains(&path) {
        return Err(LoaderError::SidecarUndeclared { path });
    }
    let bytes = read_asset(&path).map_err(|source| LoaderError::ReadSidecar {
        path: path.clone(),
        source: source.into(),
    })?;
    *prompt = String::from_utf8(bytes).map_err(|source| LoaderError::SidecarNotUtf8 {
        path: path.clone(),
        source,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests;
