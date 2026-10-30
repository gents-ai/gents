//! Raw API keys cannot be changed or returned.

pub mod agent;
mod agent_management;
mod agent_queries;
pub use agent_management::configure_agent;
use agent_management::load_agent_catalog;
#[cfg(test)]
use agent_management::{agent_mutate, agent_preview};
#[cfg(test)]
use agent_queries::node_default_agent;
use agent_queries::{agent_inspect, agent_list, guard_agent_profile_choice};
mod command;
pub(crate) use command::{is_help_call, ConfigCommandParams};
mod execution;
pub use execution::ConfigExecutionReceipt;
mod ops;
mod pack_catalog;
mod pack_documents;
mod pack_installer;
use pack_installer::{ConfigPackDistribution, PackInstallParams, PackInstaller};
mod pack_plugins;
mod read;
mod requests;
#[cfg(test)]
use requests::profile_request;
use requests::{
    agent_request, anchored_request, automation_request, automation_target, backend_request,
    local_backend_create_request, mcp_service_request, outcome_text, profile_create_request,
    profile_target_request, protect_working_agent, refuse_silent_tools_drops, tools_request,
};
#[cfg(test)]
mod tests;
#[cfg(test)]
mod text_tests;

pub use ops::{
    apply_tool_grant_selection, guard_agent_keeps_reach, guard_backend_auth, guard_backend_choice,
    guard_tools_keep_control, guard_tools_keep_grants, reselection_keeps_grants,
    validate_tool_network_selection, OperatorGrants, PatchOutcome, SelfConfigCore,
    EFFECT_TIMING_NOTE,
};

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::config_client::patch::{SelfConfigPatch, SelfConfigTarget};
use crate::graphql::escape_graphql_string;
use crate::inference::llm::tool::{Tool, ToolDefinition, ToolDyn};
use crate::tool_surface::presets::{builtin_preset_names, preset_fields};
use crate::tool_surface::SelfConfigToolConfig;
use crate::NodeIdentity;
use agent::derive_agent_id;
use defra_node::EmbeddedNode;
use ops::{decode_merged, validate_merged_selection, ApplyRequest};

pub const ENGINEER_AGENT_TAG: &str = "gents:engineer";

pub fn engineer_self_config() -> crate::documents::SelfConfigTools {
    serde_json::from_str(gents_protocol::ENGINEER_SELF_CONFIG_JSON)
        .expect("bundled Engineer grant must match canonical SelfConfigTools")
}

pub const CONFIG_TOOL_NAME: &str = "config";

pub const SELF_CONFIG_TOOL_NAMES: [&str; 1] = [CONFIG_TOOL_NAME];

pub(crate) use crate::tool_output::{ordered, Ordered};

const EFFECTIVE_ORDER: &[&str] = &[
    "execution_settings",
    "agent_id",
    "agent",
    "context",
    "inference_profile",
    "documents",
    "automation",
    "skills",
    "runtime_effective",
    "tool_grants",
    "self_config",
    "effect_timing",
];

#[derive(Debug)]
pub struct SelfConfigError(anyhow::Error);

impl std::fmt::Display for SelfConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for SelfConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.root_cause())
    }
}

impl From<anyhow::Error> for SelfConfigError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

/// Model-facing tri-state for agent edits. A present JSON null is `Clear`;
/// the property must be absent to stay `Omitted`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum StringUpdate {
    #[default]
    Omitted,
    Clear,
    Set(String),
}

impl<'de> serde::Deserialize<'de> for StringUpdate {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer).map(|value| match value {
            Some(value) => Self::Set(value),
            None => Self::Clear,
        })
    }
}

impl StringUpdate {
    fn is_present(&self) -> bool {
        !matches!(self, Self::Omitted)
    }

    fn value(&self) -> Option<&str> {
        match self {
            Self::Set(value) => Some(value),
            Self::Omitted | Self::Clear => None,
        }
    }

    fn owned_value(&self) -> Option<String> {
        self.value().map(ToOwned::to_owned)
    }
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigureAgentParams {
    pub action: String,
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub display_name: StringUpdate,
    #[serde(default)]
    pub description: StringUpdate,
    #[serde(default)]
    pub system_prompt: StringUpdate,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub clone_from: Option<String>,
    #[serde(default)]
    pub root: StringUpdate,
    #[serde(default)]
    pub preset: StringUpdate,
    #[serde(default)]
    pub profile_id: StringUpdate,
    #[serde(default)]
    pub make_default: bool,
    #[serde(default)]
    pub enable_lsp: Option<bool>,
    #[serde(default)]
    pub network_mode: Option<crate::toolset::CommandNetworkMode>,
}

pub fn build_self_config_tools(
    usage_cache: Arc<crate::inference::usage_observation::UsageCache>,
    node: Arc<EmbeddedNode>,
    node_did: String,
    identity: Option<Arc<dyn NodeIdentity>>,
    config: &SelfConfigToolConfig,
    plugins: Arc<crate::plugin::executor::PluginExecutor>,
) -> Vec<Box<dyn ToolDyn>> {
    if !config.enabled {
        return Vec::new();
    }
    let core = match SelfConfigCore::new(node.clone(), node_did.clone(), config.agent_id.clone()) {
        Ok(core) => core
            .with_no_lockout(config.no_lockout)
            .with_process_ceiling(config.process_ceiling.clone())
            .with_held_grants(OperatorGrants {
                pack_install: config.enable_pack_install,
                ..Default::default()
            }),
        Err(error) => {
            tracing::warn!(
                agent_id = %config.agent_id,
                %error,
                "self-config tools requested but not registrable; failing closed"
            );
            return Vec::new();
        }
    };

    let mut tools: Vec<Box<dyn ToolDyn>> = Vec::new();
    if !config.enabled {
        return tools;
    }
    tools.push(Box::new(command::ConfigCommandTool {
        usage_cache,
        node,
        node_did,
        identity,
        core,
        categories: config.categories.clone(),
        no_lockout: config.no_lockout,
        preview: config.preview,
        process_ceiling: config.process_ceiling.clone(),
        execution: Arc::new(execution::ExecutionObservation::default()),
        plugins,
    }));
    tools
}

pub fn self_config_tool_names(config: &SelfConfigToolConfig) -> Vec<String> {
    let mut names = Vec::new();
    if !config.enabled {
        return names;
    }
    names.push(CONFIG_TOOL_NAME.to_string());
    names
}
