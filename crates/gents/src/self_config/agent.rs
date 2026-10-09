use std::collections::{BTreeMap, BTreeSet};

use crate::config_client::patch::SelfConfigPatch;
use crate::document_config::{Agent, AgentContext, InferenceBackend, InferenceProfile};
use anyhow::{ensure, Result};

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

/// Native adapter for `SelfConfig.agentOperationAdmitted`.
pub fn decide_agent_operation(
    view: &AgentCatalogView,
    op: &AgentOperation,
    target: &str,
    make_default: bool,
) -> Result<()> {
    match op {
        AgentOperation::Create(input) => {
            ensure!(
                !view.agents.contains_key(target),
                "agent {target:?} already exists"
            );
            ensure!(!input.display_name.is_empty(), "display_name is required");
            ensure!(
                !input.inference_profile_id.trim().is_empty()
                    && view
                        .published_profiles
                        .contains(&input.inference_profile_id),
                "select a published inference profile"
            );
            let source = input.clone_from.as_deref().unwrap_or("");
            ensure!(
                !source.is_empty() || !input.system_prompt.trim().is_empty(),
                "fresh agents require system_prompt"
            );
            ensure!(
                source.is_empty() || view.agents.get(source) == Some(&true),
                "clone source must be an enabled agent"
            );
        }
        AgentOperation::Edit(patch) => {
            ensure!(
                view.agents.contains_key(target),
                "agent {target:?} does not exist"
            );
            ensure!(
                !view.protected_ids.contains(target),
                "protected agent cannot be edited"
            );
            for (field, value) in patch {
                ensure!(
                    matches!(
                        field.as_str(),
                        "display_name" | "description" | "system_prompt" | "inference_profile_id"
                    ),
                    "unsupported agent edit field {field:?}"
                );
                match (field.as_str(), value) {
                    ("display_name", Some(value)) => ensure!(
                        value.as_str().is_some_and(|s| !s.is_empty()),
                        "display_name cannot be empty"
                    ),
                    ("system_prompt", Some(value)) => ensure!(
                        value.as_str().is_some_and(|s| !s.trim().is_empty()),
                        "system_prompt cannot be blank"
                    ),
                    ("inference_profile_id", value) => ensure!(
                        value
                            .as_ref()
                            .and_then(|v| v.as_str())
                            .is_some_and(
                                |s| !s.trim().is_empty() && view.published_profiles.contains(s)
                            ),
                        "select a published inference profile"
                    ),
                    _ => {}
                }
            }
        }
        AgentOperation::Disable => {
            ensure!(
                view.agents.contains_key(target),
                "agent {target:?} does not exist"
            );
            ensure!(
                !view.protected_ids.contains(target),
                "protected agent cannot be disabled"
            );
            ensure!(
                !make_default,
                "agent disable cannot also make the target default"
            );
            ensure!(
                view.default_id.as_deref() != Some(target),
                "default agent cannot be disabled"
            );
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
    pub effort: Option<crate::config::ReasoningEffort>,
}

/// Resolves the native candidate projection of `Configuration.resolveAgent`.
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
        .find(|a| a.agent_id == target && a.node_did == node_did && a.enabled)?;
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

pub fn derive_agent_id(
    node_did: &str,
    display_name: &str,
    existing: &BTreeMap<String, bool>,
) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for ch in display_name.chars() {
        if ch.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(ch.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    let base = format!("{node_did}:{slug}");
    let mut id = base.clone();
    let mut suffix = 2;
    while existing.contains_key(&id) {
        id = format!("{base}-{suffix}");
        suffix += 1;
    }
    id
}
