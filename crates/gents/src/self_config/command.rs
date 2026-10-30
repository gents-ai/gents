use super::*;

pub(super) mod agents;
mod args;
mod authoring;
mod automation;
mod cleanup;
pub(super) mod contracts;
mod crud;
mod datastore;
mod discovery;
mod grants;
mod guidance;
pub(super) mod help;
mod inference;
mod inventory;
mod observe;
mod pack;
mod patch;
mod reads;
mod skill;
mod validate;

use agents::{agent_params, default_agent_params};
use args::{
    extract_agent_target, extract_allow_drop, parse_limit, parse_patch, parse_patch_args,
    ParsedArgs,
};
use cleanup::cleanup_target;
use contracts::help_patch_contracts;
use grants::model_resources;
use observe::{append_receipt, log_config_call, observed_call};

const HELP_INDEX: &[(&str, &str)] = &[
    (
        "agent",
        "inspect yourself, create a role, or select its configuration",
    ),
    ("context", "prompt, skills, Tools and compaction"),
    ("tools", "host, delegation and other tool grants"),
    ("datastore", "collection tool definitions"),
    ("agent-target", "connect a helper, then grant it in Tools"),
    ("skill", "reusable instructions"),
    (
        "profile",
        "separate inference settings; select them on an agent",
    ),
    ("sampling", "temperature and sampling"),
    ("execution", "run limits"),
    ("retry-policy", "retry settings"),
    ("compaction", "context compaction"),
    ("backend", "inference endpoint"),
    ("mcp-service", "MCP registration"),
    ("task", "work performed by an agent"),
    ("trigger", "route an event to a task"),
    ("schedule", "timer"),
    ("event-source", "collection change source"),
    (
        "automation",
        "connect sources, tasks and triggers; wiring recipes",
    ),
    ("pack", "pack installation"),
    (
        "plugin",
        "installed plugin inspection and explicit Tools selection",
    ),
    ("discovery", "external configuration scan"),
    ("cleanup", "atomic multi-document deletion"),
    ("validate", "check saved configuration and references"),
    ("batch", "ordered config calls"),
];

const HELP_GRAMMAR: &str = "Documents: list, get, create, update, delete. target_id names the document; set writes fields, clear removes optional fields. list takes options.limit/cursor. create requires a new ID; update requires an existing ID. agent create takes options instead of set and allocates its ID and Context/Tools.
Preview: [RESOURCE,preview,VERB]. Delete needs options.digest from preview. batch takes options.operations: ordered config calls; each commits separately, stopping on error. Packs use installation workflows. Collections and migrations use the separate schema tool.";

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct CommandGuidance {
    message: String,
    next_call: Value,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigCommandParams {
    pub argv: Vec<String>,
    #[serde(default)]
    pub target_id: Option<String>,
    #[serde(default)]
    pub set: BTreeMap<String, Value>,
    #[serde(default)]
    pub clear: Vec<String>,
    #[serde(default)]
    pub options: BTreeMap<String, Value>,
}

impl ConfigCommandParams {
    fn into_argv(self) -> Result<Vec<String>> {
        anyhow::ensure!(!self.argv.is_empty(), "argv requires a config command");
        if config_help_resource(&self.argv).is_some() {
            anyhow::ensure!(
                self.target_id.is_none()
                    && self.set.is_empty()
                    && self.clear.is_empty()
                    && self.options.is_empty(),
                "help accepts a command path only; remove target_id, set, clear and options"
            );
        }
        let mut argv = self.argv;
        normalize_verbs(&mut argv);
        if let Some(id) = self.target_id {
            required_resource_id(Some(&id), "target_id")?;
            let words = argv.iter().map(String::as_str).collect::<Vec<_>>();
            let position = match words.as_slice() {
                [resource, "preview", "create" | "update" | "delete", ..]
                    if crud::resource_target(resource).is_some() && !(*resource == "agent" && words[2] == "create") => 3,
                [resource, "get" | "create" | "update" | "delete", ..]
                    if crud::resource_target(resource).is_some() && !(*resource == "agent" && words[1] == "create") => 2,
                ["datastore" | "agent-target" | "execution", "preview", "create" | "edit", ..]
                | ["agent", "preview", "edit" | "default", ..]
                | ["backend" | "profile", "preview", "create", ..]
                | ["pack", "preview", "install" | "update", ..] => 3,
                ["datastore" | "agent-target" | "execution", "get" | "create" | "edit", ..]
                | ["mcp-service", "get" | "preview" | "edit", ..]
                | ["agent", "get" | "edit" | "default", ..]
                | ["backend", "get" | "create" | "discover", ..]
                | ["profile", "create", ..]
                | ["skill", "get", ..]
                | ["pack", "get" | "install" | "update" | "remove", ..]
                | ["plugin", "get", ..] => 2,
                ["automation", "get" | "preview" | "edit", _, ..] => 3,
                ["get", ..] => return Err(CommandGuidance {
                    message: "Put the resource before the verb: use agent get to inspect an agent.".into(),
                    next_call: json!({"argv":["agent","get"],"target_id":id}),
                }.into()),
                ["agent", "create" | "clone", ..] | ["agent", "preview", "create" | "clone", ..] => bail!(
                    "agent create/clone allocates the ID from options.display-name; omit target_id. The receipt returns the new agent, Context and Tools IDs; use resource update for later changes"
                ),
                ["tools", ..] => bail!(
                    "target_id is not accepted by tools; omit it and select the owning agent with options.agent, e.g. {{\"argv\":[\"tools\",\"get\"],\"options\":{{\"agent\":\"AGENT_ID\"}}}}"
                ),
                _ => bail!(
                    "target_id is not accepted by {:?}; see the command's --help for its parameters",
                    words.join(" ")
                ),
            };
            match argv.get(position).filter(|arg| !arg.starts_with('-')) {
                None => argv.insert(position, id),
                Some(given) if *given == id => {}
                Some(given) => bail!(
                    "target ID {given:?} in argv conflicts with target_id {id:?}; send one of them"
                ),
            }
        }
        let verb = argv.iter().skip(1).find(|word| word.as_str() != "preview");
        let composition = argv.first().is_some_and(|r| r == "agent")
            && verb.is_some_and(|v| matches!(v.as_str(), "create" | "clone"));
        if !composition && matches!(verb.map(String::as_str), Some("create" | "update" | "edit")) {
            if let Some(target) = argv.first().and_then(|r| crud::resource_target(r)) {
                let misplaced: Vec<_> = self
                    .options
                    .keys()
                    .filter(|name| {
                        let backend_option = target == SelfConfigTarget::InferenceBackend
                            && verb.is_some_and(|v| v == "create")
                            && matches!(name.as_str(), "endpoint" | "name");
                        !backend_option && target.is_writable(name)
                    })
                    .collect();
                anyhow::ensure!(misplaced.is_empty(),
                    "{} fields belong in set, not options: {}. Move all listed fields together; preserve values and keep command selectors such as agent in options",
                    target.collection_name(), misplaced.iter().map(|name| format!("set.{name}")).collect::<Vec<_>>().join(", "));
            }
        }
        for (name, value) in self.options {
            anyhow::ensure!(
                name.bytes().next().is_some_and(|c| c.is_ascii_lowercase())
                    && name.bytes().all(|c| c.is_ascii_lowercase() || c == b'-')
                    && !matches!(name.as_str(), "set" | "clear"),
                "invalid options key {name:?}; argv, target_id, set and clear belong at the top level beside options. Option names use lowercase letters and hyphens, without --"
            );
            let flag = format!("--{name}");
            anyhow::ensure!(
                !argv.contains(&flag),
                "option {flag} supplied in both argv and options"
            );
            let values = match value {
                Value::String(value) => vec![value],
                Value::Array(items) if !items.is_empty() && items.iter().all(Value::is_string) => {
                    items
                        .into_iter()
                        .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                        .collect()
                }
                value => vec![serde_json::to_string(&value)?],
            };
            for value in values {
                argv.extend([flag.clone(), value]);
            }
        }
        for (field, value) in self.set {
            anyhow::ensure!(
                !field.is_empty()
                    && field
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'_'),
                "invalid patch field {field:?}; set/clear field names use underscores, not hyphens (e.g. system_prompt). Read resource update --help for writable fields"
            );
            argv.extend([
                "--set".into(),
                format!("{field}={}", serde_json::to_string(&value)?),
            ]);
        }
        for field in self.clear {
            argv.extend(["--clear".into(), field]);
        }
        Ok(argv)
    }
}

fn config_help_resource(argv: &[String]) -> Option<Option<&str>> {
    let first = argv.first()?.as_str();
    if matches!(first, "help" | "--help" | "-h") {
        return Some(argv.get(1).map(String::as_str));
    }
    let mut index = 1;
    while let Some(arg) = argv.get(index) {
        if matches!(arg.as_str(), "--help" | "-h") || (index == 1 && arg == "help") {
            return Some(Some(first));
        }
        index += if arg.starts_with("--") && arg != "--default" {
            2
        } else {
            1
        };
    }
    None
}

pub(crate) fn is_help_call(args: &Value) -> bool {
    serde_json::from_value::<ConfigCommandParams>(args.clone())
        .ok()
        .and_then(|params| params.into_argv().ok())
        .is_some_and(|argv| config_help_resource(&argv).is_some())
}

fn help_verb(argv: &[String]) -> Vec<&str> {
    if argv
        .first()
        .is_none_or(|first| matches!(first.as_str(), "help" | "--help" | "-h"))
    {
        return Vec::new();
    }
    let mut words = argv[1..]
        .iter()
        .map(String::as_str)
        .take_while(|word| !matches!(*word, "--help" | "-h"))
        .filter(|word| !word.starts_with('-') && *word != "help")
        .take(3)
        .collect::<Vec<_>>();
    if words.len() > 1 && words[0] == "preview" {
        words.remove(0);
    }
    words
}

fn required_resource_id<'a>(value: Option<&'a String>, label: &str) -> Result<&'a String> {
    value
        .filter(|id| !id.trim().is_empty() && !id.starts_with('-'))
        .with_context(|| format!("missing {label}: supply target_id, or a non-empty resource ID in argv before options or patch fields; see [\"help\"]"))
}

#[derive(Clone)]
pub struct ConfigCommandTool {
    pub(super) usage_cache: Arc<crate::usage_observation::UsageCache>,
    pub(super) node: Arc<EmbeddedNode>,
    pub(super) node_did: String,
    pub(super) identity: Option<Arc<dyn NodeIdentity>>,
    pub(super) core: SelfConfigCore,
    pub(super) categories: BTreeSet<String>,
    pub(super) no_lockout: bool,
    pub(super) preview: bool,
    pub(super) process_ceiling: crate::tool_surface::SelfConfigProcessCeiling,
    pub(super) execution: Arc<super::execution::ExecutionObservation>,
    pub(super) plugins: Arc<crate::plugin::executor::PluginExecutor>,
}

impl Tool for ConfigCommandTool {
    const NAME: &'static str = CONFIG_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = ConfigCommandParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: r#"Read and change Gents configuration through a native API, not a shell. Collection definitions and migrations use the separate schema tool.
Call {"argv":[RESOURCE,VERB],"target_id"?:ID,"set"?:{field:value},"clear"?:[field],"options"?:{name:value}}. Documents share list, get, create, update, delete. Writes validate and commit immediately. ["validate"] audits saved configuration; fix reported errors before reporting completion. Preview writes nothing.
Agent IDs are "<DID>:<slug>"; the slug alone works. ["help"] lists granted resources; ["help",RESOURCE] explains fields and exceptions."#.to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "argv": {
                        "type": "array",
                        "items": {"type": "string"},
                        "minItems": 1,
                        "description": "Command words, never passed to a shell."
                    },
                    "target_id": {"type":"string", "description":"Exact document ID; alternatively put it after the verb in argv. agent create allocates its ID."},
                    "set": {"type":"object", "additionalProperties":true, "description":"Fields to write, as native JSON. Each replaces the whole field."},
                    "clear": {"type":"array", "items":{"type":"string"}, "description":"Optional fields to remove."},
                    "options": {"type":"object", "additionalProperties":true, "description":"Named options without --. Object options stay JSON objects."}
                },
                "required": ["argv"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let call = Self {
            execution: Arc::new(super::execution::ExecutionObservation::default()),
            ..self.clone()
        };
        let mut previewing = false;
        let mut deleting = false;
        let mut help = false;
        let observed = observed_call(&args);
        let words = args.argv.clone();
        let result = async {
            let argv = args.into_argv()?;
            previewing = argv.iter().any(|word| word == "preview");
            deleting = argv.first().is_some_and(|word| word == "cleanup")
                || argv.iter().take(3).any(|word| word == "delete");
            help = config_help_resource(&argv).is_some();
            call.dispatch(&argv).await
        }
        .await;
        let receipt = call.execution.receipt();
        log_config_call(&words, &observed, help, previewing, &result);
        match result {
            Ok(text) if help => Ok(text),
            Ok(text) => append_receipt(&text, &receipt).map_err(Into::into),
            Err(error) => {
                let (message, recovery) = call.failure_guidance(&error, &words, deleting);
                let failure = ordered! {
                    "error": message,
                    "batch": error.downcast_ref::<crud::BatchFailure>().map(|failure| json!({"atomic":false,"results":failure.results,"failed_index":failure.failed_index,"unattempted":failure.unattempted})),
                    "recovery": recovery,
                    "config_execution": receipt,
                };
                Err(
                    anyhow::anyhow!(serde_json::to_string(&failure).map_err(anyhow::Error::from)?)
                        .into(),
                )
            }
        }
    }
}

impl ConfigCommandTool {
    async fn dispatch(&self, argv: &[String]) -> Result<String> {
        if let Some(resource) = config_help_resource(argv) {
            return self.help(resource, help_verb(argv));
        }
        let Some(command) = argv.first().map(String::as_str) else {
            bail!("missing config command; see [\"help\"]");
        };
        if command == "schema" {
            bail!("Schema management uses the separate schema tool. Call schema with argv:[\"help\"]. Enable it through Tools.built_ins.enable_schema_tool; config grants do not imply schema access");
        }
        if argv.get(1).is_some_and(|verb| verb == "apply") {
            if let Some(refusal) = apply_refusal(command) {
                bail!(refusal);
            }
        }
        if crud::resource_target(command).is_some() {
            let preview = argv.get(1).is_some_and(|v| v == "preview");
            let preview_operand = argv.get(2).map(String::as_str);
            let bound_preview = preview
                && (preview_operand.is_none_or(|v| v.starts_with('-'))
                    || command == "profile"
                        && matches!(
                            preview_operand,
                            Some(
                                "profile"
                                    | "sampling"
                                    | "execution"
                                    | "retry-policy"
                                    | "compaction"
                            )
                        )
                    || command == "mcp-service");
            if let Some(verb) = argv.get(if preview && !bound_preview { 2 } else { 1 }) {
                let common = [
                    "list", "get", "create", "update", "delete", "edit", "preview",
                ];
                let extra = match command {
                    "agent" => ["clone", "disable", "default", "context"].contains(&verb.as_str()),
                    "backend" => ["discover", "accounts"].contains(&verb.as_str()),
                    "skill" => verb == "import",
                    _ => false,
                };
                if !common.contains(&verb.as_str()) && !extra {
                    if command == "datastore" && verb == "validate" {
                        return Err(CommandGuidance {
                            message: "validate audits saved configuration, including datastore selections; it is a top-level command.".into(),
                            next_call: json!({"argv":["validate"]}),
                        }.into());
                    }
                    return Err(CommandGuidance {
                        message: format!("unknown {command} verb {verb:?}; configuration documents use list, get, create, update and delete. Runtime history belongs to sessions."),
                        next_call: json!({"argv":[command,"--help"]}),
                    }.into());
                }
            }
        }
        if let Some(result) = self.crud(argv).await? {
            return Ok(result);
        }
        match command {
            "batch" => self.batch(argv).await,
            "help" => self.help(argv.get(1).map(String::as_str), Vec::new()),
            "get" => {
                let (agent_id, rest) = extract_agent_target(&argv[1..])?;
                if !rest.is_empty() {
                    return Err(CommandGuidance {
                        message: "Put the resource before the verb: use agent get to inspect an agent.".into(),
                        next_call: json!({"argv":["agent","get"],"target_id":rest[0]}),
                    }.into());
                }
                let core = self.target_core(agent_id.as_deref(), "get").await?;
                let value = core
                    .read_effective_config(&self.categories, self.no_lockout, self.preview)
                    .await?;
                Ordered::reading_order(value, EFFECTIVE_ORDER).pretty()
            }
            "agent" => self.agent(&argv[1..]).await,
            "tools" => self.bound_document("tools", &argv[1..]).await,
            "datastore" => self.datastore(&argv[1..]).await,
            "agent-target" | "execution" => bail!("use {command} list|get|create|update|delete; see [\"help\",\"{command}\"]"),
            "profile" => self.profile(&argv[1..]).await,
            "backend" => self.backend(&argv[1..]).await,
            "mcp-service" => self.mcp_service(&argv[1..]).await,
            "automation" => self.automation(&argv[1..]).await,
            "cleanup" => self.cleanup(&argv[1..]).await,
            "pack" => self.pack(&argv[1..]).await,
            "plugin" => self.plugin(&argv[1..]),
            "skill" => self.skill(&argv[1..]).await,
            "discovery" => self.discovery(&argv[1..]).await,
            "validate" => self.validate_saved(&argv[1..]).await,
            other => bail!(
                "unknown config resource or command {other:?}; put the resource first, e.g. [\"profile\",\"list\"]. Accepted: help, get, {}. See [\"help\"]",
                model_resources(&self.categories, self.core.held_grants().pack_install).join(", ")
            ),
        }
    }
}

fn normalize_verbs(argv: &mut Vec<String>) {
    let words = argv.iter().map(String::as_str).collect::<Vec<_>>();
    let drop = match words.as_slice() {
        ["automation", "preview", "edit" | "create", ..]
        | ["tools" | "backend" | "profile", "preview", "edit", ..] => Some(2),
        ["agent", "context", "preview", "edit", ..] => Some(3),
        _ => None,
    };
    if let Some(index) = drop {
        argv.remove(index);
    } else if words.starts_with(&["automation", "create"]) {
        argv[1] = "edit".to_owned();
    }
}

fn apply_refusal(resource: &str) -> Option<String> {
    let form = match resource {
        "pack" => "[\"pack\",\"install\",PACKAGE] with options.digest from [\"pack\",\"preview\",\"install\",PACKAGE]".to_owned(),
        "cleanup" => "[\"cleanup\",\"remove\"] with options.target and options.digest from [\"cleanup\",\"preview\"]".to_owned(),
        "skill" => "[\"skill\",\"import\",SKILL_ID,PATH]".to_owned(),
        _ => "the previewed call with its target_id, options and set, removing \"preview\" from argv and putting \"edit\" in its place when no create or edit follows it".to_owned(),
    };
    Some(format!("{resource} has no apply verb; use {form}"))
}
