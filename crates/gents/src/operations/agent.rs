//! The single entry points every front end calls for agent operations (Lean
//! `SelfConfig.frontEndAdmission`): each goes through the one materializer,
//! which admits through `decide_agent_operation` from the acting actor's ACP
//! grants. No category or held-grant bound is checked here: those bind an
//! Agent caller and stay in the config tool's adapter.

use gents_protocol::agent_identity::AgentIdentityBinding;
use gents_protocol::operations::agent::{AgentCreateInput, AgentEdit, AgentOperationError};

use gents_documents::Agent;
use gents_store::config_client::ConfigAccess;
use gents_tools::builtin::self_config::ConfigureAgentParams;
use gents_tools::surface::SelfConfigProcessCeiling;

pub use gents_protocol::operations::agent::{AgentOutcome, AgentRefusal};
pub use gents_tools::builtin::self_config::agent::{
    decide_agent_operation, materialize_agent, AgentActor, AgentCandidate, AgentCatalogView,
    AgentCreateRequest, AgentOperation, MaterializedAgentSession, AGENT_EDIT_FIELDS,
};

pub type AgentReceipt = gents_protocol::operations::agent::AgentReceipt<Agent>;

/// Creates or clones an Agent with its Context and Tools in one transaction,
/// written as whoever `access` signs for. `identity_binding` comes from the
/// caller's own provisioning route.
pub async fn create(
    access: &ConfigAccess,
    owner: &str,
    identity_binding: AgentIdentityBinding,
    input: &AgentCreateInput,
    ceiling: &SelfConfigProcessCeiling,
) -> Result<AgentReceipt, AgentOperationError> {
    gents_tools::builtin::self_config::create_agent(access, owner, identity_binding, input, ceiling)
        .await
}

/// Lean `SelfConfig.AgentOperation.edit`: changes the named fields of an
/// existing Agent. `enabled: Some(false)` is a disable.
pub async fn edit(
    access: &ConfigAccess,
    owner: &str,
    agent_did: &str,
    edit: &AgentEdit,
) -> Result<AgentReceipt, AgentOperationError> {
    commit(access, owner, edit_params("edit", agent_did, edit, false)).await
}

/// Lean `SelfConfig.disableFields`: the edit `enabled := false`.
pub async fn disable(
    access: &ConfigAccess,
    owner: &str,
    agent_did: &str,
) -> Result<AgentReceipt, AgentOperationError> {
    commit(
        access,
        owner,
        edit_params("disable", agent_did, &AgentEdit::default(), false),
    )
    .await
}

/// Lean `SelfConfig.chooseDefault`: makes `agent_did` the Node's default.
/// With `enable` it is the edit `enabled := true` with the default switch, so
/// a disabled Agent can become the default in one call.
pub async fn choose_default(
    access: &ConfigAccess,
    owner: &str,
    agent_did: &str,
    enable: bool,
) -> Result<AgentReceipt, AgentOperationError> {
    commit(access, owner, default_params(agent_did, enable)).await
}

/// Writes a whole Agent document, for front ends that save one: an existing
/// Agent becomes the edit of its changed writable fields; a new one, with
/// `identity_binding` for its DID, becomes a create that selects the
/// document's Context and profile.
pub async fn put(
    access: &ConfigAccess,
    owner: &str,
    agent: &Agent,
    identity_binding: Option<AgentIdentityBinding>,
    make_default: bool,
    ceiling: &SelfConfigProcessCeiling,
) -> Result<AgentReceipt, AgentOperationError> {
    gents_tools::builtin::self_config::apply_agent(
        access,
        owner,
        &put_params(agent, identity_binding, make_default),
        ceiling,
    )
    .await
}

pub use gents_tools::builtin::self_config::ObservedAdmission;

/// Observes the grants `access` holds over `agent_dids`, and the Node's
/// default, for a save that writes Agents inside its own transaction
/// (`edit_in_txn`, `put_in_txn`). Observe before opening the transaction.
pub async fn observe(
    access: &ConfigAccess,
    owner: &str,
    agent_dids: &[String],
    home: Option<std::path::PathBuf>,
) -> Result<ObservedAdmission, AgentOperationError> {
    Ok(
        gents_tools::builtin::self_config::observe_admission(access, owner, agent_dids)
            .await?
            .with_node_home(home),
    )
}

/// `edit` inside the caller's transaction, admitted from `admission`.
pub async fn edit_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    admission: &ObservedAdmission,
    agent_did: &str,
    edit: &AgentEdit,
) -> Result<AgentReceipt, AgentOperationError> {
    gents_tools::builtin::self_config::apply_agent_in_txn(
        txn,
        owner,
        admission,
        &edit_params("edit", agent_did, edit, false),
        &SelfConfigProcessCeiling::default(),
    )
    .await
}

/// `put` inside the caller's transaction, admitted from `admission`.
pub async fn put_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    admission: &ObservedAdmission,
    agent: &Agent,
    identity_binding: Option<AgentIdentityBinding>,
    context_patch: Option<&gents_protocol::operations::agent::AgentContextPatch>,
    ceiling: &SelfConfigProcessCeiling,
) -> Result<AgentReceipt, AgentOperationError> {
    if let Some(patch) = context_patch {
        if agent.context_id.as_deref() != Some(patch.context_id.as_str()) {
            return Err(AgentOperationError::ContextPatchMismatch);
        }
        let changes = patch
            .changes
            .iter()
            .map(|(field, value)| (field.clone(), Some(value.clone())))
            .collect();
        gents_documents::operations::write::write_in_txn(
            txn,
            &gents_documents::operations::write::DocumentWrite {
                target: gents_documents::patch::SelfConfigTarget::AgentContext,
                owner,
                id: &patch.context_id,
                mode: gents_protocol::operations::document::WriteMode::Update,
                patch: &changes,
                node_binding: None,
            },
            &|_| Ok(()),
            &gents_inference::resolved::ProviderProfilePolicy,
            true,
        )
        .await
        .map_err(|error| {
            error.finish(gents_protocol::operations::document::DocumentOperationError::Passthrough)
        })
        .map_err(|error| {
            AgentOperationError::Passthrough(gents_protocol::error_chain::PassedThrough::error(
                error,
            ))
        })?;
    }
    gents_tools::builtin::self_config::apply_agent_in_txn(
        txn,
        owner,
        admission,
        &put_params(agent, identity_binding, false),
        ceiling,
    )
    .await
}

fn edit_params(
    operation: &str,
    agent_did: &str,
    edit: &AgentEdit,
    make_default: bool,
) -> ConfigureAgentParams {
    ConfigureAgentParams {
        action: operation.to_owned(),
        agent_did: Some(agent_did.to_owned()),
        display_name: edit.display_name.clone(),
        description: edit.description.clone(),
        system_prompt: edit.system_prompt.clone(),
        profile_id: edit.inference_profile_id.clone(),
        context_id: edit.context_id.clone(),
        enabled: edit.enabled,
        tags: edit.tags.clone(),
        make_default,
        ..Default::default()
    }
}

fn default_params(agent_did: &str, enable: bool) -> ConfigureAgentParams {
    let edit = AgentEdit {
        enabled: enable.then_some(true),
        ..Default::default()
    };
    edit_params("edit", agent_did, &edit, true)
}

fn put_params(
    agent: &Agent,
    identity_binding: Option<AgentIdentityBinding>,
    make_default: bool,
) -> ConfigureAgentParams {
    ConfigureAgentParams {
        action: "put".to_owned(),
        identity_binding,
        make_default,
        put: Some(Box::new(agent.clone())),
        ..Default::default()
    }
}

/// Edits, disables and default choices need no process ceiling: it bounds
/// only the Tools a create writes.
async fn commit(
    access: &ConfigAccess,
    owner: &str,
    params: ConfigureAgentParams,
) -> Result<AgentReceipt, AgentOperationError> {
    gents_tools::builtin::self_config::apply_agent(
        access,
        owner,
        &params,
        &SelfConfigProcessCeiling::default(),
    )
    .await
}
