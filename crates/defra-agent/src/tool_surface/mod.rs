mod behavior_config;
mod build;
mod explain;
mod modes;
mod policy;
mod runtime_context;
mod selection;

pub use behavior_config::BehaviorToolConfig;
pub use explain::{ToolSurfaceExplanation, ToolSurfaceWarning};
pub use modes::{BashMode, FileToolMode, ToolCeiling};
pub use policy::{
    EndpointScope, RuntimeToolAvailability, ToolPolicyBash, ToolPolicySurface, ToolPolicyVersion,
    TOOL_POLICY_V1,
};
pub use runtime_context::ToolRuntimeContext;
pub(crate) use selection::{BackgroundToolConfig, OrchestrationToolConfig, SubagentToolConfig};
pub use selection::{CustomToolFactory, ToolSelection};

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::llm::tool::ToolDyn;
use anyhow::Result;

use crate::defra_query::{build_defra_query_tool, CollectionScope, DEFRA_QUERY_TOOL_NAME};
use crate::defra_write::BoundedWriteTool;
use crate::document_config::{SubagentTarget, WriteToolDecl};
use crate::meta_tools::{build_meta_tools, META_TOOL_NAMES};
use crate::toolset::{
    background_tool_names, build_background_tools, build_context_budget_tool,
    build_orchestration_tools, build_session_history_tool, build_subagent_tools,
    orchestration_tool_names, subagent_tool_names, CliToolConfig, ToolSet,
    CONTEXT_BUDGET_TOOL_NAME, SESSION_HISTORY_TOOL_NAME,
};
#[cfg(feature = "agent-memory")]
use crate::toolset::{build_memory_tool, MEMORY_TOOL_NAME};

const DEFAULT_CLI_TIMEOUT_SECS: u64 = 10;

#[derive(Clone)]
pub struct ToolSurface {
    host_tools: ToolSet,
    include_meta_tools: bool,
    allowed_mcp_service_ids: Vec<String>,
    subagent_tools: SubagentToolConfig,
    orchestration_tools: OrchestrationToolConfig,
    background_tools: BackgroundToolConfig,
    custom_tools: Vec<CustomToolFactory>,
    pub(super) enable_memory: bool,
    pub(super) enable_context_budget_tool: bool,
    pub(super) enable_session_history_tool: bool,
    pub(super) enable_defra_query: bool,
    pub(super) defra_query_collections: Vec<String>,
    pub(super) write_tools: Vec<WriteToolDecl>,
}

impl ToolSurface {
    pub fn host_tools(&self) -> &ToolSet {
        &self.host_tools
    }

    pub fn includes_meta_tools(&self) -> bool {
        self.include_meta_tools
    }

    pub fn allowed_mcp_service_ids(&self) -> &[String] {
        &self.allowed_mcp_service_ids
    }

    #[allow(dead_code)]
    pub(crate) fn subagent_tools(&self) -> &SubagentToolConfig {
        &self.subagent_tools
    }

    /// Returns the statically-allowed spawn targets when spawn is enabled,
    /// or an empty slice when spawn is disabled.
    pub(crate) fn subagent_targets(&self) -> &[SubagentTarget] {
        if self.subagent_tools.spawn_enabled {
            &self.subagent_tools.targets
        } else {
            &[]
        }
    }

    pub(crate) fn background_tools(&self) -> &BackgroundToolConfig {
        &self.background_tools
    }

    #[allow(dead_code)]
    pub(crate) fn orchestration_tools(&self) -> &OrchestrationToolConfig {
        &self.orchestration_tools
    }

    /// Drop subagent targets that cannot resolve.
    ///
    /// Local-DID targets (whose `agent_did` equals the agent's own DID) are
    /// retain-filtered against the active local behavior set, since a missing
    /// local behavior means the target genuinely cannot resolve. Remote-DID
    /// targets are retained ONLY when `subagent_allow_cross_deployment` is true;
    /// when the flag is false (the default), remote-DID targets are dropped
    /// upstream in `resolve_with_available_subagent_targets` before this method
    /// is called, so this pass sees none of them.
    pub(crate) fn retain_subagent_targets(
        &mut self,
        own_agent_did: &str,
        active_behavior_ids: &HashSet<String>,
    ) {
        self.subagent_tools.targets.retain(|target| {
            if target.agent_did == own_agent_did {
                active_behavior_ids.contains(&target.behavior_id)
            } else {
                true
            }
        });
    }

    pub fn tool_names(&self) -> Vec<String> {
        let mut names = self.host_tools.tool_names();
        if self.include_meta_tools {
            names.extend(META_TOOL_NAMES.iter().map(|name| (*name).to_string()));
        }
        names.extend(subagent_tool_names(&self.subagent_tools));
        names.extend(orchestration_tool_names(
            &self.orchestration_tools,
            &self.subagent_tools,
        ));
        names.extend(background_tool_names(&self.background_tools));
        names.extend(self.custom_tools.iter().map(|tool| tool.name().to_string()));
        #[cfg(feature = "agent-memory")]
        if self.enable_memory {
            names.push(MEMORY_TOOL_NAME.to_string());
        }
        if self.enable_context_budget_tool {
            names.push(CONTEXT_BUDGET_TOOL_NAME.to_string());
        }
        if self.enable_session_history_tool {
            names.push(SESSION_HISTORY_TOOL_NAME.to_string());
        }
        if self.enable_defra_query {
            names.push(DEFRA_QUERY_TOOL_NAME.to_string());
        }
        for decl in &self.write_tools {
            // Use the single source-of-truth gate on the declaration itself;
            // see `WriteToolDecl::is_well_formed`.
            if decl.is_well_formed() {
                names.push(decl.tool_name.clone());
            }
        }
        build::dedupe_strings(names)
    }

    pub fn build_tools(&self, runtime: &ToolRuntimeContext) -> Result<Vec<Box<dyn ToolDyn>>> {
        let mut tools = self.host_tools.build_native_tools()?;
        if self.include_meta_tools {
            tools.extend(build_meta_tools(
                runtime.node.clone(),
                runtime.mcp_pool.clone(),
                runtime.health_map.clone(),
                runtime.local_hostname.clone(),
                runtime.local_subnet.clone(),
                runtime.agent_did.clone(),
                self.allowed_mcp_service_ids.clone(),
            ));
        }
        tools.extend(build_subagent_tools(self.subagent_tools.clone()));
        tools.extend(build_orchestration_tools(
            self.orchestration_tools.clone(),
            self.subagent_tools.clone(),
        ));
        tools.extend(build_background_tools(self.background_tools.clone()));
        for tool in &self.custom_tools {
            tools.push(tool.build()?);
        }
        #[cfg(feature = "agent-memory")]
        if self.enable_memory {
            tools.push(build_memory_tool(
                runtime.node.clone(),
                runtime.agent_did.clone(),
            ));
        }
        if self.enable_context_budget_tool {
            tools.push(build_context_budget_tool(
                runtime.node.clone(),
                runtime.agent_did.clone(),
            ));
        }
        if self.enable_session_history_tool {
            tools.push(build_session_history_tool(
                runtime.node.clone(),
                runtime.agent_did.clone(),
            ));
        }
        if self.enable_defra_query {
            tools.push(build_defra_query_tool(
                runtime.node.clone(),
                CollectionScope::restricted(self.defra_query_collections.clone()),
            ));
        }
        // Apply-time validation rejects write_tools names that collide with the
        // built-in surface or sibling cli_tool_names, but runtime-discovered
        // tools (e.g. MCP) and code-injected custom tools are not visible to
        // that static check. Guard the registration here so a colliding write
        // tool is dropped (with a warning) rather than registered as a second
        // `ToolDyn` under a name an earlier tool already advertises — which the
        // name-keyed dispatch maps would otherwise resolve last-write-wins.
        let mut registered_names: HashSet<String> = tools.iter().map(|tool| tool.name()).collect();
        for decl in &self.write_tools {
            let tool = BoundedWriteTool::new(runtime.node.clone(), decl.clone());
            if !tool.is_well_formed() {
                tracing::warn!(
                    tool_name = %decl.tool_name,
                    collection = %decl.collection,
                    "skipping malformed write_tools entry",
                );
                continue;
            }
            if !registered_names.insert(tool.name()) {
                tracing::warn!(
                    tool_name = %decl.tool_name,
                    "skipping write_tools entry whose name collides with an already-registered tool",
                );
                continue;
            }
            tools.push(Box::new(tool) as Box<dyn ToolDyn>);
        }
        Ok(tools)
    }
}

impl std::fmt::Debug for ToolSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolSurface")
            .field("host_tools", &self.host_tools)
            .field("include_meta_tools", &self.include_meta_tools)
            .field("allowed_mcp_service_ids", &self.allowed_mcp_service_ids)
            .field("subagent_tools", &self.subagent_tools)
            .field("orchestration_tools", &self.orchestration_tools)
            .field("background_tools", &self.background_tools)
            .field(
                "custom_tools",
                &self
                    .custom_tools
                    .iter()
                    .map(|tool| tool.name())
                    .collect::<Vec<_>>(),
            )
            .field("enable_memory", &self.enable_memory)
            .field(
                "enable_context_budget_tool",
                &self.enable_context_budget_tool,
            )
            .field(
                "enable_session_history_tool",
                &self.enable_session_history_tool,
            )
            .field("enable_defra_query", &self.enable_defra_query)
            .field("defra_query_collections", &self.defra_query_collections)
            .field("write_tools", &self.write_tools)
            .finish()
    }
}

/// Resolves `(name, description)` pairs for the agent's spawnable subagent
/// targets. The description comes directly from the configured
/// [`SubagentTarget`] -- there is no DB lookup, so this works identically for
/// local and remote (cross-node) targets and never blocks runtime startup.
pub(crate) fn resolve_subagent_target_descriptions(
    tool_surface: &ToolSurface,
) -> Vec<(String, String)> {
    tool_surface
        .subagent_targets()
        .iter()
        .map(|target| (target.name.clone(), target.description_text().to_string()))
        .collect()
}

pub fn cli_tool(
    name: impl Into<String>,
    binary_path: impl Into<PathBuf>,
    description: impl Into<String>,
) -> CliToolConfig {
    CliToolConfig {
        name: name.into(),
        binary_path: binary_path.into(),
        description: description.into(),
        allowed_argv_prefixes: Vec::new(),
        env_vars: HashMap::new(),
        working_dir: None,
        timeout_secs: DEFAULT_CLI_TIMEOUT_SECS,
    }
}

#[cfg(test)]
mod tests;
