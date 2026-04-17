mod behavior_config;
mod build;
mod modes;
mod runtime_context;
mod selection;

pub use behavior_config::BehaviorToolConfig;
pub use modes::{BashMode, FileToolMode, ToolCeiling};
pub use runtime_context::ToolRuntimeContext;
pub use selection::{CustomToolFactory, ToolSelection};

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use rig::tool::ToolDyn;

use crate::meta_tools::{build_meta_tools, META_TOOL_NAMES};
use crate::toolset::{build_delegate_tool, CliToolConfig, ToolSet, DELEGATE_TOOL_NAME};

const DEFAULT_CLI_TIMEOUT_SECS: u64 = 10;

#[derive(Clone)]
pub struct ToolSurface {
    host_tools: ToolSet,
    include_meta_tools: bool,
    delegate_to: Vec<String>,
    custom_tools: Vec<CustomToolFactory>,
}

impl ToolSurface {
    pub fn host_tools(&self) -> &ToolSet {
        &self.host_tools
    }

    pub fn includes_meta_tools(&self) -> bool {
        self.include_meta_tools
    }

    pub fn delegate_to(&self) -> &[String] {
        &self.delegate_to
    }

    pub fn tool_names(&self) -> Vec<String> {
        let mut names = self.host_tools.tool_names();
        if self.include_meta_tools {
            names.extend(META_TOOL_NAMES.iter().map(|name| (*name).to_string()));
        }
        if !self.delegate_to.is_empty() {
            names.push(DELEGATE_TOOL_NAME.to_string());
        }
        names.extend(self.custom_tools.iter().map(|tool| tool.name().to_string()));
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
            ));
        }
        for tool in &self.custom_tools {
            tools.push(tool.build()?);
        }
        Ok(tools)
    }
}

impl std::fmt::Debug for ToolSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolSurface")
            .field("host_tools", &self.host_tools)
            .field("include_meta_tools", &self.include_meta_tools)
            .field("delegate_to", &self.delegate_to)
            .field(
                "custom_tools",
                &self
                    .custom_tools
                    .iter()
                    .map(|tool| tool.name())
                    .collect::<Vec<_>>(),
            )
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
