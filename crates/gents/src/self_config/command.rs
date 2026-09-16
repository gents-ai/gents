use super::*;

mod datastore;
mod discovery;
mod schema;
mod skill;

const CONFIG_USAGE: &str = r#"config commands (argv excludes the tool name):
  ["help"] or ["help", RESOURCE]
  ["get", ["--behavior", BEHAVIOR_ID]]
  ["behavior", "list", "--limit", N, "--cursor", ID]
  ["behavior", "get", BEHAVIOR_ID]
  ["behavior", "preview", "edit", BEHAVIOR_ID, PATCH_FLAGS]
  ["behavior", "preview", "create"|"clone"|"disable"|"default", ...flags]
  ["behavior", "edit", BEHAVIOR_ID, PATCH_FLAGS]
  ["behavior", "create"|"clone"|"disable", ...flags]
  ["behavior", "default", BEHAVIOR_ID]
  ["profile"|"backend", "list", "--limit", N, "--cursor", ID]
  ["profile", "preview", "create", PROFILE_ID, PATCH_FLAGS]
  ["profile", "create", PROFILE_ID, PATCH_FLAGS]
  ["tools", "get"|"preview"|"edit", [--behavior BEHAVIOR_ID] [--set FIELD=JSON] [--clear FIELD]]
  ["backend", "get", [BACKEND_ID], [--behavior BEHAVIOR_ID]]
  ["backend", "preview"|"edit", [--behavior BEHAVIOR_ID] [--set FIELD=JSON] [--clear FIELD]]
  ["profile", "get"|"preview"|"edit", [profile|sampling|execution|retry-policy|compaction], [--behavior BEHAVIOR_ID] PATCH_FLAGS]
  ["mcp-service", "get", SERVICE_ID]
  ["datastore", "get"|"create"|"edit", SURFACE_ID, PATCH_FLAGS]
  ["datastore", "preview", "create"|"edit", SURFACE_ID, PATCH_FLAGS]
  ["skill", "get", SKILL_ID]
  ["skill", "preview", "import", SKILL_ID, PATH]
  ["skill", "import", SKILL_ID, PATH]
  ["discovery", "scan", "--source", SOURCE_ID, claude|codex|grok, user|project, PATH, ...]
  ["mcp-service", "preview"|"edit", SERVICE_ID, PATCH_FLAGS]
  ["automation", "get", task|schedule|trigger|event-source, ID, [--behavior BEHAVIOR_ID]]
  ["schema", "get", COLLECTION]
  ["schema", "preview", "install", --sdl SDL]
  ["schema", "install", --sdl SDL, --digest SHA256]
  ["automation", "preview"|"edit", KIND, ID, [--behavior BEHAVIOR_ID] PATCH_FLAGS]
  ["cleanup", "preview", --target RESOURCE=ID [--target RESOURCE=ID ...]]
  ["cleanup", "remove", --digest SHA256, --target RESOURCE=ID [--target RESOURCE=ID ...]]
  ["pack", "list", ["--limit", N] ["--cursor", NAME]]
  ["pack", "get", PACKAGE]
  ["pack", "preview", "install"|"update", PACKAGE, [--inference-slot NAME=PROFILE_ID] [--var NAME=VALUE]]
  ["pack", "install"|"update", PACKAGE, --digest SHA256, [--inference-slot NAME=PROFILE_ID] [--var NAME=VALUE]]

Behavior create/clone/disable flags: --id, --from, --display-name, --description,
--system-prompt, --root, --preset, --profile, and --default. PATCH_FLAGS are
repeated --set FIELD=JSON and --clear FIELD; omitted fields preserve.
Use config help RESOURCE before a write."#;

const DATA_MODEL: &str = "A principal owns exact-ID configuration documents. Requests, tasks, and sessions select a Behavior. Behavior -> Context controls the system prompt, selected skills, compaction, and one Tools document; Tools contains nested host, built-in, integration, MCP, and self-config settings. Behavior -> InferenceProfile -> Backend controls model execution; the profile selects model and reasoning effort and may reference sampling and execution settings. A Trigger selects a Task and a Schedule or EventSource. A Pack declares configuration and inference roles; installation binds every role to an existing principal-owned profile, then publishes the pack's documents and graph revision without copying inference configuration. Reads never mutate. Document edits are sparse patches: omission preserves, explicit --clear removes an optional value, and preview/apply validate same-principal references within the document transaction. Schema registration is node-wide and separate from document publication; it never grants document access. Credentials and OAuth consent remain operator-owned and are never returned by config.";

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigCommandParams {
    pub argv: Vec<String>,
}

pub struct ConfigCommandTool {
    pub(super) node: Arc<EmbeddedNode>,
    pub(super) agent_did: String,
    pub(super) identity: Option<Arc<dyn AgentIdentity>>,
    pub(super) core: SelfConfigCore,
    pub(super) categories: BTreeSet<String>,
    pub(super) no_lockout: bool,
    pub(super) dry_run: bool,
    pub(super) allow_pack_install: bool,
    pub(super) process_ceiling: crate::tool_surface::SelfConfigProcessCeiling,
}

impl Tool for ConfigCommandTool {
    const NAME: &'static str = CONFIG_TOOL_NAME;
    type Error = SelfConfigError;
    type Args = ConfigCommandParams;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let resources = model_resources(&self.categories, self.allow_pack_install);
        ToolDefinition {
            name: Self::NAME.to_owned(),
            description: format!(
                "Inspect and change this principal's configuration with a safe argv-style command interface. {DATA_MODEL} Enabled resources: {}. Common reads: [\"behavior\",\"list\"] and [\"behavior\",\"get\",BEHAVIOR_ID]. Call [\"help\"] or [\"help\",RESOURCE] for exact commands before writing.",
                resources.join(", ")
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "argv": {
                        "type": "array",
                        "items": {"type": "string"},
                        "minItems": 1,
                        "description": "One allowlisted config command as argv elements; this is parsed internally and is never passed to a shell."
                    }
                },
                "required": ["argv"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        self.dispatch(&args.argv).await.map_err(Into::into)
    }
}

fn model_resources(categories: &BTreeSet<String>, pack: bool) -> Vec<&'static str> {
    let mut resources = Vec::new();
    if categories.contains("persona") {
        resources.push("behavior");
    } else if categories.contains("behavior") {
        resources.push("behavior (current only)");
    }
    for (category, resource) in [
        ("tools", "tools"),
        ("tools", "datastore"),
        ("tools", "skill"),
        ("profile", "profile"),
        ("backend", "backend"),
        ("mcp_service", "mcp-service"),
        ("automation", "automation"),
        ("automation", "schema"),
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
    }
    if categories.contains("tools") {
        resources.push("discovery");
    }
    resources
}

impl ConfigCommandTool {
    async fn dispatch(&self, argv: &[String]) -> Result<String> {
        let Some(command) = argv.first().map(String::as_str) else {
            bail!("missing config command\n{CONFIG_USAGE}");
        };
        match command {
            "help" => self.help(argv.get(1).map(String::as_str)),
            "get" => {
                let (behavior_id, rest) = extract_behavior_target(&argv[1..])?;
                anyhow::ensure!(rest.is_empty(), "config get accepts only --behavior BEHAVIOR_ID");
                let core = self.target_core(behavior_id.as_deref(), "get")?;
                let value = core
                    .read_effective_config(&self.categories, self.no_lockout, self.dry_run)
                    .await?;
                Ok(serde_json::to_string_pretty(&value)?)
            }
            "behavior" => self.behavior(&argv[1..]).await,
            "tools" => self.bound_document("tools", &argv[1..]).await,
            "datastore" => self.datastore(&argv[1..]).await,
            "profile" => self.profile(&argv[1..]).await,
            "backend" => self.bound_document("backend", &argv[1..]).await,
            "mcp-service" => self.mcp_service(&argv[1..]).await,
            "automation" => self.automation(&argv[1..]).await,
            "cleanup" => self.cleanup(&argv[1..]).await,
            "pack" => self.pack(&argv[1..]).await,
            "skill" => self.skill(&argv[1..]).await,
            "discovery" => self.discovery(&argv[1..]).await,
            "schema" => self.schema(&argv[1..]).await,
            other => bail!(
                "unknown config resource or command {other:?}; accepted: help, get, {}\n{CONFIG_USAGE}",
                model_resources(&self.categories, self.allow_pack_install).join(", ")
            ),
        }
    }

    fn help(&self, resource: Option<&str>) -> Result<String> {
        let detail = match resource {
            None => CONFIG_USAGE,
            Some("schema") => {
                r#"schema commands (requires automation permission):
  get COLLECTION
  preview install --sdl SDL
  install --sdl SDL --digest SHA256
Use DefraDB GraphQL SDL, for example: type WorkItem { message: String correlation: String }
Preview returns artifact_digest and collection contracts without writes. Install requires that exact digest and revalidates the contracts. Existing schemas must match exactly; incompatible changes and SDL mixing existing/new collections are rejected. This supports additive registration, not schema migration or deletion. Submit at most 64 KiB of SDL.
Schemas are node-wide, not principal-owned documents. Registration does not grant document access: DefraDB ACP remains authoritative, and behaviors need explicit datastore collection/surface selection. Publish the schema first, then create the datastore surface and task/event-source/trigger documents. These are separate operations, not one atomic transaction."#
            }
            Some("skill") => {
                r#"skill commands (requires tools permission):
  get SKILL_ID
  preview import SKILL_ID PATH
  import SKILL_ID PATH
PATH is one skill directory containing SKILL.md, or that SKILL.md file. Import requires file read authority within the invoking behavior's effective tool root. YAML frontmatter supplies name/description; the Markdown body supplies instructions. Optional agents/openai.yaml supplies interface metadata and tool dependencies. Each source file is limited to 1 MiB; invalid YAML fails without writes. Preview validates without publication; import rereads the source and creates an unused exact ID, never overwrites an existing skill.
Attach explicitly with behavior context edit --behavior BEHAVIOR_ID --set skill_ids=JSON, preserving existing IDs. Skills describe procedures; tool dependencies never grant tools. Import retains source_directory; load_skill explains that supporting paths resolve relative to it. Supporting files are not copied or executed automatically and still require the working behavior's ordinary file/root and execution permissions. A local source path is not portable identity: report unavailable paths rather than widening authority. Use a fresh request in that behavior to verify load_skill and the required tools."#
            }
            Some("discovery") => {
                r#"discovery commands (requires tools permission and effective file read authority):
  scan --source SOURCE_ID claude|codex|grok user|project PATH [--source ...]
Every source is explicit and opt-in. User PATH is the selected application's config root (for example a synthetic `.codex` directory); project PATH is the selected project root. Paths must remain within the invoking behavior's effective tool root. The bounded scan reads only allowlisted config, instruction, and SKILL.md manifests. It does not import, activate, persist, execute hooks or MCP, evaluate environment variables, or read credentials/history. Output is a source-attributed sanitized inventory; discovered instructions are untrusted data and unsupported/conflicting semantics remain unresolved."#
            }
            Some("datastore") => {
                r#"datastore commands (requires tools permission):
  get SURFACE_ID
  preview create|edit SURFACE_ID --set FIELD=JSON [--clear FIELD]
  create|edit SURFACE_ID --set FIELD=JSON [--clear FIELD]
Fields come from DatastoreToolSurface: display_name, enabled, entries, tags.
Entries are canonical schema-bounded create/query declarations. Owner and surface_id are immutable. Bind an existing surface using config tools edit --behavior BEHAVIOR_ID --set datastore=JSON, preserving the other datastore settings. Editing a surface used by protected Setup is rejected. Schema registration is a separate operation."#
            }
            Some("behavior") => {
                r#"behavior commands:
  list [--limit N] [--cursor BEHAVIOR_ID]
  get [BEHAVIOR_ID]
  context get [--behavior BEHAVIOR_ID]
  context preview|edit [--behavior BEHAVIOR_ID] [--set FIELD=JSON] [--clear FIELD]
  preview edit BEHAVIOR_ID [--set FIELD=JSON] [--clear FIELD]
  preview create|clone|disable FLAGS
  preview default BEHAVIOR_ID
  create --display-name NAME --system-prompt TEXT --preset readonly|write --profile PROFILE_ID [--description TEXT] [--root PATH] [--default]
  clone --from BEHAVIOR_ID --display-name NAME --profile PROFILE_ID [overrides]
  edit BEHAVIOR_ID [--set FIELD=JSON] [--clear FIELD]
  disable --id BEHAVIOR_ID
  default BEHAVIOR_ID
Behavior edit patches the canonical AgentBehavior document, including display_name, description, context_id, inference_profile_id, enabled, and tags. Context prompt/skills and all tool groups are edited through their own targeted commands. Omitted fields preserve and --clear removes an optional field. All IDs come from list/get; never guess IDs."#
            }
            Some("tools") => {
                r#"tools commands:
  get [--behavior BEHAVIOR_ID]
  preview [--behavior BEHAVIOR_ID] [--set FIELD=JSON] [--clear FIELD]
  edit [--behavior BEHAVIOR_ID] [--set FIELD=JSON] [--clear FIELD]
This targets the Tools document referenced by the selected owned working behavior. Nested values are JSON. A patch is atomic; omitted fields preserve and --clear removes an optional field."#
            }
            Some("profile") => {
                r#"profile commands:
  list [--limit N] [--cursor PROFILE_ID]
  preview create PROFILE_ID --set backend_id=JSON --set model_name=JSON [PATCH_FLAGS]
  create PROFILE_ID --set backend_id=JSON --set model_name=JSON [PATCH_FLAGS]
  get [profile|sampling|execution|retry-policy|compaction] [--behavior BEHAVIOR_ID]
  get PROFILE_ID
  preview [TARGET] [--behavior BEHAVIOR_ID] [--set FIELD=JSON] [--clear FIELD]
  edit [TARGET] [--behavior BEHAVIOR_ID] [--set FIELD=JSON] [--clear FIELD]
The profile selects backend/model/effort. Creation requires an unused exact ID plus an existing same-principal backend_id and model_name; it does not bind a behavior. Use behavior edit to select it. Optional sampling and execution documents own their respective controls; compaction is referenced by Context."#
            }
            Some("backend") => {
                r#"backend commands:
  list [--limit N] [--cursor BACKEND_ID]
  get [BACKEND_ID] [--behavior BEHAVIOR_ID]
  preview [--behavior BEHAVIOR_ID] [--set FIELD=JSON] [--clear FIELD]
  edit [--behavior BEHAVIOR_ID] [--set FIELD=JSON] [--clear FIELD]
Without BACKEND_ID, this targets the backend referenced by the selected behavior's profile. Raw credentials cannot be read or changed."#
            }
            Some("mcp-service") => {
                r#"mcp-service commands:
  get SERVICE_ID
  preview SERVICE_ID [--set FIELD=JSON] [--clear FIELD]
  edit SERVICE_ID [--set FIELD=JSON] [--clear FIELD]
The service must already exist under this principal."#
            }
            Some("automation") => {
                r#"automation commands:
  get task|schedule|trigger|event-source ID [--behavior BEHAVIOR_ID]
  preview KIND ID [--behavior BEHAVIOR_ID] [--set FIELD=JSON] [--clear FIELD]
  edit KIND ID [--behavior BEHAVIOR_ID] [--set FIELD=JSON] [--clear FIELD]
Tasks belong to the selected behavior. Triggers may reference only its tasks. Schedules and event sources are included only through those trigger links.
For per-document triggers, parallel (default) allows independent invocations; serial skips a fire while prior work is active (it is not a queue); latest_only supersedes prior active work. Use parallel when every input must produce an output, including inputs arriving before the previous request finishes.
Task templates use MiniJinja: {{ doc.message }} reads a source document field; {{ args.name }} reads an invocation argument. Missing values fail rendering; use an explicit default filter for optional fields. Go-style {{.message}} is invalid. Syntax is checked before publication, while available document fields depend on the linked source schema.
Render every source field the behavior needs into the prompt, or grant an explicit scoped read tool. For example, passing only {{ doc.correlation }} does not give the behavior the message to transform.
Delimit source values separately from instructions and metadata. Appending punctuation or a correlation ID beside a value can change what the model treats as input; specify the exact output contract and verify it with representative documents.
Document automation: define the input collection's schema, connect an event source to a task through a trigger, and template source fields into the task prompt. Grant datastore reads/writes to behaviors that consume or publish documents. An external client may submit the input instead. Inspect available schema/datastore authoring tools; these automation commands do not create schemas or datastore tools.
Results can feed later stages. Use canonical graph tools or graph packs for coordinated dependencies, branching, parallel work, and completion. Verify a workflow with a sample input and its resulting request/output, not just configuration reads."#
            }
            Some("cleanup") => {
                r#"cleanup commands:
  preview --target RESOURCE=ID [--target RESOURCE=ID ...]
  remove --digest SHA256 --target RESOURCE=ID [--target RESOURCE=ID ...]
Resources: behavior, context, tools, profile, sampling, execution, retry-policy, compaction, backend, mcp-service, task, schedule, trigger, event-source.
Cleanup is exact-ID, same-principal, and reference-aware. Preview performs the same complete retained-reference validation without writing and returns the digest required by remove. Remove requires the same target set and refuses if any target changed, then revalidates and deletes the whole set atomically, so related unreferenced cycles can be removed together. A retained document may never be left with a missing reference. Behavior/context cleanup requires the behavior catalog grant; the protected Setup behavior cannot be removed."#
            }
            Some("pack") if self.allow_pack_install => {
                r#"pack commands:
  list [--limit N] [--cursor NAME]
  get PACKAGE
  preview install|update PACKAGE [--inference-slot NAME=PROFILE_ID] [--var NAME=VALUE]
  install|update PACKAGE --digest SHA256 [--inference-slot NAME=PROFILE_ID] [--var NAME=VALUE]
Bundled names resolve locally; NAMESPACE/NAME resolves through the operator-selected registry. Preview is read-only and returns the exact canonical content digest required by install/update; registry receipts also expose the verified archive digest. Repeat --inference-slot for every declared slot; values are existing principal-owned profile IDs. Non-inference variables remain explicit --var NAME=VALUE. Pack removal remains unavailable because current installation records do not distinguish created documents from matching documents reused at install. Installation activates configuration but does not run a graph."#
            }
            Some(other) => bail!(
                "unknown config help resource {other:?}; enabled resources: {}",
                model_resources(&self.categories, self.allow_pack_install).join(", ")
            ),
        };
        Ok(serde_json::to_string_pretty(&json!({
            "ok": true,
            "data_model": DATA_MODEL,
            "help": detail,
            "enabled_resources": model_resources(&self.categories, self.allow_pack_install),
            "patch_contracts": help_patch_contracts(resource),
            "examples": if resource == Some("datastore") { datastore::entry_examples() } else { Value::Null },
            "current_limitations": {
                "pack_remove": "unavailable because installation records do not yet distinguish documents created by an install from matching documents the install reused",
            },
        }))?)
    }

    async fn behavior(&self, argv: &[String]) -> Result<String> {
        anyhow::ensure!(
            self.categories.contains("persona") || self.categories.contains("behavior"),
            "behavior configuration is not granted; enabled resources: {}",
            model_resources(&self.categories, self.allow_pack_install).join(", ")
        );
        let Some(verb) = argv.first().map(String::as_str) else {
            bail!("behavior command is required; run config help behavior");
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
                    "behavior get accepts only an optional behavior_id; run config help behavior"
                );
                let id = flags
                    .positionals
                    .first()
                    .map(String::as_str)
                    .unwrap_or(self.core.behavior_id());
                anyhow::ensure!(
                    flags.positionals.len() <= 1,
                    "behavior get accepts at most one behavior_id"
                );
                self.ensure_behavior_catalog("get", Some(id))?;
                persona_inspect(&self.node, &self.agent_did, id, &self.process_ceiling).await
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
                    self.ensure_behavior_catalog("preview default", Some(behavior_id))?;
                    let params = default_behavior_params("preview", behavior_id);
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
                        "behavior preview edit requires BEHAVIOR_ID followed by patch flags",
                    )?;
                    self.ensure_behavior_catalog("preview edit", Some(behavior_id))?;
                    let core = self.target_core(Some(behavior_id), "preview edit")?;
                    let patch = parse_patch(&argv[3..], SelfConfigTarget::AgentBehavior)?;
                    return self
                        .patch(
                            &core,
                            "preview",
                            protect_working_behavior(behavior_request(&core, patch)),
                        )
                        .await;
                }
                let params = behavior_params("preview", Some(operation.clone()), &argv[2..])?;
                self.ensure_behavior_operation(operation, params.behavior_id.as_deref())?;
                self.ensure_default_selection(&params)?;
                persona_preview(&self.node, &self.agent_did, &params, &self.process_ceiling).await
            }
            "edit" => {
                let behavior_id = argv
                    .get(1)
                    .context("behavior edit requires BEHAVIOR_ID followed by patch flags")?;
                self.ensure_behavior_catalog("edit", Some(behavior_id))?;
                let core = self.target_core(Some(behavior_id), "edit")?;
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
                let params = behavior_params(verb, None, &argv[1..])?;
                self.ensure_behavior_operation(verb, params.behavior_id.as_deref())?;
                self.ensure_default_selection(&params)?;
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
                self.ensure_behavior_catalog("default", Some(behavior_id))?;
                let identity = self.identity.as_deref().context(
                    "behavior writes require the exact local principal signer; reads remain available",
                )?;
                persona_mutate(
                    &self.node,
                    &self.agent_did,
                    identity,
                    &default_behavior_params("edit", behavior_id),
                    &self.process_ceiling,
                )
                .await
            }
            "context" => self.behavior_context(&argv[1..]).await,
            other => bail!("unknown behavior command {other:?}; run config help behavior"),
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
        let core = self.target_core(behavior_id.as_deref(), "context")?;
        match verb {
            "get" => {
                anyhow::ensure!(
                    rest.is_empty(),
                    "behavior context get accepts only --behavior BEHAVIOR_ID"
                );
                let effective = core
                    .read_effective_config(&self.categories, self.no_lockout, self.dry_run)
                    .await?;
                Ok(serde_json::to_string_pretty(&json!({
                    "resource": "AgentContext",
                    "document": effective.get("context"),
                }))?)
            }
            "preview" | "edit" => {
                let patch = parse_patch(&rest, SelfConfigTarget::AgentContext)?;
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
            format!("{resource} command is required; run config help {resource}")
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
                self.inference_inventory(
                    target,
                    parse_limit(&parsed)?,
                    parsed.one("cursor")?,
                )
                .await
            }
            "get" => {
                let (behavior_id, rest) = extract_behavior_target(&argv[1..])?;
                anyhow::ensure!(rest.len() <= 1, "{resource} get accepts at most one ID and --behavior BEHAVIOR_ID");
                let core = self.target_core(behavior_id.as_deref(), resource)?;
                match rest.first() {
                    Some(id) => self.exact_read(target, id).await,
                    None => self.bound_read(&core, target).await,
                }
            }
            "preview" | "edit" => {
                let (behavior_id, rest) = extract_behavior_target(&argv[1..])?;
                let core = self.target_core(behavior_id.as_deref(), resource)?;
                let patch = parse_patch(&rest, target)?;
                let request = match target {
                    SelfConfigTarget::Tools => {
                        tools_request(&core, patch, self.allow_pack_install)
                    }
                    SelfConfigTarget::InferenceBackend => backend_request(patch),
                    _ => unreachable!("bound resource"),
                };
                self.patch(&core, verb, protect_working_behavior(request)).await
            }
            other => bail!(
                "unknown {resource} command {other:?}; accepted: get, preview, edit; run config help {resource}"
            ),
        }
    }

    async fn profile(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("profile")?;
        let verb = argv
            .first()
            .map(String::as_str)
            .context("profile command is required; run config help profile")?;
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
                .context("profile create requires PROFILE_ID followed by patch flags")?;
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
        let core = self.target_core(behavior_id.as_deref(), "profile")?;
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
                "unknown profile command {other:?}; accepted: get, preview, edit; run config help profile"
            ),
        }
    }

    async fn mcp_service(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("mcp-service")?;
        let verb = argv
            .first()
            .map(String::as_str)
            .context("mcp-service command is required; run config help mcp-service")?;
        let id = argv.get(1).context("mcp-service requires SERVICE_ID")?;
        match verb {
            "get" => {
                anyhow::ensure!(argv.len() == 2, "mcp-service get accepts one SERVICE_ID");
                self.exact_read(SelfConfigTarget::ToolServiceRegistry, id).await
            }
            "preview" | "edit" => {
                let patch = parse_patch(&argv[2..], SelfConfigTarget::ToolServiceRegistry)?;
                self.patch(&self.core, verb, mcp_service_request(id.clone(), patch))
                    .await
            }
            other => bail!(
                "unknown mcp-service command {other:?}; accepted: get, preview, edit; run config help mcp-service"
            ),
        }
    }

    async fn automation(&self, argv: &[String]) -> Result<String> {
        self.ensure_resource("automation")?;
        let verb = argv
            .first()
            .map(String::as_str)
            .context("automation command is required; run config help automation")?;
        let kind = argv.get(1).context("automation requires KIND")?;
        let id = argv.get(2).context("automation requires ID")?;
        let target = automation_target(&kind.replace('-', "_"))?;
        match verb {
            "get" => {
                let (behavior_id, rest) = extract_behavior_target(&argv[3..])?;
                anyhow::ensure!(rest.is_empty(), "automation get accepts KIND, ID, and --behavior BEHAVIOR_ID");
                let core = self.target_core(behavior_id.as_deref(), "automation")?;
                self.automation_read(&core, target, id).await
            }
            "preview" | "edit" => {
                let (behavior_id, rest) = extract_behavior_target(&argv[3..])?;
                let core = self.target_core(behavior_id.as_deref(), "automation")?;
                let patch = parse_patch(&rest, target)?;
                self.patch(
                    &core,
                    verb,
                    protect_working_behavior(automation_request(&core, target, id.clone(), patch)),
                )
                .await
            }
            other => bail!(
                "unknown automation command {other:?}; accepted: get, preview, edit; run config help automation"
            ),
        }
    }

    async fn cleanup(&self, argv: &[String]) -> Result<String> {
        let verb = argv
            .first()
            .map(String::as_str)
            .context("cleanup requires preview or remove; run config help cleanup")?;
        anyhow::ensure!(
            matches!(verb, "preview" | "remove"),
            "unknown cleanup command {verb:?}; accepted: preview, remove"
        );
        if verb == "preview" {
            anyhow::ensure!(
                self.dry_run,
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
            if matches!(
                target,
                SelfConfigTarget::AgentBehavior | SelfConfigTarget::AgentContext
            ) {
                anyhow::ensure!(
                    self.categories.contains("persona"),
                    "{resource} cleanup requires the behavior catalog grant"
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
        Ok(serde_json::to_string_pretty(&json!({
            "committed": !preview,
            "operation": if preview { "preview cleanup" } else { "cleanup" },
            "owner": self.agent_did,
            "plan_digest": plan_digest,
            "targets": receipt_targets,
            "apply_with": preview.then(|| json!({
                "argv_prefix": ["cleanup", "remove", "--digest", plan_digest],
                "repeat_targets": targets.iter().map(|(resource, _, id)| format!("{resource}={id}")).collect::<Vec<_>>(),
            })),
            "effect": if preview {
                "No documents were changed. Repeat the same exact targets and plan digest with cleanup remove to revalidate and apply atomically."
            } else {
                "The exact target set was removed atomically after retained-reference validation."
            },
        }))?)
    }

    async fn pack(&self, argv: &[String]) -> Result<String> {
        anyhow::ensure!(self.allow_pack_install, "pack installation is not granted");
        let installer = PackInstaller {
            core: self.core.clone(),
            node: self.node.clone(),
        };
        let verb = argv
            .first()
            .map(String::as_str)
            .context("pack command is required; run config help pack")?;
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
                    "pack preview supports install and update; remove is unavailable because installation records do not distinguish created documents from reused matching documents"
                );
                installer
                    .preview(operation, parse_pack_change(&argv[2..])?)
                    .await
            }
            "install" | "update" => {
                installer.apply(verb, parse_pack_change(&argv[1..])?).await
            }
            "remove" => bail!(
                "pack remove is unavailable: installation records do not distinguish created documents from reused matching documents, and provenance tags or DefraDB ACL cannot supply that semantic ownership"
            ),
            other => bail!("unknown pack command {other:?}; run config help pack"),
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
            "behavior {operation} outside the current behavior is not granted; the behavior catalog grant is required"
        );
        Ok(())
    }

    fn ensure_behavior_operation(&self, operation: &str, behavior_id: Option<&str>) -> Result<()> {
        let current_edit =
            operation == "edit" && behavior_id.is_some_and(|id| id == self.core.behavior_id());
        anyhow::ensure!(
            self.categories.contains("persona") || current_edit,
            "behavior {operation} is not granted; only editing the current behavior is allowed without the behavior catalog grant"
        );
        Ok(())
    }

    fn ensure_default_selection(&self, params: &ConfigurePersonaParams) -> Result<()> {
        anyhow::ensure!(
            !params.make_default || self.categories.contains("persona"),
            "--default changes the principal's behavior selection and requires the behavior catalog grant"
        );
        Ok(())
    }

    fn target_core(&self, behavior_id: Option<&str>, operation: &str) -> Result<SelfConfigCore> {
        let behavior_id = behavior_id.unwrap_or(self.core.behavior_id());
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
        let outcome = match verb {
            "preview" => {
                anyhow::ensure!(self.dry_run, "preview is not granted for this behavior");
                core.preview(request).await?
            }
            "edit" => core.apply(request).await?,
            _ => unreachable!("patch verb"),
        };
        outcome_text(&outcome)
    }

    async fn bound_read(&self, core: &SelfConfigCore, target: SelfConfigTarget) -> Result<String> {
        let effective = core
            .read_effective_config(&self.categories, self.no_lockout, self.dry_run)
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
        Ok(serde_json::to_string_pretty(&json!({
            "resource": target.collection_name(),
            "document": value,
        }))?)
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
        .with_context(|| format!("no owned {} with ID {id:?}", target.collection_name()))?;
        if target == SelfConfigTarget::InferenceBackend {
            document.remove("auth");
            document.insert(
                "auth".into(),
                json!({"redacted": true, "owner": "operator credential/login flow"}),
            );
        }
        Ok(serde_json::to_string_pretty(&json!({
            "resource": target.collection_name(),
            "document": document,
        }))?)
    }

    async fn automation_read(
        &self,
        core: &SelfConfigCore,
        target: SelfConfigTarget,
        id: &str,
    ) -> Result<String> {
        let effective = core
            .read_effective_config(&self.categories, self.no_lockout, self.dry_run)
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
                    "no {} {id:?} is reachable from selected behavior {:?}",
                    target.collection_name(),
                    core.behavior_id()
                )
            })?;
        Ok(serde_json::to_string_pretty(&json!({
            "resource": target.collection_name(),
            "document": document,
        }))?)
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
                "backend_id name provider_kind openai_wire_api endpoint connect_timeout_secs discovery_timeout_secs max_concurrent max_queue_depth enabled probe_status last_probe",
            ),
            _ => unreachable!("inference inventory target"),
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
        let next_cursor = truncated
            .then(|| {
                selected
                    .last()?
                    .get(id_field)?
                    .as_str()
                    .map(ToOwned::to_owned)
            })
            .flatten();
        Ok(serde_json::to_string_pretty(&json!({
            "resource": collection,
            "page": {
                "limit": limit,
                "total": total,
                "returned": selected.len(),
                "truncated": truncated,
                "next_cursor": next_cursor,
            },
            "items": selected,
            "note": "Inventory is read-only and does not rebind any behavior. Backend credentials are excluded.",
        }))?)
    }
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
        other => bail!(
            "unknown cleanup resource {other:?}; run config help cleanup for accepted resources"
        ),
    }
}

fn profile_target(name: &str) -> Result<(&'static str, SelfConfigTarget)> {
    match name {
        "profile" => Ok(("profile", SelfConfigTarget::InferenceProfile)),
        "sampling" => Ok(("sampling", SelfConfigTarget::InferenceSampling)),
        "execution" => Ok(("execution", SelfConfigTarget::InferenceExecution)),
        "retry-policy" => Ok(("retry_policy", SelfConfigTarget::InferenceRetryPolicy)),
        "compaction" => Ok(("compaction", SelfConfigTarget::Compaction)),
        other => bail!("unknown profile target {other:?}; accepted: profile, sampling, execution, retry-policy, compaction"),
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
    let contracts = match resource {
        Some("skill") => vec![patch_contract(
            SelfConfigTarget::Skill,
            json!({
                "name":"string|null; imported from SKILL.md frontmatter, default SKILL_ID",
                "description":"string|null; imported from SKILL.md frontmatter",
                "instructions":"string|null; imported from the Markdown body",
                "source_directory":"string|null; resolved local SKILL.md parent; supporting-file base, never an access grant",
                "tool_refs":"array<string>; dependencies from agents/openai.yaml, default []; never tool grants",
                "display_name":"string|null; from agents/openai.yaml interface.display_name",
                "interface_json":"string|null; serialized agents/openai.yaml interface",
                "enabled":"boolean; default true",
                "tags":"array<string>; default []"
            }),
        )],
        Some("datastore") => vec![patch_contract(
            SelfConfigTarget::DatastoreToolSurface,
            json!({
                "display_name":"string|null",
                "enabled":"boolean; default true",
                "entries":"array<SurfaceToolDecl>|null (reads use {entries:[...]} envelope). Create: {tool_name:string,collection:string,description?:string,fields:[{name:string,required?:boolean,fill?:correlation|{source_field:string}}],output_obligation?:{scope:request|trigger,minimum_writes?:positive integer,expected_count_field?:string}}. Query: {kind:query,tool_name:string,collection:string,description?:string,fields:[string],filter_fields?:[same field objects]}. Runtime-filled fields must not be required; other fields are model arguments. Surface writes validate declaration syntax. Selecting the surface in Tools checks tool-name collisions; runtime invocation validates the target collection/fields. Register schemas first, then bind and exercise the tools to establish readiness.",
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
                "host": {"root":"string|null; absent uses runtime cwd", "files":{"mode":"Off|ReadOnly|ReadWrite (default Off)","timeout_secs":"positive integer|null"}, "bash":{"mode":"Off|ReadOnly|Unrestricted (default Off)","execution_mode":"read_only|workspace_write|artifact_write|unrestricted|null","network_mode":"inherit|disabled|enabled|null","allowed_argv_prefixes":"array<array<string>>|null","forbidden_argv_prefixes":"array<array<string>>|null","read_only_commands":"array<string>|null","background_enabled":"boolean; default false","timeout_secs":"positive integer|null; default 120","max_timeout_secs":"positive integer|null","background_timeout_secs":"positive integer|null; default 36000","wait_timeout_secs":"positive integer|null; default 30","max_wait_timeout_secs":"positive integer|null; default 600"}, "cli":"array<{name:string,timeout_secs?:positive integer}>; default []"},
                "remote": {"services":"array<{mcp_service_id:string,tool_names:array<string>,style:flat|discovery(default),required:boolean(default false),background_tool_names:array<string>,connect_timeout_secs?:integer,discovery_timeout_secs?:integer,timeout_secs?:integer,stale_timeout_secs?:integer,background_timeout_secs?:integer,wait_timeout_secs?:integer,max_wait_timeout_secs?:integer}>; default []"},
                "subagents": {"target_ids":"array<existing same-principal SubagentTarget ID>; default []","spawn_enabled":"boolean|null","steering_enabled":"boolean|null","background_enabled":"boolean|null","default_await_mode":"foreground|background|null","allow_cross_principal":"boolean|null","cross_principal_spawn_timeout_secs":"positive integer|null; default 60","wait_timeout_secs":"positive integer|null; default 30","max_wait_timeout_secs":"positive integer|null; default 600"},
                "built_ins": {"enable_graph_tools":"boolean|null","enable_goal_tools":"boolean|null","enable_goal_creation":"boolean|null","enable_memory":"boolean|null","enable_session_history_tool":"boolean|null","enable_context_budget":"boolean|null","timeout_secs":"positive integer|null; absent uses enclosing request deadline"},
                "datastore": {"enable_defra_query":"boolean|null","defra_query_collections":"array<string>|null","datastore_tool_surface_ids":"array<existing same-principal DatastoreToolSurface ID>|null","timeout_secs":"positive integer|null"},
                "integrations": {"lsp":{"config":"JSON encoded as a string|null","timeout_secs":"positive integer|null; default 20","max_timeout_secs":"positive integer|null; maximum 300","rpc_timeout_secs":"positive integer|null; default 30"},"eth_tool_ids":"array<existing same-principal EthTool ID>|null"},
                "self_config": {"enable_self_config":"boolean|null; absent is disabled","self_config_categories":"array<behavior|tools|profile|backend|mcp_service|automation|persona>|null; absent selects behavior, tools, profile","self_config_no_lockout":"boolean|null","self_config_dry_run":"boolean|null","enable_pack_install":"boolean|null; cannot be self-granted","timeout_secs":"positive integer|null"},
                "tags": "array<string>; default []",
            }),
        )],
        Some("profile") => vec![
            patch_contract(
                SelfConfigTarget::InferenceProfile,
                json!({
                    "display_name":"string|null","description":"string|null","backend_id":"existing same-principal backend ID","model_name":"string","reasoning_effort":"none|minimal|low|medium|high|xhigh|max|ultra|null","context_window":"positive integer|null","max_output_tokens":"positive integer|null","sampling_id":"existing same-principal sampling ID|null","execution_id":"existing same-principal execution ID|null","tags":"array<string>; default []"
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
                    "display_name":"string|null","max_turns":"positive integer|null","max_total_tokens":"positive integer|null; null is unlimited","stream_batch_ms":"positive integer|null; default 1000","stream_liveness_timeout_secs":"positive integer|null; default 1800 and less than deadline","deadline_duration_secs":"positive integer|null; default 86400","retry_policy_id":"existing same-principal retry policy ID|null","tags":"array<string>; default []"
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
                    "display_name":"string|null","description":"string|null","prompt_template":"string; rendered per invocation","goal_objective_template":"string|null","goal_token_budget":"positive integer|null","hooks":"array<{hook_id:string,phase:before|after_success|after_failure|finally,command:nonempty array<string>,timeout_secs?:positive integer}>; default []","enabled":"boolean; default true","output_schema_ref":"string|null","tags":"array<string>; default []"
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
                    "display_name":"string|null","description":"string|null","task_id":"existing task ID owned by selected behavior","source":"{kind:schedule,schedule_id:string}|{kind:event,event_source_id:string}","enabled":"boolean; default true","concurrency":"parallel|serial|latest_only|null; default parallel","tags":"array<string>; default []"
                }),
            ),
            patch_contract(
                SelfConfigTarget::EventSource,
                json!({
                    "display_name":"string|null","source_collection":"valid GraphQL collection name","event_kind":"created|null; default created","filter":"GraphQL filter fragment|null","correlation_field":"GraphQL field name|null","group":"{expected_count?:positive integer|{source_field:string},timeout_secs?:positive integer,min_count?:positive integer}|null","workspace_authority":"canonical workspace authority object|null","tags":"array<string>; default []"
                }),
            ),
        ],
        _ => Vec::new(),
    };
    Value::Array(contracts)
}

fn parse_pack_change(argv: &[String]) -> Result<PackInstallParams> {
    let package = argv
        .first()
        .context("pack operation requires PACKAGE")?
        .clone();
    let parsed = ParsedArgs::parse(&argv[1..])?;
    anyhow::ensure!(
        parsed.positionals.is_empty() && parsed.switches.is_empty(),
        "unexpected pack argument; run config help pack"
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
    let parsed = ParsedArgs::parse(argv)?;
    anyhow::ensure!(
        parsed.positionals.is_empty(),
        "unexpected positional argument {:?}; use --set FIELD=JSON or --clear FIELD",
        parsed.positionals[0]
    );
    anyhow::ensure!(
        parsed.switches.is_empty(),
        "unexpected switch; use --set FIELD=JSON or --clear FIELD"
    );
    for name in parsed.options.keys() {
        anyhow::ensure!(
            matches!(name.as_str(), "set" | "clear"),
            "unknown patch option --{name}; accepted: --set FIELD=JSON, --clear FIELD"
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
        "empty patch; use --set FIELD=JSON or --clear FIELD"
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
    for option in parsed.options.keys() {
        anyhow::ensure!(
            allowed.contains(&option.as_str()),
            "unknown behavior option --{option}; run config help behavior"
        );
    }
    anyhow::ensure!(
        parsed.positionals.is_empty(),
        "unexpected positional argument {:?}; run config help behavior",
        parsed.positionals[0]
    );
    let clear: BTreeSet<&str> = parsed
        .options
        .get("clear")
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();
    for field in &clear {
        anyhow::ensure!(matches!(*field, "display_name" | "description" | "system_prompt" | "root"), "field {field:?} cannot be cleared; accepted: display_name, description, system_prompt, root");
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
        assert!(error.to_string().contains("config help behavior"));
    }
}
