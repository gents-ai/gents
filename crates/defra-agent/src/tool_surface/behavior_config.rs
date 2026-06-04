use std::collections::HashSet;

use anyhow::Result;
use defra_node::EmbeddedNode;

use crate::toolset::ToolSet;

use super::build::{
    build_host_tools, dedupe_strings, dedupe_subagent_targets, downgrade_bash,
    downgrade_file_tools, has_registered_mcp_services,
};
use super::modes::ToolCeiling;
use super::selection::{
    BackgroundToolConfig, CustomToolFactory, SubagentToolConfig, ToolSelection,
};
use super::ToolSurface;

#[derive(Clone)]
pub struct BehaviorToolConfig {
    host_tools: ToolSet,
    enable_meta_tools: bool,
    allowed_mcp_service_ids: Vec<String>,
    delegate_to: Vec<String>,
    subagent_tools: SubagentToolConfig,
    background_tools: BackgroundToolConfig,
    custom_tools: Vec<CustomToolFactory>,
    enable_defra_query: bool,
    defra_query_collections: Vec<String>,
}

impl BehaviorToolConfig {
    pub fn meta_only() -> Self {
        Self {
            host_tools: ToolSet::meta_only(),
            enable_meta_tools: true,
            allowed_mcp_service_ids: Vec::new(),
            delegate_to: Vec::new(),
            subagent_tools: SubagentToolConfig::default(),
            background_tools: BackgroundToolConfig::default(),
            custom_tools: Vec::new(),
            enable_defra_query: true,
            defra_query_collections: Vec::new(),
        }
    }

    pub fn from_selection(
        behavior_name: &str,
        selection: ToolSelection,
        ceiling: &ToolCeiling,
        custom_tools: Vec<CustomToolFactory>,
    ) -> Result<Self> {
        Self::from_selection_with_subagent_tools(
            behavior_name,
            selection,
            ceiling,
            SubagentToolConfig::default(),
            custom_tools,
        )
    }

    pub(crate) fn from_selection_with_subagent_tools(
        behavior_name: &str,
        selection: ToolSelection,
        ceiling: &ToolCeiling,
        subagent_tools: SubagentToolConfig,
        custom_tools: Vec<CustomToolFactory>,
    ) -> Result<Self> {
        let ToolSelection {
            file_tools: requested_file_tools,
            file_tool_root,
            bash: requested_bash,
            command_policy,
            cli_tool_names,
            enable_meta_tools,
            allowed_mcp_service_ids,
            delegate_to,
            backgroundable_tool_names,
            enable_defra_query,
            defra_query_collections,
        } = selection;
        let file_tools =
            downgrade_file_tools(behavior_name, requested_file_tools, ceiling.file_tools());
        let bash = downgrade_bash(behavior_name, requested_bash, ceiling.bash());
        let host_tools = build_host_tools(
            behavior_name,
            file_tools,
            bash,
            command_policy,
            file_tool_root.as_deref(),
            &cli_tool_names,
            ceiling,
        )?;

        let background_allowlist = dedupe_strings(backgroundable_tool_names);
        for name in &background_allowlist {
            let allowed_mcp_wrapper = enable_meta_tools && name == "call_tool";
            if !allowed_mcp_wrapper && !host_tools.is_backgroundable_tool_name(name) {
                anyhow::bail!(
                    "behavior {behavior_name} backgroundable_tool_names entry {name:?} is not a registered backgroundable tool"
                );
            }
        }

        Ok(Self {
            host_tools,
            enable_meta_tools,
            allowed_mcp_service_ids: dedupe_strings(allowed_mcp_service_ids),
            delegate_to: dedupe_strings(delegate_to),
            subagent_tools: SubagentToolConfig {
                targets: dedupe_subagent_targets(subagent_tools.targets),
                spawn_enabled: subagent_tools.spawn_enabled,
                steering_enabled: subagent_tools.steering_enabled,
                background_enabled: subagent_tools.background_enabled,
            },
            background_tools: BackgroundToolConfig {
                allowlist: background_allowlist,
            },
            custom_tools,
            enable_defra_query,
            defra_query_collections: dedupe_strings(defra_query_collections),
        })
    }

    pub fn host_tools(&self) -> &ToolSet {
        &self.host_tools
    }

    pub fn meta_tools_requested(&self) -> bool {
        self.enable_meta_tools
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

    #[allow(dead_code)]
    pub(crate) fn background_tools(&self) -> &BackgroundToolConfig {
        &self.background_tools
    }

    pub fn custom_tool_names(&self) -> Vec<String> {
        self.custom_tools
            .iter()
            .map(|tool| tool.name().to_string())
            .collect()
    }

    pub async fn resolve(&self, node: &EmbeddedNode) -> Result<ToolSurface> {
        self.resolve_with_subagent_tools(node, SubagentToolConfig::default())
            .await
    }

    async fn resolve_with_subagent_tools(
        &self,
        node: &EmbeddedNode,
        subagent_tools: SubagentToolConfig,
    ) -> Result<ToolSurface> {
        let include_meta_tools = if self.enable_meta_tools {
            has_registered_mcp_services(node).await?
        } else {
            false
        };

        Ok(ToolSurface {
            host_tools: self.host_tools.clone(),
            include_meta_tools,
            allowed_mcp_service_ids: self.allowed_mcp_service_ids.clone(),
            delegate_to: self.delegate_to.clone(),
            subagent_tools,
            background_tools: self.background_tools.clone(),
            custom_tools: self.custom_tools.clone(),
            enable_defra_query: self.enable_defra_query,
            defra_query_collections: self.defra_query_collections.clone(),
        })
    }

    /// Resolve the tool surface, dropping local-DID subagent targets whose
    /// behavior is not in the active local set. Remote-DID targets always
    /// survive (they resolve out-of-band via P2P), removing the cross-node
    /// delegation seam.
    pub(crate) async fn resolve_with_available_subagent_targets(
        &self,
        node: &EmbeddedNode,
        own_agent_did: &str,
        active_behavior_ids: &HashSet<String>,
    ) -> Result<ToolSurface> {
        let mut subagent_tools = self.subagent_tools.clone();
        subagent_tools.targets.retain(|target| {
            if target.agent_did == own_agent_did {
                active_behavior_ids.contains(&target.behavior_id)
            } else {
                true
            }
        });
        self.resolve_with_subagent_tools(node, subagent_tools).await
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
