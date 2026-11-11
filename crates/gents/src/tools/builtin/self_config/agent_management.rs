use super::*;
use crate::session::output_render::{ordered, Ordered};
use crate::documents::{
    validate_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::documents::{Agent, AgentContext, BashTools, BuiltInTools, FileTools, HostTools, Tools};
use crate::store::config_client::{ConfigAccess, ConfigClientError, TransactionFailure};
use agent::admit;
use gents_protocol::agent_identity::AgentIdentityBinding;
use gents_protocol::operations::agent::{
    AgentCreateInput, AgentOperationError, AgentOperationKind, AgentReceipt,
};

/// A materializer failure: the operation's own refusal, or another owner's
/// failure on its way through. Keeping the two apart lets each front end
/// answer a refusal from its declared recovery and hand any other failure to
/// that owner's classifiers unchanged.
#[derive(Debug)]
pub(super) enum AgentApplyError {
    Operation(AgentOperationError),
    Other(anyhow::Error),
}

impl From<AgentOperationError> for AgentApplyError {
    fn from(error: AgentOperationError) -> Self {
        Self::Operation(error)
    }
}

impl From<anyhow::Error> for AgentApplyError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

impl From<ConfigClientError> for AgentApplyError {
    fn from(error: ConfigClientError) -> Self {
        Self::Other(error.into())
    }
}

impl From<serde_json::Error> for AgentApplyError {
    fn from(error: serde_json::Error) -> Self {
        Self::Other(error.into())
    }
}

impl From<AgentApplyError> for anyhow::Error {
    fn from(error: AgentApplyError) -> Self {
        match error {
            AgentApplyError::Operation(error) => error.into(),
            AgentApplyError::Other(error) => error,
        }
    }
}

impl From<AgentApplyError> for AgentOperationError {
    fn from(error: AgentApplyError) -> Self {
        match error {
            AgentApplyError::Operation(error) => error,
            AgentApplyError::Other(error) => {
                Self::Passthrough(error.reallocate_into_boxed_dyn_error_without_backtrace())
            }
        }
    }
}

impl TransactionFailure for AgentApplyError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Operation(_) => None,
            Self::Other(error) => error.config_client_error(),
        }
    }
}

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
                    .get("default_agent_did")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned);
            }
            _ => {}
        }
    }
    Ok(view)
}

pub(super) async fn load_agent_catalog(core: &SelfConfigCore) -> Result<agent::AgentCatalogView> {
    let node = core.node_handle();
    let owner = core.node_did();
    ConfigAccess::transact_local_readonly(
        &node,
        Some(core.identity()?),
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

/// The receipt as the model reads it: fields in declaration order, each
/// value compact.
pub(super) fn receipt_text(receipt: &AgentReceipt<Agent>) -> Result<String> {
    Ordered::from_serialize(receipt)?.pretty()
}

fn create_params(input: &AgentCreateInput, preview: bool) -> ConfigureAgentParams {
    let operation = input.operation.kind().as_str();
    ConfigureAgentParams {
        action: if preview { "preview" } else { operation }.to_owned(),
        identity_binding: None,
        operation: preview.then(|| operation.to_owned()),
        display_name: StringUpdate::Set(input.display_name.clone()),
        description: input.description.clone(),
        system_prompt: input.system_prompt.clone(),
        agent_did: None,
        clone_from: input.clone_from.clone(),
        root: input.root.clone(),
        preset: input
            .preset
            .clone()
            .map(StringUpdate::Set)
            .unwrap_or_default(),
        profile_id: StringUpdate::Set(input.profile_id.clone()),
        make_default: input.make_default,
        enable_lsp: None,
        network_mode: None,
    }
}

#[cfg(test)]
pub(super) async fn agent_mutate(
    node: &Arc<EmbeddedNode>,
    owner: &str,
    identity: &dyn NodeIdentity,
    args: &ConfigureAgentParams,
    ceiling: &crate::tools::surface::SelfConfigProcessCeiling,
) -> Result<String> {
    anyhow::ensure!(
        identity.did() == owner,
        "agent writes require the exact local node identity"
    );
    agent_apply(node, owner, args, ceiling, false).await
}

pub async fn configure_agent(
    access: &ConfigAccess,
    owner: &str,
    args: &ConfigureAgentParams,
    ceiling: &crate::tools::surface::SelfConfigProcessCeiling,
) -> Result<String> {
    let receipt = agent_apply_access(
        access,
        owner,
        args,
        ceiling,
        args.action == "preview",
        None,
        None,
    )
    .await?;
    receipt_text(&receipt)
}

/// `operations::agent::create`: the caller's access decides who signs, and
/// no held-grant bound applies because the caller is not an Agent.
pub(crate) async fn create_agent(
    access: &ConfigAccess,
    owner: &str,
    identity_binding: AgentIdentityBinding,
    input: &AgentCreateInput,
    ceiling: &crate::tools::surface::SelfConfigProcessCeiling,
) -> Result<AgentReceipt<Agent>, AgentOperationError> {
    let mut args = create_params(input, false);
    args.identity_binding = Some(identity_binding);
    Ok(agent_apply_access(access, owner, &args, ceiling, false, None, None).await?)
}

#[cfg(test)]
async fn agent_apply(
    node: &Arc<EmbeddedNode>,
    owner: &str,
    args: &ConfigureAgentParams,
    ceiling: &crate::tools::surface::SelfConfigProcessCeiling,
    preview: bool,
) -> Result<String> {
    let receipt = agent_apply_access(
        &ConfigAccess::local(node.clone()),
        owner,
        args,
        ceiling,
        preview,
        None,
        None,
    )
    .await?;
    receipt_text(&receipt)
}

pub(super) async fn model_agent_preview(
    core: &SelfConfigCore,
    args: &ConfigureAgentParams,
) -> Result<String> {
    let mut args = args.clone();
    if matches!(
        args.operation.as_deref().unwrap_or(&args.action),
        "create" | "clone"
    ) {
        let runtime = core.runtime_identity()?;
        anyhow::ensure!(
            runtime.did() == core.node_did(),
            "runtime identity does not match node"
        );
        args.identity_binding = Some(crate::store::identity::provision_agent_identity(runtime)?);
    }
    match agent_apply_access(
        &core.access()?,
        core.node_did(),
        &args,
        core.process_ceiling(),
        true,
        Some(core.held_grants()),
        core.node_binding(),
    )
    .await
    {
        Ok(receipt) => receipt_text(&receipt),
        Err(error) => {
            let error = anyhow::Error::from(error);
            ordered! {"admitted":false,"committed":false,"rejection":format!("{error:#}")}.pretty()
        }
    }
}

pub(super) async fn model_agent_create_preview(
    core: &SelfConfigCore,
    input: &AgentCreateInput,
) -> Result<String> {
    model_agent_preview(core, &create_params(input, true)).await
}

pub(super) async fn model_agent_mutate(
    core: &SelfConfigCore,
    identity: &dyn NodeIdentity,
    args: &ConfigureAgentParams,
) -> Result<String> {
    let receipt = model_agent_apply(core, identity, args.clone()).await?;
    receipt_text(&receipt)
}

pub(super) async fn model_agent_create(
    core: &SelfConfigCore,
    identity: &dyn NodeIdentity,
    input: &AgentCreateInput,
) -> Result<AgentReceipt<Agent>, AgentApplyError> {
    model_agent_apply(core, identity, create_params(input, false)).await
}

async fn model_agent_apply(
    core: &SelfConfigCore,
    identity: &dyn NodeIdentity,
    mut args: ConfigureAgentParams,
) -> Result<AgentReceipt<Agent>, AgentApplyError> {
    let actor = core.identity()?;
    if identity.did() != actor.to_string() {
        return Err(anyhow!("agent configuration requires its selected actor signer").into());
    }
    if matches!(
        args.operation.as_deref().unwrap_or(&args.action),
        "create" | "clone"
    ) {
        args.identity_binding = Some(
            crate::store::identity::provision_agent_identity_for_agent(
                &core.node_handle(),
                core.runtime_identity()?,
                identity,
                core.agent_did(),
            )
            .await
            .map_err(anyhow::Error::from)?,
        );
    }
    agent_apply_access(
        &ConfigAccess::local_managed_agent(core.node_handle(), actor, identity)?,
        core.node_did(),
        &args,
        core.process_ceiling(),
        false,
        Some(core.held_grants()),
        core.node_binding(),
    )
    .await
}

async fn agent_apply_access(
    access: &ConfigAccess,
    owner: &str,
    args: &ConfigureAgentParams,
    ceiling: &crate::tools::surface::SelfConfigProcessCeiling,
    preview: bool,
    held: Option<&ops::OperatorGrants>,
    node_binding: Option<&crate::documents::ConfigurationNodeBinding>,
) -> Result<AgentReceipt<Agent>, AgentApplyError> {
    if preview {
        access
            .transact_readonly("self_config.agent", |txn| {
                Box::pin(agent_apply_in_txn(
                    txn,
                    owner,
                    args,
                    ceiling,
                    preview,
                    held,
                    node_binding,
                ))
            })
            .await
    } else {
        access
            .transact_write("self_config.agent", |txn| {
                Box::pin(agent_apply_in_txn(
                    txn,
                    owner,
                    args,
                    ceiling,
                    preview,
                    held,
                    node_binding,
                ))
            })
            .await
    }
}

async fn agent_apply_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    args: &ConfigureAgentParams,
    ceiling: &crate::tools::surface::SelfConfigProcessCeiling,
    preview: bool,
    held: Option<&ops::OperatorGrants>,
    node_binding: Option<&crate::documents::ConfigurationNodeBinding>,
) -> Result<AgentReceipt<Agent>, AgentApplyError> {
    let refs = crate::ConfigReferences::load_in_txn(txn, owner).await?;
    let view = catalog_from_documents(&refs)?;
    admit(
        args.action != "preview" || args.operation.is_some(),
        AgentOperationError::PreviewRequiresOperation,
    )?;
    let operation = args.operation.as_deref().unwrap_or(&args.action);
    if operation == "edit" {
        admit(
            args.clone_from.is_none(),
            AgentOperationError::EditRejectsCloneFrom,
        )?;
        admit(
            !args.root.is_present()
                && !args.preset.is_present()
                && args.enable_lsp.is_none()
                && args.network_mode.is_none(),
            AgentOperationError::EditRejectsToolsFields,
        )?;
    }
    if operation == "disable" {
        admit(
            !args.display_name.is_present()
                && !args.description.is_present()
                && !args.system_prompt.is_present()
                && !args.profile_id.is_present()
                && !args.root.is_present()
                && !args.preset.is_present()
                && args.clone_from.is_none()
                && args.enable_lsp.is_none()
                && args.network_mode.is_none(),
            AgentOperationError::DisableAcceptsOnlyTarget,
        )?;
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
    let (kind, op) = match operation {
        "create" | "clone" => {
            let kind = if operation == "clone" {
                AgentOperationKind::Clone
            } else {
                AgentOperationKind::Create
            };
            let request = agent::AgentCreateRequest {
                display_name: args.display_name.value().unwrap_or_default().to_owned(),
                system_prompt: args.system_prompt.value().unwrap_or_default().to_owned(),
                inference_profile_id: args.profile_id.value().unwrap_or_default().to_owned(),
                clone_from: if operation == "clone" {
                    Some(
                        args.clone_from
                            .clone()
                            .ok_or(AgentOperationError::CloneRequiresFrom)?,
                    )
                } else {
                    None
                },
            };
            (kind, agent::AgentOperation::Create(request))
        }
        "edit" => (AgentOperationKind::Edit, agent::AgentOperation::Edit(patch)),
        "disable" => (AgentOperationKind::Disable, agent::AgentOperation::Disable),
        _ => return Err(AgentOperationError::UnknownOperation(operation.to_owned()).into()),
    };
    let id = match &op {
        agent::AgentOperation::Create(_) => args
            .identity_binding
            .as_ref()
            .ok_or(AgentOperationError::IdentityBindingRequired)?
            .agent_did
            .clone(),
        _ => args
            .agent_did
            .clone()
            .ok_or(AgentOperationError::AgentDidRequired)?,
    };
    agent::decide_agent_operation(&view, &op, &id, args.make_default)?;
    let load = |collection: crate::Collection, id: &str| {
        refs.documents()
            .find(|((c, key), _)| *c == collection && key == id)
            .map(|(_, value)| value.clone())
            .ok_or_else(|| AgentOperationError::MissingDocument {
                collection: collection.graphql_type(),
                id: id.to_owned(),
            })
    };
    let mut documents = Vec::new();
    let mut doc: Agent;
    let mut root_access = None;
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
            admit(
                !refs.documents().any(|((c, key), _)| {
                    *c == crate::Collection::AgentContext && key == &context.context_id
                }),
                AgentOperationError::GeneratedContextIdExists,
            )?;
            context.display_name = Some(input.display_name.clone());
            if args.system_prompt.is_present() {
                context.system_prompt = args.system_prompt.owned_value();
            }
            if args.description.is_present() {
                context.description = args.description.owned_value();
            }
            let mut tools: Option<Tools> = match context.tools_id.as_deref() {
                Some(id) => Some(serde_json::from_value(load(crate::Collection::Tools, id)?)?),
                None => None,
            };
            if operation == "clone" {
                if let Some(tools) = tools.as_mut() {
                    tools.self_config = None;
                }
            }
            if let Some(preset) = args.preset.value() {
                admit(
                    operation != "clone",
                    AgentOperationError::ClonePresetRefused,
                )?;
                let fields = preset_fields(preset).ok_or(AgentOperationError::UnknownPreset)?;
                let write = preset == crate::tools::surface::presets::PRESET_WRITE;
                tools = Some(Tools {
                    host: Some(HostTools {
                        files: Some(FileTools {
                            mode: crate::documents::FileToolMode::parse(&fields.file_tools_mode)?,
                            ..Default::default()
                        }),
                        bash: Some(BashTools {
                            mode: crate::documents::BashMode::parse(&fields.bash_mode)?,
                            execution_mode: write.then_some(if cfg!(target_os = "macos") {
                                crate::tools::native::CommandExecutionMode::WorkspaceWrite
                            } else {
                                crate::tools::native::CommandExecutionMode::Unrestricted
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
            if args.enable_lsp.is_some() || args.network_mode.is_some() {
                tools.get_or_insert_with(Tools::default);
            }
            if let Some(mut tools) = tools {
                tools.tools_id = format!("{id}:tools");
                admit(
                    !refs.documents().any(|((c, key), _)| {
                        *c == crate::Collection::Tools && key == &tools.tools_id
                    }),
                    AgentOperationError::GeneratedToolsIdExists,
                )?;
                tools.node_did = owner.to_owned();
                let policy = crate::tools::surface::load_workspace_root_policy_in_txn(
                    txn,
                    ceiling.root.as_deref(),
                    owner,
                )
                .await?;
                let stored_root = if args.root.is_present() {
                    None
                } else {
                    tools.host.as_ref().and_then(|host| host.root.clone())
                };
                crate::tools::surface::canonicalize_tools_root(
                    &mut tools,
                    &policy,
                    stored_root.as_deref(),
                )?;
                apply_tool_grant_selection(&mut tools, args.enable_lsp, args.network_mode);
                // A new Agent DID holds no WorkspaceRoot relationship yet.
                let published = match crate::tools::surface::tools_root_reader_in_txn(
                    txn, owner, &policy, &tools,
                )
                .await?
                {
                    crate::tools::surface::ToolRootReader::NotNeeded => None,
                    crate::tools::surface::ToolRootReader::Published(root) => Some(Some(root)),
                    crate::tools::surface::ToolRootReader::Unpublished(_) => Some(None),
                };
                if let Some(published) = published {
                    root_access = Some(crate::tools::surface::reader_grant_step(
                        &id,
                        published.as_deref(),
                        true,
                    ));
                }
                validate_tool_network_selection(args.network_mode)?;
                context.tools_id = Some(tools.tools_id.clone());
                if let Some(held) = held {
                    let candidate = serde_json::to_value(&tools)?;
                    ops::guard_tools_keep_grants(
                        held,
                        None,
                        candidate
                            .as_object()
                            .ok_or(AgentOperationError::ObjectRequired("Tools"))?,
                    )
                    .map_err(|source| {
                        AgentOperationError::CloneSourceGrantNotHeld {
                            from: input.clone_from.clone().unwrap_or_default(),
                            source: source.into(),
                        }
                    })?;
                }
                documents.push(replacement(crate::Collection::Tools, tools)?);
            }
            doc = serde_json::from_value(
                json!({"agent_did":id,"creation_nonce":args.identity_binding.as_ref().ok_or(AgentOperationError::IdentityBindingMissing)?.creation_nonce,"node_did":owner,"display_name":input.display_name,"description":args.description.value(),"context_id":context.context_id,"inference_profile_id":input.inference_profile_id}),
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
            let mut merged = stored
                .as_object()
                .ok_or(AgentOperationError::ObjectRequired("agent"))?
                .clone();
            let agent_patch = patch
                .iter()
                .filter(|(field, _)| field != "system_prompt")
                .cloned()
                .collect();
            merged = crate::documents::patch::apply_patch(
                SelfConfigTarget::Agent,
                &merged,
                &agent_patch,
            );
            if let Some(held) = held {
                let selected_tools = |agent: &serde_json::Map<String, Value>| -> Result<
                    Option<serde_json::Map<String, Value>>,
                    AgentOperationError,
                > {
                    let Some(context_id) = agent.get("context_id").and_then(Value::as_str) else {
                        return Ok(None);
                    };
                    let context = load(crate::Collection::AgentContext, context_id)?;
                    let Some(tools_id) = context.get("tools_id").and_then(Value::as_str) else {
                        return Ok(None);
                    };
                    Ok(Some(
                        load(crate::Collection::Tools, tools_id)?
                            .as_object()
                            .ok_or(AgentOperationError::ObjectRequired("Tools"))?
                            .clone(),
                    ))
                };
                let before = selected_tools(
                    stored
                        .as_object()
                        .ok_or(AgentOperationError::ObjectRequired("Agent"))?,
                )?;
                let after = selected_tools(&merged)?;
                ops::reselection_keeps_grants(held, before.as_ref(), after.as_ref())?;
            }
            if args.system_prompt.is_present() {
                let context_id = merged
                    .get("context_id")
                    .and_then(Value::as_str)
                    .ok_or(AgentOperationError::AgentHasNoContext)?;
                let sharers = refs
                    .documents()
                    .filter(|((c, _), value)| {
                        *c == crate::Collection::Agent
                            && value.get("context_id").and_then(Value::as_str) == Some(context_id)
                    })
                    .count();
                admit(sharers == 1, AgentOperationError::SystemPromptContextShared)?;
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
            .ok_or(AgentOperationError::ObjectRequired("node"))?
            .insert("default_agent_did".into(), json!(id));
        documents.push(replacement(crate::Collection::Node, node)?);
    }
    let plan = DesiredStateApplyPlan::new(documents)?;
    validate_desired_state_plan(
        txn,
        &plan,
        &crate::inference::resolved::ProviderProfilePolicy,
    )
    .await?;
    if doc.enabled && !matches!(op, agent::AgentOperation::Disable) {
        let mut merged: BTreeMap<_, _> = refs
            .documents()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        for replacement in plan.documents() {
            let key = replacement.add[replacement.collection.unique_field()]
                .as_str()
                .ok_or(AgentOperationError::CandidateIdentityMissing)?
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
        admit(
            agent::materialize_agent(&view, &op, &id, args.make_default, &candidate, owner)
                .is_some(),
            AgentOperationError::CandidateDoesNotResolve,
        )?;
    }
    if !preview {
        crate::documents::apply_desired_state_plan_with_node_binding(
            txn,
            &plan,
            node_binding,
            &crate::inference::resolved::ProviderProfilePolicy,
        )
        .await?;
    }
    Ok(AgentReceipt {
        committed: !preview,
        admitted: true,
        operation: kind,
        agent_did: id,
        host_root_access: root_access,
        agent: doc,
        make_default: args.make_default,
    })
}

/// Lean `SelfConfig.yoloGroup`: the User grants `agent_did`, an Agent the
/// User created as its own unrestricted agent, the self-configuration of
/// `source_did` (its Tools' `self_config` group) and native write on its own
/// selected configuration (`grant_selected_self_configuration`, as `gents
/// init` grants the Engineer), in one transaction as the User. It grants no
/// node permission set and no relation on any other Agent. Granting again
/// changes nothing.
pub async fn grant_own_self_configuration(
    access: &ConfigAccess,
    node_did: &str,
    agent_did: &str,
    source_did: &str,
) -> Result<()> {
    access
        .transact_write("self_config.grant_own", |txn| {
            Box::pin(async move {
                let source = crate::documents::load_agent_tools_in_txn(txn, node_did, source_did)
                    .await?
                    .with_context(|| format!("{source_did} selects no Tools"))?;
                let mut tools = crate::documents::load_agent_tools_in_txn(txn, node_did, agent_did)
                    .await?
                    .with_context(|| format!("{agent_did} selects no Tools"))?;
                if serde_json::to_value(&tools.self_config)?
                    != serde_json::to_value(&source.self_config)?
                {
                    tools.self_config = source.self_config;
                    tools.validate()?;
                    let value = serde_json::to_value(&tools)?;
                    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                        collection: crate::Collection::Tools,
                        add: value.clone(),
                        update: value,
                    }])?;
                    crate::documents::apply_desired_state_plan(
                        txn,
                        &plan,
                        &crate::inference::resolved::ProviderProfilePolicy,
                    )
                    .await?;
                }
                crate::documents::grant_selected_self_configuration(txn, node_did, agent_did).await
            })
        })
        .await
}
