use super::*;

mod crud;
mod datastore;
mod discovery;
pub(super) mod help;
mod skill;
mod validate;

/// Resource index returned by `["help"]`: one line per resource, filtered to
/// the granted ones. Fields, syntax and recipes live in `["help", RESOURCE]`.
const HELP_INDEX: &[(&str, &str)] = &[
    (
        "behavior",
        "inspect yourself, create a role, or select its configuration",
    ),
    ("context", "prompt, skills, Tools and compaction"),
    ("tools", "host, delegation and other tool grants"),
    ("datastore", "collection tool definitions"),
    (
        "subagent-target",
        "connect a helper, then grant it in Tools",
    ),
    ("skill", "reusable instructions"),
    (
        "profile",
        "separate inference settings; select them on a behavior",
    ),
    ("sampling", "temperature and sampling"),
    ("execution", "run limits"),
    ("retry-policy", "retry settings"),
    ("compaction", "context compaction"),
    ("backend", "inference endpoint"),
    ("mcp-service", "MCP registration"),
    ("task", "work performed by a behavior"),
    ("trigger", "route an event to a task"),
    ("schedule", "timer"),
    ("event-source", "collection change source"),
    (
        "automation",
        "connect sources, tasks and triggers; wiring recipes",
    ),
    ("pack", "pack and graph installation"),
    ("discovery", "external configuration scan"),
    ("cleanup", "atomic multi-document deletion"),
    ("validate", "check saved configuration and references"),
    ("batch", "ordered config calls"),
];

/// Where `preview` goes, and the help aliases, stated once in the index.
const HELP_GRAMMAR: &str = "Documents: list, get, create, update, delete. target_id names the document; set writes fields, clear removes optional fields. list takes options.limit/cursor. create requires a new ID; update requires an existing ID. behavior create takes options instead of set and allocates its ID and Context/Tools.
Preview: [RESOURCE,preview,VERB]. Delete needs options.digest from preview. batch takes options.operations: ordered config calls; each commits separately, stopping on error. Packs use installation workflows. Collections and migrations use the separate schema tool; graph authoring is unavailable.";

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
    #[cfg(test)]
    pub(crate) fn into_argv_for_test(self) -> Result<Vec<String>> {
        self.into_argv()
    }

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
                    if crud::resource_target(resource).is_some() && !(*resource == "behavior" && words[2] == "create") => 3,
                [resource, "get" | "create" | "update" | "delete", ..]
                    if crud::resource_target(resource).is_some() && !(*resource == "behavior" && words[1] == "create") => 2,
                ["datastore" | "subagent-target" | "execution", "preview", "create" | "edit", ..]
                | ["behavior", "preview", "edit" | "default", ..]
                | ["backend" | "profile", "preview", "create", ..]
                | ["pack", "preview", "install" | "update", ..] => 3,
                ["datastore" | "subagent-target" | "execution", "get" | "create" | "edit", ..]
                | ["mcp-service", "get" | "preview" | "edit", ..]
                | ["behavior", "get" | "edit" | "default", ..]
                | ["backend", "get" | "create" | "discover", ..]
                | ["profile", "create", ..]
                | ["skill", "get", ..]
                | ["pack", "get" | "install" | "update" | "remove", ..] => 2,
                ["automation", "get" | "preview" | "edit", _, ..] => 3,
                ["get", ..] => return Err(CommandGuidance {
                    message: "Put the resource before the verb: use behavior get to inspect a behavior.".into(),
                    next_call: json!({"argv":["behavior","get"],"target_id":id}),
                }.into()),
                ["behavior", "create" | "clone", ..] | ["behavior", "preview", "create" | "clone", ..] => bail!(
                    "behavior create/clone allocates the ID from options.display-name; omit target_id. The receipt returns the new behavior, Context and Tools IDs; use resource update for later changes"
                ),
                ["tools", ..] => bail!(
                    "target_id is not accepted by tools; omit it and select the owning behavior with options.behavior, e.g. {{\"argv\":[\"tools\",\"get\"],\"options\":{{\"behavior\":\"BEHAVIOR_ID\"}}}}"
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
        for (name, value) in self.options {
            let verb = argv.iter().skip(1).find(|word| word.as_str() != "preview");
            let composition = argv.first().is_some_and(|r| r == "behavior")
                && verb.is_some_and(|v| matches!(v.as_str(), "create" | "clone"));
            if !composition
                && matches!(verb.map(String::as_str), Some("create" | "update" | "edit"))
            {
                if let Some(target) = argv.first().and_then(|r| crud::resource_target(r)) {
                    let backend_option = target == SelfConfigTarget::InferenceBackend
                        && verb.is_some_and(|v| v == "create")
                        && matches!(name.as_str(), "endpoint" | "name");
                    anyhow::ensure!(backend_option || !target.is_writable(&name),
                        "{name:?} is a {} field: put it in set.{name}, not options; field names keep their underscores", target.collection_name());
                }
            }
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
            // A list of strings is the repeated flag (cleanup targets, pack
            // slots); any other structured value stays one JSON value.
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

/// Recognize help before parsing command operands, never inside option values.
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

/// Help bypasses dispatch and returns plain text, so it has no execution receipt.
pub(crate) fn is_help_call(args: &Value) -> bool {
    serde_json::from_value::<ConfigCommandParams>(args.clone())
        .ok()
        .and_then(|params| params.into_argv().ok())
        .is_some_and(|argv| config_help_resource(&argv).is_some())
}

/// The command words a `RESOURCE ... --help` path names, so help can narrow
/// to that command: operands after the resource, without `preview` or flags.
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
    // `preview create` narrows to create; a bare `preview` is the verb.
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

/// Native `config` tool. Its model-facing text is layered for progressive
/// disclosure (#2088): the description is a small core (call shape, ID and
/// write rules, how to get more); `["help"]` lists resources; `["help",
/// RESOURCE]` gives that resource's commands, fields and one recipe; errors
/// name the next call. Every layer is paid for in tokens on each turn or help
/// call, so nothing is repeated across layers or responses, and concepts that
/// explain how documents fit together belong to the Engineer prompt, not here.
#[derive(Clone)]
pub struct ConfigCommandTool {
    pub(super) node: Arc<EmbeddedNode>,
    pub(super) agent_did: String,
    pub(super) identity: Option<Arc<dyn AgentIdentity>>,
    pub(super) core: SelfConfigCore,
    pub(super) categories: BTreeSet<String>,
    pub(super) no_lockout: bool,
    pub(super) preview: bool,
    pub(super) allow_pack_install: bool,
    pub(super) process_ceiling: crate::tool_surface::SelfConfigProcessCeiling,
    pub(super) execution: Arc<super::execution::ExecutionObservation>,
    /// Whose home holds the pack store and the plugin store this tool installs into.
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
Behavior IDs are "<DID>:<slug>"; the slug alone works. ["help"] lists granted resources; ["help",RESOURCE] explains fields and exceptions."#.to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "argv": {
                        "type": "array",
                        "items": {"type": "string"},
                        "minItems": 1,
                        "description": "Command words, never passed to a shell."
                    },
                    "target_id": {"type":"string", "description":"Exact document ID; alternatively put it after the verb in argv. behavior create allocates its ID."},
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
            // Help is the page itself: plain text with no envelope. It is
            // answered before any command parses, so it never mutates.
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

/// The model's call as compact JSON for the call log, cut at about 2 KB.
/// The config tool accepts no credential fields, so none can appear here.
fn observed_call(args: &ConfigCommandParams) -> String {
    let call = json!({
        "argv": args.argv,
        "target_id": args.target_id,
        "set": args.set,
        "clear": args.clear,
        "options": args.options,
    });
    truncated(&call.to_string(), 2048)
}

fn truncated(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

/// One `tool call` event per config call, and a `self-config write` event
/// for each call that asks to change configuration, so a trial's
/// exploration path and write counts can be followed from the log while its
/// node is held.
fn log_config_call(
    argv: &[String],
    call: &str,
    help: bool,
    previewing: bool,
    result: &Result<String>,
) {
    let outcome = if result.is_ok() { "ok" } else { "error" };
    let error = result
        .as_ref()
        .err()
        .map(|error| truncated(&format!("{error:#}"), 500));
    tracing::info!(
        target: "gents::self_config",
        call = %call,
        outcome,
        error = error.as_deref().unwrap_or(""),
        "tool call"
    );
    let resource = argv.first().map(String::as_str).unwrap_or("");
    let verb = argv.get(1).map(String::as_str).unwrap_or("");
    let reads = [
        "get", "list", "discover", "context", "help", "--help", "-h", "scan",
    ];
    if help
        || previewing
        || resource == "help"
        || resource == "get"
        || resource == "batch"
        || resource == "validate"
        || reads.contains(&verb)
    {
        return;
    }
    let collection = match (resource, argv.get(2).map(String::as_str)) {
        ("automation", Some(kind)) => kind,
        (resource, _) => resource,
    };
    let verb = match verb {
        "remove" | "delete" => "delete",
        "apply" => "plan",
        "create" | "clone" | "install" | "import" => "create",
        _ => "edit",
    };
    let error_kind = error.as_deref().map(|error| {
        error
            .split(|c: char| c == ':' || c == ';')
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(80)
            .collect::<String>()
    });
    tracing::info!(
        target: "gents::self_config",
        collection,
        verb,
        outcome = if result.is_ok() { "committed" } else { "refused" },
        error_kind = error_kind.as_deref().unwrap_or(""),
        "self-config write"
    );
}

/// A preview that references a document which does not exist yet names the
/// call that can check it, or the order that makes the reference valid.
fn missing_reference_next_step(
    missing: &crate::document_config::MissingReference,
    deleting: bool,
) -> String {
    let target = missing.target.graphql_type();
    let id = &missing.target_id;
    if deleting {
        return format!("Deletion would leave a broken reference to {target} {id:?}. Update the referring document to remove or redirect the reference, then retry deletion.");
    }
    if missing.target == crate::Collection::Skill {
        return format!("Skill {id:?} does not exist yet; create it with skill create or import it with skill import, then attach its ID with context update set.skill_ids.");
    }
    format!("{target} {id:?} does not exist yet; create it with its own resource command first, or reference an existing ID.")
}

/// Every non-help result ends with the execution receipt, after the answer
/// and its next step, without re-serializing (and so re-sorting) the answer.
fn append_receipt(
    text: &str,
    receipt: &super::execution::ConfigExecutionReceipt,
) -> Result<String> {
    let body = text
        .trim_end()
        .strip_suffix('}')
        .filter(|_| text.trim_start().starts_with('{'))
        .context("config output must be an object")?
        .trim_end();
    let separator = if body.ends_with('{') { "" } else { "," };
    Ok(format!(
        "{body}{separator}\n  \"config_execution\": {}\n}}",
        serde_json::to_string(receipt)?
    ))
}

fn model_resources(categories: &BTreeSet<String>, pack: bool) -> Vec<&'static str> {
    let mut resources = Vec::new();
    if categories.contains("persona") {
        resources.push("behavior");
    } else if categories.contains("behavior") {
        resources.push("behavior (current only)");
    }
    for (category, resource) in [
        ("persona", "context"),
        ("tools", "tools"),
        ("tools", "datastore"),
        ("tools", "skill"),
        ("tools", "subagent-target"),
        ("profile", "profile"),
        ("profile", "execution"),
        ("profile", "sampling"),
        ("profile", "retry-policy"),
        ("profile", "compaction"),
        ("backend", "backend"),
        ("mcp_service", "mcp-service"),
        ("automation", "automation"),
        ("automation", "task"),
        ("automation", "trigger"),
        ("automation", "schedule"),
        ("automation", "event-source"),
    ] {
        if categories.contains(category) {
            resources.push(resource);
        }
    }
    if pack {
        resources.push("pack");
    }
    if !categories.is_empty() {
        resources.push("cleanup");
        resources.push("batch");
    }
    if categories.contains("tools") {
        resources.push("discovery");
    }
    if categories.contains("persona") {
        resources.push("validate");
    }
    resources
}

impl ConfigCommandTool {
    fn failure_guidance(
        &self,
        error: &anyhow::Error,
        argv: &[String],
        deleting: bool,
    ) -> (String, Value) {
        let missing = error.downcast_ref::<crate::document_config::MissingReference>();
        let message = missing.map_or_else(
            || format!("{error:#}"),
            |missing| {
                format!(
                    "{} Cause: {error:#}",
                    missing_reference_next_step(missing, deleting)
                )
            },
        );
        if let Some(hint) = error.downcast_ref::<CommandGuidance>() {
            return (message, json!({"next_call":hint.next_call}));
        }
        if let Some(missing) = error.downcast_ref::<super::ops::MissingConfigDocument>() {
            if let Some((resource, _)) = HELP_INDEX
                .iter()
                .find(|(r, _)| crud::resource_target(r) == Some(missing.target))
            {
                return (message, json!({"next_call":{"argv":[resource,"list"]}}));
            }
        }
        let resources = model_resources(&self.categories, self.allow_pack_install);
        if error
            .downcast_ref::<super::ops::MissingBehavior>()
            .is_some()
            && self.categories.contains("persona")
        {
            return (message, json!({"next_call":{"argv":["behavior","list"]}}));
        }
        if let Some(missing) = missing {
            let collection = if deleting {
                missing.collection
            } else {
                missing.target
            };
            if let Some(resource) = resources.iter().find(|resource| {
                crud::resource_target(resource)
                    .is_some_and(|target| target.collection() == collection)
            }) {
                let next = if deleting {
                    json!({"argv":[resource,"get"],"target_id":missing.id})
                } else {
                    json!({"argv":[resource,"list"]})
                };
                return (message, json!({"next_call":next}));
            }
        }
        if argv.first().is_some_and(|word| word == "schema") {
            return (
                message,
                json!({"tool":"schema","next_call":{"argv":["help"]}}),
            );
        }
        let resource = argv.first().map(String::as_str).unwrap_or("help");
        let next = if resources
            .iter()
            .any(|name| name.split(' ').next() == Some(resource))
        {
            let verb = argv.iter().skip(1).find(|word| word.as_str() != "preview");
            match verb.map(String::as_str) {
                Some(verb @ ("create" | "update" | "edit"))
                    if crud::resource_target(resource).is_some() =>
                {
                    json!({"argv":[resource,verb,"--help"]})
                }
                _ => json!({"argv":["help",resource]}),
            }
        } else {
            json!({"argv":["help"]})
        };
        (message, json!({"next_call":next}))
    }

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
                    "behavior" => {
                        ["clone", "disable", "default", "context"].contains(&verb.as_str())
                    }
                    "backend" => verb == "discover",
                    "skill" => verb == "import",
                    _ => false,
                };
                if !common.contains(&verb.as_str()) && !extra {
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
                let (behavior_id, rest) = extract_behavior_target(&argv[1..])?;
                if !rest.is_empty() {
                    return Err(CommandGuidance {
                        message: "Put the resource before the verb: use behavior get to inspect a behavior.".into(),
                        next_call: json!({"argv":["behavior","get"],"target_id":rest[0]}),
                    }.into());
                }
                let core = self.target_core(behavior_id.as_deref(), "get").await?;
                let value = core
                    .read_effective_config(&self.categories, self.no_lockout, self.preview)
                    .await?;
                Ordered::reading_order(value, EFFECTIVE_ORDER).pretty()
            }
            "behavior" => self.behavior(&argv[1..]).await,
            "tools" => self.bound_document("tools", &argv[1..]).await,
            "datastore" => self.datastore(&argv[1..]).await,
            "subagent-target" | "execution" => bail!("use {command} list|get|create|update|delete; see [\"help\",\"{command}\"]"),
            "profile" => self.profile(&argv[1..]).await,
            "backend" => self.backend(&argv[1..]).await,
            "mcp-service" => self.mcp_service(&argv[1..]).await,
            "automation" => self.automation(&argv[1..]).await,
            "cleanup" => self.cleanup(&argv[1..]).await,
            "pack" => self.pack(&argv[1..]).await,
            "skill" => self.skill(&argv[1..]).await,
            "discovery" => self.discovery(&argv[1..]).await,
            "validate" => self.validate_saved(&argv[1..]).await,
            other => bail!(
                "unknown config resource or command {other:?}; put the resource first, e.g. [\"profile\",\"list\"]. Accepted: help, get, {}. See [\"help\"]",
                model_resources(&self.categories, self.allow_pack_install).join(", ")
            ),
        }
    }

    async fn behavior(&self, argv: &[String]) -> Result<String> {
        anyhow::ensure!(
            self.categories.contains("persona") || self.categories.contains("behavior"),
            "behavior configuration is not granted; enabled resources: {}",
            model_resources(&self.categories, self.allow_pack_install).join(", ")
        );
        let Some(verb) = argv.first().map(String::as_str) else {
            bail!("behavior command is required; see [\"help\",\"behavior\"]");
        };
        match verb {
            "list" => {
                self.ensure_behavior_catalog("list", None)?;
                let flags = ParsedArgs::parse(&argv[1..])?;
                flags.reject_mutation_flags()?;
                let limit = flags
                    .one("limit")?
                    .map(str::parse::<usize>)
                    .transpose()
                    .context("--limit must be a positive integer")?
                    .unwrap_or(20);
                persona_list(
                    &self.node,
                    &self.agent_did,
                    &self.process_ceiling,
                    limit,
                    flags.one("cursor")?,
                )
                .await
            }
            "get" => {
                let flags = ParsedArgs::parse(&argv[1..])?;
                anyhow::ensure!(
                    flags.switches.is_empty() && flags.options.is_empty(),
                    "behavior get accepts only an optional behavior_id; see [\"help\",\"behavior\"]"
                );
                anyhow::ensure!(
                    flags.positionals.len() <= 1,
                    "behavior get accepts at most one behavior_id"
                );
                let id = match flags.positionals.first() {
                    Some(id) => self.resolve_behavior_id(id).await?,
                    None => self.core.behavior_id().to_owned(),
                };
                self.ensure_behavior_catalog("get", Some(&id))?;
                persona_inspect(&self.node, &self.agent_did, &id, &self.process_ceiling).await
            }
            "preview" => {
                let operation = argv
                    .get(1)
                    .context("behavior preview requires create|edit|clone|disable|default")?;
                if operation == "default" {
                    let behavior_id = argv
                        .get(2)
                        .context("behavior preview default requires BEHAVIOR_ID")?;
                    anyhow::ensure!(
                        argv.len() == 3,
                        "behavior preview default accepts exactly one BEHAVIOR_ID"
                    );
                    let behavior_id = self.resolve_behavior_id(behavior_id).await?;
                    self.ensure_behavior_catalog("preview default", Some(&behavior_id))?;
                    let params = default_behavior_params("preview", &behavior_id);
                    return persona_preview(
                        &self.node,
                        &self.agent_did,
                        &params,
                        &self.process_ceiling,
                    )
                    .await;
                }
                if operation == "edit" {
                    let behavior_id = argv.get(2).context(
                        "behavior preview update requires target_id and fields in set or clear",
                    )?;
                    let core = self.target_core(Some(behavior_id), "preview edit").await?;
                    let patch = parse_patch(&argv[3..], SelfConfigTarget::AgentBehavior)?;
                    return self
                        .patch(
                            &core,
                            "preview",
                            protect_working_behavior(behavior_request(&core, patch)),
                        )
                        .await;
                }
                let mut params = behavior_params("preview", Some(operation.clone()), &argv[2..])?;
                self.resolve_persona_ids(operation, &mut params).await?;
                self.ensure_behavior_operation(operation, params.behavior_id.as_deref())?;
                self.ensure_default_selection(&params)?;
                persona_preview(&self.node, &self.agent_did, &params, &self.process_ceiling).await
            }
            "edit" => {
                let behavior_id = argv
                    .get(1)
                    .context("behavior update requires target_id and fields in set or clear")?;
                let core = self.target_core(Some(behavior_id), "edit").await?;
                let patch = parse_patch(&argv[2..], SelfConfigTarget::AgentBehavior)?;
                self.patch(
                    &core,
                    "edit",
                    protect_working_behavior(behavior_request(&core, patch)),
                )
                .await
            }
            "create" | "clone" | "disable" => {
                let identity = self.identity.as_deref().context(
                    "behavior writes require the exact local principal signer; reads remain available",
                )?;
                let mut params = behavior_params(verb, None, &argv[1..])?;
                self.resolve_persona_ids(verb, &mut params).await?;
                self.ensure_behavior_operation(verb, params.behavior_id.as_deref())?;
                self.ensure_default_selection(&params)?;
                anyhow::ensure!(
                    !(self.no_lockout
                        && verb == "disable"
                        && params.behavior_id.as_deref() == Some(self.core.behavior_id())),
                    "no-lockout guard: behavior must remain enabled"
                );
                self.execution.enter_mutation();
                persona_mutate(
                    &self.node,
                    &self.agent_did,
                    identity,
                    &params,
                    &self.process_ceiling,
                )
                .await
            }
            "default" => {
                let behavior_id = argv
                    .get(1)
                    .context("behavior default requires BEHAVIOR_ID")?;
                anyhow::ensure!(
                    argv.len() == 2,
                    "behavior default accepts exactly one BEHAVIOR_ID"
                );
                let behavior_id = self.resolve_behavior_id(behavior_id).await?;
                self.ensure_behavior_catalog("default", Some(&behavior_id))?;
                let identity = self.identity.as_deref().context(
                    "behavior writes require the exact local principal signer; reads remain available",
                )?;
                self.execution.enter_mutation();
                persona_mutate(
                    &self.node,
                    &self.agent_did,
                    identity,
                    &default_behavior_params("edit", &behavior_id),
                    &self.process_ceiling,
                )
                .await
            }
            "context" => self.behavior_context(&argv[1..]).await,
            other => bail!("unknown behavior command {other:?}; see [\"help\",\"behavior\"]"),
        }
    }

    async fn behavior_context(&self, argv: &[String]) -> Result<String> {
        anyhow::ensure!(
            self.categories.contains("behavior") || self.categories.contains("persona"),
            "behavior/context configuration is not granted"
        );
        let verb = argv
            .first()
            .map(String::as_str)
            .context("behavior context requires get, preview, or edit")?;
        let (behavior_id, rest) = extract_behavior_target(&argv[1..])?;
        let core = self.target_core(behavior_id.as_deref(), "context").await?;
        match verb {
            "get" => {
                anyhow::ensure!(
                    rest.is_empty(),
                    "behavior context get accepts only --behavior BEHAVIOR_ID"
                );
                let effective = core
                    .read_effective_config(&self.categories, self.no_lockout, self.preview)
                    .await?;
                ordered! {"resource": "AgentContext", "document": effective.get("context")}.pretty()
            }
            "preview" | "edit" => {
                let parsed = ParsedArgs::parse(&rest)?;
                if let Some(word) = parsed.positionals.first() {
                    bail!(
                        "behavior context {verb} takes no positional argument {word:?}; name the behavior in options.behavior and the fields in set: {{\"argv\":[\"behavior\",\"context\",\"{verb}\"],\"options\":{{\"behavior\":\"BEHAVIOR_ID\"}},\"set\":{{\"FIELD\":VALUE}}}}"
                    );
                }
                let patch = parse_patch_args(parsed, SelfConfigTarget::AgentContext)?;
                self.patch(
                    &core,
                    verb,
                    protect_working_behavior(anchored_request(
                        SelfConfigTarget::AgentContext,
                        "context_id",
                        patch,
                    )),
                )
                .await
            }
            other => {
                bail!("unknown behavior context command {other:?}; accepted: get, preview, edit")
            }
        }
    }

    async fn bound_document(&self, resource: &str, argv: &[String]) -> Result<String> {
        self.ensure_resource(resource)?;
        let verb = argv.first().map(String::as_str).with_context(|| {
            format!("{resource} command is required; see [\"help\",\"{resource}\"]")
        })?;
        let target = match resource {
            "tools" => SelfConfigTarget::Tools,
            "backend" => SelfConfigTarget::InferenceBackend,
            _ => unreachable!("bound resource"),
        };
        match verb {
            "list" if resource == "backend" => {
                let parsed = ParsedArgs::parse(&argv[1..])?;
                parsed.reject_mutation_flags()?;
                self.inference_inventory(target, parse_limit(&parsed)?, parsed.one("cursor")?)
                    .await
            }
            "list" => bail!(
                "{resource} has no list: each behavior has one; read it with {{\"argv\":[\"{resource}\",\"get\"],\"options\":{{\"behavior\":\"BEHAVIOR_ID\"}}}} and find behaviors with [\"behavior\",\"list\"]"
            ),
            "get" => {
                let (behavior_id, rest) = extract_behavior_target(&argv[1..])?;
                anyhow::ensure!(
                    rest.len() <= 1,
                    "{resource} get accepts at most one ID and --behavior BEHAVIOR_ID"
                );
                let core = self.target_core(behavior_id.as_deref(), resource).await?;
                match rest.first() {
                    Some(id) => self.exact_read(target, id).await,
                    None => self.bound_read(&core, target).await,
                }
            }
            "create" | "preview"
                if target == SelfConfigTarget::Tools
                    && (verb == "create" || argv.get(1).is_some_and(|word| word == "create")) =>
            {
                bail!(
                "tools are created with their behavior (behavior create); change a behavior's tools with {{\"argv\":[\"tools\",\"edit\"],\"options\":{{\"behavior\":\"BEHAVIOR_ID\"}},\"set\":{{\"GROUP\":VALUE}}}}"
                )
            }
            "preview" | "edit" => {
                let (behavior_id, rest) = extract_behavior_target(&argv[1..])?;
                let core = self.target_core(behavior_id.as_deref(), resource).await?;
                let (allow_drop, rest) = extract_allow_drop(&rest)?;
                anyhow::ensure!(
                    allow_drop.is_empty() || target == SelfConfigTarget::Tools,
                    "allow-drop applies only to tools; see [\"help\",\"tools\"]"
                );
                let patch = parse_patch(&rest, target)?;
                let request = match target {
                    SelfConfigTarget::Tools => refuse_silent_tools_drops(
                        tools_request(&core, patch, self.allow_pack_install),
                        allow_drop,
                    ),
                    SelfConfigTarget::InferenceBackend => backend_request(patch),
                    _ => unreachable!("bound resource"),
                };
                self.patch(&core, verb, protect_working_behavior(request))
                    .await
            }
            other => bail!(
                "unknown {resource} command {other:?}; accepted: get, preview, edit; see [\"help\",\"{resource}\"]"
            ),
        }
    }

    async fn backend(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("backend")?;
        let verb = argv
            .first()
            .map(String::as_str)
            .context("backend command is required; see [\"help\",\"backend\"]")?;
        if verb == "list" {
            let parsed = ParsedArgs::parse(&argv[1..])?;
            parsed.reject_mutation_flags()?;
            return self
                .inference_inventory(
                    SelfConfigTarget::InferenceBackend,
                    parse_limit(&parsed)?,
                    parsed.one("cursor")?,
                )
                .await;
        }
        let create_args = match verb {
            "create" => Some(&argv[1..]),
            "preview" if argv.get(1).map(String::as_str) == Some("create") => Some(&argv[2..]),
            _ => None,
        };
        if let Some(create_args) = create_args {
            let parsed = ParsedArgs::parse(create_args)?;
            anyhow::ensure!(
                parsed.positionals.len() == 1,
                "backend create requires exactly one BACKEND_ID"
            );
            anyhow::ensure!(
                parsed.switches.is_empty(),
                "backend create accepts no switches"
            );
            for option in parsed.options.keys() {
                anyhow::ensure!(
                    matches!(option.as_str(), "endpoint" | "name" | "wire-api"),
                    "unknown backend create option --{option}; accepted: --endpoint, --name, --wire-api"
                );
            }
            let endpoint = parsed
                .one("endpoint")?
                .filter(|value| !value.trim().is_empty())
                .context("backend create requires --endpoint URL")?
                .to_owned();
            let wire_api =
                crate::openai_wire::OpenAiWireApi::parse_optional(parsed.one("wire-api")?)?;
            let request = local_backend_create_request(
                self.agent_did.clone(),
                parsed.positionals[0].clone(),
                endpoint,
                parsed.one("name")?.map(ToOwned::to_owned),
                wire_api,
            );
            return self
                .patch(
                    &self.core,
                    if verb == "preview" { "preview" } else { "edit" },
                    request,
                )
                .await;
        }
        if verb == "discover" {
            anyhow::ensure!(argv.len() == 2, "backend discover requires one BACKEND_ID");
            let backend_id = &argv[1];
            let matches = crate::backend_registry::list_enabled_backends_for_agent(
                &self.node,
                &self.agent_did,
            )
            .await?
            .into_iter()
            .filter(|backend| backend.backend_id == *backend_id)
            .collect::<Vec<_>>();
            anyhow::ensure!(
                matches.len() == 1,
                "no unique enabled backend with ID {backend_id:?}; inspect config backend list"
            );
            let backend = &matches[0];
            if backend.provider_kind != crate::BackendProviderKind::OpenAiCompatible
                || !matches!(
                    backend.auth,
                    crate::document_config::BackendAuth::Unauthenticated
                )
            {
                // Credentialed discovery stays with the operator paths and the
                // runtime prober; the agent reads what they last published.
                let observation = self
                    .backend_observation_view(backend_id, backend.provider_kind)
                    .await?;
                return ordered! {
                    "backend_id": backend_id,
                    "refreshed": false,
                    "observation": observation,
                    "note": "This backend is credentialed, so self-config did not contact the provider. The catalog is the last credential-free observation published by operator discovery (desktop setup, gents config backend discover-models --backend-id) or the runtime prober; null means none has been recorded yet.",
                    "resource": "InferenceBackend",
                    "endpoint": backend.endpoint,
                    "provider_kind": backend.provider_kind,
                }
                .pretty();
            }
            self.execution.enter_mutation();
            let observation =
                crate::backend_registry::discover_shared_backend(&self.node, backend).await?;
            let observation = crate::backend_registry::scoped_observation_view(
                Some(&observation),
                &self.agent_did,
                backend.provider_kind,
            )?;
            return ordered! {
                "backend_id": backend_id,
                "refreshed": true,
                "observation": observation,
                "note": "Provider-advertised facts were recorded for this persisted backend. Discovery did not create a profile, select a model, or change a behavior.",
                "resource": "InferenceBackend",
                "endpoint": backend.endpoint,
            }
            .pretty();
        }
        self.bound_document("backend", argv).await
    }

    async fn profile(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("profile")?;
        let verb = argv
            .first()
            .map(String::as_str)
            .context("profile command is required; see [\"help\",\"profile\"]")?;
        if verb == "list" {
            let parsed = ParsedArgs::parse(&argv[1..])?;
            parsed.reject_mutation_flags()?;
            return self
                .inference_inventory(
                    SelfConfigTarget::InferenceProfile,
                    parse_limit(&parsed)?,
                    parsed.one("cursor")?,
                )
                .await;
        }
        let create_args = match verb {
            "create" => Some(&argv[1..]),
            "preview" if argv.get(1).map(String::as_str) == Some("create") => Some(&argv[2..]),
            _ => None,
        };
        if let Some(create_args) = create_args {
            let profile_id = create_args
                .first()
                .filter(|value| !value.starts_with("--"))
                .context(
                    "profile create requires target_id plus set.backend_id and set.model_name",
                )?;
            let patch = parse_patch(&create_args[1..], SelfConfigTarget::InferenceProfile)?;
            let fields = patch
                .iter()
                .filter_map(|(field, value)| value.as_ref().map(|_| field.as_str()))
                .collect::<BTreeSet<_>>();
            anyhow::ensure!(
                fields.contains("backend_id") && fields.contains("model_name"),
                "profile create requires --set backend_id=JSON and --set model_name=JSON"
            );
            let request = profile_create_request(self.agent_did.clone(), profile_id.clone(), patch);
            return self
                .patch(
                    &self.core,
                    if verb == "preview" { "preview" } else { "edit" },
                    request,
                )
                .await;
        }
        let (behavior_id, target_args) = extract_behavior_target(&argv[1..])?;
        if behavior_id.is_none() && matches!(verb, "preview" | "edit") {
            self.require_named_profile_owner(verb).await?;
        }
        let core = self.target_core(behavior_id.as_deref(), "profile").await?;
        let mut rest = target_args.as_slice();
        let target_name = rest
            .first()
            .filter(|value| !value.starts_with("--"))
            .map(String::as_str)
            .unwrap_or("profile");
        if !rest.is_empty() && !rest[0].starts_with("--") {
            rest = &rest[1..];
        }
        if verb == "get"
            && !matches!(
                target_name,
                "profile" | "sampling" | "execution" | "retry-policy" | "compaction"
            )
        {
            anyhow::ensure!(rest.is_empty(), "profile get accepts one PROFILE_ID");
            return self
                .exact_read(SelfConfigTarget::InferenceProfile, target_name)
                .await;
        }
        let (request_name, target) = profile_target(target_name)?;
        match verb {
            "get" => {
                anyhow::ensure!(rest.is_empty(), "profile get accepts at most one target");
                self.bound_read(&core, target).await
            }
            "preview" | "edit" => {
                let patch = parse_patch(rest, target)?;
                self.patch(
                    &core,
                    verb,
                    protect_working_behavior(profile_target_request(Some(request_name), patch)?),
                )
                .await
            }
            other => bail!(
                "unknown profile command {other:?}; accepted: get, preview, edit; see [\"help\",\"profile\"]"
            ),
        }
    }

    /// A local SubagentTarget names its behavior by the same short slug; a
    /// foreign target_agent_did keeps the ID exactly as given.
    async fn resolve_target_behavior(
        &self,
        target_id: &str,
        patch: &mut SelfConfigPatch,
    ) -> Result<()> {
        let Some(index) = patch.iter().position(|(field, value)| {
            field == "behavior_id" && value.as_ref().is_some_and(Value::is_string)
        }) else {
            return Ok(());
        };
        let local = match patch.iter().find(|(field, _)| field == "target_agent_did") {
            Some((_, value)) => {
                value.as_ref().and_then(Value::as_str) == Some(self.agent_did.as_str())
            }
            None => {
                let owner = self.agent_did.clone();
                let target_id = target_id.to_owned();
                crate::config_client::ConfigAccess::transact_local(
                    &self.node,
                    Some(self.core.identity()?),
                    "self_config.subagent_target_owner",
                    |txn| {
                        let owner = owner.clone();
                        let target_id = target_id.clone();
                        Box::pin(async move {
                            Ok(ops::read_owned_doc(
                                txn,
                                SelfConfigTarget::SubagentTarget,
                                &owner,
                                &target_id,
                            )
                            .await?
                            .and_then(|(_, doc)| doc.get("target_agent_did").cloned())
                                == Some(json!(owner)))
                        })
                    },
                )
                .await?
            }
        };
        if !local {
            return Ok(());
        }
        let short = patch[index]
            .1
            .as_ref()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if let Ok(resolved) = self.resolve_behavior_id(&short).await {
            patch[index].1 = Some(json!(resolved));
        }
        Ok(())
    }

    async fn mcp_service(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("mcp-service")?;
        let verb = argv
            .first()
            .map(String::as_str)
            .context("mcp-service command is required; see [\"help\",\"mcp-service\"]")?;
        let id = required_resource_id(argv.get(1), "SERVICE_ID")?;
        match verb {
            "get" => {
                anyhow::ensure!(argv.len() == 2, "mcp-service get accepts one SERVICE_ID");
                self.exact_read(SelfConfigTarget::ToolServiceRegistry, id)
                    .await
            }
            "preview" | "edit" => {
                let patch = parse_patch(&argv[2..], SelfConfigTarget::ToolServiceRegistry)?;
                self.patch(&self.core, verb, mcp_service_request(id.clone(), patch))
                    .await
            }
            other => bail!(
                "unknown mcp-service command {other:?}; accepted: get, preview, edit; see [\"help\",\"mcp-service\"]"
            ),
        }
    }

    async fn automation(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("automation")?;
        let verb = argv
            .first()
            .map(String::as_str)
            .context("automation command is required; see [\"help\",\"automation\"]")?;
        if verb == "list" {
            let kind = argv
                .get(1)
                .context("automation list requires task, trigger, schedule or event-source")?;
            let target = automation_target(&kind.replace('-', "_"))?;
            let parsed = ParsedArgs::parse(&argv[2..])?;
            parsed.reject_mutation_flags()?;
            return self
                .inference_inventory(target, parse_limit(&parsed)?, parsed.one("cursor")?)
                .await;
        }
        let kind = argv.get(1).context("automation requires KIND")?;
        let id = required_resource_id(argv.get(2), "automation ID")?;
        let target = automation_target(&kind.replace('-', "_"))?;
        match verb {
            "get" => {
                let (behavior_id, rest) = extract_behavior_target(&argv[3..])?;
                anyhow::ensure!(
                    rest.is_empty(),
                    "automation get accepts KIND, ID, and --behavior BEHAVIOR_ID"
                );
                let core = self.target_core(behavior_id.as_deref(), "automation").await?;
                self.automation_read(&core, target, id).await
            }
            "preview" | "edit" => {
                let (behavior_id, rest) = extract_behavior_target(&argv[3..])?;
                let core = self.target_core(behavior_id.as_deref(), "automation").await?;
                let patch = parse_patch(&rest, target)?;
                anyhow::ensure!(
                    target != SelfConfigTarget::EventSource
                        || patch.iter().all(|(field, value)| {
                            field != "filter"
                                || value.as_ref().is_none_or(|value| value.is_string() || value.is_null())
                        }),
                    "filter must be a string holding a GraphQL object literal with unquoted keys, e.g. \"{{kind: {{_eq: \\\"review\\\"}}}}\", not a JSON object; see [\"help\",\"automation\"]"
                );
                self.patch(
                    &core,
                    verb,
                    protect_working_behavior(automation_request(&core, target, id.clone(), patch)),
                )
                .await
            }
            other => bail!(
                "unknown automation command {other:?}; accepted: get, preview, edit; see [\"help\",\"automation\"]"
            ),
        }
    }

    async fn cleanup(&self, argv: &[String]) -> Result<String> {
        let verb = argv
            .first()
            .map(String::as_str)
            .context("cleanup requires preview or remove; see [\"help\",\"cleanup\"]")?;
        anyhow::ensure!(
            matches!(verb, "preview" | "remove"),
            "unknown cleanup command {verb:?}; accepted: preview, remove"
        );
        if verb == "preview" {
            anyhow::ensure!(
                self.preview,
                "cleanup preview is not granted for this behavior"
            );
        }
        let parsed = ParsedArgs::parse(&argv[1..])?;
        anyhow::ensure!(
            parsed.positionals.is_empty() && parsed.switches.is_empty(),
            "cleanup accepts only repeated --target RESOURCE=ID"
        );
        for option in parsed.options.keys() {
            anyhow::ensure!(
                matches!(option.as_str(), "target" | "digest"),
                "unknown cleanup option --{option}; accepted: --target RESOURCE=ID, --digest SHA256"
            );
        }
        let expected_digest = parsed.one("digest")?.map(ToOwned::to_owned);
        if verb == "preview" {
            anyhow::ensure!(
                expected_digest.is_none(),
                "cleanup preview does not accept --digest; it returns the digest to authorize remove"
            );
        } else {
            anyhow::ensure!(
                expected_digest.is_some(),
                "cleanup remove requires --digest from cleanup preview"
            );
        }
        let values = parsed
            .options
            .get("target")
            .context("cleanup requires at least one --target RESOURCE=ID")?;
        let mut targets = Vec::with_capacity(values.len());
        let mut seen = BTreeSet::new();
        for value in values {
            let (resource, id) = value
                .split_once('=')
                .context("--target must be RESOURCE=ID")?;
            anyhow::ensure!(
                !id.trim().is_empty(),
                "cleanup target {resource:?} requires a non-empty ID"
            );
            let target = cleanup_target(resource)?;
            let id = if target == SelfConfigTarget::AgentBehavior {
                self.resolve_behavior_id(id).await?
            } else {
                id.to_owned()
            };
            let id = id.as_str();
            if matches!(
                target,
                SelfConfigTarget::AgentBehavior | SelfConfigTarget::AgentContext
            ) {
                anyhow::ensure!(
                    self.categories.contains("persona"),
                    "{resource} cleanup requires the behavior catalog grant (category persona)"
                );
            } else {
                self.ensure_resource(match target.category() {
                    "mcp_service" => "mcp-service",
                    category => category,
                })?;
            }
            anyhow::ensure!(
                seen.insert((target.collection_name(), id.to_owned())),
                "duplicate cleanup target {resource}={id}"
            );
            targets.push((resource.to_owned(), target, id.to_owned()));
        }
        targets.sort_by(|left, right| {
            (left.1.collection_name(), left.2.as_str())
                .cmp(&(right.1.collection_name(), right.2.as_str()))
        });

        let owner = self.agent_did.clone();
        let removals = targets
            .iter()
            .map(|(_, target, id)| (target.collection(), owner.clone(), id.clone()))
            .collect::<Vec<_>>();
        let plan = crate::config_client::DesiredStateApplyPlan::new(Vec::new())?
            .with_removals(removals)?;
        let preview = verb == "preview";
        if !preview {
            self.execution.enter_mutation();
        }
        let (receipt_targets, plan_digest) = crate::config_client::ConfigAccess::transact_local(
            &self.node,
            Some(self.core.identity()?),
            if preview {
                "self_config.cleanup_preview"
            } else {
                "self_config.cleanup_remove"
            },
            |txn| {
                let owner = owner.clone();
                let targets = targets.clone();
                let expected_digest = expected_digest.clone();
                let plan = &plan;
                Box::pin(async move {
                    let mut receipt = Vec::with_capacity(targets.len());
                    for (resource, target, id) in &targets {
                        let document = ops::read_owned_doc(txn, *target, &owner, id)
                            .await?
                            .map(|(_, document)| document)
                            .with_context(|| {
                                format!(
                                    "no owned {} with ID {id:?}; list or inspect exact IDs before cleanup",
                                    target.collection_name()
                                )
                            })?;
                        if *target == SelfConfigTarget::AgentBehavior {
                            let protected = document
                                .get("tags")
                                .and_then(Value::as_array)
                                .is_some_and(|tags| {
                                    tags.iter().any(|tag| {
                                        tag.as_str()
                                            == Some(crate::agent::persona_ops::SETUP_STEWARD_BEHAVIOR_TAG)
                                    })
                                });
                            anyhow::ensure!(
                                !protected,
                                "target behavior {id:?} is the protected Setup configurator and cannot be removed"
                            );
                        }
                        receipt.push(json!({
                            "resource": resource,
                            "collection": target.collection_name(),
                            "id": id,
                            "content_digest": crate::config_client::desired_state_document_digest(&Value::Object(document))?,
                        }));
                    }
                    let plan_digest = cleanup_plan_digest(&owner, &receipt)?;
                    if let Some(expected) = expected_digest.as_deref() {
                        anyhow::ensure!(
                            expected == plan_digest,
                            "cleanup target content changed: preview authorized {expected:?}, current plan is {plan_digest:?}; preview again"
                        );
                    }
                    crate::config_client::validate_desired_state_plan(txn, plan).await?;
                    if !preview {
                        crate::config_client::apply_desired_state_plan(txn, plan).await?;
                        for (_, target, id) in &targets {
                            anyhow::ensure!(
                                ops::read_owned_doc(txn, *target, &owner, id).await?.is_none(),
                                "cleanup did not remove {} {id:?}",
                                target.collection_name()
                            );
                        }
                    }
                    Ok((receipt, plan_digest))
                })
            },
        )
        .await?;
        ordered! {
            "committed": !preview,
            "operation": if preview { "preview cleanup" } else { "cleanup" },
            "targets": receipt_targets,
            "effect": if preview {
                "No documents were changed. Repeat the same exact targets and plan digest with cleanup remove to revalidate and apply atomically."
            } else {
                "The exact target set was removed atomically after retained-reference validation."
            },
            "apply_with": preview.then(|| json!({
                "argv_prefix": ["cleanup", "remove", "--digest", plan_digest],
                "repeat_targets": targets.iter().map(|(resource, _, id)| format!("{resource}={id}")).collect::<Vec<_>>(),
            })),
            "plan_digest": plan_digest,
            "owner": self.agent_did,
        }
        .pretty()
    }

    async fn pack(&self, argv: &[String]) -> Result<String> {
        anyhow::ensure!(self.allow_pack_install, "pack installation is not granted");
        let installer = PackInstaller {
            core: self.core.clone(),
            node: self.node.clone(),
            home: self.plugins.home().map(std::path::Path::to_path_buf),
        };
        let verb = argv
            .first()
            .map(String::as_str)
            .context("pack command is required; see [\"help\",\"pack\"]")?;
        match verb {
            "list" => {
                let parsed = ParsedArgs::parse(&argv[1..])?;
                parsed.reject_mutation_flags()?;
                installer
                    .list(parse_limit(&parsed)?, parsed.one("cursor")?)
                    .await
            }
            "get" => {
                anyhow::ensure!(argv.len() == 2, "pack get requires exactly one PACKAGE");
                installer.get(&argv[1]).await
            }
            "preview" => {
                let operation = argv
                    .get(1)
                    .context("pack preview requires install or update")?;
                anyhow::ensure!(
                    matches!(operation.as_str(), "install" | "update"),
                    "pack preview supports install and update"
                );
                installer
                    .preview(operation, parse_pack_change(&argv[2..])?)
                    .await
            }
            "install" | "update" => {
                let change = parse_pack_change(&argv[1..])?;
                self.execution.enter_mutation();
                installer.apply(verb, change).await
            }
            "remove" => {
                anyhow::ensure!(argv.len() == 2, "pack remove requires exactly one PACKAGE");
                self.execution.enter_mutation();
                installer.remove(&argv[1]).await
            }
            other => bail!("unknown pack command {other:?}; see [\"help\",\"pack\"]"),
        }
    }

    fn ensure_resource(&self, resource: &str) -> Result<()> {
        let category = match resource {
            "mcp-service" => "mcp_service",
            other => other,
        };
        anyhow::ensure!(
            self.categories.contains(category),
            "{resource} configuration is not granted; enabled resources: {}",
            model_resources(&self.categories, self.allow_pack_install).join(", ")
        );
        Ok(())
    }

    fn ensure_behavior_catalog(&self, operation: &str, behavior_id: Option<&str>) -> Result<()> {
        let current_only = behavior_id.is_some_and(|id| id == self.core.behavior_id());
        anyhow::ensure!(
            self.categories.contains("persona") || current_only,
            "behavior {operation} outside the current behavior is not granted; the behavior catalog grant (category persona) is required"
        );
        Ok(())
    }

    fn ensure_behavior_operation(&self, operation: &str, behavior_id: Option<&str>) -> Result<()> {
        let current_edit =
            operation == "edit" && behavior_id.is_some_and(|id| id == self.core.behavior_id());
        anyhow::ensure!(
            self.categories.contains("persona") || current_edit,
            "behavior {operation} is not granted; only editing the current behavior is allowed without the behavior catalog grant (category persona)"
        );
        Ok(())
    }

    fn ensure_default_selection(&self, params: &ConfigurePersonaParams) -> Result<()> {
        anyhow::ensure!(
            !params.make_default || self.categories.contains("persona"),
            "--default changes the principal's behavior selection and requires the behavior catalog grant (category persona)"
        );
        Ok(())
    }

    /// Behavior create stores `<principal DID>:<slug>`
    /// (`persona_ops::derive_behavior_id`) while every other configuration ID
    /// is stored as given, so wherever config accepts a behavior ID it also
    /// accepts that principal-local slug. An exact ID wins, and the slug only
    /// resolves under this principal's own DID.
    async fn resolve_behavior_id(&self, id: &str) -> Result<String> {
        let current = self.core.behavior_id();
        if !self.categories.contains("persona") {
            // Only the current behavior is reachable; the catalog check names
            // the missing grant for any other ID.
            let slug_of_current = format!("{}:{id}", self.agent_did) == current;
            return Ok(if slug_of_current { current } else { id }.to_owned());
        }
        if id == current {
            return Ok(id.to_owned());
        }
        let owner = self.agent_did.clone();
        let candidates = [id.to_owned(), format!("{owner}:{id}")];
        let listing = format!(
            "{{ AgentBehavior(filter: {{agent_did: {{_eq: \"{}\"}}}}) {{behavior_id display_name}} }}",
            escape_graphql_string(&owner)
        );
        let found = crate::config_client::ConfigAccess::transact_local(
            &self.node,
            Some(self.core.identity()?),
            "self_config.resolve_behavior",
            |txn| {
                let owner = owner.clone();
                let candidates = candidates.clone();
                let listing = listing.clone();
                Box::pin(async move {
                    for candidate in candidates {
                        if ops::read_owned_doc(
                            txn,
                            SelfConfigTarget::AgentBehavior,
                            &owner,
                            &candidate,
                        )
                        .await?
                        .is_some()
                        {
                            return Ok(Ok(candidate));
                        }
                    }
                    Ok(Err(txn.execute(&listing).await?))
                })
            },
        )
        .await?;
        found.map_err(|listing| {
            let prefix = format!("{owner}:");
            let mut suggestions =
                gents_protocol::graphql::graphql_rows_from_response(&listing, "AgentBehavior")
                    .into_iter()
                    .filter_map(|row| {
                        let behavior_id = row.get("behavior_id")?.as_str()?;
                        let slug = behavior_id.strip_prefix(&prefix);
                        let display_name = row.get("display_name").and_then(Value::as_str);
                        [Some(behavior_id), slug, display_name]
                            .into_iter()
                            .flatten()
                            .any(|name| name.eq_ignore_ascii_case(id))
                            .then(|| slug.unwrap_or(behavior_id).to_owned())
                    })
                    .collect::<Vec<_>>();
            suggestions.sort();
            suggestions.dedup();
            ops::MissingBehavior {
                behavior_id: id.to_owned(),
                suggestions,
            }
            .into()
        })
    }

    /// A profile edit without options.behavior changes the invoking
    /// behavior's own model and limits. Once the principal has other
    /// behaviors that default is the likely mistake (the call was meant for
    /// a worker), and the tool keeps no memory of which behaviors a caller
    /// just created, so any second behavior makes the target explicit.
    async fn require_named_profile_owner(&self, verb: &str) -> Result<()> {
        let owner = escape_graphql_string(&self.agent_did);
        let query = format!(
            "{{ AgentBehavior(filter: {{agent_did: {{_eq: \"{owner}\"}}}}) {{behavior_id}} }}"
        );
        let response = crate::config_client::ConfigAccess::transact_local(
            &self.node,
            Some(self.core.identity()?),
            "self_config.profile_owners",
            |txn| {
                let query = query.clone();
                Box::pin(async move { txn.execute(&query).await })
            },
        )
        .await?;
        let mut ids =
            gents_protocol::graphql::graphql_rows_from_response(&response, "AgentBehavior")
                .into_iter()
                .filter_map(|row| {
                    row.get("behavior_id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .collect::<Vec<_>>();
        ids.sort();
        anyhow::ensure!(
            ids.len() <= 1,
            "profile {verb} needs options.behavior because this principal has several behaviors: {}. Name the behavior whose profile to change; use {:?} for your own",
            ids.join(", "),
            self.core.behavior_id()
        );
        Ok(())
    }

    /// Create and clone derive the new ID from the display name, so an
    /// explicit ID would be silently ignored; clone sources and disable
    /// targets resolve like every other behavior ID.
    async fn resolve_persona_ids(
        &self,
        operation: &str,
        params: &mut ConfigurePersonaParams,
    ) -> Result<()> {
        if matches!(operation, "create" | "clone") {
            anyhow::ensure!(
                params.behavior_id.is_none(),
                "behavior {operation} takes no id: it derives behavior_id \"<DID>:<slug of display-name>\" and returns it; remove options.id"
            );
        } else if let Some(id) = params.behavior_id.take() {
            params.behavior_id = Some(self.resolve_behavior_id(&id).await?);
        }
        if let Some(source) = params.clone_from.take() {
            params.clone_from = Some(self.resolve_behavior_id(&source).await?);
        }
        Ok(())
    }

    async fn target_core(
        &self,
        behavior_id: Option<&str>,
        operation: &str,
    ) -> Result<SelfConfigCore> {
        let behavior_id = match behavior_id {
            Some(id) => self.resolve_behavior_id(id).await?,
            None => self.core.behavior_id().to_owned(),
        };
        let behavior_id = behavior_id.as_str();
        self.ensure_behavior_catalog(operation, Some(behavior_id))?;
        let invoking_behavior_id = self.core.behavior_id().to_owned();
        SelfConfigCore::new(
            self.node.clone(),
            self.agent_did.clone(),
            behavior_id.to_owned(),
        )
        .map(|core| {
            core.with_lockout_behavior_id(invoking_behavior_id)
                .with_no_lockout(self.no_lockout)
                .with_process_ceiling(self.process_ceiling.clone())
        })
    }

    async fn patch(
        &self,
        core: &SelfConfigCore,
        verb: &str,
        request: ApplyRequest<'static>,
    ) -> Result<String> {
        // A JSON-stringified object where a native one belongs is the common
        // model mistake; name the field instead of the decoder's complaint.
        let stringified = request
            .patch
            .iter()
            .find(|(_, value)| {
                value.as_ref().and_then(Value::as_str).is_some_and(|text| {
                    serde_json::from_str::<Value>(text)
                        .is_ok_and(|parsed| parsed.is_object() || parsed.is_array())
                })
            })
            .map(|(field, _)| field.clone());
        let collection = request.target.collection_name();
        self.patch_outcome(core, verb, request).await.map_err(|error| {
            match stringified.filter(|_| format!("{error:#}").contains("invalid type: string")) {
                Some(field) => anyhow!(
                    "{collection} field {field:?} holds a JSON string; send a native JSON object or array in set, not a string. ({error:#})"
                ),
                None => error,
            }
        })
    }

    async fn patch_outcome(
        &self,
        core: &SelfConfigCore,
        verb: &str,
        request: ApplyRequest<'static>,
    ) -> Result<String> {
        let outcome = match verb {
            "preview" => {
                anyhow::ensure!(self.preview, "preview is not granted for this behavior");
                core.preview(request).await?
            }
            "edit" => {
                self.execution.enter_mutation();
                core.apply(request).await?
            }
            _ => unreachable!("patch verb"),
        };
        outcome_text(&outcome)
    }

    async fn bound_read(&self, core: &SelfConfigCore, target: SelfConfigTarget) -> Result<String> {
        let effective = core
            .read_effective_config(&self.categories, self.no_lockout, self.preview)
            .await?;
        let value = match target {
            SelfConfigTarget::Tools => effective.pointer("/documents/Tools"),
            SelfConfigTarget::InferenceProfile => effective.get("inference_profile"),
            SelfConfigTarget::InferenceBackend => {
                effective.pointer("/documents/InferenceBackend")
            }
            SelfConfigTarget::InferenceSampling => {
                effective.pointer("/documents/InferenceSampling")
            }
            SelfConfigTarget::InferenceExecution => {
                effective.pointer("/documents/InferenceExecution")
            }
            SelfConfigTarget::InferenceRetryPolicy => {
                effective.pointer("/documents/InferenceRetryPolicy")
            }
            SelfConfigTarget::Compaction => effective.pointer("/documents/Compaction"),
            _ => None,
        }
        .with_context(|| {
            format!(
                "selected behavior does not reference a {}; inspect config behavior get {} to see its exact reference chain",
                target.collection_name(),
                core.behavior_id()
            )
        })?;
        if target == SelfConfigTarget::InferenceBackend {
            let observation = self.backend_document_observation(value).await?;
            return ordered! {"resource": target.collection_name(), "document": value, "observation": observation}.pretty();
        }
        if target == SelfConfigTarget::InferenceExecution {
            return ordered! {"resource": target.collection_name(), "effective": super::read::execution_settings(Some(value))?, "document": value}.pretty();
        }
        ordered! {"resource": target.collection_name(), "document": value}.pretty()
    }

    /// Credential-free observation of one owned backend in its own
    /// authentication scope, read under the invoking principal's identity.
    async fn backend_observation_view(
        &self,
        backend_id: &str,
        provider_kind: crate::BackendProviderKind,
    ) -> Result<Value> {
        let owner = self.agent_did.clone();
        let backend_id = backend_id.to_owned();
        let observation = crate::config_client::ConfigAccess::transact_local(
            &self.node,
            Some(self.core.identity()?),
            "self_config.backend_observation",
            |txn| {
                let owner = owner.clone();
                let backend_id = backend_id.clone();
                Box::pin(async move {
                    crate::backend_registry::lookup_backend_observation_in_txn(
                        txn,
                        &owner,
                        &backend_id,
                    )
                    .await
                })
            },
        )
        .await?;
        crate::backend_registry::scoped_observation_view(
            observation.as_ref(),
            &self.agent_did,
            provider_kind,
        )
    }

    async fn backend_document_observation(&self, document: &Value) -> Result<Value> {
        let backend_id = document
            .get("backend_id")
            .and_then(Value::as_str)
            .context("backend document has no backend_id")?;
        let provider_kind = serde_json::from_value(
            document
                .get("provider_kind")
                .cloned()
                .context("backend document has no provider_kind")?,
        )
        .context("decoding backend provider_kind")?;
        self.backend_observation_view(backend_id, provider_kind)
            .await
    }

    async fn exact_read(&self, target: SelfConfigTarget, id: &str) -> Result<String> {
        let id = id.to_owned();
        let lookup_id = id.clone();
        let owner = self.agent_did.clone();
        let mut document = crate::config_client::ConfigAccess::transact_local(
            &self.node,
            Some(self.core.identity()?),
            "self_config.command_read",
            |txn| {
                let owner = owner.clone();
                let lookup_id = lookup_id.clone();
                Box::pin(async move { ops::read_owned_doc(txn, target, &owner, &lookup_id).await })
            },
        )
        .await?
        .map(|(_, document)| document)
        .ok_or_else(|| super::ops::MissingConfigDocument { target, id })?;
        if target == SelfConfigTarget::InferenceBackend {
            document.remove("auth");
            document.insert(
                "auth".into(),
                json!({"redacted": true, "owner": "operator credential/login flow"}),
            );
            let document = Value::Object(document);
            let observation = self.backend_document_observation(&document).await?;
            return ordered! {"resource": target.collection_name(), "document": document, "observation": observation}.pretty();
        }
        if target == SelfConfigTarget::SubagentTarget {
            return ordered! {"resource": target.collection_name(), "connection": ops::target_destination(&document, &self.agent_did), "document": document}.pretty();
        }
        if target == SelfConfigTarget::InferenceExecution {
            let document = Value::Object(document);
            return ordered! {"resource": target.collection_name(), "effective": super::read::execution_settings(Some(&document))?, "document": document}.pretty();
        }
        ordered! {"resource": target.collection_name(), "document": document}.pretty()
    }

    async fn automation_read(
        &self,
        core: &SelfConfigCore,
        target: SelfConfigTarget,
        id: &str,
    ) -> Result<String> {
        let effective = core
            .read_effective_config(&self.categories, self.no_lockout, self.preview)
            .await?;
        let document = effective
            .pointer(&format!("/automation/{}", target.collection_name()))
            .and_then(Value::as_array)
            .and_then(|rows| {
                rows.iter()
                    .find(|row| row.get(target.unique_field()).and_then(Value::as_str) == Some(id))
            })
            .with_context(|| {
                format!(
                    "no {} {id:?} is reachable from selected behavior {:?}; use options.behavior to select its owning behavior, then get to read its automation IDs",
                    target.collection_name(),
                    core.behavior_id()
                )
            })?;
        ordered! {"resource": target.collection_name(), "document": document}.pretty()
    }

    async fn inference_inventory(
        &self,
        target: SelfConfigTarget,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<String> {
        anyhow::ensure!(
            (1..=50).contains(&limit),
            "--limit must be between 1 and 50"
        );
        let (collection, id_field, fields) = match target {
            SelfConfigTarget::InferenceProfile => (
                "InferenceProfile",
                "profile_id",
                "profile_id display_name description backend_id model_name reasoning_effort context_window max_output_tokens sampling_id execution_id",
            ),
            SelfConfigTarget::InferenceBackend => (
                "InferenceBackend",
                "backend_id",
                "backend_id name provider_kind openai_wire_api endpoint connect_timeout_secs discovery_timeout_secs max_concurrent max_queue_depth enabled catalogs probe_status last_probe",
            ),
            _ => (
                target.collection_name(),
                target.unique_field(),
                "",
            ),
        };
        let all_fields = target
            .all_fields()
            .iter()
            .copied()
            .filter(|field| {
                matches!(
                    target,
                    SelfConfigTarget::SubagentTarget | SelfConfigTarget::InferenceExecution
                ) || *field == target.unique_field()
                    || [
                        "display_name",
                        "name",
                        "description",
                        "enabled",
                        "tags",
                        "context_id",
                        "tools_id",
                        "inference_profile_id",
                        "backend_id",
                        "model_name",
                        "sampling_id",
                        "execution_id",
                        "retry_policy_id",
                        "compaction_id",
                        "behavior_id",
                        "task_id",
                        "source_collection",
                    ]
                    .contains(field)
            })
            .collect::<Vec<_>>()
            .join(" ");
        let fields = if fields.is_empty() {
            all_fields.as_str()
        } else {
            fields
        };
        let owner = escape_graphql_string(&self.agent_did);
        let query =
            format!("{{ {collection}(filter: {{agent_did: {{_eq: \"{owner}\"}}}}) {{{fields}}} }}");
        let response = crate::config_client::ConfigAccess::transact_local(
            &self.node,
            Some(self.core.identity()?),
            "self_config.inference_inventory",
            |txn| {
                let query = query.clone();
                Box::pin(async move { txn.execute(&query).await })
            },
        )
        .await?;
        let mut rows = response
            .get("data")
            .and_then(|data| data.get(collection))
            .and_then(Value::as_array)
            .context("inventory query returned no rows array")?
            .clone();
        if target == SelfConfigTarget::InferenceBackend {
            for row in &mut rows {
                let observation: crate::document_config::InferenceBackendObservation =
                    serde_json::from_value(row.clone()).context("decoding backend observation")?;
                let provider_kind = serde_json::from_value(
                    row.get("provider_kind")
                        .cloned()
                        .context("backend row has no provider_kind")?,
                )
                .context("decoding backend provider_kind")?;
                let view = crate::backend_registry::scoped_observation_view(
                    Some(&observation),
                    &self.agent_did,
                    provider_kind,
                )?;
                let object = row
                    .as_object_mut()
                    .context("backend row must be an object")?;
                object.remove("catalogs");
                object.insert("catalog".into(), view["catalog"].clone());
            }
        }
        rows.sort_by(|left, right| {
            left.get(id_field)
                .and_then(Value::as_str)
                .cmp(&right.get(id_field).and_then(Value::as_str))
        });
        let total = rows.len();
        let mut selected = rows
            .into_iter()
            .filter(|row| {
                row.get(id_field)
                    .and_then(Value::as_str)
                    .is_some_and(|id| cursor.is_none_or(|cursor| id > cursor))
            })
            .take(limit + 1)
            .collect::<Vec<_>>();
        let truncated = selected.len() > limit;
        selected.truncate(limit);
        if target == SelfConfigTarget::InferenceExecution {
            for row in &mut selected {
                let effective = super::read::execution_settings(Some(row))?;
                row.as_object_mut()
                    .context("execution row must be an object")?
                    .insert("effective".into(), effective);
            }
        }
        let next_cursor = truncated
            .then(|| {
                selected
                    .last()?
                    .get(id_field)?
                    .as_str()
                    .map(ToOwned::to_owned)
            })
            .flatten();
        ordered! {
            "resource": collection,
            "items": selected,
            "page": json!({
                "limit": limit,
                "total": total,
                "returned": selected.len(),
                "truncated": truncated,
                "next_cursor": next_cursor,
            }),
            "note": "Inventory is read-only and does not rebind any behavior. Backend credentials are excluded.",
        }
        .pretty()
    }
}

/// Verbs a model reasonably guesses that mean an existing command: `preview
/// edit` is `preview` where preview replaces edit, and automation `create` is
/// its upsert through `edit`. Resources whose `preview edit ID` or `create`
/// are distinct commands are left alone.
fn normalize_verbs(argv: &mut Vec<String>) {
    let words = argv.iter().map(String::as_str).collect::<Vec<_>>();
    let drop = match words.as_slice() {
        ["automation", "preview", "edit" | "create", ..]
        | ["tools" | "backend" | "profile", "preview", "edit", ..] => Some(2),
        ["behavior", "context", "preview", "edit", ..] => Some(3),
        _ => None,
    };
    if let Some(index) = drop {
        argv.remove(index);
    } else if words.starts_with(&["automation", "create"]) {
        argv[1] = "edit".to_owned();
    }
}

/// The exact call that applies a preview, for a model that guessed `apply`.
fn apply_refusal(resource: &str) -> Option<String> {
    let form = match resource {
        "pack" => "[\"pack\",\"install\",PACKAGE] with options.digest from [\"pack\",\"preview\",\"install\",PACKAGE]".to_owned(),
        "cleanup" => "[\"cleanup\",\"remove\"] with options.target and options.digest from [\"cleanup\",\"preview\"]".to_owned(),
        "skill" => "[\"skill\",\"import\",SKILL_ID,PATH]".to_owned(),
        _ => "the previewed call with its target_id, options and set, removing \"preview\" from argv and putting \"edit\" in its place when no create or edit follows it".to_owned(),
    };
    Some(format!("{resource} has no apply verb; use {form}"))
}

fn cleanup_plan_digest(owner: &str, targets: &[Value]) -> Result<String> {
    use sha2::{Digest, Sha256};
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&json!({
            "owner": owner,
            "targets": targets,
        }))?)
    ))
}

fn default_behavior_params(action: &str, behavior_id: &str) -> ConfigurePersonaParams {
    ConfigurePersonaParams {
        action: action.to_owned(),
        operation: (action == "preview").then(|| "edit".to_owned()),
        behavior_id: Some(behavior_id.to_owned()),
        make_default: true,
        ..Default::default()
    }
}

fn cleanup_target(name: &str) -> Result<SelfConfigTarget> {
    match name {
        "behavior" => Ok(SelfConfigTarget::AgentBehavior),
        "context" => Ok(SelfConfigTarget::AgentContext),
        "tools" => Ok(SelfConfigTarget::Tools),
        "subagent-target" => Ok(SelfConfigTarget::SubagentTarget),
        "profile" => Ok(SelfConfigTarget::InferenceProfile),
        "sampling" => Ok(SelfConfigTarget::InferenceSampling),
        "execution" => Ok(SelfConfigTarget::InferenceExecution),
        "retry-policy" => Ok(SelfConfigTarget::InferenceRetryPolicy),
        "compaction" => Ok(SelfConfigTarget::Compaction),
        "backend" => Ok(SelfConfigTarget::InferenceBackend),
        "mcp-service" => Ok(SelfConfigTarget::ToolServiceRegistry),
        "task" => Ok(SelfConfigTarget::Task),
        "schedule" => Ok(SelfConfigTarget::Schedule),
        "trigger" => Ok(SelfConfigTarget::Trigger),
        "event-source" => Ok(SelfConfigTarget::EventSource),
        "datastore" => Ok(SelfConfigTarget::DatastoreToolSurface),
        "skill" => Ok(SelfConfigTarget::Skill),
        other => bail!("unknown cleanup resource {other:?}; see [\"help\",\"cleanup\"]"),
    }
}

fn profile_target(name: &str) -> Result<(&'static str, SelfConfigTarget)> {
    match name {
        "profile" => Ok(("profile", SelfConfigTarget::InferenceProfile)),
        "sampling" => Ok(("sampling", SelfConfigTarget::InferenceSampling)),
        "execution" => Ok(("execution", SelfConfigTarget::InferenceExecution)),
        "retry-policy" => Ok(("retry_policy", SelfConfigTarget::InferenceRetryPolicy)),
        "compaction" => Ok(("compaction", SelfConfigTarget::Compaction)),
        other => bail!(
            "unknown profile target {other:?}; accepted: profile, sampling, execution, retry-policy, compaction"
        ),
    }
}

fn parse_limit(parsed: &ParsedArgs) -> Result<usize> {
    parsed
        .one("limit")?
        .map(str::parse::<usize>)
        .transpose()
        .context("--limit must be a positive integer")
        .map(|limit| limit.unwrap_or(20))
}

fn extract_behavior_target(argv: &[String]) -> Result<(Option<String>, Vec<String>)> {
    let mut behavior_id = None;
    let mut rest = Vec::with_capacity(argv.len());
    let mut index = 0;
    while index < argv.len() {
        let arg = &argv[index];
        let value = if arg == "--behavior" {
            index += 1;
            Some(
                argv.get(index)
                    .filter(|value| !value.starts_with("--"))
                    .context("--behavior requires a BEHAVIOR_ID")?
                    .clone(),
            )
        } else {
            arg.strip_prefix("--behavior=").map(ToOwned::to_owned)
        };
        if let Some(value) = value {
            anyhow::ensure!(
                !value.trim().is_empty(),
                "--behavior requires a non-empty BEHAVIOR_ID"
            );
            anyhow::ensure!(
                behavior_id.replace(value).is_none(),
                "--behavior may be supplied once"
            );
        } else {
            rest.push(arg.clone());
        }
        index += 1;
    }
    Ok((behavior_id, rest))
}

/// `--allow-drop GROUP[,GROUP]`, repeatable: the Tools groups whose omitted
/// settings the caller means to drop.
fn extract_allow_drop(argv: &[String]) -> Result<(BTreeSet<String>, Vec<String>)> {
    let mut groups = BTreeSet::new();
    let mut rest = Vec::with_capacity(argv.len());
    let mut index = 0;
    while index < argv.len() {
        if argv[index] == "--allow-drop" {
            index += 1;
            let value = argv
                .get(index)
                .filter(|value| !value.starts_with("--"))
                .context("--allow-drop requires a Tools group name")?;
            let names: Vec<String> = if value.starts_with('[') {
                serde_json::from_str(value).context("allow-drop must name Tools groups")?
            } else {
                value
                    .split(',')
                    .map(|name| name.trim().to_owned())
                    .collect()
            };
            for name in names {
                anyhow::ensure!(
                    SelfConfigTarget::Tools
                        .writable_fields()
                        .contains(&name.as_str()),
                    "allow-drop names unknown Tools group {name:?}; see [\"help\",\"tools\"]"
                );
                groups.insert(name);
            }
        } else {
            rest.push(argv[index].clone());
        }
        index += 1;
    }
    Ok((groups, rest))
}

fn patch_contract(target: SelfConfigTarget, field_shapes: Value) -> Value {
    json!({
        "collection": target.collection_name(),
        "writable_fields": target.writable_fields(),
        "protected_fields": target.protected_fields(),
        "field_shapes": field_shapes,
        "contract_source": "Top-level fields come from canonical serde configuration metadata. Nested field inventories and advertised enum values are conformance-tested against the canonical Rust types; every preview/apply performs full typed decode and validation.",
        "semantics": "--set replaces one top-level field with the supplied JSON value; nested objects are complete typed values, not recursive merge patches. --clear removes an optional top-level field. Omitted fields are preserved.",
    })
}

pub(super) fn help_patch_contracts(resource: Option<&str>) -> Value {
    if let Some(name) = resource {
        let family = match name {
            "context" => Some("behavior"),
            "sampling" | "retry-policy" | "compaction" => Some("profile"),
            "task" | "trigger" | "schedule" | "event-source" => Some("automation"),
            _ => None,
        };
        if let Some(family) = family {
            let target = crud::resource_target(name).expect("resource metadata");
            return Value::Array(
                help_patch_contracts(Some(family))
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|contract| contract["collection"] == target.collection_name())
                    .cloned()
                    .collect(),
            );
        }
    }
    let contracts = match resource {
        Some("skill") => vec![patch_contract(
            SelfConfigTarget::Skill,
            json!({
                "name":"string|null; skill name",
                "description":"string|null; when to use this skill",
                "instructions":"string|null; Markdown instructions",
                "source_directory":"string|null; resolved local SKILL.md parent; supporting-file base, never an access grant",
                "tool_refs":"array<string>; tool dependencies, default []; never tool grants",
                "display_name":"string|null; display label",
                "interface_json":"string|null; serialized interface metadata",
                "enabled":"boolean; default true",
                "tags":"array<string>; default []"
            }),
        )],
        Some("datastore") => vec![patch_contract(
            SelfConfigTarget::DatastoreToolSurface,
            json!({
                "display_name":"string|null",
                "enabled":"boolean; default true",
                "entries":"array<SurfaceToolDecl>|null (reads use {entries:[...]} envelope). Create: {tool_name:string,collection:string,description?:string,fields:[{name:string,required?:boolean,fill?:correlation|{source_field:string}}],output_obligation?:{scope:request|trigger,minimum_writes?:positive integer,expected_count_field?:string}}. Query: {kind:query,tool_name:string,collection:string,description?:string,fields:[string],filter_fields?:[same field objects]}. fill fields are runtime-filled and never model arguments (what fill means: [\"help\",\"datastore\"]); other fields are model arguments. output_obligation is for workflows that must produce stored output, not a guarantee about one tool call. It gates request completion: request covers every request, including automated handlers; trigger covers automated work only. Requiring writes to a watched input collection can retrigger the handler. Omit for inbox writers and general-purpose tools. Surface writes validate declaration syntax. Selecting the surface in Tools checks tool-name collisions; runtime invocation validates the target collection/fields. Register schemas first, then bind and exercise the tools to establish readiness.",
                "tags":"array<string>; default []"
            }),
        )],
        Some("behavior") => vec![
            patch_contract(
                SelfConfigTarget::AgentBehavior,
                json!({
                    "display_name": "string|null",
                    "description": "string|null",
                    "context_id": "existing same-principal AgentContext ID|null",
                    "inference_profile_id": "existing same-principal InferenceProfile ID (required)",
                    "enabled": "boolean; default true",
                    "tags": "array<string>; default [] (UI/discovery labels only)",
                }),
            ),
            patch_contract(
                SelfConfigTarget::AgentContext,
                json!({
                    "display_name": "string|null",
                    "description": "string|null",
                    "system_prompt": "literal string|null; never template-evaluated",
                    "tools_id": "existing same-principal Tools ID|null; null grants no tools",
                    "compaction_id": "existing same-principal CompactionConfig ID|null; null uses runtime compaction defaults",
                    "skill_ids": "array<existing same-principal Skill ID>; default []",
                    "tags": "array<string>; default []",
                }),
            ),
        ],
        Some("tools") => vec![patch_contract(
            SelfConfigTarget::Tools,
            json!({
                "display_name": "string|null",
                "host": {"root":"string|null; absent uses runtime cwd", "files":{"mode":"Off|ReadOnly|ReadWrite (default Off)","max_read_chars":"integer 1..1000000|null; default 32000; read_file bytes, default and maximum","max_list_entries":"integer 1..5000|null; default 200","max_matches":"integer 1..5000|null; default 200; glob and grep"}, "bash":{"mode":"Off|ReadOnly|Unrestricted; default Off; ReadOnly still runs shell commands","execution_mode":"read_only|workspace_write|artifact_write|unrestricted|null","network_mode":"inherit|disabled|enabled|null","allowed_argv_prefixes":"array<array<string>>|null","forbidden_argv_prefixes":"array<array<string>>|null","read_only_commands":"array<string>|null","background_enabled":"boolean; default false","timeout_secs":"positive integer|null; default host --command-timeout-secs (120); clamped to the host maximum","max_timeout_secs":"positive integer|null; default timeout_secs if set, else host maximum; clamped to host --command-timeout-max-secs","background_timeout_secs":"positive integer|null; default 36000; clamped to 36000","wait_timeout_secs":"positive integer|null; default 30","max_wait_timeout_secs":"positive integer|null; default 600; clamped to 600","max_output_chars":"integer 1..1000000|null; default 16000; stdout and stderr each"}, "cli":[{"name":"string; host-registered CLI tool name (cli default [])","timeout_secs":"positive integer|null; default host registration (10); clamped to host --command-timeout-max-secs","max_output_chars":"integer 1..1000000|null; default host registration (16000); stdout and stderr each"}]},
                "remote": {"services":"array<{mcp_service_id:string,tool_names:array<string>,style:flat|discovery(default),required:boolean(default false),background_tool_names:array<string>,connect_timeout_secs?:integer,discovery_timeout_secs?:integer,timeout_secs?:integer,stale_timeout_secs?:integer,background_timeout_secs?:integer (default 36000; clamped to 36000),wait_timeout_secs?:integer (default 30),max_wait_timeout_secs?:integer (default 600; clamped to 600)}>; default []"},
                "subagents": {"target_ids":"array<existing same-principal SubagentTarget ID>; default []; the agents agent_new may address (create them with subagent-target)","enabled":"boolean|null; enables the agents tools: agent_message, agent_interrupt and agent_list, plus agent_new when target_ids selects a target"},
                "built_ins": {"enable_graph_tools":"boolean|null","enable_goal_tools":"boolean|null","enable_goal_creation":"boolean|null","enable_memory":"boolean|null","enable_session_history_tool":"boolean|null","enable_schema_tool":"boolean|null","enable_context_budget":"boolean|null"},
                "datastore": {"enable_defra_query":"boolean|null","defra_query_collections":"array<string>|null","datastore_tool_surface_ids":"array<existing same-principal DatastoreToolSurface ID>|null"},
                "integrations": {"lsp":{"config":"JSON encoded as a string|null","timeout_secs":"positive integer|null; default 20","max_timeout_secs":"positive integer|null; default 300; clamped to 300"},"eth_tool_ids":"array<existing same-principal EthTool ID>|null","plugins":"array<{plugin: installed namespace/name, digest: sha256:<hex>|null}>|null"},
                "self_config": {"enable_self_config":"boolean|null; absent is disabled","self_config_categories":"array<behavior|tools|profile|backend|mcp_service|automation|persona>|null; absent selects behavior, tools, profile; persona is the behavior catalog grant (every behavior, not only the current one)","self_config_no_lockout":"boolean|null","self_config_preview":"boolean|null; grants the preview verb","enable_pack_install":"boolean|null; cannot be self-granted"},
                "tags": "array<string>; default []",
            }),
        )],
        Some("profile") => vec![
            patch_contract(
                SelfConfigTarget::InferenceProfile,
                json!({
                    "display_name":"string|null","description":"string|null","backend_id":"existing same-principal backend ID","model_name":"string","reasoning_effort":"none|minimal|low|medium|high|xhigh|max|ultra|null","context_window":"positive integer <= the model's advertised maximum context window, when the backend advertises one|null","max_output_tokens":"positive integer|null","sampling_id":"existing same-principal sampling ID|null","execution_id":"existing same-principal execution ID|null","tags":"array<string>; default []"
                }),
            ),
            patch_contract(
                SelfConfigTarget::InferenceSampling,
                json!({
                    "display_name":"string|null","temperature":"number >= 0|null","top_p":"number 0..1|null","top_k":"positive integer|null","seed":"integer >= 0|null","min_p":"number 0..1|null","frequency_penalty":"number -2..2|null","presence_penalty":"number -2..2|null","repetition_penalty":"number > 0|null","tags":"array<string>; default []"
                }),
            ),
            patch_contract(
                SelfConfigTarget::InferenceExecution,
                json!({
                    "display_name":"string|null","max_turns":format!("positive integer|null; default {}", crate::config::DEFAULT_MAX_TURNS),"max_total_tokens":"positive integer|null; null is unlimited","stream_batch_ms":"positive integer|null; persistence batching interval; default 1000 ms","stream_liveness_timeout_secs":format!("positive integer|null; renewed execution lease, independent of provider output; default {} seconds and less than deadline", crate::config::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS),"provider_idle_timeout_secs":format!("positive integer|null; maximum provider transport silence, not total request duration; default {} seconds", crate::config::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS),"deadline_duration_secs":format!("positive integer|null; total wall time for one request across model/tool turns, not the whole session; default 86400; at most {}", crate::document_config::MAX_DEADLINE_DURATION_SECS),"retry_policy_id":"existing same-principal retry policy ID|null; null uses request-origin retry defaults","tags":"array<string>; default []"
                }),
            ),
            patch_contract(
                SelfConfigTarget::InferenceRetryPolicy,
                json!({
                    "display_name":"string|null","max_transport_retries":"integer >= 0|null","backoff_ms":"array<positive integer>|null","max_resample_retries":"integer >= 0|null","allow_repair":"boolean|null","interactive_max_retries":"integer >= 0|null","tags":"array<string>; default []"
                }),
            ),
            patch_contract(
                SelfConfigTarget::Compaction,
                json!({
                    "display_name":"string|null","strategy":"StripToolResults|StripThenSummarize|null; absent uses StripThenSummarize","threshold":"number 0..1|null; default 0.75","keep_recent_tokens":"non-negative integer|null","tool_result_max_chars":"positive integer|null","summary_max_output_tokens":"positive integer|null","summary_file_list_max":"non-negative integer|null","inference_profile_id":"existing same-principal profile ID|null","tags":"array<string>; default []"
                }),
            ),
        ],
        Some("backend") => vec![patch_contract(
            SelfConfigTarget::InferenceBackend,
            json!({
                "name":"string","provider_kind":"OpenAiCompatible|OpenRouter|ChatGptCodex|XaiGrokOAuth|ClaudeCliSubscription","openai_wire_api":"chat_completions|responses|null","endpoint":"URL string","auth":"{kind:unauthenticated}|{kind:environment,variable:string}|{kind:principal_oauth}; raw api_key values are operator-managed","connect_timeout_secs":"positive integer|null; default 10","discovery_timeout_secs":"positive integer|null; default 10","max_concurrent":"positive integer|null; default 1","max_queue_depth":"integer >= 0|null; default 100","enabled":"boolean; default true","tags":"array<string>; default []"
            }),
        )],
        Some("mcp-service") => vec![patch_contract(
            SelfConfigTarget::ToolServiceRegistry,
            json!({
                "display_name":"string|null","description":"string|null","hostname":"string|null","tailscale_ip":"string|null","lan_ip":"string|null","mcp_port":"integer|null","mcp_path":"string|null; absent uses endpoint root","send_agent_did":"boolean; default false","enabled":"boolean; default true","tags":"array<string>; default []"
            }),
        )],
        Some("automation") => vec![
            patch_contract(
                SelfConfigTarget::Task,
                json!({
                    "display_name":"string|null","description":"string|null","prompt_template":"string; rendered per invocation; state what to return or write and where. Writing to the watched collection triggers it again","emit_outcome":"boolean; true writes the standard per-input success/failure record (FireOutcome) at request or Goal termination. Default false writes no completion record; leave false for outcome consumers","goal_objective_template":"string|null; starts a durable goal that continues until update_goal completes it; requires built_ins.enable_goal_tools on the behavior. Omit for ordinary one-request tasks","goal_token_budget":"positive integer|null; requires goal_objective_template; absent means unlimited","hooks":"array<{hook_id:string,phase:before|after_success|after_failure|finally,command:nonempty array<string>,timeout_secs?:positive integer}>; default []","enabled":"boolean; default true","output_schema_ref":"string|null","tags":"array<string>; default []"
                }),
            ),
            patch_contract(
                SelfConfigTarget::Schedule,
                json!({
                    "display_name":"string|null","cadence":"{kind:interval,interval_secs:positive integer}|{kind:cron,expression:string,timezone:string,missed_run_policy?:latest_only}","tags":"array<string>; default []"
                }),
            ),
            patch_contract(
                SelfConfigTarget::Trigger,
                json!({
                    "display_name":"string|null","description":"string|null","task_id":"existing task ID owned by selected behavior","source":"{kind:schedule,schedule_id:string}|{kind:event,event_source_id:string}","enabled":"boolean; default true","concurrency":"parallel|queued_serial|serial|latest_only|null; default parallel; controls overlap, not session destination; queued_serial needs an event source","session_id_template":"string|null; template rendering an existing session ID; event sources only. Null creates a session per fire. Independent of concurrency","tags":"array<string>; default []"
                }),
            ),
            patch_contract(
                SelfConfigTarget::EventSource,
                json!({
                    "display_name":"string|null","source_collection":"installed collection name","event_kind":"created|null; default created","filter":"string|null; a GraphQL filter object literal with unquoted keys","correlation_field":"GraphQL field name|null","group":"{expected_count?:positive integer|{source_field:string},timeout_secs?:positive integer,min_count?:positive integer}|null","workspace_authority":"canonical workspace authority object|null","tags":"array<string>; default []"
                }),
            ),
        ],
        Some("subagent-target") => vec![patch_contract(
            SelfConfigTarget::SubagentTarget,
            json!({
                "name":"string; the agent name agent_new exposes","target_agent_did":"principal DID; omit on create for a local helper (authenticated principal). Set explicitly for remote targets. Omitted on update preserves the destination; never infer identity from a document ID","behavior_id":"behavior ID on target_agent_did; must exist when local","description":"string|null","tags":"array<string>; default []"
            }),
        )],
        Some("execution") => help_patch_contracts(Some("profile"))
            .as_array()
            .into_iter()
            .flatten()
            .filter(|contract| contract["collection"] == "InferenceExecution")
            .cloned()
            .collect(),
        _ => Vec::new(),
    };
    Value::Array(contracts)
}

/// The advertised field shape at a decode error path (`host.cli[0].name`), so
/// a type error names what the field takes instead of an internal Rust type.
pub(super) fn field_shape(collection: &str, path: &str) -> Option<Value> {
    let contract = [
        "behavior",
        "tools",
        "datastore",
        "skill",
        "profile",
        "backend",
        "mcp-service",
        "automation",
        "subagent-target",
    ]
    .into_iter()
    .flat_map(|resource| match help_patch_contracts(Some(resource)) {
        Value::Array(contracts) => contracts,
        _ => Vec::new(),
    })
    .find(|contract| contract["collection"] == collection)?;
    let mut shape = &contract["field_shapes"];
    let path = path.replace('[', ".[");
    for segment in path.split('.').filter(|segment| !segment.is_empty()) {
        shape = if segment.starts_with('[') {
            match shape {
                Value::Array(items) => items.first()?,
                other => other,
            }
        } else {
            shape.get(segment)?
        };
    }
    Some(shape.clone())
}

fn parse_pack_change(argv: &[String]) -> Result<PackInstallParams> {
    let package = argv
        .first()
        .context("pack operation requires PACKAGE")?
        .clone();
    let parsed = ParsedArgs::parse(&argv[1..])?;
    anyhow::ensure!(
        parsed.positionals.is_empty() && parsed.switches.is_empty(),
        "unexpected pack argument; see [\"help\",\"pack\"]"
    );
    for name in parsed.options.keys() {
        anyhow::ensure!(
            matches!(name.as_str(), "var" | "inference-slot" | "digest"),
            "unknown pack option --{name}; accepted: --inference-slot, --var, --digest"
        );
    }
    let pairs = |name: &str| -> Result<BTreeMap<String, String>> {
        let mut values = BTreeMap::new();
        for binding in parsed.options.get(name).into_iter().flatten() {
            let (key, value) = binding
                .split_once('=')
                .with_context(|| format!("--{name} must be NAME=VALUE"))?;
            anyhow::ensure!(
                !key.is_empty() && !value.is_empty(),
                "--{name} requires non-empty NAME and VALUE"
            );
            anyhow::ensure!(
                values.insert(key.to_owned(), value.to_owned()).is_none(),
                "duplicate --{name} name {key:?}"
            );
        }
        Ok(values)
    };
    Ok(PackInstallParams {
        package,
        variables: pairs("var")?,
        inference_slots: pairs("inference-slot")?,
        expected_digest: parsed.one("digest")?.map(ToOwned::to_owned),
    })
}

fn parse_patch(argv: &[String], target: SelfConfigTarget) -> Result<SelfConfigPatch> {
    parse_patch_args(ParsedArgs::parse(argv)?, target)
}

fn parse_patch_args(parsed: ParsedArgs, target: SelfConfigTarget) -> Result<SelfConfigPatch> {
    anyhow::ensure!(
        parsed.positionals.is_empty(),
        "unexpected positional argument {:?}; use top-level set for fields or clear for optional field names",
        parsed.positionals[0]
    );
    anyhow::ensure!(
        parsed.switches.is_empty(),
        "unexpected switch; use top-level set for fields or clear for optional field names"
    );
    for name in parsed.options.keys() {
        anyhow::ensure!(
            matches!(name.as_str(), "set" | "clear"),
            "unknown patch option --{name}; put fields in top-level set or clear, not options"
        );
    }
    let mut seen = BTreeSet::new();
    let mut patch = Vec::new();
    for assignment in parsed.options.get("set").into_iter().flatten() {
        let (field, raw) = assignment
            .split_once('=')
            .context("--set must be FIELD=JSON; strings need JSON quotes")?;
        anyhow::ensure!(!field.is_empty(), "--set field must not be empty");
        anyhow::ensure!(
            seen.insert(field.to_owned()),
            "field {field:?} was supplied more than once"
        );
        let value = serde_json::from_str(raw).with_context(|| {
            format!("invalid JSON value for field {field:?}; strings need JSON quotes")
        })?;
        patch.push((field.to_owned(), Some(value)));
    }
    for field in parsed.options.get("clear").into_iter().flatten() {
        anyhow::ensure!(
            seen.insert(field.to_owned()),
            "field {field:?} was supplied more than once"
        );
        patch.push((field.to_owned(), None));
    }
    anyhow::ensure!(
        !patch.is_empty(),
        "empty patch; use top-level set for fields or clear for optional field names"
    );
    crate::config_client::patch::ensure_admissible(target, &patch)?;
    Ok(patch)
}

#[derive(Default)]
struct ParsedArgs {
    positionals: Vec<String>,
    options: BTreeMap<String, Vec<String>>,
    switches: BTreeSet<String>,
}

impl ParsedArgs {
    fn parse(argv: &[String]) -> Result<Self> {
        let mut parsed = Self::default();
        let mut index = 0;
        while index < argv.len() {
            let arg = &argv[index];
            if let Some(raw) = arg.strip_prefix("--") {
                if raw.is_empty() {
                    bail!("empty option name");
                }
                if raw == "default" {
                    anyhow::ensure!(parsed.switches.insert(raw.into()), "duplicate --default");
                    index += 1;
                    continue;
                }
                let (name, inline) = raw
                    .split_once('=')
                    .map_or((raw, None), |(name, value)| (name, Some(value)));
                let value = match inline {
                    Some(value) => value.to_owned(),
                    None => {
                        index += 1;
                        argv.get(index)
                            .filter(|value| !value.starts_with("--"))
                            .cloned()
                            .with_context(|| format!("--{name} requires a value"))?
                    }
                };
                parsed
                    .options
                    .entry(name.to_owned())
                    .or_default()
                    .push(value);
            } else {
                parsed.positionals.push(arg.clone());
            }
            index += 1;
        }
        Ok(parsed)
    }

    fn one(&self, name: &str) -> Result<Option<&str>> {
        let Some(values) = self.options.get(name) else {
            return Ok(None);
        };
        anyhow::ensure!(values.len() == 1, "--{name} may be supplied once");
        Ok(values.first().map(String::as_str))
    }

    fn reject_mutation_flags(&self) -> Result<()> {
        anyhow::ensure!(
            self.positionals.is_empty(),
            "list command accepts no positional arguments"
        );
        anyhow::ensure!(
            self.switches.is_empty(),
            "read command received a write-only switch"
        );
        for name in self.options.keys() {
            anyhow::ensure!(
                matches!(name.as_str(), "limit" | "cursor"),
                "unknown read option --{name}"
            );
        }
        Ok(())
    }
}

pub(super) fn behavior_params(
    action: &str,
    operation: Option<String>,
    argv: &[String],
) -> Result<ConfigurePersonaParams> {
    let parsed = ParsedArgs::parse(argv)?;
    let allowed = [
        "id",
        "from",
        "display-name",
        "description",
        "system-prompt",
        "root",
        "preset",
        "profile",
        "clear",
    ];
    let verb = operation.as_deref().unwrap_or(action);
    let usage = match verb {
        "create" => "behavior create takes options.display-name, system-prompt, preset and profile; it allocates the ID and Context/Tools. Use behavior update for an existing role",
        "clone" => "behavior clone takes options.from and display-name, with optional profile, system-prompt, root and preset overrides; it allocates a new ID",
        "disable" => "behavior disable takes options.id for the existing behavior",
        _ => "behavior options use hyphenated names; see the command help",
    };
    for option in parsed.options.keys() {
        if !allowed.contains(&option.as_str()) {
            return Err(CommandGuidance {
                message: format!("unknown behavior option --{option}. {usage}; do not supply set fields to this command"),
                next_call: json!({"argv":["behavior",verb,"--help"]}),
            }
            .into());
        }
    }
    if let Some(argument) = parsed.positionals.first() {
        return Err(CommandGuidance {
            message: format!("unexpected positional argument {argument:?}. {usage}; do not supply a positional ID"),
            next_call: json!({"argv":["behavior",verb,"--help"]}),
        }
        .into());
    }
    let clear: BTreeSet<&str> = parsed
        .options
        .get("clear")
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();
    for field in &clear {
        anyhow::ensure!(
            matches!(
                *field,
                "display_name" | "description" | "system_prompt" | "root"
            ),
            "field {field:?} cannot be cleared; accepted: display_name, description, system_prompt, root"
        );
    }
    let update = |option: &str, field: &str| -> Result<StringUpdate> {
        let value = parsed.one(option)?;
        anyhow::ensure!(
            !(value.is_some() && clear.contains(field)),
            "{field} cannot be both set and cleared"
        );
        Ok(match value {
            Some(value) => StringUpdate::Set(value.to_owned()),
            None if clear.contains(field) => StringUpdate::Clear,
            None => StringUpdate::Omitted,
        })
    };
    Ok(ConfigurePersonaParams {
        action: action.to_owned(),
        operation,
        display_name: update("display-name", "display_name")?,
        description: update("description", "description")?,
        system_prompt: update("system-prompt", "system_prompt")?,
        behavior_id: parsed.one("id")?.map(ToOwned::to_owned),
        clone_from: parsed.one("from")?.map(ToOwned::to_owned),
        root: update("root", "root")?,
        preset: update("preset", "preset")?,
        profile_id: update("profile", "profile_id")?,
        make_default: parsed.switches.contains("default"),
        enable_lsp: None,
        enable_graph_tools: None,
        network_mode: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_aliases_are_recognized_before_operands_but_not_in_values() {
        for argv in [vec!["--help"], vec!["-h"], vec!["help"]] {
            let argv = argv.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert_eq!(config_help_resource(&argv), Some(None));
        }
        for argv in [
            vec!["help", "datastore"],
            vec!["datastore", "--help"],
            vec!["datastore", "create", "-h"],
            vec!["datastore", "preview", "create", "--help"],
            vec!["datastore", "help", "create"],
            vec![
                "datastore",
                "create",
                "surface",
                "--set",
                "display_name=\"Test\"",
                "--help",
            ],
        ] {
            let argv = argv.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert_eq!(config_help_resource(&argv), Some(Some("datastore")));
        }
        for argv in [
            vec!["behavior", "create", "--system-prompt", "--help"],
            vec!["behavior", "create", "--description", "help"],
            vec!["datastore", "get", "help"],
        ] {
            let argv = argv.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert_eq!(config_help_resource(&argv), None);
        }
    }

    #[test]
    fn required_ids_never_consume_patch_flags() {
        for id in [None, Some(""), Some(" "), Some("--set"), Some("-h")] {
            let id = id.map(str::to_owned);
            assert!(required_resource_id(id.as_ref(), "SURFACE_ID")
                .unwrap_err()
                .to_string()
                .contains("missing SURFACE_ID"));
        }
        let id = "monitor-notifications".to_owned();
        assert_eq!(required_resource_id(Some(&id), "SURFACE_ID").unwrap(), &id);
    }

    #[test]
    fn named_target_uses_the_existing_positional_command_path() {
        for (argv, expected) in [
            (
                json!(["datastore", "preview", "create"]),
                json!(["datastore", "preview", "create", "surface"]),
            ),
            (
                json!(["automation", "preview", "task"]),
                json!(["automation", "preview", "task", "surface"]),
            ),
            (
                json!(["mcp-service", "get"]),
                json!(["mcp-service", "get", "surface"]),
            ),
        ] {
            let params: ConfigCommandParams =
                serde_json::from_value(json!({"argv":argv,"target_id":"surface"})).unwrap();
            assert_eq!(json!(params.into_argv().unwrap()), expected);
        }
        for input in [
            json!({"argv":["datastore","create","one"],"target_id":"two"}),
            json!({"argv":["datastore","create"],"target_id":"--set"}),
            json!({"argv":["datastore","--help"],"target_id":"one"}),
            json!({"argv":["behavior","create"],"target_id":"one"}),
        ] {
            assert!(serde_json::from_value::<ConfigCommandParams>(input)
                .unwrap()
                .into_argv()
                .is_err());
        }
    }

    #[test]
    fn structured_config_preserves_literal_values_and_uses_canonical_patch_validation() {
        let prompt = "A \"quoted\" prompt\nActual newline; literal \\n; {{ doc.message }}; λ";
        let params: ConfigCommandParams = serde_json::from_value(json!({
            "argv":["behavior","context","preview"],
            "options":{"behavior":"working"},
            "set":{"system_prompt":prompt,"skill_ids":["one","two"]},
            "clear":["description"]
        }))
        .unwrap();
        let argv = params.into_argv().unwrap();
        let (behavior, rest) = extract_behavior_target(&argv[3..]).unwrap();
        assert_eq!(behavior.as_deref(), Some("working"));
        let patch = parse_patch(&rest, SelfConfigTarget::AgentContext).unwrap();
        assert!(patch.contains(&("system_prompt".into(), Some(json!(prompt)))));
        assert!(patch.contains(&("skill_ids".into(), Some(json!(["one", "two"])))));
        assert!(patch.contains(&("description".into(), None)));
        for input in [
            json!({"argv":[],"set":{"description":"x"}}),
            json!({"argv":["get","--behavior","one"],"options":{"behavior":"two"}}),
            json!({"argv":["get"],"options":{"--behavior":"two"}}),
            json!({"argv":["get"],"set":{"description=enabled":"x"}}),
        ] {
            assert!(serde_json::from_value::<ConfigCommandParams>(input)
                .unwrap()
                .into_argv()
                .is_err());
        }
    }

    #[test]
    fn native_behavior_create_uses_options_and_never_a_positional_id_or_patch() {
        let params: ConfigCommandParams = serde_json::from_value(json!({
            "argv": ["behavior", "create"],
            "options": {
                "id": "monitor",
                "display-name": "Monitor",
                "system-prompt": "Observe only",
                "preset": "readonly",
                "profile": "default-profile"
            }
        }))
        .unwrap();
        let argv = params.into_argv().unwrap();
        let created = behavior_params("create", None, &argv[2..]).unwrap();
        assert_eq!(created.behavior_id.as_deref(), Some("monitor"));
        assert_eq!(created.display_name, StringUpdate::Set("Monitor".into()));

        let invalid: ConfigCommandParams = serde_json::from_value(json!({
            "argv": ["behavior", "create", "monitor"],
            "set": {"display_name": "Monitor"}
        }))
        .unwrap();
        assert!(behavior_params("create", None, &invalid.into_argv().unwrap()[2..]).is_err());
    }

    #[test]
    fn native_datastore_create_adapts_mailbox_options_and_set_to_argv() {
        let params: ConfigCommandParams = serde_json::from_value(json!({
            "argv": ["datastore", "preview", "create"],
            "target_id": "monitor-mailbox",
            "options": {"mailbox": {"identity": {"mode": "event"}, "kind": "flag", "action": "ack"}},
            "set": {"enabled": true}
        }))
        .unwrap();
        let argv = params.into_argv().unwrap();
        assert_eq!(
            argv[..5],
            [
                "datastore",
                "preview",
                "create",
                "monitor-mailbox",
                "--mailbox"
            ]
        );
        assert_eq!(
            serde_json::from_str::<Value>(&argv[5]).unwrap(),
            json!({"identity": {"mode": "event"}, "kind": "flag", "action": "ack"})
        );
        assert_eq!(&argv[6..], ["--set", "enabled=true"]);
    }

    #[test]
    fn structured_config_rejects_conflicts_and_protected_fields() {
        for input in [
            json!({"argv":["--set","system_prompt=\"old\""],"set":{"system_prompt":"new"}}),
            json!({"argv":["--clear","system_prompt"],"set":{"system_prompt":"new"}}),
            json!({"argv":["--set","description=null"],"set":{"agent_did":"foreign"}}),
        ] {
            let args = serde_json::from_value::<ConfigCommandParams>(input)
                .unwrap()
                .into_argv()
                .unwrap();
            assert!(parse_patch(&args, SelfConfigTarget::AgentContext).is_err());
        }
        let args = serde_json::from_value::<ConfigCommandParams>(json!({
            "argv":["behavior","preview","create"],
            "options":{"display-name":"Writer", "system-prompt":"First\nSecond", "root":"/tmp/work"}
        }))
        .unwrap()
        .into_argv()
        .unwrap();
        let params = behavior_params("preview", Some("create".into()), &args[3..]).unwrap();
        assert_eq!(
            params.system_prompt,
            StringUpdate::Set("First\nSecond".into())
        );
    }

    #[test]
    fn edit_argv_preserves_omitted_fields_and_marks_clear() {
        let params = behavior_params(
            "edit",
            None,
            &[
                "--id".into(),
                "review".into(),
                "--display-name".into(),
                "Review".into(),
                "--clear".into(),
                "root".into(),
            ],
        )
        .unwrap();
        assert_eq!(params.display_name, StringUpdate::Set("Review".into()));
        assert_eq!(params.root, StringUpdate::Clear);
        assert_eq!(params.profile_id, StringUpdate::Omitted);
        assert_eq!(persona_edit_fields(&params), vec!["display_name", "root"]);
    }

    #[test]
    fn invalid_behavior_argv_names_the_option_and_help() {
        let error =
            behavior_params("edit", None, &["--persona-name".into(), "Review".into()]).unwrap_err();
        assert!(error.to_string().contains("--persona-name"));
        assert_eq!(
            error.downcast_ref::<CommandGuidance>().unwrap().next_call,
            json!({"argv":["behavior","edit","--help"]})
        );
    }
}
