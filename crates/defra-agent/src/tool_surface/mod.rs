mod behavior_config;
mod build;
mod modes;
mod runtime_context;
mod selection;

pub use behavior_config::BehaviorToolConfig;
pub use modes::{BashMode, FileToolMode, ToolCeiling};
pub use runtime_context::ToolRuntimeContext;
pub(crate) use selection::{BackgroundToolConfig, SubagentToolConfig};
pub use selection::{CustomToolFactory, ToolSelection};

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use rig::tool::ToolDyn;

use crate::defra_query::{build_defra_query_tool, CollectionScope, DEFRA_QUERY_TOOL_NAME};
use crate::meta_tools::{build_meta_tools, META_TOOL_NAMES};
use crate::toolset::{
    background_tool_names, build_background_tools, build_context_budget_tool, build_delegate_tool,
    build_subagent_tools, subagent_tool_names, CliToolConfig, ToolSet, CONTEXT_BUDGET_TOOL_NAME,
    DELEGATE_TOOL_NAME,
};

const DEFAULT_CLI_TIMEOUT_SECS: u64 = 10;

#[derive(Clone)]
pub struct ToolSurface {
    host_tools: ToolSet,
    include_meta_tools: bool,
    allowed_mcp_service_ids: Vec<String>,
    delegate_to: Vec<String>,
    subagent_tools: SubagentToolConfig,
    background_tools: BackgroundToolConfig,
    custom_tools: Vec<CustomToolFactory>,
    pub(super) enable_defra_query: bool,
    pub(super) defra_query_collections: Vec<String>,
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

    pub fn delegate_to(&self) -> &[String] {
        &self.delegate_to
    }

    #[allow(dead_code)]
    pub(crate) fn subagent_tools(&self) -> &SubagentToolConfig {
        &self.subagent_tools
    }

    pub(crate) fn background_tools(&self) -> &BackgroundToolConfig {
        &self.background_tools
    }

    pub(crate) fn retain_subagent_targets(&mut self, active_behavior_ids: &HashSet<String>) {
        self.subagent_tools
            .targets
            .retain(|target| active_behavior_ids.contains(target));
    }

    pub fn tool_names(&self) -> Vec<String> {
        let mut names = self.host_tools.tool_names();
        if self.include_meta_tools {
            names.extend(META_TOOL_NAMES.iter().map(|name| (*name).to_string()));
        }
        if !self.delegate_to.is_empty() {
            names.push(DELEGATE_TOOL_NAME.to_string());
        }
        names.extend(subagent_tool_names(&self.subagent_tools));
        names.extend(background_tool_names(&self.background_tools));
        names.extend(self.custom_tools.iter().map(|tool| tool.name().to_string()));
        names.push(CONTEXT_BUDGET_TOOL_NAME.to_string());
        if self.enable_defra_query {
            names.push(DEFRA_QUERY_TOOL_NAME.to_string());
        }
        build::dedupe_strings(names)
    }

    pub fn build_tools(&self, runtime: &ToolRuntimeContext) -> Result<Vec<Box<dyn ToolDyn>>> {
        let mut tools = self.host_tools.build_native_tools()?;
        if !self.delegate_to.is_empty() {
            tools.push(build_delegate_tool(
                runtime.node.clone(),
                self.delegate_to.clone(),
            ));
        }
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
        tools.extend(build_background_tools(self.background_tools.clone()));
        for tool in &self.custom_tools {
            tools.push(tool.build()?);
        }
        tools.push(build_context_budget_tool(
            runtime.node.clone(),
            runtime.agent_did.clone(),
        ));
        if self.enable_defra_query {
            tools.push(build_defra_query_tool(
                runtime.node.clone(),
                CollectionScope::restricted(self.defra_query_collections.clone()),
            ));
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
            .field("delegate_to", &self.delegate_to)
            .field("subagent_tools", &self.subagent_tools)
            .field("background_tools", &self.background_tools)
            .field(
                "custom_tools",
                &self
                    .custom_tools
                    .iter()
                    .map(|tool| tool.name())
                    .collect::<Vec<_>>(),
            )
            .field("enable_defra_query", &self.enable_defra_query)
            .field("defra_query_collections", &self.defra_query_collections)
            .finish()
    }
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
