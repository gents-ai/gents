use std::collections::{BTreeMap, BTreeSet};

use crate::documents::patch::SelfConfigPatch;
use crate::documents::{Agent, AgentContext, InferenceBackend, InferenceProfile};
use anyhow::{ensure, Result};
use gents_protocol::operations::agent::AgentOperationError;

#[derive(Clone, Debug, Default)]
pub struct AgentCatalogView {
    pub agents: BTreeMap<String, bool>,
    pub protected_ids: BTreeSet<String>,
    pub default_id: Option<String>,
    pub published_profiles: BTreeSet<String>,
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

pub fn decide_agent_operation(
    view: &AgentCatalogView,
    op: &AgentOperation,
    target: &str,
    make_default: bool,
) -> Result<(), AgentOperationError> {
    match op {
        AgentOperation::Create(input) => {
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
        AgentOperation::Edit(patch) => {
            admit(
                view.agents.contains_key(target),
                AgentOperationError::AgentMissing(target.to_owned()),
            )?;
            admit(
                !view.protected_ids.contains(target),
                AgentOperationError::ProtectedAgentEdit,
            )?;
            for (field, value) in patch {
                admit(
                    matches!(
                        field.as_str(),
                        "display_name" | "description" | "system_prompt" | "inference_profile_id"
                    ),
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
                    _ => {}
                }
            }
        }
        AgentOperation::Disable => {
            admit(
                view.agents.contains_key(target),
                AgentOperationError::AgentMissing(target.to_owned()),
            )?;
            admit(
                !view.protected_ids.contains(target),
                AgentOperationError::ProtectedAgentDisable,
            )?;
            admit(!make_default, AgentOperationError::DisableMakesDefault)?;
            admit(
                view.default_id.as_deref() != Some(target),
                AgentOperationError::DefaultAgentDisable,
            )?;
        }
    }
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
    pub effort: Option<crate::inference::resolved::ReasoningEffort>,
}

pub fn materialize_agent(
    view: &AgentCatalogView,
    op: &AgentOperation,
    target: &str,
    make_default: bool,
    candidate: &AgentCandidate,
    node_did: &str,
) -> Option<MaterializedAgentSession> {
    decide_agent_operation(view, op, target, make_default).ok()?;
    if matches!(op, AgentOperation::Disable) {
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
    pub protected: bool,
    pub shared_context: bool,
    pub shared_tools: bool,
}

pub fn admit_sibling_tools_target(target: &SiblingToolsTarget) -> Result<()> {
    ensure!(
        target.owner_matches && !target.protected && !target.shared_context && !target.shared_tools,
        "sibling tools require an owned unprotected agent with unshared Context and Tools"
    );
    Ok(())
}
