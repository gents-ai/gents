use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::defra_query::{CollectionScope, DefraQueryParams};
use crate::llm::tool::{Tool, ToolDefinition};

pub const WRITE_TOOL_NAME: &str = "write";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriteParams {
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub options: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Observation {
    pub operation: String,
    pub granted: bool,
    pub application: bool,
    pub protected: bool,
    pub credential: bool,
    pub bounded: bool,
    pub targets: usize,
    pub limit: usize,
    pub preview: bool,
    pub digest_matches: bool,
}
impl Observation {
    pub(crate) fn admitted(&self) -> bool {
        self.granted
            && self.application
            && !self.protected
            && !self.credential
            && self.bounded
            && self.limit > 0
            && self.limit <= 100
            && self.targets <= self.limit
            && (self.operation == "create" || self.targets > 0)
    }
    pub(crate) fn may_apply(&self) -> bool {
        self.admitted() && !self.preview && self.digest_matches
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct WriteError(String);

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct WriteRecovery {
    message: String,
    recovery: Value,
}
fn recovery(message: impl Into<String>, tool: &str, args: Value) -> anyhow::Error {
    WriteRecovery {
        message: message.into(),
        recovery: json!({"tool":tool,"args":args}),
    }
    .into()
}
fn preview_call(args: &WriteParams) -> WriteParams {
    let mut next = args.clone();
    next.options.remove("digest");
    if next.argv.first().is_none_or(|v| v != "preview") {
        next.argv.insert(0, "preview".into());
    }
    next
}

#[derive(Clone)]
pub struct WriteTool {
    access: ConfigAccess,
    collections: BTreeSet<String>,
    actor: Option<identity::Did>,
}
impl WriteTool {
    pub fn new(
        access: ConfigAccess,
        collections: BTreeSet<String>,
        actor: Option<identity::Did>,
    ) -> Self {
        Self {
            access,
            collections,
            actor,
        }
    }
    pub async fn execute(&self, args: &WriteParams) -> Result<Value> {
        if args.argv.first().is_some_and(|v| v == "help")
            || args.argv.last().is_some_and(|v| v == "--help")
        {
            ensure!(
                args.collection.is_none() && args.options.is_empty(),
                "help accepts argv only"
            );
            let words: Vec<_> = args.argv.iter().map(String::as_str).collect();
            let command = match words.as_slice() {
                ["help"] => "help",
                ["help", command] | [command, "--help"] => *command,
                _ => anyhow::bail!("help accepts [help] or [help, command]"),
            };
            let help = match command {
                "help" => "Write application documents within exact configured collection grants. Commands: preview create, preview update, preview delete; apply the returned next_call after checking its effect. Call [help, create], [help, update], or [help, delete] for syntax. Configuration uses config; definitions use schema.",
                "create" => "Preview with {argv:[preview,create],collection:Shipment,options:{input:{reference:S1,status:queued}}}. Inspect effect and apply next_call. Input fields must exist in the native schema. Empty nillable lists become null; JSON scalar arrays remain JSON.",
                "update" => "Preview with {argv:[preview,update],collection:Shipment,options:{filter:{reference:{_eq:S1}},max_targets:1,input:{status:delivered}}}. A nonempty filter and max_targets 1–100 are required. Inspect targets, then apply next_call. Its digest binds input, schema and observed document versions; changed state requires a fresh preview.",
                "delete" => "Preview with {argv:[preview,delete],collection:Shipment,options:{filter:{reference:{_eq:S1}},max_targets:1}}. A nonempty filter and max_targets 1–100 are required. Inspect targets, then apply next_call. Zero matches or too many matches are refused. Changed state requires a fresh preview.",
                _ => anyhow::bail!("unknown write help command; call [help]"),
            };
            return Ok(json!({"help":help}));
        }
        let preview = args.argv.first().is_some_and(|v| v == "preview");
        let words: Vec<_> = args.argv.iter().map(String::as_str).collect();
        let operation = match words.as_slice() {
            ["preview", op @ ("create" | "update" | "delete")] if preview => *op,
            [op @ ("create" | "update" | "delete")] => *op,
            _ => anyhow::bail!("unknown write command; call write with {{\"argv\":[\"help\"]}}"),
        };
        let collection = args
            .collection
            .as_deref()
            .context("collection is required; discover fields with query argv:[fields]")?;
        crate::graphql::validate_collection_identifier(collection)?;
        let managed = crate::migration::DEFAULT_REGISTRY
            .managed_names()
            .any(|name| name == collection);
        let protected =
            crate::document_config::PROTECTED_DATASTORE_COLLECTIONS.contains(&collection);
        let credential = gents_protocol::schemas::is_credential_collection(collection);
        ensure!(
            !protected,
            "{collection} is protected and cannot be mutated"
        );
        if credential {
            return Err(recovery(
                format!("{collection} contains credentials; use its credential/config owner"),
                "config",
                json!({"argv":["help"]}),
            ));
        }
        if managed {
            return Err(recovery(
                format!("{collection} is Gents-managed; use config or its runtime owner"),
                "config",
                json!({"argv":["help"]}),
            ));
        }
        if !self.collections.contains(collection) {
            return Err(recovery(format!("collection {collection:?} is outside the exact write grant {:?}; configure datastore.write_collections",self.collections),"config",json!({"argv":["help","tools"]})));
        }
        let allowed: &[&str] = match operation {
            "create" => &["input", "digest"],
            "update" => &["input", "filter", "max_targets", "digest"],
            "delete" => &["filter", "max_targets", "digest"],
            _ => unreachable!(),
        };
        for key in args.options.keys() {
            ensure!(
                allowed.contains(&key.as_str()),
                "unknown option {key:?}; accepted options: {allowed:?}"
            );
        }
        let limit = if operation == "create" {
            1
        } else {
            args.options
                .get("max_targets")
                .and_then(Value::as_u64)
                .context("max_targets is required and must be 1–100")? as usize
        };
        ensure!(limit > 0 && limit <= 100, "max_targets must be 1–100");
        let filter = if operation == "create" {
            None
        } else {
            Some(args.options.get("filter").filter(|v|v.as_object().is_some_and(|m|!m.is_empty())).context("update/delete require a nonempty filter object; use query find to choose exact targets")?.clone())
        };
        let input = if operation == "delete" {
            None
        } else {
            Some(
                args.options
                    .get("input")
                    .and_then(Value::as_object)
                    .filter(|v| !v.is_empty())
                    .context(
                        "input must be a nonempty object; discover fields with query argv:[fields]",
                    )?
                    .clone(),
            )
        };
        ensure!(
            !preview || !args.options.contains_key("digest"),
            "preview returns a digest; omit digest while previewing"
        );
        match &self.access {
            ConfigAccess::Local(node) => {
                ConfigAccess::transact_local(node, self.actor.clone(), "application_write", |txn| {
                    Box::pin(self.in_transaction(
                        txn,
                        args,
                        operation,
                        preview,
                        collection,
                        limit,
                        filter.clone(),
                        input.clone(),
                    ))
                })
                .await
            }
            ConfigAccess::Graphql(_) => {
                self.access
                    .transact("application_write", |txn| {
                        Box::pin(self.in_transaction(
                            txn,
                            args,
                            operation,
                            preview,
                            collection,
                            limit,
                            filter.clone(),
                            input.clone(),
                        ))
                    })
                    .await
            }
        }
    }
    async fn in_transaction(
        &self,
        txn: &ConfigApplyTxn<'_>,
        args: &WriteParams,
        operation: &str,
        preview: bool,
        collection: &str,
        limit: usize,
        filter: Option<Value>,
        input: Option<Map<String, Value>>,
    ) -> Result<Value> {
        let schema = txn.collection_version(collection).await?.ok_or_else(|| {
            recovery(
                "collection not found",
                "schema",
                json!({"argv":["collection","list"]}),
            )
        })?;
        let version: ::schema::CollectionVersion = serde_json::from_value(schema.clone())?;
        let mut input = input;
        if let Some(input) = &mut input {
            for (name, value) in input.iter_mut() {
                crate::graphql::validate_graphql_name(name)?;
                ensure!(
                    !name.starts_with('_'),
                    "internal field {name:?} cannot be written"
                );
                let field = version
                    .fields
                    .iter()
                    .find(|f| &f.name == name)
                    .ok_or_else(|| {
                        recovery(
                            format!("unknown field {name:?}"),
                            "query",
                            json!({"argv":["fields"],"collection":collection}),
                        )
                    })?;
                if value.as_array().is_some_and(Vec::is_empty) && field.kind.is_array() {
                    *value = Value::Null;
                }
            }
        }
        let before = if operation == "create" {
            txn.execute(&format!("{{ COUNT({collection}: {{}}) }}"))
                .await?["data"]
                .clone()
        } else {
            if let Some(filter) = filter.as_ref() {
                crate::defra_query::validate_filter(txn, collection, filter).await?;
            }
            let params = DefraQueryParams {
                collection: collection.into(),
                fields: vec!["_docID".into()],
                filter: filter.clone(),
                limit: Some((limit + 1) as u32),
            };
            let query = crate::defra_query::build_paged_query(
                &params,
                &CollectionScope::restricted(vec![collection.into()]),
                None,
                0,
            )?
            .replace("{ _docID }", "{ _docID _version { cid height fieldName } }");
            let rows = txn.execute(&query).await?["data"][collection].clone();
            ensure!(rows.is_array(), "DefraDB returned no target rows");
            rows
        };
        let targets = if operation == "create" {
            1
        } else {
            before.as_array().unwrap().len()
        };
        let mut intent = args.clone();
        intent.options.remove("digest");
        intent.argv = vec![operation.into()];
        let digest = format!(
            "sha256:{:x}",
            Sha256::digest(serde_json::to_vec(
                &json!({"intent":intent,"schema":schema,"before":before})
            )?)
        );
        let observation = Observation {
            operation: operation.into(),
            granted: self.collections.contains(collection),
            application: true,
            protected: false,
            credential: false,
            bounded: operation == "create" || filter.is_some(),
            targets,
            limit,
            preview,
            digest_matches: args.options.get("digest").and_then(Value::as_str)
                == Some(digest.as_str()),
        };
        if !observation.admitted() {
            return Err(recovery(format!("selected {targets} targets; expected 1–{limit}. Narrow the filter before previewing"), WRITE_TOOL_NAME, serde_json::to_value(preview_call(args))?));
        }
        let mut next = intent.clone();
        next.options.insert("digest".into(), json!(digest));
        if preview {
            return Ok(
                json!({"effect":{"operation":operation,"collection":collection,"target_count":targets,"input":input,"targets":if operation=="create"{Value::Null}else{before}},"next_call":{"tool":"write","args":next},"metadata":{"committed":false,"digest":digest}}),
            );
        }
        if !observation.may_apply() {
            return Err(recovery(
                "missing or stale digest; inspect a fresh preview before applying its next_call",
                WRITE_TOOL_NAME,
                serde_json::to_value(preview_call(args))?,
            ));
        }
        let target_filter = if operation == "create" {
            String::new()
        } else {
            let ids = before
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["_docID"].clone())
                .collect::<Vec<_>>();
            let rendered =
                crate::defra_query::render::render_filter(&json!({"_docID":{"_in":ids}}))?;
            format!("filter: {rendered}")
        };
        let native_operation = if operation == "create" {
            "add"
        } else {
            operation
        };
        let (mutation, variables) = if operation == "delete" {
            (
                format!("mutation {{ delete_{collection}({target_filter}) {{ _docID }} }}"),
                json!({}),
            )
        } else {
            let comma = if target_filter.is_empty() { "" } else { ", " };
            (format!("mutation($input: {collection}MutationInputArg!) {{ {native_operation}_{collection}({target_filter}{comma}input: $input) {{ _docID }} }}"),json!({"input":input}))
        };
        let response = txn.execute_with_variables(&mutation, &variables).await?;
        let native_receipts = response["data"][format!("{native_operation}_{collection}")].clone();
        let receipts = if operation == "create" && native_receipts.is_object() {
            json!([native_receipts])
        } else {
            native_receipts
        };
        ensure!(
            receipts
                .as_array()
                .is_some_and(|rows| rows.len() == targets),
            "mutation receipt differs from observed target count; transaction rolled back: {response}"
        );
        Ok(
            json!({"effect":{"operation":operation,"collection":collection,"affected_count":targets,"documents":receipts},"committed":true}),
        )
    }
}

impl Tool for WriteTool {
    const NAME: &'static str = WRITE_TOOL_NAME;
    type Error = WriteError;
    type Args = WriteParams;
    type Output = String;
    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition{name:Self::NAME.into(),description:"Create, update or delete application documents within your exact collection grant. Call [help] for syntax. Mutations require [preview,VERB], then apply its observed-state-bound next_call. Configuration uses config; schemas use schema.".into(),parameters:json!({"type":"object","required":["argv"],"additionalProperties":false,"properties":{"argv":{"type":"array","items":{"type":"string"}},"collection":{"type":"string"},"options":{"type":"object"}}})}
    }
    async fn call(&self, args: WriteParams) -> std::result::Result<String, WriteError> {
        match self.execute(&args).await {
            Ok(value) => serde_json::to_string(&crate::self_config::Ordered::reading_order(
                value,
                &["effect", "next_call", "metadata", "committed"],
            ))
            .map_err(|e| WriteError(e.to_string())),
            Err(error) => {
                let next = error
                    .downcast_ref::<WriteRecovery>()
                    .map(|v| v.recovery.clone())
                    .unwrap_or_else(|| json!({"tool":"write","args":{"argv":["help"]}}));
                Err(WriteError(
                    json!({"error":format!("{error:#}"),"recovery":next}).to_string(),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests;
