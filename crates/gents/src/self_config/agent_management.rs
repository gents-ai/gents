use super::*;
use crate::config_client::{
    apply_desired_state_plan, validate_desired_state_plan, ConfigAccess, DesiredStateApplyDocument,
    DesiredStateApplyPlan,
};
use crate::document_config::{
    Agent, AgentContext, BashTools, BuiltInTools, FileTools, HostTools, Tools,
};

fn catalog_from_documents(refs: &crate::ConfigReferences) -> Result<agent::AgentCatalogView> {
    let mut view = agent::AgentCatalogView::default();
    for ((collection, id), value) in refs.documents() {
        match collection {
            crate::Collection::Agent => {
                let doc: Agent = serde_json::from_value(value.clone())?;
                view.agents.insert(id.clone(), doc.enabled);
                if doc.tags.iter().any(|tag| tag == ENGINEER_AGENT_TAG) {
                    view.protected_ids.insert(id.clone());
                }
            }
            crate::Collection::InferenceProfile => {
                view.published_profiles.insert(id.clone());
            }
            crate::Collection::Node => {
                view.default_id = value
                    .get("default_agent_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned);
            }
            _ => {}
        }
    }
    Ok(view)
}

pub(super) async fn load_agent_catalog(
    node: &Arc<EmbeddedNode>,
    owner: &str,
) -> Result<agent::AgentCatalogView> {
    ConfigAccess::transact_local(
        node,
        Some(::identity::Did::new(owner.to_owned())?),
        "self_config.agent_catalog",
        |txn| {
            Box::pin(async move {
                catalog_from_documents(&crate::ConfigReferences::load_in_txn(txn, owner).await?)
            })
        },
    )
    .await
}

fn replacement(
    collection: crate::Collection,
    value: impl serde::Serialize,
) -> Result<DesiredStateApplyDocument> {
    let value = serde_json::to_value(value)?;
    Ok(DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    })
}

pub(super) async fn agent_preview(
    node: &Arc<EmbeddedNode>,
    owner: &str,
    args: &ConfigureAgentParams,
    ceiling: &crate::tool_surface::SelfConfigProcessCeiling,
) -> Result<String> {
    match agent_apply(node, owner, args, ceiling, true).await {
        Ok(result) => Ok(result),
        Err(error) => {
            ordered! {"admitted":false,"committed":false,"rejection":format!("{error:#}")}.pretty()
        }
    }
}

pub(super) async fn agent_mutate(
    node: &Arc<EmbeddedNode>,
    owner: &str,
    identity: &dyn NodeIdentity,
    args: &ConfigureAgentParams,
    ceiling: &crate::tool_surface::SelfConfigProcessCeiling,
) -> Result<String> {
    anyhow::ensure!(
        identity.did() == owner,
        "agent writes require the exact local node identity"
    );
    agent_apply(node, owner, args, ceiling, false).await
}

/// Publish or preview a node-owned agent operation through the canonical
/// configuration transaction owner. The supplied identity must own the target
/// node; HTTP access additionally retains its endpoint's DefraDB authentication.
/// Direct prompt edits require an existing, unshared Context, so they cannot
/// alter another Agent's instructions or write into the absent-context fallback.
pub async fn configure_agent(
    access: &ConfigAccess,
    owner: &str,
    identity: &dyn NodeIdentity,
    args: &ConfigureAgentParams,
    ceiling: &crate::tool_surface::SelfConfigProcessCeiling,
) -> Result<String> {
    anyhow::ensure!(
        identity.did() == owner,
        "agent writes require the exact local node identity"
    );
    agent_apply_access(access, owner, args, ceiling, args.action == "preview", None).await
}

async fn agent_apply(
    node: &Arc<EmbeddedNode>,
    owner: &str,
    args: &ConfigureAgentParams,
    ceiling: &crate::tool_surface::SelfConfigProcessCeiling,
    preview: bool,
) -> Result<String> {
    agent_apply_access(
        &ConfigAccess::Local(node.clone()),
        owner,
        args,
        ceiling,
        preview,
        None,
    )
    .await
}

pub(super) async fn model_agent_preview(
    core: &SelfConfigCore,
    args: &ConfigureAgentParams,
) -> Result<String> {
    match agent_apply_access(
        &ConfigAccess::Local(core.node_handle()),
        core.node_did(),
        args,
        core.process_ceiling(),
        true,
        Some(core.held_grants()),
    )
    .await
    {
        Ok(result) => Ok(result),
        Err(error) => {
            ordered! {"admitted":false,"committed":false,"rejection":format!("{error:#}")}.pretty()
        }
    }
}

pub(super) async fn model_agent_mutate(
    core: &SelfConfigCore,
    identity: &dyn NodeIdentity,
    args: &ConfigureAgentParams,
) -> Result<String> {
    anyhow::ensure!(
        identity.did() == core.node_did(),
        "agent writes require the exact local node identity"
    );
    agent_apply_access(
        &ConfigAccess::Local(core.node_handle()),
        core.node_did(),
        args,
        core.process_ceiling(),
        false,
        Some(core.held_grants()),
    )
    .await
}

async fn agent_apply_access(
    access: &ConfigAccess,
    owner: &str,
    args: &ConfigureAgentParams,
    ceiling: &crate::tool_surface::SelfConfigProcessCeiling,
    preview: bool,
    held: Option<&ops::OperatorGrants>,
) -> Result<String> {
    match access {
        ConfigAccess::Local(node) => {
            ConfigAccess::transact_local(
                node,
                Some(::identity::Did::new(owner.to_owned())?),
                "self_config.agent",
                |txn| Box::pin(agent_apply_in_txn(txn, owner, args, ceiling, preview, held)),
            )
            .await
        }
        ConfigAccess::Graphql(_) => {
            access
                .transact("self_config.agent", |txn| {
                    Box::pin(agent_apply_in_txn(txn, owner, args, ceiling, preview, held))
                })
                .await
        }
    }
}

async fn agent_apply_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    args: &ConfigureAgentParams,
    ceiling: &crate::tool_surface::SelfConfigProcessCeiling,
    preview: bool,
    held: Option<&ops::OperatorGrants>,
) -> Result<String> {
    let refs = crate::ConfigReferences::load_in_txn(txn, owner).await?;
    let view = catalog_from_documents(&refs)?;
    anyhow::ensure!(
        args.action != "preview" || args.operation.is_some(),
        "agent preview requires operation create, clone, edit, or disable"
    );
    let operation = args.operation.as_deref().unwrap_or(&args.action);
    if operation == "edit" {
        anyhow::ensure!(
            args.clone_from.is_none(),
            "agent edit does not accept clone_from; use agent clone"
        );
        anyhow::ensure!(!args.root.is_present() && !args.preset.is_present() && args.enable_lsp.is_none() && args.enable_graph_tools.is_none() && args.network_mode.is_none(), "agent edit changes Agent fields and its system_prompt; change root and tool grants through tools update");
    }
    if operation == "disable" {
        anyhow::ensure!(
            !args.display_name.is_present()
                && !args.description.is_present()
                && !args.system_prompt.is_present()
                && !args.profile_id.is_present()
                && !args.root.is_present()
                && !args.preset.is_present()
                && args.clone_from.is_none()
                && args.enable_lsp.is_none()
                && args.enable_graph_tools.is_none()
                && args.network_mode.is_none(),
            "agent disable accepts only the target ID"
        );
    }
    let mut patch = Vec::new();
    for (name, update) in [
        ("display_name", &args.display_name),
        ("description", &args.description),
        ("system_prompt", &args.system_prompt),
        ("inference_profile_id", &args.profile_id),
    ] {
        if update.is_present() {
            patch.push((name.to_owned(), update.value().map(|value| json!(value))));
        }
    }
    let op = match operation {
        "create" | "clone" => agent::AgentOperation::Create(agent::AgentCreateRequest {
            display_name: args.display_name.value().unwrap_or_default().to_owned(),
            system_prompt: args.system_prompt.value().unwrap_or_default().to_owned(),
            inference_profile_id: args.profile_id.value().unwrap_or_default().to_owned(),
            clone_from: if operation == "clone" {
                Some(args.clone_from.clone().context("clone requires from")?)
            } else {
                None
            },
        }),
        "edit" => agent::AgentOperation::Edit(patch),
        "disable" => agent::AgentOperation::Disable,
        _ => bail!("unknown agent operation {operation:?}"),
    };
    let id = match &op {
        agent::AgentOperation::Create(input) => {
            derive_agent_id(owner, &input.display_name, &view.agents)
        }
        _ => args
            .agent_id
            .clone()
            .context("agent operation requires an exact agent_id")?,
    };
    agent::decide_agent_operation(&view, &op, &id, args.make_default)?;
    let load = |collection: crate::Collection, id: &str| {
        refs.documents()
            .find(|((c, key), _)| *c == collection && key == id)
            .map(|(_, value)| value.clone())
            .with_context(|| format!("missing {} {id:?}", collection.graphql_type()))
    };
    let mut documents = Vec::new();
    let mut doc: Agent;
    match &op {
        agent::AgentOperation::Create(input) => {
            let mut context: AgentContext = match input.clone_from.as_deref() {
                Some(source) => {
                    let source: Agent =
                        serde_json::from_value(load(crate::Collection::Agent, source)?)?;
                    match source.context_id {
                        Some(id) => {
                            serde_json::from_value(load(crate::Collection::AgentContext, &id)?)?
                        }
                        None => serde_json::from_value(json!({"context_id":"", "node_did":owner}))?,
                    }
                }
                None => serde_json::from_value(json!({"context_id":"", "node_did":owner}))?,
            };
            context.context_id = format!("{id}:context");
            anyhow::ensure!(
                !refs
                    .documents()
                    .any(|((c, key), _)| *c == crate::Collection::AgentContext
                        && key == &context.context_id),
                "generated context ID already exists"
            );
            context.display_name = Some(input.display_name.clone());
            if args.system_prompt.is_present() {
                context.system_prompt = args.system_prompt.owned_value();
            }
            if args.description.is_present() {
                context.description = args.description.owned_value();
            }
            // Clone tool grants into a distinct document, so later edits cannot change the source.
            let mut tools: Option<Tools> = match context.tools_id.as_deref() {
                Some(id) => Some(serde_json::from_value(load(crate::Collection::Tools, id)?)?),
                None => None,
            };
            if let Some(preset) = args.preset.value() {
                anyhow::ensure!(
                    operation != "clone",
                    "clone inherits its source tools; edit the cloned Tools to change grants"
                );
                let fields = preset_fields(preset).context("unknown permission preset")?;
                let write = preset == crate::tool_surface::presets::PRESET_WRITE;
                tools = Some(Tools {
                    host: Some(HostTools {
                        files: Some(FileTools {
                            mode: crate::tool_surface::FileToolMode::parse(
                                &fields.file_tools_mode,
                            )?,
                            ..Default::default()
                        }),
                        bash: Some(BashTools {
                            mode: crate::tool_surface::BashMode::parse(&fields.bash_mode)?,
                            execution_mode: write.then_some(if cfg!(target_os = "macos") {
                                crate::toolset::CommandExecutionMode::WorkspaceWrite
                            } else {
                                crate::toolset::CommandExecutionMode::Unrestricted
                            }),
                            background_enabled: write,
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    built_ins: Some(BuiltInTools {
                        enable_context_budget: Some(true),
                        ..Default::default()
                    }),
                    ..Default::default()
                });
            }
            if args.root.is_present() {
                tools
                    .get_or_insert_with(Tools::default)
                    .host
                    .get_or_insert_with(HostTools::default)
                    .root = args.root.owned_value();
            }
            if args.enable_lsp.is_some()
                || args.enable_graph_tools.is_some()
                || args.network_mode.is_some()
            {
                tools.get_or_insert_with(Tools::default);
            }
            if let Some(mut tools) = tools {
                tools.tools_id = format!("{id}:tools");
                anyhow::ensure!(
                    !refs
                        .documents()
                        .any(|((c, key), _)| *c == crate::Collection::Tools
                            && key == &tools.tools_id),
                    "generated tools ID already exists"
                );
                tools.node_did = owner.to_owned();
                let policy = crate::tool_surface::load_workspace_root_policy_in_txn(
                    txn,
                    ceiling.root.as_deref(),
                )
                .await?;
                crate::tool_surface::canonicalize_tools_root(&mut tools, &policy)?;
                apply_tool_grant_selection(
                    &mut tools,
                    args.enable_lsp,
                    args.enable_graph_tools,
                    args.network_mode,
                );
                validate_tool_network_selection(args.network_mode)?;
                context.tools_id = Some(tools.tools_id.clone());
                if let Some(held) = held {
                    let candidate = serde_json::to_value(&tools)?;
                    ops::guard_tools_keep_grants(
                        held,
                        None,
                        candidate.as_object().context("Tools object required")?,
                    ).with_context(|| format!("clone source {:?} carries an operator grant this agent does not hold; clone a source without it or ask the operator to grant it", input.clone_from.as_deref().unwrap_or_default()))?;
                }
                documents.push(replacement(crate::Collection::Tools, tools)?);
            }
            doc = serde_json::from_value(
                json!({"agent_id":id,"node_did":owner,"display_name":input.display_name,"description":args.description.value(),"context_id":context.context_id,"inference_profile_id":input.inference_profile_id}),
            )?;
            guard_agent_profile_choice(
                txn,
                owner,
                None,
                input.clone_from.as_deref(),
                Some(&input.inference_profile_id),
            )
            .await?;
            documents.push(replacement(crate::Collection::AgentContext, context)?);
        }
        agent::AgentOperation::Edit(patch) => {
            let stored = load(crate::Collection::Agent, &id)?;
            let mut merged = stored.as_object().context("agent object required")?.clone();
            let agent_patch = patch
                .iter()
                .filter(|(field, _)| field != "system_prompt")
                .cloned()
                .collect();
            merged = crate::config_client::patch::apply_patch(
                SelfConfigTarget::Agent,
                &merged,
                &agent_patch,
            );
            if let Some(held) = held {
                let selected_tools = |agent: &serde_json::Map<String, Value>| -> Result<Option<serde_json::Map<String, Value>>> {
                    let Some(context_id) = agent.get("context_id").and_then(Value::as_str) else { return Ok(None); };
                    let context = load(crate::Collection::AgentContext, context_id)?;
                    let Some(tools_id) = context.get("tools_id").and_then(Value::as_str) else { return Ok(None); };
                    Ok(Some(load(crate::Collection::Tools, tools_id)?.as_object().context("Tools object required")?.clone()))
                };
                let before = selected_tools(stored.as_object().context("Agent object required")?)?;
                let after = selected_tools(&merged)?;
                ops::reselection_keeps_grants(held, before.as_ref(), after.as_ref())?;
            }
            if args.system_prompt.is_present() {
                let context_id = merged
                    .get("context_id")
                    .and_then(Value::as_str)
                    .context("agent has no context; create and select a context first")?;
                let sharers = refs
                    .documents()
                    .filter(|((c, _), value)| {
                        *c == crate::Collection::Agent
                            && value.get("context_id").and_then(Value::as_str) == Some(context_id)
                    })
                    .count();
                anyhow::ensure!(
                    sharers == 1,
                    "system_prompt edit requires an unshared context"
                );
                let mut context: AgentContext =
                    serde_json::from_value(load(crate::Collection::AgentContext, context_id)?)?;
                context.system_prompt = args.system_prompt.owned_value();
                documents.push(replacement(crate::Collection::AgentContext, context)?);
            }
            guard_agent_profile_choice(txn, owner, Some(&id), None, args.profile_id.value())
                .await?;
            doc = serde_json::from_value(Value::Object(merged))?;
        }
        agent::AgentOperation::Disable => {
            doc = serde_json::from_value(load(crate::Collection::Agent, &id)?)?;
            doc.enabled = false;
        }
    }
    documents.push(replacement(crate::Collection::Agent, &doc)?);
    if args.make_default {
        let mut node = load(crate::Collection::Node, owner)?;
        node.as_object_mut()
            .context("node object required")?
            .insert("default_agent_id".into(), json!(id));
        documents.push(replacement(crate::Collection::Node, node)?);
    }
    let plan = DesiredStateApplyPlan::new(documents)?;
    validate_desired_state_plan(txn, &plan).await?;
    if doc.enabled && !matches!(op, agent::AgentOperation::Disable) {
        let mut merged: BTreeMap<_, _> = refs
            .documents()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        for replacement in plan.documents() {
            let key = replacement.add[replacement.collection.unique_field()]
                .as_str()
                .context("candidate identity missing")?
                .to_owned();
            merged.insert((replacement.collection, key), replacement.add.clone());
        }
        let mut candidate = agent::AgentCandidate::default();
        for ((collection, _), value) in merged {
            match collection {
                crate::Collection::Agent => candidate.agents.push(serde_json::from_value(value)?),
                crate::Collection::AgentContext => {
                    candidate.contexts.push(serde_json::from_value(value)?)
                }
                crate::Collection::InferenceProfile => candidate
                    .inference_profiles
                    .push(serde_json::from_value(value)?),
                crate::Collection::InferenceBackend => candidate
                    .inference_backends
                    .push(serde_json::from_value(value)?),
                _ => {}
            }
        }
        anyhow::ensure!(
            agent::materialize_agent(&view, &op, &id, args.make_default, &candidate, owner)
                .is_some(),
            "candidate agent does not resolve; inspect its context, profile and backend"
        );
    }
    if !preview {
        apply_desired_state_plan(txn, &plan).await?;
    }
    ordered! {"committed":!preview,"admitted":true,"operation":operation,"agent_id":id,"agent":doc,"make_default":args.make_default}.pretty()
}
