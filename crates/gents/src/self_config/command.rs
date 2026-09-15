use super::*;

const CONFIG_USAGE: &str = r#"config commands (argv excludes the tool name):
  ["help"] or ["help", RESOURCE]
  ["get"]
  ["behavior", "list", "--limit", N, "--cursor", ID]
  ["behavior", "get", BEHAVIOR_ID]
  ["behavior", "preview", OPERATION, ...flags]
  ["behavior", "create"|"edit"|"clone"|"disable", ...flags]
  ["behavior", "tools", BEHAVIOR_ID, "--lsp", on|off, "--graphs", on|off, "--network", disabled]
  ["profile"|"backend", "list", "--limit", N, "--cursor", ID]
  ["tools", "get"|"preview"|"edit", [--set FIELD=JSON] [--clear FIELD]]
  ["backend", "get", [BACKEND_ID]]
  ["backend", "preview"|"edit", [--set FIELD=JSON] [--clear FIELD]]
  ["profile", "get"|"preview"|"edit", [profile|sampling|execution|retry-policy|compaction], PATCH_FLAGS]
  ["mcp-service", "get", SERVICE_ID]
  ["mcp-service", "preview"|"edit", SERVICE_ID, PATCH_FLAGS]
  ["automation", "get", task|schedule|trigger|event-source, ID]
  ["automation", "preview"|"edit", KIND, ID, PATCH_FLAGS]
  ["pack", "list", ["--limit", N] ["--cursor", NAME]]
  ["pack", "get", PACKAGE]
  ["pack", "preview", "install"|"update", PACKAGE, [--inference-slot NAME=PROFILE_ID] [--var NAME=VALUE]]
  ["pack", "install"|"update", PACKAGE, --digest SHA256, [--inference-slot NAME=PROFILE_ID] [--var NAME=VALUE]]

Behavior flags: --id, --from, --display-name, --description, --system-prompt,
--root, --preset, --profile, --default, and repeated --clear FIELD. Omitted edit
fields preserve their values. Clearable fields: display_name, description,
system_prompt, root. PATCH_FLAGS are repeated --set FIELD=JSON and --clear FIELD.
Use config help RESOURCE before a write."#;

const DATA_MODEL: &str = "A principal owns exact-ID configuration documents. Requests, tasks, and sessions select a Behavior. Behavior -> Context controls the system prompt, selected skills, compaction, and one Tools document; Tools contains nested host, built-in, integration, MCP, and self-config settings. Behavior -> InferenceProfile -> Backend controls model execution; the profile selects model and reasoning effort and may reference sampling and execution settings. A Trigger selects a Task and a Schedule or EventSource. A Pack declares configuration and inference roles; installation binds every role to an existing principal-owned profile, then publishes the pack's documents and graph revision without copying inference configuration. Reads never mutate. Writes are sparse patches: omission preserves, explicit --clear removes an optional value, and preview/apply validate the complete same-principal reference chain atomically. Credentials and OAuth consent remain operator-owned and are never returned by config.";

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
        ("profile", "profile"),
        ("backend", "backend"),
        ("mcp_service", "mcp-service"),
        ("automation", "automation"),
    ] {
        if categories.contains(category) {
            resources.push(resource);
        }
    }
    if pack {
        resources.push("pack");
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
            "get" if argv.len() == 1 => {
                let value = self
                    .core
                    .read_effective_config(&self.categories, self.no_lockout, self.dry_run)
                    .await?;
                Ok(serde_json::to_string_pretty(&value)?)
            }
            "get" => bail!("config get accepts no arguments; use config behavior get BEHAVIOR_ID for a targeted behavior read"),
            "behavior" => self.behavior(&argv[1..]).await,
            "tools" => self.bound_document("tools", &argv[1..]).await,
            "profile" => self.profile(&argv[1..]).await,
            "backend" => self.bound_document("backend", &argv[1..]).await,
            "mcp-service" => self.mcp_service(&argv[1..]).await,
            "automation" => self.automation(&argv[1..]).await,
            "pack" => self.pack(&argv[1..]).await,
            other => bail!(
                "unknown config resource or command {other:?}; accepted: help, get, {}\n{CONFIG_USAGE}",
                model_resources(&self.categories, self.allow_pack_install).join(", ")
            ),
        }
    }

    fn help(&self, resource: Option<&str>) -> Result<String> {
        let detail = match resource {
            None => CONFIG_USAGE,
            Some("behavior") => {
                r#"behavior commands:
  list [--limit N] [--cursor BEHAVIOR_ID]
  get [BEHAVIOR_ID]
  context get
  context preview|edit [--set FIELD=JSON] [--clear FIELD]
  preview create|edit|clone|disable FLAGS
  create --display-name NAME --system-prompt TEXT --preset readonly|write --profile PROFILE_ID [--description TEXT] [--root PATH] [--default]
  clone --from BEHAVIOR_ID --display-name NAME --profile PROFILE_ID [overrides]
  edit --id BEHAVIOR_ID [--display-name NAME] [--description TEXT] [--system-prompt TEXT] [--root PATH] [--preset readonly|write] [--profile PROFILE_ID] [--clear FIELD] [--default]
  disable --id BEHAVIOR_ID
  tools BEHAVIOR_ID [--lsp on|off] [--graphs on|off] [--network disabled]
On edit, omitted fields preserve. --clear is distinct and is valid for display_name, description, system_prompt, and root. profile cannot be cleared. All IDs come from list/get; never guess IDs."#
            }
            Some("tools") => {
                r#"tools commands:
  get
  preview [--set FIELD=JSON] [--clear FIELD]
  edit [--set FIELD=JSON] [--clear FIELD]
This targets the Tools document referenced by the current behavior. Nested values are JSON. A patch is atomic; omitted fields preserve and --clear removes an optional field."#
            }
            Some("profile") => {
                r#"profile commands:
  list [--limit N] [--cursor PROFILE_ID]
  get [profile|sampling|execution|retry-policy|compaction]
  get PROFILE_ID
  preview [TARGET] [--set FIELD=JSON] [--clear FIELD]
  edit [TARGET] [--set FIELD=JSON] [--clear FIELD]
The profile selects backend/model/effort. Optional sampling and execution documents own their respective controls; compaction is referenced by Context."#
            }
            Some("backend") => {
                r#"backend commands:
  list [--limit N] [--cursor BACKEND_ID]
  get [BACKEND_ID]
  preview [--set FIELD=JSON] [--clear FIELD]
  edit [--set FIELD=JSON] [--clear FIELD]
This targets the backend referenced by the current profile. Raw credentials cannot be read or changed."#
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
  get task|schedule|trigger|event-source ID
  preview KIND ID [--set FIELD=JSON] [--clear FIELD]
  edit KIND ID [--set FIELD=JSON] [--clear FIELD]
Tasks belong to this behavior. Triggers may reference only its tasks. Schedules and event sources are included only through those trigger links."#
            }
            Some("pack") if self.allow_pack_install => {
                r#"pack commands:
  list [--limit N] [--cursor NAME]
  get PACKAGE
  preview install|update PACKAGE [--inference-slot NAME=PROFILE_ID] [--var NAME=VALUE]
  install|update PACKAGE --digest SHA256 [--inference-slot NAME=PROFILE_ID] [--var NAME=VALUE]
Only bundled graph packages are installable. Preview is read-only and returns the exact digest required by install/update. Repeat --inference-slot for every declared slot; values are existing principal-owned profile IDs. Non-inference variables remain explicit --var NAME=VALUE. Registry graph install and pack removal are unavailable until their canonical adapters exist. Installation activates configuration but does not run a graph."#
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
                    .context("behavior preview requires create|edit|clone|disable")?;
                let params = behavior_params("preview", Some(operation.clone()), &argv[2..])?;
                self.ensure_behavior_operation(operation, params.behavior_id.as_deref())?;
                self.ensure_default_selection(&params)?;
                persona_preview(&self.node, &self.agent_did, &params, &self.process_ceiling).await
            }
            "create" | "edit" | "clone" | "disable" => {
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
            "tools" => self.behavior_tools(&argv[1..]).await,
            "context" => self.behavior_context(&argv[1..]).await,
            other => bail!("unknown behavior command {other:?}; run config help behavior"),
        }
    }

    async fn behavior_context(&self, argv: &[String]) -> Result<String> {
        anyhow::ensure!(
            self.categories.contains("behavior"),
            "current behavior/context configuration is not granted"
        );
        let verb = argv
            .first()
            .map(String::as_str)
            .context("behavior context requires get, preview, or edit")?;
        match verb {
            "get" => {
                anyhow::ensure!(argv.len() == 1, "behavior context get accepts no arguments");
                let effective = self
                    .core
                    .read_effective_config(&self.categories, self.no_lockout, self.dry_run)
                    .await?;
                Ok(serde_json::to_string_pretty(&json!({
                    "resource": "AgentContext",
                    "document": effective.get("context"),
                }))?)
            }
            "preview" | "edit" => {
                let patch = parse_patch(&argv[1..], SelfConfigTarget::AgentContext)?;
                self.patch(
                    verb,
                    anchored_request(SelfConfigTarget::AgentContext, "context_id", patch),
                )
                .await
            }
            other => {
                bail!("unknown behavior context command {other:?}; accepted: get, preview, edit")
            }
        }
    }

    async fn behavior_tools(&self, argv: &[String]) -> Result<String> {
        anyhow::ensure!(
            self.categories.contains("tools") || self.categories.contains("persona"),
            "tools configuration is not granted"
        );
        let parsed = ParsedArgs::parse(argv)?;
        anyhow::ensure!(
            parsed.switches.is_empty(),
            "behavior tools accepts --lsp, --graphs, and --network; switches are not supported"
        );
        for name in parsed.options.keys() {
            anyhow::ensure!(
                matches!(name.as_str(), "lsp" | "graphs" | "network"),
                "unknown behavior tools option --{name}; accepted: --lsp, --graphs, --network"
            );
        }
        let behavior_id = parsed
            .positionals
            .first()
            .context("behavior tools requires BEHAVIOR_ID")?;
        anyhow::ensure!(
            parsed.positionals.len() == 1,
            "behavior tools accepts one BEHAVIOR_ID"
        );
        self.ensure_behavior_catalog("tools", Some(behavior_id))?;
        let toggle = |name: &str| -> Result<Option<bool>> {
            parsed
                .one(name)?
                .map(|value| match value {
                    "on" => Ok(true),
                    "off" => Ok(false),
                    _ => bail!("--{name} accepts on|off"),
                })
                .transpose()
        };
        let network = parsed
            .one("network")?
            .map(|value| match value {
                "disabled" => Ok(crate::toolset::CommandNetworkMode::Disabled),
                _ => bail!("--network may only narrow to disabled"),
            })
            .transpose()?;
        anyhow::ensure!(
            parsed.options.contains_key("lsp")
                || parsed.options.contains_key("graphs")
                || parsed.options.contains_key("network"),
            "behavior tools requires at least one of --lsp, --graphs, or --network"
        );
        let core = SelfConfigCore::new(
            self.node.clone(),
            self.agent_did.clone(),
            behavior_id.clone(),
        )?
        .with_no_lockout(self.no_lockout)
        .with_process_ceiling(self.process_ceiling.clone());
        let outcome = core
            .select_sibling_tools(toggle("lsp")?, toggle("graphs")?, network)
            .await?;
        let effective = core
            .read_effective_config(&BTreeSet::new(), false, false)
            .await?;
        Ok(serde_json::to_string_pretty(&json!({
            "ok": true,
            "outcome": outcome,
            "effective_config": effective,
            "next": ["behavior", "get", behavior_id],
        }))?)
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
                anyhow::ensure!(argv.len() <= 2, "{resource} get accepts at most one ID");
                match argv.get(1) {
                    Some(id) => self.exact_read(target, id).await,
                    None => self.bound_read(target).await,
                }
            }
            "preview" | "edit" => {
                let patch = parse_patch(&argv[1..], target)?;
                let request = match target {
                    SelfConfigTarget::Tools => {
                        tools_request(&self.core, patch, self.allow_pack_install)
                    }
                    SelfConfigTarget::InferenceBackend => backend_request(patch),
                    _ => unreachable!("bound resource"),
                };
                self.patch(verb, request).await
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
        let mut rest = &argv[1..];
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
                self.bound_read(target).await
            }
            "preview" | "edit" => {
                let patch = parse_patch(rest, target)?;
                self.patch(verb, profile_target_request(Some(request_name), patch)?)
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
                self.patch(verb, mcp_service_request(id.clone(), patch)).await
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
                anyhow::ensure!(argv.len() == 3, "automation get accepts KIND and ID");
                self.automation_read(target, id).await
            }
            "preview" | "edit" => {
                let patch = parse_patch(&argv[3..], target)?;
                self.patch(
                    verb,
                    automation_request(&self.core, target, id.clone(), patch),
                )
                .await
            }
            other => bail!(
                "unknown automation command {other:?}; accepted: get, preview, edit; run config help automation"
            ),
        }
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
                    "pack preview supports install and update; remove is unavailable because canonical cleanup ownership is not implemented"
                );
                installer
                    .preview(operation, parse_pack_change(&argv[2..])?)
                    .await
            }
            "install" | "update" => {
                installer.apply(verb, parse_pack_change(&argv[1..])?).await
            }
            "remove" => bail!(
                "pack remove is unavailable: provenance tags are not deletion authority and no canonical installation cleanup owner exists"
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

    async fn patch(&self, verb: &str, request: ApplyRequest<'static>) -> Result<String> {
        let outcome = match verb {
            "preview" => {
                anyhow::ensure!(self.dry_run, "preview is not granted for this behavior");
                self.core.preview(request).await?
            }
            "edit" => self.core.apply(request).await?,
            _ => unreachable!("patch verb"),
        };
        outcome_text(&outcome)
    }

    async fn bound_read(&self, target: SelfConfigTarget) -> Result<String> {
        let effective = self
            .core
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
                "current behavior does not reference a {}; inspect config behavior get {} to see its exact reference chain",
                target.collection_name(),
                self.core.behavior_id()
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

    async fn automation_read(&self, target: SelfConfigTarget, id: &str) -> Result<String> {
        let effective = self
            .core
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
                    "no {} {id:?} is reachable from current behavior {:?}",
                    target.collection_name(),
                    self.core.behavior_id()
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
