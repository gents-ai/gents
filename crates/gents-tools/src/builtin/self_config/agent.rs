use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Result};
use gents_documents::patch::SelfConfigPatch;
use gents_documents::{Agent, AgentContext, InferenceBackend, InferenceProfile};
use gents_protocol::operations::agent::AgentOperationError;
use serde_json::Value;

/// Lean `SelfConfig.AgentCatalog`: the node's Agents with their `enabled`
/// value, the current default read authoritatively, and the published
/// inference profiles.
#[derive(Clone, Debug, Default)]
pub struct AgentCatalogView {
    pub agents: BTreeMap<String, bool>,
    pub default_id: Option<String>,
    pub published_profiles: BTreeSet<String>,
}

/// Lean `SelfConfig.Actor` with its `ActorGrants`, as native ACP answers for
/// the acting DID: `catalog` is `nodes manage_agents` on the Node, and
/// `configures` holds the Agents whose document it may update. Admission
/// reads nothing else about the actor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentActor {
    pub did: String,
    pub catalog: bool,
    pub configures: BTreeSet<String>,
}

#[derive(Clone, Debug)]
pub struct AgentCreateRequest {
    pub display_name: String,
    pub system_prompt: String,
    pub inference_profile_id: String,
    pub clone_from: Option<String>,
}

#[derive(Clone, Debug)]
pub enum AgentOperation {
    Create(AgentCreateRequest),
    Edit(SelfConfigPatch),
    Disable,
}

/// Lean `SelfConfig.agentEditKeys`: every writable Agent field, and
/// `system_prompt`, which writes the Agent's selected Context.
pub const AGENT_EDIT_FIELDS: &[&str] = &[
    "display_name",
    "description",
    "system_prompt",
    "inference_profile_id",
    "context_id",
    "enabled",
    "tags",
];

pub(super) fn admit(
    admitted: bool,
    refusal: AgentOperationError,
) -> Result<(), AgentOperationError> {
    if admitted {
        Ok(())
    } else {
        Err(refusal)
    }
}

impl AgentOperation {
    /// Lean `AgentOperation.normalize`: disable is the edit `enabled := false`.
    pub fn edit_fields(&self) -> Option<SelfConfigPatch> {
        match self {
            Self::Create(_) => None,
            Self::Edit(patch) => Some(patch.clone()),
            Self::Disable => Some(vec![("enabled".to_owned(), Some(Value::Bool(false)))]),
        }
    }

    /// Lean `AgentRequest.writesEnabled`. A cleared `enabled` stores the
    /// field's default, which is enabled.
    fn writes_enabled(&self) -> Option<bool> {
        match self.edit_fields() {
            None => Some(true),
            Some(patch) => patch
                .iter()
                .rev()
                .find(|(field, _)| field == "enabled")
                .map(|(_, value)| value.as_ref().and_then(Value::as_bool).unwrap_or(true)),
        }
    }
}

/// Lean `SelfConfig.enabledAfter` for the request's target.
fn enabled_after(view: &AgentCatalogView, op: &AgentOperation, target: &str) -> bool {
    op.writes_enabled()
        .unwrap_or_else(|| view.agents.get(target) == Some(&true))
}

/// Lean `SelfConfig.agentAdmission`: the one admission for create, clone,
/// edit, disable and choosing the default, whoever asks and through whichever
/// front end. Checks run in the model's order and the first failure is the
/// refusal: the catalog, the target and the input, the agent category on the
/// target, then the two lockout rules.
pub fn decide_agent_operation(
    view: &AgentCatalogView,
    actor: &AgentActor,
    op: &AgentOperation,
    target: &str,
    make_default: bool,
) -> Result<(), AgentOperationError> {
    let fields = op.edit_fields();
    admit(
        !(make_default || fields.is_none()) || actor.catalog,
        AgentOperationError::CatalogNotHeld,
    )?;
    match (op, &fields) {
        (AgentOperation::Create(input), _) => {
            admit(
                !view.agents.contains_key(target),
                AgentOperationError::AgentExists(target.to_owned()),
            )?;
            admit(
                !input.display_name.is_empty(),
                AgentOperationError::DisplayNameRequired,
            )?;
            admit(
                !input.inference_profile_id.trim().is_empty()
                    && view
                        .published_profiles
                        .contains(&input.inference_profile_id),
                AgentOperationError::ProfileNotPublished,
            )?;
            let source = input.clone_from.as_deref().unwrap_or("");
            admit(
                !source.is_empty() || !input.system_prompt.trim().is_empty(),
                AgentOperationError::SystemPromptRequired,
            )?;
            admit(
                source.is_empty() || view.agents.get(source) == Some(&true),
                AgentOperationError::CloneSourceNotEnabled,
            )?;
        }
        (_, Some(patch)) => {
            admit(
                view.agents.contains_key(target),
                AgentOperationError::AgentMissing(target.to_owned()),
            )?;
            for (field, value) in patch {
                admit(
                    AGENT_EDIT_FIELDS.contains(&field.as_str()),
                    AgentOperationError::UnsupportedEditField(field.clone()),
                )?;
                match (field.as_str(), value) {
                    ("display_name", Some(value)) => admit(
                        value.as_str().is_some_and(|s| !s.is_empty()),
                        AgentOperationError::DisplayNameEmpty,
                    )?,
                    ("system_prompt", Some(value)) => admit(
                        value.as_str().is_some_and(|s| !s.trim().is_empty()),
                        AgentOperationError::SystemPromptBlank,
                    )?,
                    ("inference_profile_id", value) => admit(
                        value.as_ref().and_then(|v| v.as_str()).is_some_and(|s| {
                            !s.trim().is_empty() && view.published_profiles.contains(s)
                        }),
                        AgentOperationError::ProfileNotPublished,
                    )?,
                    ("context_id", Some(value)) => admit(
                        value.as_str().is_some_and(|s| !s.trim().is_empty()),
                        AgentOperationError::ContextIdBlank,
                    )?,
                    _ => {}
                }
            }
        }
        (_, None) => {}
    }
    admit(
        !fields.as_ref().is_some_and(|patch| !patch.is_empty())
            || actor.configures.contains(target),
        AgentOperationError::AgentNotHeld(target.to_owned()),
    )?;
    let enabled_after = enabled_after(view, op, target);
    let default_after = if make_default {
        Some(target)
    } else {
        view.default_id.as_deref()
    };
    admit(
        !(default_after == Some(target) && !enabled_after),
        AgentOperationError::DefaultAgentDisabled(target.to_owned()),
    )?;
    admit(
        !(target == actor.did && !enabled_after),
        AgentOperationError::SelfDisable,
    )?;
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct AgentCandidate {
    pub agents: Vec<Agent>,
    pub contexts: Vec<AgentContext>,
    pub inference_profiles: Vec<InferenceProfile>,
    pub inference_backends: Vec<InferenceBackend>,
}

#[derive(Clone, Debug)]
pub struct MaterializedAgentSession {
    pub instructions: String,
    pub skill_ids: Vec<String>,
    pub tool_names: Vec<String>,
    pub backend_id: String,
    pub model: String,
    pub effort: Option<gents_documents::inference_defaults::ReasoningEffort>,
}

pub fn materialize_agent(
    view: &AgentCatalogView,
    actor: &AgentActor,
    op: &AgentOperation,
    target: &str,
    make_default: bool,
    candidate: &AgentCandidate,
    node_did: &str,
) -> Option<MaterializedAgentSession> {
    decide_agent_operation(view, actor, op, target, make_default).ok()?;
    if !enabled_after(view, op, target) {
        return None;
    }
    let agent = candidate
        .agents
        .iter()
        .find(|a| a.agent_did == target && a.node_did == node_did && a.enabled)?;
    let context = match &agent.context_id {
        Some(id) => Some(
            candidate
                .contexts
                .iter()
                .find(|c| &c.context_id == id && c.node_did == node_did)?,
        ),
        None => None,
    };
    let profile = candidate
        .inference_profiles
        .iter()
        .find(|p| p.profile_id == agent.inference_profile_id && p.node_did == node_did)?;
    candidate
        .inference_backends
        .iter()
        .find(|b| b.backend_id == profile.backend_id && b.node_did == node_did && b.enabled)?;
    Some(MaterializedAgentSession {
        instructions: context
            .and_then(|c| c.system_prompt.clone())
            .unwrap_or_default(),
        skill_ids: context.map(|c| c.skill_ids.clone()).unwrap_or_default(),
        tool_names: Vec::new(),
        backend_id: profile.backend_id.clone(),
        model: profile.model_name.clone(),
        effort: profile.reasoning_effort,
    })
}

pub struct SiblingToolsTarget {
    pub owner_matches: bool,
    pub shared_context: bool,
    pub shared_tools: bool,
}

pub fn admit_sibling_tools_target(target: &SiblingToolsTarget) -> Result<()> {
    ensure!(
        target.owner_matches && !target.shared_context && !target.shared_tools,
        "sibling tools require an owned agent with unshared Context and Tools"
    );
    Ok(())
}
