use super::*;
use agent_management::AgentApplyError;
use gents_protocol::operations::agent::{
    AgentCreateInput, AgentCreateOperation, AgentOperationError, AgentOutcome, AgentReceipt,
    AGENT_CLONE, AGENT_CREATE,
};
use gents_session::session::output_render::ordered;

impl ConfigCommandTool {
    pub(super) async fn agent(&self, argv: &[String]) -> Result<String> {
        anyhow::ensure!(
            super::grants::agent_command_exposed(&self.categories),
            "agent configuration is not granted; enabled resources: {}",
            model_resources(&self.categories, self.core.held_grants().pack_install).join(", ")
        );
        let Some(verb) = argv.first().map(String::as_str) else {
            bail!("agent command is required; see [\"help\",\"agent\"]");
        };
        match verb {
            "list" => {
                self.ensure_agent_catalog("list", None)?;
                let flags = ParsedArgs::parse(&argv[1..])?;
                flags.reject_mutation_flags()?;
                let limit = flags
                    .one("limit")?
                    .map(str::parse::<usize>)
                    .transpose()
                    .context("--limit must be a positive integer")?
                    .unwrap_or(20);
                agent_list(&self.core, limit, flags.one("cursor")?).await
            }
            "get" => {
                let flags = ParsedArgs::parse(&argv[1..])?;
                anyhow::ensure!(
                    flags.switches.is_empty() && flags.options.is_empty(),
                    "agent get accepts only an optional agent_did; see [\"help\",\"agent\"]"
                );
                anyhow::ensure!(
                    flags.positionals.len() <= 1,
                    "agent get accepts at most one agent_did"
                );
                let id = match flags.positionals.first() {
                    Some(id) => self.resolve_agent_did(id).await?,
                    None => self.core.agent_did().to_owned(),
                };
                self.ensure_config_scope(Some("agent"), &id)?;
                agent_inspect(&self.core, &id).await
            }
            "preview" => {
                let operation = argv
                    .get(1)
                    .context("agent preview requires create|update|clone|disable|default")?;
                let params = match operation.as_str() {
                    "default" => {
                        let agent_did = argv
                            .get(2)
                            .context("agent preview default requires AGENT_DID")?;
                        anyhow::ensure!(
                            argv.len() == 3,
                            "agent preview default accepts exactly one AGENT_DID"
                        );
                        default_agent_params("preview", &self.resolve_agent_did(agent_did).await?)
                    }
                    "edit" | "update" => {
                        anyhow::ensure!(self.preview, "preview is not granted for this agent");
                        match self.agent_update_params(&argv[2..], true).await? {
                            Ok(params) => params,
                            Err((params, error)) => {
                                return agent_preview_text(
                                    Err(AgentApplyError::Operation(error)),
                                    &params,
                                )
                            }
                        }
                    }
                    _ => {
                        let options = agent_options(operation, &argv[2..])?;
                        if let Some(creation) = AgentCreateOperation::for_verb(operation) {
                            let input = self.agent_create_input(creation, options).await?;
                            let params = creation_preview_params(creation);
                            return agent_preview_text(
                                agent_management::model_agent_create_preview(&self.core, &input)
                                    .await,
                                &params,
                            );
                        }
                        let mut params = options.params("preview", Some(operation.clone()));
                        self.resolve_agent_dids(&mut params).await?;
                        params
                    }
                };
                agent_preview_text(
                    agent_management::model_agent_outcome(&self.core, &params).await,
                    &params,
                )
            }
            "edit" | "update" => {
                let params = match self.agent_update_params(&argv[1..], false).await? {
                    Ok(params) => params,
                    Err((params, error)) => {
                        return Err(guidance::agent_failure(
                            AgentApplyError::Operation(error),
                            &params,
                        ))
                    }
                };
                let identity = self.agent_signer()?;
                self.execution.enter_mutation();
                agent_commit_text(
                    agent_management::model_agent_apply(&self.core, identity, params.clone()).await,
                    &params,
                )
            }
            "create" | "clone" | "disable" => {
                let identity = self.agent_signer()?;
                let options = agent_options(verb, &argv[1..])?;
                if let Some(creation) = AgentCreateOperation::for_verb(verb) {
                    let input = self.agent_create_input(creation, options).await?;
                    self.execution.enter_mutation();
                    return agent_commit_text(
                        agent_management::model_agent_create(&self.core, identity, &input).await,
                        &options_free_params(verb),
                    );
                }
                let mut params = options.params(verb, None);
                self.resolve_agent_dids(&mut params).await?;
                self.execution.enter_mutation();
                agent_commit_text(
                    agent_management::model_agent_apply(&self.core, identity, params.clone()).await,
                    &params,
                )
            }
            "default" => {
                let agent_did = argv.get(1).context("agent default requires AGENT_DID")?;
                anyhow::ensure!(
                    argv.len() == 2,
                    "agent default accepts exactly one AGENT_DID"
                );
                let agent_did = self.resolve_agent_did(agent_did).await?;
                let identity = self.agent_signer()?;
                let params = default_agent_params("edit", &agent_did);
                self.execution.enter_mutation();
                agent_commit_text(
                    agent_management::model_agent_apply(&self.core, identity, params.clone()).await,
                    &params,
                )
            }
            "context" => self.agent_context(&argv[1..]).await,
            other => bail!("unknown agent command {other:?}; see [\"help\",\"agent\"]"),
        }
    }

    pub(super) async fn agent_context(&self, argv: &[String]) -> Result<String> {
        anyhow::ensure!(
            self.categories.contains("agent") || self.categories.contains("node"),
            "agent/context configuration is not granted"
        );
        let verb = argv
            .first()
            .map(String::as_str)
            .context("agent context requires get, preview, or edit")?;
        let (agent_did, rest) = extract_agent_target(&argv[1..])?;
        let core = self
            .target_core(agent_did.as_deref(), Some("agent"))
            .await?;
        match verb {
            "get" => {
                anyhow::ensure!(
                    rest.is_empty(),
                    "agent context get accepts only --agent AGENT_DID"
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
                        "agent context {verb} takes no positional argument {word:?}; name the agent in options.agent and the fields in set: {{\"argv\":[\"agent\",\"context\",\"{verb}\"],\"options\":{{\"agent\":\"AGENT_DID\"}},\"set\":{{\"FIELD\":VALUE}}}}"
                    );
                }
                let (shared_ack, rest) = extract_shared(&rest)?;
                let patch =
                    parse_patch_args(ParsedArgs::parse(&rest)?, SelfConfigTarget::AgentContext)?;
                let mut request = protect_working_agent(
                    anchored_request(SelfConfigTarget::AgentContext, "context_id", patch),
                    shared_ack,
                    self.holds_catalog_grant(),
                );
                request.shared_ack = shared_ack;
                self.patch(&core, verb, request).await
            }
            other => {
                bail!("unknown agent context command {other:?}; accepted: get, preview, edit")
            }
        }
    }

    pub(super) async fn resolve_target_agent(
        &self,
        target_id: &str,
        patch: &mut SelfConfigPatch,
    ) -> Result<()> {
        let Some(index) = patch.iter().position(|(field, value)| {
            field == "agent_did" && value.as_ref().is_some_and(Value::is_string)
        }) else {
            return Ok(());
        };
        let local = match patch.iter().find(|(field, _)| field == "target_node_did") {
            Some((_, value)) => {
                value.as_ref().and_then(Value::as_str) == Some(self.node_did.as_str())
            }
            None => {
                let owner = self.node_did.clone();
                let target_id = target_id.to_owned();
                gents_store::config_client::ConfigAccess::transact_local_readonly(
                    &self.node,
                    Some(self.core.identity()?),
                    "self_config.agent_target_owner",
                    |txn| {
                        let owner = owner.clone();
                        let target_id = target_id.clone();
                        Box::pin(async move {
                            Ok::<_, anyhow::Error>(
                                ops::read_owned_doc(
                                    txn,
                                    SelfConfigTarget::AgentTarget,
                                    &owner,
                                    &target_id,
                                )
                                .await?
                                .and_then(|(_, doc)| doc.get("target_node_did").cloned())
                                    == Some(json!(owner)),
                            )
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
        if let Ok(resolved) = self.resolve_agent_did(&short).await {
            patch[index].1 = Some(json!(resolved));
        }
        Ok(())
    }

    pub(super) async fn resolve_agent_did(&self, id: &str) -> Result<String> {
        let current = self.core.agent_did();
        if !self.categories.contains("node") {
            ::identity::Did::new(id.to_owned()).context("agent selection requires its DID")?;
            return Ok(id.to_owned());
        }
        if id == current {
            return Ok(id.to_owned());
        }
        let owner = self.node_did.clone();
        let candidates = [id.to_owned()];
        let listing = format!(
            "{{ Agent(filter: {{node_did: {{_eq: \"{}\"}}}}) {{agent_did display_name}} }}",
            escape_graphql_string(&owner)
        );
        let found = gents_store::config_client::ConfigAccess::transact_local_readonly(
            &self.node,
            Some(self.core.identity()?),
            "self_config.resolve_agent",
            |txn| {
                let owner = owner.clone();
                let candidates = candidates.clone();
                let listing = listing.clone();
                Box::pin(async move {
                    for candidate in candidates {
                        if ops::read_owned_doc(txn, SelfConfigTarget::Agent, &owner, &candidate)
                            .await?
                            .is_some()
                        {
                            return Ok::<_, anyhow::Error>(Ok(candidate));
                        }
                    }
                    Ok(Err(txn.execute(&listing).await?))
                })
            },
        )
        .await?;
        found.map_err(|listing| {
            let mut suggestions =
                gents_protocol::graphql::graphql_rows_from_response(&listing, "Agent")
                    .into_iter()
                    .filter_map(|row| {
                        let agent_did = row.get("agent_did")?.as_str()?;
                        let display_name = row.get("display_name").and_then(Value::as_str);
                        [Some(agent_did), display_name]
                            .into_iter()
                            .flatten()
                            .any(|name| name.eq_ignore_ascii_case(id))
                            .then(|| agent_did.to_owned())
                    })
                    .collect::<Vec<_>>();
            suggestions.sort();
            suggestions.dedup();
            ops::MissingAgent {
                agent_did: id.to_owned(),
                suggestions,
            }
            .into()
        })
    }

    fn agent_signer(&self) -> Result<&dyn NodeIdentity> {
        self.identity.as_deref().context(
            "agent configuration requires its selected actor signer; reads remain available",
        )
    }

    /// `agent update AGENT_DID` with set/clear over the agent edit's fields
    /// (Lean `SelfConfig.agentEditKeys`). A field outside the edit is the
    /// edit's own refusal, returned with the parameters so the caller answers
    /// it as a commit or a preview.
    async fn agent_update_params(
        &self,
        argv: &[String],
        preview: bool,
    ) -> Result<
        std::result::Result<ConfigureAgentParams, (ConfigureAgentParams, AgentOperationError)>,
    > {
        let agent_did = argv
            .first()
            .filter(|word| !word.starts_with('-'))
            .context("agent update requires target_id and fields in set or clear")?;
        let (shared_ack, rest) = extract_shared(&argv[1..])?;
        anyhow::ensure!(
            !shared_ack,
            "shared applies only to a Context or Tools; an Agent document is not shared by reference"
        );
        let patch = parse_patch(&rest, SelfConfigTarget::Agent)?;
        let agent_did = self.resolve_agent_did(agent_did).await?;
        let mut params = ConfigureAgentParams {
            action: if preview { "preview" } else { "edit" }.to_owned(),
            operation: preview.then(|| "edit".to_owned()),
            agent_did: Some(agent_did),
            ..Default::default()
        };
        for (field, value) in patch {
            let text = |value: Option<Value>| -> Result<StringUpdate> {
                match value {
                    None => Ok(StringUpdate::Clear),
                    Some(Value::String(text)) => Ok(StringUpdate::Set(text)),
                    Some(_) => bail!("Agent field {field:?} takes a JSON string"),
                }
            };
            match field.as_str() {
                "display_name" => params.display_name = text(value)?,
                "description" => params.description = text(value)?,
                "system_prompt" => params.system_prompt = text(value)?,
                "inference_profile_id" => params.profile_id = text(value)?,
                "context_id" => params.context_id = text(value)?,
                "enabled" => {
                    params.enabled = Some(match value {
                        None => true,
                        Some(Value::Bool(enabled)) => enabled,
                        Some(_) => bail!("Agent field \"enabled\" takes true or false"),
                    })
                }
                "tags" => {
                    params.tags = Some(match value {
                        None => Vec::new(),
                        Some(value) => serde_json::from_value(value)
                            .context("Agent field \"tags\" takes an array of strings")?,
                    })
                }
                _ => {
                    return Ok(Err((
                        params,
                        AgentOperationError::UnsupportedEditField(field),
                    )))
                }
            }
        }
        Ok(Ok(params))
    }

    async fn resolve_agent_dids(&self, params: &mut ConfigureAgentParams) -> Result<()> {
        if let Some(id) = params.agent_did.take() {
            params.agent_did = Some(self.resolve_agent_did(&id).await?);
        }
        if let Some(source) = params.clone_from.take() {
            params.clone_from = Some(self.resolve_agent_did(&source).await?);
        }
        Ok(())
    }

    async fn agent_create_input(
        &self,
        operation: AgentCreateOperation,
        options: AgentOptions,
    ) -> Result<AgentCreateInput> {
        let verb = operation.declaration().verb;
        anyhow::ensure!(
            options.id.is_none(),
            "agent {verb} provisions a fresh agent DID and returns it; remove options.id"
        );
        let clone_from = match options.from.as_deref() {
            Some(source) => Some(self.resolve_agent_did(source).await?),
            None => None,
        };
        Ok(AgentCreateInput {
            operation,
            clone_from: clone_from.filter(|_| operation == AgentCreateOperation::Clone),
            display_name: options.display_name.owned_value().unwrap_or_default(),
            description: options.description,
            system_prompt: options.system_prompt,
            profile_id: options.profile_id.owned_value().unwrap_or_default(),
            preset: options.preset.owned_value(),
            root: options.root,
            make_default: options.make_default,
        })
    }

    /// Core for the Agent whose configuration an operation touches. `category`
    /// is the document category it reads or edits (`ensure_config_scope`); None
    /// is an effective read that needs no category grant on the invoking Agent.
    pub(super) async fn target_core(
        &self,
        agent_did: Option<&str>,
        category: Option<&str>,
    ) -> Result<SelfConfigCore> {
        let agent_did = match agent_did {
            Some(id) => self.resolve_agent_did(id).await?,
            None => self.core.agent_did().to_owned(),
        };
        let agent_did = agent_did.as_str();
        self.ensure_config_scope(category, agent_did)?;
        let invoking_agent_did = self.core.agent_did().to_owned();
        SelfConfigCore::new(
            self.node.clone(),
            self.node_did.clone(),
            agent_did.to_owned(),
        )
        .map(|core| {
            core.with_lockout_agent_did(invoking_agent_did)
                .inherit_invoker_capabilities(&self.core)
                .with_no_lockout(self.no_lockout)
                .with_process_ceiling(self.process_ceiling.clone())
                .with_held_grants(self.core.held_grants().clone())
        })
    }
}

/// A committed operation's receipt, or its refusal as guidance whose next call
/// is the one the refusal declares.
fn agent_commit_text(
    result: std::result::Result<AgentReceipt<gents_documents::Agent>, AgentApplyError>,
    params: &ConfigureAgentParams,
) -> Result<String> {
    match result {
        Ok(receipt) => agent_management::receipt_text(&receipt),
        Err(error) => Err(guidance::agent_failure(error, params)),
    }
}

/// Lean `SelfConfig.agentOutcome .preview`: a refused preview succeeds and
/// carries the refusal, message and next call its commit would raise.
fn agent_preview_text(
    result: std::result::Result<AgentOutcome<String>, AgentApplyError>,
    params: &ConfigureAgentParams,
) -> Result<String> {
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(AgentApplyError::Operation(error)) => {
            let next_call = agent_management::recovery_for(params, &error);
            match error.preview_outcome(next_call) {
                Ok(outcome) => outcome,
                Err(error) => {
                    return Err(guidance::agent_failure(
                        AgentApplyError::Operation(error),
                        params,
                    ))
                }
            }
        }
        Err(error) => return Err(guidance::agent_failure(error, params)),
    };
    match outcome {
        AgentOutcome::Admitted(text) => Ok(text),
        AgentOutcome::Refused {
            refusal,
            message,
            next_call,
        } => ordered! {
            "admitted": false,
            "committed": false,
            "refusal": refusal.as_str(),
            "message": message,
            "next_call": guidance::next_call(next_call),
        }
        .pretty(),
    }
}

fn creation_preview_params(operation: AgentCreateOperation) -> ConfigureAgentParams {
    ConfigureAgentParams {
        action: "preview".to_owned(),
        operation: Some(operation.declaration().verb.to_owned()),
        ..Default::default()
    }
}

fn options_free_params(verb: &str) -> ConfigureAgentParams {
    ConfigureAgentParams {
        action: verb.to_owned(),
        ..Default::default()
    }
}

pub(super) fn default_agent_params(action: &str, agent_did: &str) -> ConfigureAgentParams {
    ConfigureAgentParams {
        action: action.to_owned(),
        operation: (action == "preview").then(|| "edit".to_owned()),
        agent_did: Some(agent_did.to_owned()),
        make_default: true,
        ..Default::default()
    }
}

#[cfg(test)]
pub(in crate::builtin::self_config) fn agent_params(
    action: &str,
    operation: Option<String>,
    argv: &[String],
) -> Result<ConfigureAgentParams> {
    let verb = operation.as_deref().unwrap_or(action);
    Ok(agent_options(verb, argv)?.params(action, operation))
}

struct AgentOptions {
    id: Option<String>,
    from: Option<String>,
    display_name: StringUpdate,
    description: StringUpdate,
    system_prompt: StringUpdate,
    root: StringUpdate,
    preset: StringUpdate,
    profile_id: StringUpdate,
    make_default: bool,
}

impl AgentOptions {
    fn params(self, action: &str, operation: Option<String>) -> ConfigureAgentParams {
        ConfigureAgentParams {
            action: action.to_owned(),
            operation,
            display_name: self.display_name,
            description: self.description,
            system_prompt: self.system_prompt,
            agent_did: self.id,
            clone_from: self.from,
            root: self.root,
            preset: self.preset,
            profile_id: self.profile_id,
            make_default: self.make_default,
            ..Default::default()
        }
    }
}

/// The agent verbs share one option vocabulary: what create and clone
/// declare, `id` (disable's target, refused by name on create and clone) and
/// `clear`.
fn agent_options(verb: &str, argv: &[String]) -> Result<AgentOptions> {
    let parsed = ParsedArgs::parse(argv)?;
    let allowed = |option: &str| {
        matches!(option, "id" | "clear")
            || [AGENT_CREATE, AGENT_CLONE]
                .iter()
                .flat_map(|declaration| declaration.options)
                .any(|declared| declared.name == option)
    };
    let usage = match (AgentCreateOperation::for_verb(verb), verb) {
        (Some(operation), _) => operation.declaration().usage,
        (None, "disable") => "agent disable takes options.id for the existing agent",
        _ => "agent options use hyphenated names; see the command help",
    };
    for option in parsed.options.keys() {
        if !allowed(option) {
            return Err(CommandGuidance {
                message: format!("unknown agent option --{option}. {usage}; do not supply set fields to this command"),
                next_call: json!({"argv":["agent",verb,"--help"]}),
            }
            .into());
        }
    }
    if let Some(argument) = parsed.positionals.first() {
        return Err(CommandGuidance {
            message: format!("unexpected positional argument {argument:?}. {usage}; do not supply a positional ID"),
            next_call: json!({"argv":["agent",verb,"--help"]}),
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
    Ok(AgentOptions {
        display_name: update("display-name", "display_name")?,
        description: update("description", "description")?,
        system_prompt: update("system-prompt", "system_prompt")?,
        id: parsed.one("id")?.map(ToOwned::to_owned),
        from: parsed.one("from")?.map(ToOwned::to_owned),
        root: update("root", "root")?,
        preset: update("preset", "preset")?,
        profile_id: update("profile", "profile_id")?,
        make_default: parsed.switches.contains("default"),
    })
}
