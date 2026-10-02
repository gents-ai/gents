//! Model-facing DefraDB schema administration. Configuration documents and data
//! access retain their existing owners; the tool delegates schema operations to
//! ConfigAccess and DefraDB, under an explicit node-management capability.

use crate::config_client::ConfigAccess;
use crate::defra_node::EmbeddedNode;
use crate::llm::tool::{Tool, ToolDefinition};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

mod help;
#[cfg(test)]
mod tests;

pub const SCHEMA_TOOL_NAME: &str = "schema";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaParams {
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub options: Map<String, Value>,
}

struct SchemaCommand {
    args: SchemaParams,
    preview: bool,
    digest: Option<String>,
}

impl SchemaCommand {
    fn parse(mut args: SchemaParams) -> Result<Self> {
        let preview = args.argv.get(1).is_some_and(|word| word == "preview");
        if preview {
            args.argv.remove(1);
        }
        let words: Vec<_> = args.argv.iter().map(String::as_str).collect();
        let takes_id = matches!(
            words.as_slice(),
            ["collection", "get" | "update" | "materialize", _]
                | ["version", "list" | "get" | "activate", _]
        );
        if takes_id {
            let id = args.argv.pop().unwrap();
            ensure!(
                args.target_id.as_ref().is_none_or(|value| value == &id),
                "positional ID conflicts with target_id; supply one ID"
            );
            args.target_id = Some(id);
        }
        let digest = args
            .options
            .remove("digest")
            .map(|value| {
                value
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .context("options.digest must be a nonempty string returned by preview")
            })
            .transpose()?;
        Ok(Self {
            args,
            preview,
            digest,
        })
    }
}

fn preview_call(args: &SchemaParams) -> SchemaParams {
    let mut next = args.clone();
    next.argv.insert(1, "preview".into());
    next.options.remove("digest");
    next
}

fn apply_call(args: &SchemaParams, digest: &str) -> SchemaParams {
    let mut next = args.clone();
    next.options.insert("digest".into(), json!(digest));
    next
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SchemaError(String);

#[derive(Debug, thiserror::Error)]
#[error("missing or stale digest: run the returned preview call, inspect the effect, then use its next_call. options.digest binds the command and observed schema")]
struct SchemaDigestMismatch;

#[derive(Clone)]
pub struct SchemaTool {
    node: Arc<EmbeddedNode>,
}

impl SchemaTool {
    pub fn new(node: Arc<EmbeddedNode>) -> Self {
        Self { node }
    }

    async fn execute(&self, command: &SchemaCommand) -> Result<Value> {
        let args = &command.args;
        let access = ConfigAccess::Local(self.node.clone());
        let words: Vec<_> = args.argv.iter().map(String::as_str).collect();
        let target = || {
            args.target_id.as_deref().filter(|s| !s.is_empty()).context("target_id is required; use collection list or version list to discover exact names and IDs")
        };
        let option = |key: &str| {
            args.options.get(key).context(format!(
                "options.{key} is required; read help for this command"
            ))
        };
        let text = |key: &str| -> Result<&str> {
            option(key)?
                .as_str()
                .filter(|s| !s.is_empty())
                .context(format!("options.{key} must be a nonempty string"))
        };
        let allowed: &[&str] = match words.as_slice() {
            ["collection", "create"] => &["sdl"],
            ["collection", "update"] => &["patch"],
            ["view", "create"] => &["query", "sdl"],
            ["migration", "set"] => &["config"],
            ["collection", "list" | "get" | "materialize"] | ["version", "list" | "get" | "activate"] => &[],
            _ => bail!("unknown schema command; use RESOURCE VERB [ID]. Read [\"help\"] for resources, or RESOURCE VERB --help for parameters"),
        };
        for key in args.options.keys() {
            ensure!(
                allowed.contains(&key.as_str()),
                "unknown option {key:?}; accepted options: {allowed:?}"
            );
        }
        let read = matches!(words.as_slice(), ["collection" | "version", "list" | "get"]);
        ensure!(
            !read || (!command.preview && command.digest.is_none()),
            "reads do not use preview or digest"
        );
        let uses_target = !matches!(
            words.as_slice(),
            ["collection", "list" | "create"] | ["view", "create"] | ["migration", "set"]
        );
        ensure!(
            uses_target || args.target_id.is_none(),
            "this command does not accept target_id"
        );
        match words.as_slice() {
            ["collection", "list"] => {
                return Ok(json!({"collections":access.collection_names().await?}))
            }
            ["collection", "get"] => {
                return Ok(
                    json!({"collection":access.collection_version(target()?).await?.context("collection not found; use collection list")?}),
                )
            }
            ["version", "list"] => {
                let versions = access.collection_versions().await?;
                let values: Vec<_> = versions
                    .into_iter()
                    .filter(|v| {
                        args.target_id
                            .as_ref()
                            .is_none_or(|name| v["Name"].as_str() == Some(name))
                    })
                    .map(|v| json!({"Name":v["Name"],"VersionID":v["VersionID"],"IsActive":v["IsActive"],"PreviousVersion":v["PreviousVersion"]}))
                    .collect();
                return Ok(json!({"versions":values}));
            }
            ["version", "get"] => {
                return Ok(json!({"version":find_version(&access, target()?).await?}))
            }
            _ => {}
        }
        ensure!(
            !(command.preview && command.digest.is_some()),
            "preview returns a digest; omit options.digest while previewing"
        );
        let before = match words.as_slice() {
            ["collection", "create"] | ["view", "create"] => {
                if words[0] == "view" {
                    text("query")?;
                }
                let definitions = query::parse_sdl(text("sdl")?)?;
                ensure!(
                    !definitions.is_empty(),
                    "SDL must declare at least one collection"
                );
                let mut existing = Map::new();
                for definition in definitions {
                    ensure_application_collection(&definition.name)?;
                    existing.insert(
                        definition.name.clone(),
                        access
                            .collection_version(&definition.name)
                            .await?
                            .unwrap_or(Value::Null),
                    );
                }
                if words[0] == "collection" {
                    crate::config_client::preview_schema_install(&access, text("sdl")?).await.context("collection create installs new schemas or verifies matching schemas; use collection update to evolve an existing schema")?;
                }
                Value::Object(existing)
            }
            ["collection", "update" | "materialize"] => {
                let name = target()?;
                ensure_application_collection(name)?;
                let current = access
                    .collection_version(name)
                    .await?
                    .context("collection not found; use collection list")?;
                if words[1] == "update" {
                    validate_patch(name, option("patch")?)?;
                }
                current
            }
            ["version", "activate"] => {
                let version = find_version(&access, target()?).await?;
                let name = version["Name"]
                    .as_str()
                    .context("version has no collection name")?;
                ensure_application_collection(name)?;
                json!({"destination":version,"active":access.collection_version(name).await?})
            }
            ["migration", "set"] => {
                let config: crate::defra_node::LensConfig =
                    serde_json::from_value(option("config")?.clone())?;
                config.validate_for_http()?;
                ensure!(
                    !config.lenses.is_empty(),
                    "config.Lenses must contain inline WASM modules"
                );
                let source = find_version(&access, &config.source_schema_version_id).await?;
                let destination =
                    find_version(&access, &config.destination_schema_version_id).await?;
                for v in [&source, &destination] {
                    ensure_application_collection(
                        v["Name"]
                            .as_str()
                            .context("version has no collection name")?,
                    )?;
                }
                json!({"source":source,"destination":destination})
            }
            _ => unreachable!(),
        };
        let intent = args;
        let digest = format!(
            "sha256:{:x}",
            Sha256::digest(serde_json::to_vec(
                &json!({"intent":intent,"before":before})
            )?)
        );
        if command.preview {
            return Ok(
                json!({"committed":false,"digest":digest,"command":args.argv,"target_id":args.target_id,"before":before,
                "effect":help::effect(&words),
                "validation":"Input shape and observed schema checked. DefraDB validates and authorizes publication when applied; preview is not a transaction.",
                "next_call":{"tool":"schema","args":apply_call(args, &digest)}}),
            );
        }
        if command.digest.as_deref() != Some(digest.as_str()) {
            return Err(SchemaDigestMismatch.into());
        }
        let result = match words.as_slice() {
            ["collection", "create"] => {
                let plan =
                    crate::config_client::preview_schema_install(&access, text("sdl")?).await?;
                serde_json::to_value(
                    crate::config_client::apply_schema_install(
                        &access,
                        text("sdl")?,
                        &plan.artifact_digest,
                    )
                    .await?,
                )?
            }
            ["collection", "update"] => {
                let mut patch = option("patch")?
                    .as_array()
                    .context("patch must be an array")?
                    .clone();
                // The version test reaches DefraDB with the patch; a stale caller
                // cannot silently apply its positional edits to another schema.
                patch.insert(0,json!({"op":"test","path":format!("/{}/VersionID",target()?),"value":before["VersionID"]}));
                access
                    .patch_collection_schema(target()?, &Value::Array(patch))
                    .await?
            }
            ["version", "activate"] => {
                access.activate_collection_version(target()?).await?;
                json!({"version_id":target()?})
            }
            ["migration", "set"] => {
                access
                    .set_schema_migration(serde_json::from_value(option("config")?.clone())?)
                    .await?
            }
            ["view", "create"] => {
                access.add_schema_view(text("query")?, text("sdl")?).await?;
                json!({"created":true})
            }
            ["collection", "materialize"] => {
                json!({"documents_materialized":access.materialize_schema_collection(target()?).await?})
            }
            _ => unreachable!(),
        };
        Ok(json!({"committed":true,"result":result}))
    }
}

async fn find_version(access: &ConfigAccess, id: &str) -> Result<Value> {
    access
        .collection_versions()
        .await?
        .into_iter()
        .find(|v| v["VersionID"].as_str() == Some(id))
        .context("version not found; use version list and copy its VersionID")
}

/// Product schemas are tied to compiled protocol contracts and the migration
/// registry. Runtime administration may evolve application collections, but
/// changing a product contract requires a matching application release.
fn ensure_application_collection(name: &str) -> Result<()> {
    crate::graphql::validate_collection_identifier(name)?;
    ensure!(!crate::migration::DEFAULT_REGISTRY.managed_names().any(|n| n==name), "{name} is a Gents-managed schema; its evolution belongs to the product migration registry. Use config for its documents");
    Ok(())
}

fn validate_patch(collection: &str, patch: &Value) -> Result<()> {
    let ops = patch
        .as_array()
        .context("options.patch must be an RFC 6902 JSON array")?;
    ensure!(!ops.is_empty(), "options.patch cannot be empty");
    let prefix = format!("/{collection}/");
    for op in ops {
        ensure!(op.is_object(), "every patch operation must be an object");
        ensure!(
            matches!(
                op["op"].as_str(),
                Some("add" | "remove" | "replace" | "test" | "copy" | "move")
            ),
            "patch op must be add, remove, replace, test, copy or move"
        );
        ensure!(
            !matches!(op["op"].as_str(), Some("add" | "replace" | "test"))
                || op.get("value").is_some(),
            "patch add/replace/test requires value (null is allowed)"
        );
        for key in ["path", "from"] {
            if key == "from" && !matches!(op["op"].as_str(), Some("copy" | "move")) {
                continue;
            }
            let path = op[key]
                .as_str()
                .context(format!("patch {key} is required"))?;
            ensure!(path != format!("/{collection}/Name"), "collection renames require a separately reviewed migration; this tool preserves collection names");
            ensure!(path.starts_with(&prefix), "patch {key} must address fields within {prefix}; collection renames/copies require a separately reviewed migration");
        }
    }
    Ok(())
}

impl Tool for SchemaTool {
    const NAME: &'static str = SCHEMA_TOOL_NAME;
    type Error = SchemaError;
    type Args = SchemaParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.into(),
            description: "Manage DefraDB application schemas. Commands: argv:[RESOURCE,VERB]; IDs follow the verb or use target_id. Discover commands and SDL concepts with [\"help\"], then [\"help\",TOPIC,SUBTOPIC]; command inputs: [RESOURCE,VERB,\"--help\"]. Configuration and document access use their own tools.".into(),
            parameters: json!({"type":"object","required":["argv"],"additionalProperties":false,"properties":{
                "argv":{"type":"array","items":{"type":"string"},"minItems":1},
                "target_id":{"type":"string","description":"Collection name or exact version ID; may instead follow the verb in argv."},
                "options":{"type":"object","additionalProperties":true}
            }})
        }
    }

    async fn call(&self, args: Self::Args) -> std::result::Result<String, Self::Error> {
        if serde_json::to_vec(&args).map_or(true, |v| v.len() > 32 * 1024 * 1024) {
            return Err(SchemaError(
                "schema call exceeds 32 MiB; submit smaller schemas or modules".into(),
            ));
        }
        if args.argv == ["batch"] {
            if args.target_id.is_some() || args.options.len() != 1 {
                return Err(SchemaError(
                    "batch accepts only options.operations; each item is a schema command".into(),
                ));
            }
            let operations = args.options.get("operations").and_then(Value::as_array).filter(|ops|!ops.is_empty()&&ops.len()<=64)
                .ok_or_else(||SchemaError("options.operations must be an array of 1–64 schema calls; nested batches are not supported".into()))?;
            let mut results = Vec::new();
            for (index, operation) in operations.iter().enumerate() {
                let result = match serde_json::from_value::<SchemaParams>(operation.clone()) {
                    Ok(call) if call.argv != ["batch"] => self.call_one(call).await,
                    Ok(_) => Err(SchemaError("nested batches are not supported".into())),
                    Err(error) => Err(SchemaError(error.to_string())),
                };
                match result {
                    Ok(result) => results.push(json!({"index":index,"ok":true,"result":serde_json::from_str::<Value>(&result).unwrap_or(Value::String(result))})),
                    Err(error) => {
                        results.push(json!({"index":index,"ok":false,"error":serde_json::from_str::<Value>(&error.0).unwrap_or(Value::String(error.0))}));
                        return Err(SchemaError(json!({"error":format!("batch stopped at item {index}; earlier operations retain their results"),"atomic":false,"results":results,"unattempted":operations.len()-index-1}).to_string()));
                    }
                }
            }
            return Ok(json!({"atomic":false,"results":results}).to_string());
        }
        self.call_one(args).await
    }
}

impl SchemaTool {
    async fn call_one(&self, mut args: SchemaParams) -> std::result::Result<String, SchemaError> {
        if args
            .argv
            .last()
            .is_some_and(|word| matches!(word.as_str(), "--help" | "-h"))
        {
            args.argv.pop();
            if args.argv.get(1).is_some_and(|word| word == "preview") {
                args.argv.remove(1);
            }
            args.argv.truncate(2);
            args.argv.insert(0, "help".into());
        }
        if args.argv.first().is_some_and(|s| s == "help") {
            if args.target_id.is_some() || !args.options.is_empty() {
                return Err(SchemaError(
                    "help accepts argv only; omit target_id and options".into(),
                ));
            }
            return help::page(&args.argv[1..])
                .map(str::to_owned)
                .map_err(|e| SchemaError(e.to_string()));
        }
        let command = match SchemaCommand::parse(args.clone()) {
            Ok(command) => command,
            Err(error) => return Err(schema_error(error, help_call(&args))),
        };
        let args = &command.args;
        if let Some(value) = args.options.get("preview") {
            if let Some(preview) = value.as_bool().filter(|_| args.argv.len() >= 2) {
                let mut next = args.clone();
                next.options.remove("preview");
                if preview || command.preview {
                    next = preview_call(&next);
                } else if let Some(digest) = &command.digest {
                    next = apply_call(&next, digest);
                }
                return Err(schema_error(anyhow::anyhow!(
                    "preview is a command word, not an option; use RESOURCE preview VERB, then apply the returned next_call"
                ), json!({"tool":"schema","args":next})));
            }
        }
        match self.execute(&command).await {
            Ok(mut value) => {
                let encoded = if command.preview {
                    let effect = value.as_object_mut().unwrap().remove("effect").unwrap();
                    let next_call = value.as_object_mut().unwrap().remove("next_call").unwrap();
                    serde_json::to_string(&crate::self_config::Ordered::reading_order(
                        json!({"effect":effect,"next_call":next_call,"metadata":value}),
                        &["effect", "next_call", "metadata"],
                    ))
                } else {
                    serde_json::to_string(&value)
                };
                encoded.map_err(|error| SchemaError(error.to_string()))
            }
            Err(error) => {
                let recovery = if error.is::<SchemaDigestMismatch>() {
                    json!({"tool":"schema","args":preview_call(args)})
                } else if let Some(mismatch) =
                    error.downcast_ref::<crate::config_client::SchemaInstallMismatch>()
                {
                    json!({"tool":"schema","args":{"argv":["collection","get"],"target_id":mismatch.collection}})
                } else {
                    help_call(args)
                };
                Err(schema_error(error, recovery))
            }
        }
    }
}

fn help_call(args: &SchemaParams) -> Value {
    let resource = args.argv.first().map(String::as_str).unwrap_or("");
    let argv = if matches!(resource, "collection" | "version" | "migration" | "view") {
        if args.argv.len() >= 2 && help::page(&args.argv[..2]).is_ok() {
            vec!["help", resource, args.argv[1].as_str()]
        } else {
            vec!["help", resource]
        }
    } else {
        vec!["help"]
    };
    json!({"tool":"schema","args":{"argv":argv}})
}

fn schema_error(error: anyhow::Error, recovery: Value) -> SchemaError {
    SchemaError(json!({"error":format!("{error:#}"),"recovery":recovery}).to_string())
}
