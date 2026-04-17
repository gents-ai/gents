use anyhow::Result;
use defra_node::EmbeddedNode;

use crate::toolset::ToolSet;

use super::build::{
    build_host_tools, dedupe_strings, downgrade_bash, downgrade_file_tools,
    has_registered_mcp_services,
};
use super::modes::ToolCeiling;
use super::selection::{CustomToolFactory, ToolSelection};
use super::ToolSurface;

#[derive(Clone)]
pub struct BehaviorToolConfig {
    host_tools: ToolSet,
    enable_meta_tools: bool,
    delegate_to: Vec<String>,
    custom_tools: Vec<CustomToolFactory>,
}

impl BehaviorToolConfig {
    pub fn meta_only() -> Self {
        Self {
            host_tools: ToolSet::meta_only(),
            enable_meta_tools: true,
            delegate_to: Vec::new(),
            custom_tools: Vec::new(),
        }
    }

    pub fn from_selection(
        behavior_name: &str,
        selection: ToolSelection,
        ceiling: &ToolCeiling,
        custom_tools: Vec<CustomToolFactory>,
    ) -> Result<Self> {
        let ToolSelection {
            file_tools: requested_file_tools,
            file_tool_root,
            bash: requested_bash,
            cli_tool_names,
            enable_meta_tools,
            delegate_to,
        } = selection;
        let file_tools =
            downgrade_file_tools(behavior_name, requested_file_tools, ceiling.file_tools());
        let bash = downgrade_bash(behavior_name, requested_bash, ceiling.bash());
        let host_tools = build_host_tools(
            behavior_name,
            file_tools,
            bash,
            file_tool_root.as_deref(),
            &cli_tool_names,
            ceiling,
        )?;

        Ok(Self {
            host_tools,
            enable_meta_tools,
            delegate_to: dedupe_strings(delegate_to),
            custom_tools,
        })
    }

    pub fn host_tools(&self) -> &ToolSet {
        &self.host_tools
    }

    pub fn meta_tools_requested(&self) -> bool {
        self.enable_meta_tools
    }

    pub fn delegate_to(&self) -> &[String] {
        &self.delegate_to
    }

    pub fn custom_tool_names(&self) -> Vec<String> {
        self.custom_tools
            .iter()
            .map(|tool| tool.name().to_string())
            .collect()
    }

    pub async fn resolve(&self, node: &EmbeddedNode) -> Result<ToolSurface> {
        let include_meta_tools = if self.enable_meta_tools {
            has_registered_mcp_services(node).await?
        } else {
            false
        };

        Ok(ToolSurface {
            host_tools: self.host_tools.clone(),
            include_meta_tools,
            delegate_to: self.delegate_to.clone(),
            custom_tools: self.custom_tools.clone(),
        })
    }
}

impl Default for BehaviorToolConfig {
    fn default() -> Self {
        Self::meta_only()
    }
}

impl std::fmt::Debug for BehaviorToolConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BehaviorToolConfig")
            .field("host_tools", &self.host_tools)
            .field("enable_meta_tools", &self.enable_meta_tools)
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
