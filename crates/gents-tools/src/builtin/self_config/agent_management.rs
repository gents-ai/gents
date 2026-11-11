use super::*;
use agent::admit;
use gents_documents::{
    validate_desired_state_plan, DefaultAgentSelection, DesiredStateApplyDocument,
    DesiredStateApplyPlan,
};
use gents_documents::{Agent, AgentContext, BashTools, BuiltInTools, FileTools, HostTools, Tools};
use gents_protocol::agent_identity::{AgentAdmissionFacts, AgentIdentityBinding};
use gents_protocol::operations::agent::{
    AgentCreateInput, AgentCreateOperation, AgentOperationError, AgentOperationKind, AgentOutcome,
    AgentReceipt, NextCall,
};
use gents_session::session::output_render::Ordered;
use gents_store::config_client::{ConfigAccess, ConfigClientError, TransactionFailure};

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

fn catalog_from_documents(
    refs: &gents_documents::ConfigReferences,
) -> Result<agent::AgentCatalogView> {
    let mut view = agent::AgentCatalogView::default();
    for ((collection, id), value) in refs.documents() {
        match collection {
            gents_store::collection::Collection::Agent => {
                let doc: Agent = serde_json::from_value(value.clone())?;
                view.agents.insert(id.clone(), doc.enabled);
            }
            gents_store::collection::Collection::InferenceProfile => {
                view.published_profiles.insert(id.clone());
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
                catalog_from_documents(
                    &gents_documents::ConfigReferences::load_in_txn(txn, owner).await?,
                )
            })
        },
    )
    .await
}

/// Lean `SelfConfig.ActorGrants` and the Node's current default, for one
/// materializer call.
pub(super) enum AdmissionFacts {
    Observed(AgentAdmissionFacts),
    /// Identity-less embedded access. DefraDB evaluates no document ACP for
    /// an operation without an identity, so such access holds every grant the
    /// admission reads; the default is read in the materializer's transaction.
    Unrestricted,
}

/// The one facts entry of the materializer. Embedded access with an actor
/// asks native ACP on its node directly; GraphQL access asks the runtime,
/// which answers for the endpoint's authenticated principal; identity-less
/// embedded access is [`AdmissionFacts::Unrestricted`].
async fn admission_facts(
    access: &ConfigAccess,
    node_did: &str,
    targets: &[String],
) -> Result<AdmissionFacts, AgentApplyError> {
    let facts = match (access, access.local_node(), access.actor_did()) {
        (ConfigAccess::Graphql(endpoint), _, _) => {
            gents_store::identity::observe_agent_admission_remote(endpoint, node_did, targets)
                .await
                .map_err(anyhow::Error::from)?
        }
        (_, Some(_), None) => return Ok(AdmissionFacts::Unrestricted),
        (_, Some(node), Some(actor)) => {
            let actor = ::identity::Did::new(actor).map_err(anyhow::Error::from)?;
            gents_store::identity::observe_agent_admission(node, node_did, &actor, targets)
                .await
                .map_err(anyhow::Error::from)?
        }
        (_, None, _) => {
            return Err(anyhow!("agent admission requires embedded or GraphQL access").into())
        }
    };
    Ok(AdmissionFacts::Observed(facts))
}

/// Lean `SelfConfig.Actor` from the facts, and the catalog's default.
async fn admission_actor(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    facts: &AdmissionFacts,
    actor_did: &str,
    view: &mut agent::AgentCatalogView,
) -> Result<agent::AgentActor, AgentApplyError> {
    Ok(match facts {
        AdmissionFacts::Observed(facts) => {
            view.default_id = facts.default_agent_did.clone();
            agent::AgentActor {
                did: actor_did.to_owned(),
                catalog: facts.catalog,
                configures: facts.configures.iter().cloned().collect(),
            }
        }
        AdmissionFacts::Unrestricted => {
            view.default_id = gents_documents::load_default_agent_did_in_txn(txn, owner).await?;
            agent::AgentActor {
                did: actor_did.to_owned(),
                catalog: true,
                configures: view.agents.keys().cloned().collect(),
            }
        }
    })
}

/// What a materializer call carries besides its request: the caller's
/// held-grant bound and Node binding when the caller is an Agent, and the
/// invoking Agent's own no-lockout chain.
#[derive(Clone, Copy, Default)]
pub(super) struct AgentCaller<'a> {
    pub held: Option<&'a ops::OperatorGrants>,
    pub node_binding: Option<&'a gents_documents::ConfigurationNodeBinding>,
    pub lockout: Option<&'a SelfConfigCore>,
}

fn replacement(
    collection: gents_store::collection::Collection,
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

/// The operation the parameters request: `operation` on a preview, else the
/// action.
fn requested_operation(args: &ConfigureAgentParams) -> &str {
    args.operation.as_deref().unwrap_or(&args.action)
}

/// The call that corrects a failure of the requested operation.
pub(super) fn recovery_for(args: &ConfigureAgentParams, error: &AgentOperationError) -> NextCall {
    match AgentCreateOperation::for_verb(requested_operation(args)) {
        Some(operation) => error.recovery(operation),
        None => error.edit_recovery(),
    }
}

fn create_params(input: &AgentCreateInput, preview: bool) -> ConfigureAgentParams {
    let operation = input.operation.kind().as_str();
    ConfigureAgentParams {
        action: if preview { "preview" } else { operation }.to_owned(),
        operation: preview.then(|| operation.to_owned()),
        display_name: StringUpdate::Set(input.display_name.clone()),
        description: input.description.clone(),
        system_prompt: input.system_prompt.clone(),
        clone_from: input.clone_from.clone(),
        root: input.root.clone(),
        preset: input
            .preset
            .clone()
            .map(StringUpdate::Set)
            .unwrap_or_default(),
        profile_id: StringUpdate::Set(input.profile_id.clone()),
        make_default: input.make_default,
        ..Default::default()
    }
}

/// `operations::agent::put`: a whole Agent document becomes the edit of its
/// changed writable fields when the Agent exists, else a create that selects
/// the document's Context and profile; that create's input prompt is the
/// selected Context's.
fn put_params(
    args: &ConfigureAgentParams,
    agent: &Agent,
    refs: &gents_documents::ConfigReferences,
) -> Result<ConfigureAgentParams, AgentApplyError> {
    let find = |collection: gents_store::collection::Collection, id: &str| {
        refs.documents()
            .find(|((c, key), _)| *c == collection && key == id)
            .map(|(_, value)| value.clone())
    };
    let update = |before: Option<&str>, after: Option<&str>| match (before, after) {
        (before, after) if before == after => StringUpdate::Omitted,
        (_, Some(after)) => StringUpdate::Set(after.to_owned()),
        (_, None) => StringUpdate::Clear,
    };
    let mut params = ConfigureAgentParams {
        make_default: args.make_default,
        identity_binding: args.identity_binding.clone(),
        ..Default::default()
    };
    match find(gents_store::collection::Collection::Agent, &agent.agent_did) {
        Some(stored) => {
            let stored: Agent = serde_json::from_value(stored)?;
            params.action = AgentOperationKind::Edit.as_str().to_owned();
            params.agent_did = Some(agent.agent_did.clone());
            params.display_name = update(
                stored.display_name.as_deref(),
                agent.display_name.as_deref(),
            );
            params.description =
                update(stored.description.as_deref(), agent.description.as_deref());
            params.context_id = update(stored.context_id.as_deref(), agent.context_id.as_deref());
            params.profile_id = update(
                Some(&stored.inference_profile_id),
                Some(&agent.inference_profile_id),
            );
            params.enabled = (stored.enabled != agent.enabled).then_some(agent.enabled);
            params.tags = (stored.tags != agent.tags).then(|| agent.tags.clone());
        }
        None => {
            let binding = args
                .identity_binding
                .as_ref()
                .ok_or(AgentOperationError::IdentityBindingRequired)?;
            if binding.agent_did != agent.agent_did {
                return Err(AgentOperationError::IdentityBindingMismatch {
                    binding: binding.agent_did.clone(),
                    agent: agent.agent_did.clone(),
                }
                .into());
            }
            admit(agent.enabled, AgentOperationError::NewAgentDisabled)?;
            params.action = AgentOperationKind::Create.as_str().to_owned();
            params.display_name = agent
                .display_name
                .clone()
                .map(StringUpdate::Set)
                .unwrap_or_default();
            params.description = agent
                .description
                .clone()
                .map(StringUpdate::Set)
                .unwrap_or_default();
            params.profile_id = StringUpdate::Set(agent.inference_profile_id.clone());
            params.tags = Some(agent.tags.clone()).filter(|tags| !tags.is_empty());
            if let Some(context_id) = &agent.context_id {
                params.context_id = StringUpdate::Set(context_id.clone());
                params.system_prompt = find(
                    gents_store::collection::Collection::AgentContext,
                    context_id,
                )
                .and_then(|context| {
                    context
                        .get("system_prompt")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .map(StringUpdate::Set)
                .unwrap_or_default();
            }
        }
    }
    Ok(params)
}

/// The Agents whose `update` grant the request's admission reads.
fn admission_targets(args: &ConfigureAgentParams) -> Vec<String> {
    args.put
        .as_ref()
        .map(|agent| agent.agent_did.clone())
        .or_else(|| args.agent_did.clone())
        .into_iter()
        .collect()
}

#[cfg(test)]
pub(super) async fn agent_mutate(
    node: &Arc<EmbeddedNode>,
    owner: &str,
    identity: &dyn NodeIdentity,
    args: &ConfigureAgentParams,
    ceiling: &crate::surface::SelfConfigProcessCeiling,
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
    ceiling: &crate::surface::SelfConfigProcessCeiling,
) -> Result<String> {
    let receipt = agent_apply_access(
        access,
        owner,
        args,
        ceiling,
        args.action == "preview",
        AgentCaller::default(),
    )
    .await?;
    receipt_text(&receipt)
}

/// `operations::agent::create`: the caller's access decides who signs, and
/// no held-grant bound applies because the caller is not an Agent.
pub async fn create_agent(
    access: &ConfigAccess,
    owner: &str,
    identity_binding: AgentIdentityBinding,
    input: &AgentCreateInput,
    ceiling: &crate::surface::SelfConfigProcessCeiling,
) -> Result<AgentReceipt<Agent>, AgentOperationError> {
    let mut args = create_params(input, false);
    args.identity_binding = Some(identity_binding);
    apply_agent(access, owner, &args, ceiling).await
}

/// The operations layer's commit of any agent operation: admission refusals
/// and other owners' failures are both errors.
pub async fn apply_agent(
    access: &ConfigAccess,
    owner: &str,
    args: &ConfigureAgentParams,
    ceiling: &crate::surface::SelfConfigProcessCeiling,
) -> Result<AgentReceipt<Agent>, AgentOperationError> {
    Ok(agent_apply_access(access, owner, args, ceiling, false, AgentCaller::default()).await?)
}

/// The operations layer's preview (Lean `agentOutcome .preview`): a refusal is
/// the successful refused answer with the commit's recovery; other failures
/// stay errors.
pub async fn preview_agent(
    access: &ConfigAccess,
    owner: &str,
    args: &ConfigureAgentParams,
    ceiling: &crate::surface::SelfConfigProcessCeiling,
) -> Result<AgentOutcome<AgentReceipt<Agent>>, AgentOperationError> {
    match agent_apply_access(access, owner, args, ceiling, true, AgentCaller::default()).await {
        Ok(receipt) => Ok(AgentOutcome::Admitted(receipt)),
        Err(error) => {
            let error = AgentOperationError::from(error);
            let next_call = recovery_for(args, &error);
            error.preview_outcome(next_call)
        }
    }
}

/// The grants and default observed for one access, for a caller that
/// admits agent writes inside a transaction it already holds on that access.
pub struct ObservedAdmission {
    facts: AdmissionFacts,
    actor_did: String,
}

/// Observes what `access` holds over `targets` before its transaction opens:
/// a transaction reads only as its own actor, and the default must be read
/// authoritatively.
pub async fn observe_admission(
    access: &ConfigAccess,
    owner: &str,
    targets: &[String],
) -> Result<ObservedAdmission, AgentOperationError> {
    Ok(ObservedAdmission {
        facts: admission_facts(access, owner, targets).await?,
        actor_did: access.actor_did().unwrap_or_default(),
    })
}

/// The operations layer's commit inside the caller's transaction, for a
/// front end whose agent write is one part of a larger atomic save. The
/// transaction must be on the access `admission` was observed for.
pub async fn apply_agent_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    admission: &ObservedAdmission,
    args: &ConfigureAgentParams,
    ceiling: &crate::surface::SelfConfigProcessCeiling,
) -> Result<AgentReceipt<Agent>, AgentOperationError> {
    admit(
        txn.actor_did().unwrap_or_default() == admission.actor_did,
        AgentOperationError::AdmissionActorMismatch,
    )?;
    Ok(agent_apply_in_txn(
        txn,
        &AgentApplyRequest {
            owner,
            args,
            ceiling,
            preview: false,
            facts: &admission.facts,
            actor_did: &admission.actor_did,
            caller: AgentCaller::default(),
        },
    )
    .await?)
}

#[cfg(test)]
async fn agent_apply(
    node: &Arc<EmbeddedNode>,
    owner: &str,
    args: &ConfigureAgentParams,
    ceiling: &crate::surface::SelfConfigProcessCeiling,
    preview: bool,
) -> Result<String> {
    let receipt = agent_apply_access(
        &ConfigAccess::local(node.clone()),
        owner,
        args,
        ceiling,
        preview,
        AgentCaller::default(),
    )
    .await?;
    receipt_text(&receipt)
}

/// The model's preview text: the receipt when admitted; when refused, the
/// refusal with the message and next call the commit's guidance carries.
pub(super) async fn model_agent_outcome(
    core: &SelfConfigCore,
    args: &ConfigureAgentParams,
) -> Result<AgentOutcome<String>, AgentApplyError> {
    let mut args = args.clone();
    if AgentCreateOperation::for_verb(requested_operation(&args)).is_some() {
        let runtime = core.runtime_identity()?;
        if runtime.did() != core.node_did() {
            return Err(anyhow!("runtime identity does not match node").into());
        }
        args.identity_binding = Some(
            gents_store::identity::provision_agent_identity(runtime)
                .map_err(anyhow::Error::from)?,
        );
    }
    let caller = AgentCaller {
        held: Some(core.held_grants()),
        node_binding: core.node_binding(),
        lockout: Some(core),
    };
    match agent_apply_access(
        &core.access()?,
        core.node_did(),
        &args,
        core.process_ceiling(),
        true,
        caller,
    )
    .await
    {
        Ok(receipt) => Ok(AgentOutcome::Admitted(receipt_text(&receipt)?)),
        Err(AgentApplyError::Operation(error)) => {
            let next_call = recovery_for(&args, &error);
            error.preview_outcome(next_call).map_err(Into::into)
        }
        Err(error) => Err(error),
    }
}

pub(super) async fn model_agent_create_preview(
    core: &SelfConfigCore,
    input: &AgentCreateInput,
) -> Result<AgentOutcome<String>, AgentApplyError> {
    model_agent_outcome(core, &create_params(input, true)).await
}

/// The preview as text, a refusal without its rendered next call.
#[cfg(test)]
pub(super) async fn model_agent_preview(
    core: &SelfConfigCore,
    args: &ConfigureAgentParams,
) -> Result<String> {
    match model_agent_outcome(core, args).await? {
        AgentOutcome::Admitted(text) => Ok(text),
        AgentOutcome::Refused {
            refusal, message, ..
        } => gents_session::session::output_render::ordered! {
            "admitted": false,
            "committed": false,
            "refusal": refusal.as_str(),
            "message": message,
        }
        .pretty(),
    }
}

#[cfg(test)]
pub(super) async fn model_agent_mutate(
    core: &SelfConfigCore,
    identity: &dyn NodeIdentity,
    args: &ConfigureAgentParams,
) -> Result<String> {
    receipt_text(&model_agent_apply(core, identity, args.clone()).await?)
}

pub(super) async fn model_agent_create(
    core: &SelfConfigCore,
    identity: &dyn NodeIdentity,
    input: &AgentCreateInput,
) -> Result<AgentReceipt<Agent>, AgentApplyError> {
    model_agent_apply(core, identity, create_params(input, false)).await
}

pub(super) async fn model_agent_apply(
    core: &SelfConfigCore,
    identity: &dyn NodeIdentity,
    mut args: ConfigureAgentParams,
) -> Result<AgentReceipt<Agent>, AgentApplyError> {
    let actor = core.identity()?;
    if identity.did() != actor.to_string() {
        return Err(anyhow!("agent configuration requires its selected actor signer").into());
    }
    if AgentCreateOperation::for_verb(requested_operation(&args)).is_some() {
        args.identity_binding = Some(
            gents_store::identity::provision_agent_identity_for_agent(
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
        AgentCaller {
            held: Some(core.held_grants()),
            node_binding: core.node_binding(),
            lockout: Some(core),
        },
    )
    .await
}

async fn agent_apply_access(
    access: &ConfigAccess,
    owner: &str,
    args: &ConfigureAgentParams,
    ceiling: &crate::surface::SelfConfigProcessCeiling,
    preview: bool,
    caller: AgentCaller<'_>,
) -> Result<AgentReceipt<Agent>, AgentApplyError> {
    let facts = admission_facts(access, owner, &admission_targets(args)).await?;
    let actor_did = access.actor_did().unwrap_or_default();
    let request = AgentApplyRequest {
        owner,
        args,
        ceiling,
        preview,
        facts: &facts,
        actor_did: &actor_did,
        caller,
    };
    let request = &request;
    if preview {
        access
            .transact_readonly("self_config.agent", |txn| {
                Box::pin(agent_apply_in_txn(txn, request))
            })
            .await
    } else {
        access
            .transact_write("self_config.agent", |txn| {
                Box::pin(agent_apply_in_txn(txn, request))
            })
            .await
    }
}

struct AgentApplyRequest<'a> {
    owner: &'a str,
    args: &'a ConfigureAgentParams,
    ceiling: &'a crate::surface::SelfConfigProcessCeiling,
    preview: bool,
    facts: &'a AdmissionFacts,
    actor_did: &'a str,
    caller: AgentCaller<'a>,
}

/// Lean `SelfConfig.materializedAgent`: admits the request through
/// `decide_agent_operation` and publishes its documents in this transaction.
async fn agent_apply_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    request: &AgentApplyRequest<'_>,
) -> Result<AgentReceipt<Agent>, AgentApplyError> {
    let AgentApplyRequest {
        owner,
        ceiling,
        preview,
        facts,
        actor_did,
        caller,
        ..
    } = *request;
    let refs = gents_documents::ConfigReferences::load_in_txn(txn, owner).await?;
    let mut view = catalog_from_documents(&refs)?;
    let actor = admission_actor(txn, owner, facts, actor_did, &mut view).await?;
    let resolved;
    let args = match request.args.put.as_deref() {
        Some(agent) => {
            resolved = put_params(request.args, agent, &refs)?;
            &resolved
        }
        None => request.args,
    };
    admit(
        args.action != "preview" || args.operation.is_some(),
        AgentOperationError::PreviewRequiresOperation,
    )?;
    let operation = requested_operation(args);
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
                && !args.context_id.is_present()
                && args.enabled.is_none()
                && args.tags.is_none()
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
        ("context_id", &args.context_id),
    ] {
        if update.is_present() {
            patch.push((name.to_owned(), update.value().map(|value| json!(value))));
        }
    }
    if let Some(enabled) = args.enabled {
        patch.push(("enabled".to_owned(), Some(json!(enabled))));
    }
    if let Some(tags) = &args.tags {
        patch.push(("tags".to_owned(), (!tags.is_empty()).then(|| json!(tags))));
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
    agent::decide_agent_operation(&view, &actor, &op, &id, args.make_default)?;
    let load = |collection: gents_store::collection::Collection, id: &str| {
        refs.documents()
            .find(|((c, key), _)| *c == collection && key == id)
            .map(|(_, value)| value.clone())
            .ok_or_else(|| AgentOperationError::MissingDocument {
                collection: collection.graphql_type(),
                id: id.to_owned(),
            })
    };
    let selected_tools = |agent: &serde_json::Map<String, Value>| -> Result<
        Option<serde_json::Map<String, Value>>,
        AgentOperationError,
    > {
        let Some(context_id) = agent.get("context_id").and_then(Value::as_str) else {
            return Ok(None);
        };
        let context = load(
            gents_store::collection::Collection::AgentContext,
            context_id,
        )?;
        let Some(tools_id) = context.get("tools_id").and_then(Value::as_str) else {
            return Ok(None);
        };
        Ok(Some(
            load(gents_store::collection::Collection::Tools, tools_id)?
                .as_object()
                .ok_or(AgentOperationError::ObjectRequired("Tools"))?
                .clone(),
        ))
    };
    let mut documents = Vec::new();
    let doc: Agent;
    let mut root_access = None;
    match &op {
        agent::AgentOperation::Create(input) if args.context_id.is_present() => {
            let context_id = args
                .context_id
                .value()
                .ok_or(AgentOperationError::ContextIdBlank)?;
            load(
                gents_store::collection::Collection::AgentContext,
                context_id,
            )?;
            doc = serde_json::from_value(
                json!({"agent_did":id,"creation_nonce":args.identity_binding.as_ref().ok_or(AgentOperationError::IdentityBindingMissing)?.creation_nonce,"node_did":owner,"display_name":input.display_name,"description":args.description.value(),"context_id":context_id,"inference_profile_id":input.inference_profile_id,"tags":args.tags}),
            )?;
            if let Some(held) = caller.held {
                let selected =
                    serde_json::Map::from_iter([("context_id".to_owned(), json!(context_id))]);
                ops::reselection_keeps_grants(held, None, selected_tools(&selected)?.as_ref())?;
            }
            guard_agent_profile_choice(txn, owner, None, None, Some(&input.inference_profile_id))
                .await?;
        }
        agent::AgentOperation::Create(input) => {
            let mut context: AgentContext = match input.clone_from.as_deref() {
                Some(source) => {
                    let source: Agent = serde_json::from_value(load(
                        gents_store::collection::Collection::Agent,
                        source,
                    )?)?;
                    match source.context_id {
                        Some(id) => serde_json::from_value(load(
                            gents_store::collection::Collection::AgentContext,
                            &id,
                        )?)?,
                        None => serde_json::from_value(json!({"context_id":"", "node_did":owner}))?,
                    }
                }
                None => serde_json::from_value(json!({"context_id":"", "node_did":owner}))?,
            };
            context.context_id = format!("{id}:context");
            admit(
                !refs.documents().any(|((c, key), _)| {
                    *c == gents_store::collection::Collection::AgentContext
                        && key == &context.context_id
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
                Some(id) => Some(serde_json::from_value(load(
                    gents_store::collection::Collection::Tools,
                    id,
                )?)?),
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
                let write = preset == crate::surface::presets::PRESET_WRITE;
                tools = Some(Tools {
                    host: Some(HostTools {
                        files: Some(FileTools {
                            mode: gents_documents::FileToolMode::parse(&fields.file_tools_mode)?,
                            ..Default::default()
                        }),
                        bash: Some(BashTools {
                            mode: gents_documents::BashMode::parse(&fields.bash_mode)?,
                            execution_mode: write.then_some(if cfg!(target_os = "macos") {
                                crate::native::CommandExecutionMode::WorkspaceWrite
                            } else {
                                crate::native::CommandExecutionMode::Unrestricted
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
                        *c == gents_store::collection::Collection::Tools && key == &tools.tools_id
                    }),
                    AgentOperationError::GeneratedToolsIdExists,
                )?;
                tools.node_did = owner.to_owned();
                let policy = crate::surface::load_workspace_root_policy_in_txn(
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
                crate::surface::canonicalize_tools_root(
                    &mut tools,
                    &policy,
                    stored_root.as_deref(),
                )?;
                apply_tool_grant_selection(&mut tools, args.enable_lsp, args.network_mode);
                // A new Agent DID holds no WorkspaceRoot relationship yet.
                let published =
                    match crate::surface::tools_root_reader_in_txn(txn, owner, &policy, &tools)
                        .await?
                    {
                        crate::surface::ToolRootReader::NotNeeded => None,
                        crate::surface::ToolRootReader::Published(root) => Some(Some(root)),
                        crate::surface::ToolRootReader::Unpublished(_) => Some(None),
                    };
                if let Some(published) = published {
                    root_access = Some(crate::surface::reader_grant_step(
                        &id,
                        published.as_deref(),
                        true,
                    ));
                }
                validate_tool_network_selection(args.network_mode)?;
                context.tools_id = Some(tools.tools_id.clone());
                if let Some(held) = caller.held {
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
                documents.push(replacement(
                    gents_store::collection::Collection::Tools,
                    tools,
                )?);
            }
            doc = serde_json::from_value(
                json!({"agent_did":id,"creation_nonce":args.identity_binding.as_ref().ok_or(AgentOperationError::IdentityBindingMissing)?.creation_nonce,"node_did":owner,"display_name":input.display_name,"description":args.description.value(),"context_id":context.context_id,"inference_profile_id":input.inference_profile_id,"tags":args.tags}),
            )?;
            guard_agent_profile_choice(
                txn,
                owner,
                None,
                input.clone_from.as_deref(),
                Some(&input.inference_profile_id),
            )
            .await?;
            documents.push(replacement(
                gents_store::collection::Collection::AgentContext,
                context,
            )?);
        }
        agent::AgentOperation::Edit(_) | agent::AgentOperation::Disable => {
            let edit = op.edit_fields().unwrap_or_default();
            let stored = load(gents_store::collection::Collection::Agent, &id)?;
            let stored = stored
                .as_object()
                .ok_or(AgentOperationError::ObjectRequired("agent"))?
                .clone();
            let agent_patch = edit
                .iter()
                .filter(|(field, _)| field != "system_prompt")
                .cloned()
                .collect();
            let merged =
                gents_documents::patch::apply_patch(SelfConfigTarget::Agent, &stored, &agent_patch);
            if let Some(held) = caller.held {
                let before = selected_tools(&stored)?;
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
                        *c == gents_store::collection::Collection::Agent
                            && value.get("context_id").and_then(Value::as_str) == Some(context_id)
                    })
                    .count();
                admit(sharers == 1, AgentOperationError::SystemPromptContextShared)?;
                let mut context: AgentContext = serde_json::from_value(load(
                    gents_store::collection::Collection::AgentContext,
                    context_id,
                )?)?;
                context.system_prompt = args.system_prompt.owned_value();
                documents.push(replacement(
                    gents_store::collection::Collection::AgentContext,
                    context,
                )?);
            }
            guard_agent_profile_choice(txn, owner, Some(&id), None, args.profile_id.value())
                .await?;
            let anchor = ops::AgentAnchor {
                doc: stored.clone(),
                context: serde_json::Map::new(),
                profile: serde_json::Map::new(),
                execution: serde_json::Map::new(),
            };
            ops::guard_compaction_choice_in_txn(
                txn,
                SelfConfigTarget::Agent,
                &anchor,
                &stored,
                &merged,
            )
            .await?;
            if let Some(core) = caller.lockout {
                core.guard_lockout_chain_in_txn(txn, &merged).await?;
            }
            doc = serde_json::from_value(Value::Object(merged))?;
        }
    }
    documents.push(replacement(
        gents_store::collection::Collection::Agent,
        &doc,
    )?);
    if args.make_default {
        documents.push(replacement(
            gents_store::collection::Collection::DefaultAgentSelection,
            DefaultAgentSelection::new(owner, &id),
        )?);
    }
    let plan = DesiredStateApplyPlan::new(documents)?;
    validate_desired_state_plan(
        txn,
        &plan,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await?;
    if doc.enabled {
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
                gents_store::collection::Collection::Agent => {
                    candidate.agents.push(serde_json::from_value(value)?)
                }
                gents_store::collection::Collection::AgentContext => {
                    candidate.contexts.push(serde_json::from_value(value)?)
                }
                gents_store::collection::Collection::InferenceProfile => candidate
                    .inference_profiles
                    .push(serde_json::from_value(value)?),
                gents_store::collection::Collection::InferenceBackend => candidate
                    .inference_backends
                    .push(serde_json::from_value(value)?),
                _ => {}
            }
        }
        admit(
            agent::materialize_agent(
                &view,
                &actor,
                &op,
                &id,
                args.make_default,
                &candidate,
                owner,
            )
            .is_some(),
            AgentOperationError::CandidateDoesNotResolve,
        )?;
    }
    if !preview {
        gents_documents::apply_desired_state_plan_with_node_binding(
            txn,
            &plan,
            caller.node_binding,
            &gents_inference::resolved::ProviderProfilePolicy,
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
                let source = gents_documents::load_agent_tools_in_txn(txn, node_did, source_did)
                    .await?
                    .with_context(|| format!("{source_did} selects no Tools"))?;
                let mut tools = gents_documents::load_agent_tools_in_txn(txn, node_did, agent_did)
                    .await?
                    .with_context(|| format!("{agent_did} selects no Tools"))?;
                if serde_json::to_value(&tools.self_config)?
                    != serde_json::to_value(&source.self_config)?
                {
                    tools.self_config = source.self_config;
                    tools.validate()?;
                    let value = serde_json::to_value(&tools)?;
                    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                        collection: gents_store::collection::Collection::Tools,
                        add: value.clone(),
                        update: value,
                    }])?;
                    gents_documents::apply_desired_state_plan(
                        txn,
                        &plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await?;
                }
                gents_documents::grant_selected_self_configuration(txn, node_did, agent_did).await
            })
        })
        .await
}
