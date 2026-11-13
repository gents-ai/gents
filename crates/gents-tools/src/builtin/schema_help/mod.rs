use anyhow::{bail, ensure, Context, Result};
use defra_node::Node as EmbeddedNode;
use gents_loop::tool::{Tool, ToolDefinition};
use gents_protocol::tool_names::SCHEMA_TOOL_NAME;
use gents_store::config_client::ConfigAccess;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::sync::Arc;

mod help;
#[cfg(test)]
mod tests;

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
            [
                "collection",
                "get" | "update" | "materialize" | "empty" | "remove",
                _
            ] | ["version", "list" | "get" | "activate", _]
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
pub enum SchemaIdentityError {
    #[error(transparent)]
    InvalidDid(#[from] identity::Error),
    #[error("{0}")]
    ConfigClient(#[from] gents_store::config_client::ConfigClientError),
}

#[derive(Debug, thiserror::Error)]
#[error("missing or stale digest: run the returned preview call, inspect the effect, then use its next_call. options.digest binds the command and observed schema")]
struct SchemaDigestMismatch;

#[derive(Debug, thiserror::Error)]
#[error("SDL definition is invalid")]
struct SchemaSdlFailure;

/// Why an application collection cannot be emptied or removed yet.
#[derive(Debug, thiserror::Error)]
enum CleanupRefusal {
    #[error("{collection} is still in use; release every user in recovery.release_first, then preview again")]
    InUse {
        collection: String,
        users: Vec<crate::collection_use::CollectionUser>,
    },
    #[error("{collection} holds {documents} document(s) and DefraDB removes only an empty collection; empty it with the recovery call, then preview remove again")]
    NotEmpty {
        collection: String,
        documents: usize,
    },
}

impl CleanupRefusal {
    fn recovery(&self, args: &SchemaParams) -> Value {
        match self {
            Self::InUse { users, .. } => {
                json!({"release_first":users,"then":{"tool":"schema","args":preview_call(args)}})
            }
            Self::NotEmpty { collection, .. } => {
                json!({"tool":"schema","args":{"argv":["collection","preview","empty",collection]}})
            }
        }
    }
}

#[derive(Clone)]
pub struct SchemaTool {
    access: ConfigAccess,
    node_did: String,
    home: Option<std::path::PathBuf>,
    /// The calling agent and whether it holds the `tools` category of
    /// `config`: what a create receipt needs to say how the new collection
    /// becomes writable.
    caller: Option<(String, bool)>,
}

impl SchemaTool {
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn new(node: Arc<EmbeddedNode>) -> Self {
        let node_did = node.node_identity_did().unwrap_or_default().to_owned();
        Self {
            access: match identity::Did::new(node_did.as_str()) {
                Ok(actor) => ConfigAccess::local_as(node, actor),
                Err(_) => ConfigAccess::local(node),
            },
            node_did,
            home: None,
            caller: None,
        }
    }

    /// `node_did` owns the configuration, and `home` holds the installed
    /// packs, that collection cleanup checks for remaining users.
    pub fn for_managed_agent(
        node: Arc<EmbeddedNode>,
        node_did: &str,
        home: Option<&std::path::Path>,
        agent: &dyn gents_store::identity::NodeIdentity,
    ) -> std::result::Result<Self, SchemaIdentityError> {
        Ok(Self {
            access: ConfigAccess::local_managed_agent(
                node,
                identity::Did::new(agent.did())?,
                agent,
            )?,
            node_did: node_did.to_owned(),
            home: home.map(ToOwned::to_owned),
            caller: Some((agent.did().to_owned(), false)),
        })
    }

    /// Whether the calling agent holds the `tools` category of `config`.
    pub fn with_tools_category(mut self, held: bool) -> Self {
        if let Some((_, holds_tools)) = &mut self.caller {
            *holds_tools = held;
        }
        self
    }

    /// How the calling agent writes to the collections it just created.
    async fn write_access(&self, collections: &[String]) -> Option<Value> {
        let (agent_did, holds_tools) = self.caller.as_ref()?;
        let datastore = self
            .access
            .transact_readonly("schema.write_grant", |txn| {
                Box::pin(async move {
                    gents_documents::load_agent_context_and_tools_in_txn(
                        txn,
                        &self.node_did,
                        agent_did,
                    )
                    .await
                })
            })
            .await
            .ok()
            .and_then(|(_, tools)| tools)
            .map(|tools| tools.datastore.unwrap_or_default());
        Some(crate::builtin::defra_write::write_grant_guidance(
            *holds_tools,
            datastore.as_ref(),
            collections,
        ))
    }

    /// The application collection `name` and what emptying or removing it
    /// loses. Emptying leaves every reference intact, so its users are listed
    /// in the loss; removal is refused while any remain.
    async fn cleanup_target(
        &self,
        name: &str,
        remove: bool,
    ) -> Result<std::result::Result<Value, CleanupRefusal>> {
        ensure_application_collection(name)?;
        let access = &self.access;
        let current = access
            .collection_version(name)
            .await?
            .context("collection not found; use collection list")?;
        let users = crate::collection_use::application_collection_users(
            access,
            self.home.as_deref(),
            &self.node_did,
            name,
            None,
        )
        .await?;
        if remove && !users.is_empty() {
            return Ok(Err(CleanupRefusal::InUse {
                collection: name.to_owned(),
                users,
            }));
        }
        let rows = access
            .execute(&format!("{{ {name} {{ _docID }} }}"))
            .await?;
        let documents = rows["data"][name]
            .as_array()
            .context("document count query did not return rows")?
            .len();
        if remove && documents > 0 {
            return Ok(Err(CleanupRefusal::NotEmpty {
                collection: name.to_owned(),
                documents,
            }));
        }
        let versions = access.collection_versions().await?;
        let names = |key: &str| -> Vec<&Value> {
            current[key]
                .as_array()
                .into_iter()
                .flatten()
                .map(|index| index.get("Name").unwrap_or(index))
                .collect()
        };
        let mut loss = json!({
            "documents": documents,
            "indexes": names("Indexes"),
            "fulltext_indexes": names("FullTextIndexes"),
            "encrypted_indexes": names("EncryptedIndexes"),
            "versions": versions.iter().filter(|v| v["CollectionID"] == current["CollectionID"]).count(),
            "views_reading_it": versions
                .iter()
                .filter(|v| v["Query"]["Query"]["Name"].as_str() == Some(name))
                .filter_map(|v| v["Name"].as_str())
                .collect::<BTreeSet<_>>(),
        });
        if !users.is_empty() {
            loss["still_used_by"] = json!(users);
        }
        if !current["Policy"].is_null() {
            loss["documents_note"] = json!("This collection has a document policy: the count covers only documents you can read, while cleanup removes every document.");
        }
        Ok(Ok(json!({"VersionID": current["VersionID"], "loss": loss})))
    }

    async fn execute(
        &self,
        command: &SchemaCommand,
        refused: &mut Option<CleanupRefusal>,
    ) -> Result<Value> {
        let args = &command.args;
        let access = &self.access;
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
            ["collection", "list" | "get" | "materialize" | "empty" | "remove"] | ["version", "list" | "get" | "activate"] => &[],
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
                return Ok(json!({"version":find_version(access, target()?).await?}))
            }
            _ => {}
        }
        ensure!(
            !(command.preview && command.digest.is_some()),
            "preview returns a digest; omit options.digest while previewing"
        );
        let mut sdl_warnings = Vec::new();
        let before = match words.as_slice() {
            ["collection", "create"] | ["view", "create"] => {
                if words[0] == "view" {
                    text("query")?;
                }
                let parsed = query::sdl_parse::parse_sdl_with_warnings(text("sdl")?)
                    .context(SchemaSdlFailure)?;
                sdl_warnings = parsed
                    .warnings
                    .into_iter()
                    .map(|warning| warning.to_string())
                    .collect();
                let definitions = parsed.collections;
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
                    gents_store::config_client::preview_client_schema_install(access, text("sdl")?, false).await.context("collection create installs new schemas or verifies matching schemas; use collection update to evolve an existing schema")?;
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
            ["collection", "empty" | "remove"] => {
                match self.cleanup_target(target()?, words[1] == "remove").await? {
                    Ok(before) => before,
                    Err(refusal) => {
                        let message = refusal.to_string();
                        *refused = Some(refusal);
                        bail!(message)
                    }
                }
            }
            ["version", "activate"] => {
                let version = find_version(access, target()?).await?;
                let name = version["Name"]
                    .as_str()
                    .context("version has no collection name")?;
                ensure_application_collection(name)?;
                json!({"destination":version,"active":access.collection_version(name).await?})
            }
            ["migration", "set"] => {
                let config: defra_node::LensConfig =
                    serde_json::from_value(option("config")?.clone())?;
                config.validate_for_http()?;
                ensure!(
                    !config.lenses.is_empty(),
                    "config.Lenses must contain inline WASM modules"
                );
                let source = find_version(access, &config.source_collection_version_id).await?;
                let destination =
                    find_version(access, &config.destination_collection_version_id).await?;
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
            let mut preview = json!({"committed":false,"digest":digest,"command":args.argv,"target_id":args.target_id,"before":before,
                "effect":help::effect(&words),
                "validation":"Input shape and observed schema checked. DefraDB validates and authorizes publication when applied; preview is not a transaction.",
                "next_call":{"tool":"schema","args":apply_call(args, &digest)}});
            if !sdl_warnings.is_empty() {
                preview["warnings"] = json!(sdl_warnings);
            }
            if let Some(loss) = preview["before"]
                .as_object_mut()
                .and_then(|before| before.remove("loss"))
            {
                preview["loss"] = loss;
            }
            return Ok(preview);
        }
        if command.digest.as_deref() != Some(digest.as_str()) {
            return Err(SchemaDigestMismatch.into());
        }
        let result = match words.as_slice() {
            ["collection", "create"] => {
                let plan = gents_store::config_client::preview_client_schema_install(
                    access,
                    text("sdl")?,
                    false,
                )
                .await?;
                let installed = gents_store::config_client::apply_client_schema_install(
                    access,
                    text("sdl")?,
                    &plan.artifact_digest,
                    false,
                )
                .await?;
                let collections: Vec<String> =
                    installed.collection_contracts.keys().cloned().collect();
                let mut receipt = json!({"collections": collections});
                if let Some(write_access) = self.write_access(&collections).await {
                    receipt["write_access"] = write_access;
                }
                receipt
            }
            ["collection", "update"] => {
                let mut patch = option("patch")?
                    .as_array()
                    .context("patch must be an array")?
                    .clone();
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
            ["collection", "empty"] => {
                access.truncate_collection(target()?).await?;
                json!({"collection":target()?,"documents_removed":before["loss"]["documents"]})
            }
            ["collection", "remove"] => {
                access.remove_collections(&[target()?.to_owned()]).await?;
                json!({"collection":target()?,"removed":true})
            }
            _ => unreachable!(),
        };
        let mut response = json!({"committed":true,"result":result});
        if !sdl_warnings.is_empty() {
            response["warnings"] = json!(sdl_warnings);
        }
        Ok(response)
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

fn ensure_application_collection(name: &str) -> Result<()> {
    gents_store::graphql::validate_collection_identifier(name)?;
    ensure!(!gents_store::schema::migration::DEFAULT_REGISTRY.managed_names().any(|n| n==name), "{name} is a Gents-managed schema; its evolution belongs to the product migration registry. Use config for its documents");
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
            description: "Manage DefraDB application schemas. Commands: argv:[RESOURCE,VERB]. collection/view create names come from options.sdl; omit IDs. Commands taking an ID accept it after the verb or in target_id. Discover commands and SDL concepts with [\"help\"], then [\"help\",TOPIC,SUBTOPIC]; command inputs: [RESOURCE,VERB,\"--help\"]. Configuration and document access use their own tools.".into(),
            parameters: json!({"type":"object","required":["argv"],"additionalProperties":false,"properties":{
                "argv":{"type":"array","items":{"type":"string"},"minItems":1,"description":"Required array of command strings, not a single string. Start with a resource, not a collection name. Copy returned next_call.args for apply."},
                "target_id":{"type":"string","description":"Existing collection name or exact version ID for commands that take one. Omit for collection/view create: names come from options.sdl."},
                "options":{"type":"object","additionalProperties":true,"description":"JSON object of command inputs, not a JSON-encoded string. Put SDL text in options.sdl. Read command --help for its fields."}
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
            args.target_id = None;
            args.options.clear();
        }
        if args.argv.first().is_some_and(|s| s == "help") {
            if args.target_id.is_some() || !args.options.is_empty() {
                return Err(schema_error(
                    anyhow::anyhow!("help accepts argv only; use this call to inspect the topic"),
                    json!({"tool":"schema","args":{"argv":args.argv}}),
                ));
            }
            return help::page(&args.argv[1..])
                .map(str::to_owned)
                .map_err(|error| {
                    let mut argv = vec!["help".to_owned()];
                    argv.extend(help::recovery_path(&args.argv[1..]));
                    schema_error(error, json!({"tool":"schema","args":{"argv":argv}}))
                });
        }
        if let Some((message, next)) = argument_correction(&args) {
            return Err(schema_error(
                anyhow::anyhow!(message),
                json!({"tool":"schema","args":next}),
            ));
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
        let mut refused = None;
        match self.execute(&command, &mut refused).await {
            Ok(mut value) => {
                let encoded = if command.preview {
                    let effect = value.as_object_mut().unwrap().remove("effect").unwrap();
                    let next_call = value.as_object_mut().unwrap().remove("next_call").unwrap();
                    let warnings = value.as_object_mut().unwrap().remove("warnings");
                    let loss = value.as_object_mut().unwrap().remove("loss");
                    let mut output =
                        json!({"effect":effect,"next_call":next_call,"metadata":value});
                    if let Some(warnings) = warnings {
                        output["warnings"] = warnings;
                    }
                    if let Some(loss) = loss {
                        output["loss"] = loss;
                    }
                    serde_json::to_string(
                        &gents_session::session::output_render::Ordered::reading_order(
                            output,
                            &["effect", "loss", "warnings", "next_call", "metadata"],
                        ),
                    )
                } else {
                    serde_json::to_string(&value)
                };
                encoded.map_err(|error| SchemaError(error.to_string()))
            }
            Err(error) => {
                let recovery = if let Some(refusal) = &refused {
                    refusal.recovery(args)
                } else if error.is::<SchemaDigestMismatch>() {
                    json!({"tool":"schema","args":preview_call(args)})
                } else if let Some(mismatch) =
                    gents_store::config_client::schema_install_mismatch(error.as_ref())
                {
                    json!({"tool":"schema","args":{"argv":["collection","get"],"target_id":mismatch.collection}})
                } else if let Some(gents_protocol::operations::NextCall::VerbHelp {
                    resource,
                    verb,
                }) = gents_store::config_client::schema_install_refusal(error.as_ref())
                    .and_then(|refusal| refusal.recovery())
                {
                    json!({"tool":"schema","args":{"argv":["help",resource,verb]}})
                } else if error.is::<SchemaSdlFailure>() {
                    sdl_help_call(args, &format!("{error:#}"))
                } else {
                    help_call(args)
                };
                Err(schema_error(error, recovery))
            }
        }
    }
}

fn argument_correction(args: &SchemaParams) -> Option<(String, SchemaParams)> {
    let mut next = args.clone();
    if next.argv.get(1).is_some_and(|word| word == "preview") {
        next.argv.remove(1);
    }
    let mut reasons = Vec::new();
    let create = matches!(
        next.argv.first().map(String::as_str),
        Some("collection" | "view")
    ) && next.argv.get(1).is_some_and(|word| word == "create");
    if create && (next.target_id.is_some() || next.argv.len() == 3) {
        next.target_id = None;
        next.argv.truncate(2);
        reasons.push(
            "create takes collection names from options.sdl; omit target_id and positional names",
        );
    }
    if create && !next.options.contains_key("sdl") && next.options.contains_key("schema") {
        let sdl = next.options.remove("schema").unwrap();
        next.options.insert("sdl".into(), sdl);
        reasons.push("SDL belongs in options.sdl, not options.schema");
    }
    let read = matches!(
        next.argv
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["collection" | "version", "list" | "get"] | ["collection" | "version", "list" | "get", _]
    );
    if read
        && (args.argv.get(1).is_some_and(|word| word == "preview")
            || next.options.contains_key("digest"))
    {
        next.options.remove("digest");
        reasons.push("reads need neither preview nor digest");
    }
    if reasons.is_empty() {
        return None;
    }
    if !read {
        next = preview_call(&next);
    }
    Some((
        format!(
            "{}. Review the corrected call; it has not been executed",
            reasons.join("; ")
        ),
        next,
    ))
}

fn help_call(args: &SchemaParams) -> Value {
    let mut path = args.argv.clone();
    if path.get(1).is_some_and(|word| word == "preview") {
        path.remove(1);
    }
    path.truncate(2);
    let mut argv = vec!["help".to_owned()];
    argv.extend(help::recovery_path(&path));
    json!({"tool":"schema","args":{"argv":argv}})
}

/// The help page that explains an SDL rejection.
///
/// External premise: DefraDB's SDL parser and schema validators report what
/// they rejected only as message text (`no type found`, `relation`,
/// directive names), so the page is chosen from that text.
fn sdl_help_call(args: &SchemaParams, error: &str) -> Value {
    let error = error.to_ascii_lowercase();
    let path = if error.contains("no type found")
        || error.contains("nonnull")
        || error.contains("unknown type")
    {
        "fields types"
    } else if error.contains("relation") || error.contains("primary") {
        "relationships"
    } else {
        let sdl = args
            .options
            .get("sdl")
            .and_then(Value::as_str)
            .unwrap_or("");
        let topics = [
            ("fulltext", "indexes fulltext"),
            ("embedding", "embeddings"),
            ("vector", "indexes vector"),
            ("index", "indexes"),
            ("default", "fields defaults"),
            ("crdt", "fields crdt"),
            ("immutable", "fields immutable"),
            ("constraints", "fields constraints"),
            ("relation", "relationships"),
            ("primary", "relationships"),
            ("policy", "collections policy"),
            ("governed", "collections governed"),
            ("downsample", "collections downsample"),
            ("branchable", "collections branchable"),
        ];
        topics
            .iter()
            .find(|(term, _)| error.contains(term))
            .or_else(|| {
                topics
                    .iter()
                    .find(|(term, _)| sdl.contains(&format!("@{term}")))
            })
            .map(|(_, path)| *path)
            .unwrap_or("fields")
    };
    let mut argv = vec!["help"];
    argv.extend(path.split_whitespace());
    json!({"tool":"schema","args":{"argv":argv}})
}

fn schema_error(error: anyhow::Error, recovery: Value) -> SchemaError {
    SchemaError(json!({"error":format!("{error:#}"),"recovery":recovery}).to_string())
}
