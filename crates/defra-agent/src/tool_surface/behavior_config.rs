use std::collections::HashSet;

use anyhow::Result;
use defra_node::EmbeddedNode;

use crate::toolset::ToolSet;

use super::build::{
    build_host_tools, dedupe_strings, dedupe_subagent_targets, downgrade_bash,
    downgrade_file_tools, online_mcp_service_ids,
};
use super::modes::ToolCeiling;
use super::policy::{EndpointScope, RuntimeToolAvailability, ToolPolicySurface};
use super::selection::{
    BackgroundToolConfig, CustomToolFactory, OrchestrationToolConfig, SubagentToolConfig,
    ToolSelection,
};
use super::ToolSurface;
use crate::document_config::WriteToolDecl;

#[derive(Clone)]
pub struct BehaviorToolConfig {
    host_tools: ToolSet,
    enable_meta_tools: bool,
    allowed_mcp_service_ids: Vec<String>,
    subagent_tools: SubagentToolConfig,
    orchestration_tools: OrchestrationToolConfig,
    background_tools: BackgroundToolConfig,
    custom_tools: Vec<CustomToolFactory>,
    enable_memory: bool,
    enable_context_budget_tool: bool,
    enable_session_history_tool: bool,
    enable_defra_query: bool,
    defra_query_collections: Vec<String>,
    write_tools: Vec<WriteToolDecl>,
    behavior_policy: ToolPolicySurface,
    ceiling_policy: ToolPolicySurface,
    static_policy: ToolPolicySurface,
}

impl BehaviorToolConfig {
    pub fn meta_only() -> Self {
        Self {
            host_tools: ToolSet::meta_only(),
            enable_meta_tools: true,
            allowed_mcp_service_ids: Vec::new(),
            subagent_tools: SubagentToolConfig::default(),
            orchestration_tools: OrchestrationToolConfig::default(),
            background_tools: BackgroundToolConfig::default(),
            custom_tools: Vec::new(),
            enable_memory: false,
            enable_context_budget_tool: true,
            enable_session_history_tool: false,
            enable_defra_query: true,
            defra_query_collections: Vec::new(),
            write_tools: Vec::new(),
            behavior_policy: ToolPolicySurface::legacy_non_host_wide(
                super::FileToolMode::Off,
                super::BashMode::Off,
            ),
            ceiling_policy: ToolPolicySurface::legacy_non_host_wide(
                super::FileToolMode::Off,
                super::BashMode::Off,
            ),
            static_policy: ToolPolicySurface::legacy_non_host_wide(
                super::FileToolMode::Off,
                super::BashMode::Off,
            ),
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

    pub fn from_tool_selection_document(
        behavior_name: &str,
        selection: &crate::document_config::ToolSelectionDocument,
        ceiling: &ToolCeiling,
        custom_tools: Vec<CustomToolFactory>,
    ) -> Result<Self> {
        Self::from_selection_with_subagent_tools(
            behavior_name,
            ToolSelection::from_document(selection)?,
            ceiling,
            SubagentToolConfig::from_document(selection),
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
        let behavior_policy = ToolPolicySurface::from_selection(&selection, &subagent_tools);
        let ceiling_policy = ceiling.policy().clone();
        let static_policy = ToolPolicySurface::effective(
            &behavior_policy,
            &ceiling_policy,
            &ToolPolicySurface::runtime_all(),
        );
        let ToolSelection {
            file_tools: requested_file_tools,
            file_tool_root,
            bash: requested_bash,
            command_policy,
            cli_tool_names,
            enable_meta_tools: _,
            allowed_mcp_service_ids,
            backgroundable_tool_names,
            orchestration_enabled: _,
            enable_memory,
            enable_session_history_tool: _,
            enable_context_budget,
            enable_defra_query: _,
            defra_query_collections: _,
            write_tools,
        } = selection;
        let file_tools =
            downgrade_file_tools(behavior_name, requested_file_tools, static_policy.file);
        let bash = downgrade_bash(behavior_name, requested_bash, static_policy.bash.tool);
        let cli_tool_names = static_policy.filter_cli_names(cli_tool_names);
        let host_tools = build_host_tools(
            behavior_name,
            file_tools,
            bash,
            command_policy,
            &static_policy.bash,
            file_tool_root.as_deref(),
            &cli_tool_names,
            ceiling,
        )?;

        let effective_allowed_mcp_service_ids =
            effective_string_allowlist(allowed_mcp_service_ids, &static_policy.mcp_services);

        let background_allowlist =
            dedupe_strings(static_policy.filter_background_tools(backgroundable_tool_names));
        for name in &background_allowlist {
            let allowed_mcp_wrapper = static_policy.meta && name == "call_tool";
            if !allowed_mcp_wrapper && !host_tools.is_backgroundable_tool_name(name) {
                anyhow::bail!(
                    "behavior {behavior_name} backgroundable_tool_names entry {name:?} is not a registered backgroundable tool"
                );
            }
        }

        let mut effective_subagent_targets = dedupe_subagent_targets(subagent_tools.targets);
        effective_subagent_targets.retain(|target| {
            static_policy
                .subagent_targets
                .permits(&(target.agent_did.clone(), target.behavior_id.clone()))
        });

        Ok(Self {
            host_tools,
            enable_meta_tools: static_policy.meta,
            allowed_mcp_service_ids: effective_allowed_mcp_service_ids,
            subagent_tools: SubagentToolConfig {
                targets: effective_subagent_targets,
                spawn_enabled: static_policy.spawn,
                steering_enabled: static_policy.steering,
                background_enabled: static_policy.background,
                default_await_mode: subagent_tools.default_await_mode,
                allow_cross_deployment: static_policy.cross_deployment,
            },
            orchestration_tools: OrchestrationToolConfig {
                enabled: static_policy.orchestration,
            },
            background_tools: BackgroundToolConfig {
                allowlist: background_allowlist,
            },
            custom_tools,
            enable_memory: static_policy.memory && enable_memory,
            enable_context_budget_tool: static_policy.context_budget && enable_context_budget,
            enable_session_history_tool: static_policy.session_history,
            enable_defra_query: static_policy.include_defra_query(),
            defra_query_collections: static_policy.defra_query_collections_for_runtime(),
            write_tools: static_policy.write_decls_for_runtime(&write_tools),
            behavior_policy,
            ceiling_policy,
            static_policy,
        })
    }

    pub fn host_tools(&self) -> &ToolSet {
        &self.host_tools
    }

    pub fn meta_tools_requested(&self) -> bool {
        self.enable_meta_tools
    }

    pub(crate) fn memory_requested(&self) -> bool {
        self.enable_memory
    }

    pub(crate) fn defra_query_requested(&self) -> bool {
        self.enable_defra_query
    }

    pub(crate) fn context_budget_requested(&self) -> bool {
        self.enable_context_budget_tool
    }

    pub fn allowed_mcp_service_ids(&self) -> &[String] {
        &self.allowed_mcp_service_ids
    }

    #[allow(dead_code)]
    pub(crate) fn subagent_tools(&self) -> &SubagentToolConfig {
        &self.subagent_tools
    }

    #[allow(dead_code)]
    pub(crate) fn background_tools(&self) -> &BackgroundToolConfig {
        &self.background_tools
    }

    #[allow(dead_code)]
    pub(crate) fn orchestration_tools(&self) -> &OrchestrationToolConfig {
        &self.orchestration_tools
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
        let availability =
            RuntimeToolAvailability::from_online_mcp_services(online_mcp_service_ids(node).await?);
        Ok(self.resolve_with_subagent_tools_for_runtime_availability(availability, subagent_tools))
    }

    #[allow(dead_code)]
    pub(crate) fn resolve_with_subagent_tools_for_mcp_presence(
        &self,
        mcp_services_online: bool,
        subagent_tools: SubagentToolConfig,
    ) -> ToolSurface {
        self.resolve_with_subagent_tools_for_runtime_availability(
            RuntimeToolAvailability::for_mcp_presence(mcp_services_online),
            subagent_tools,
        )
    }

    pub(crate) fn resolve_with_subagent_tools_for_runtime_availability(
        &self,
        availability: RuntimeToolAvailability,
        subagent_tools: SubagentToolConfig,
    ) -> ToolSurface {
        let effective_policy = self.static_policy.meet(&availability.policy);
        let include_meta_tools = effective_policy.include_meta_tools();
        let allowed_mcp_service_ids = effective_string_allowlist(
            self.allowed_mcp_service_ids.clone(),
            &effective_policy.mcp_services,
        );

        ToolSurface {
            host_tools: self.host_tools.clone(),
            include_meta_tools,
            allowed_mcp_service_ids,
            subagent_tools,
            orchestration_tools: self.orchestration_tools.clone(),
            background_tools: self.background_tools.clone(),
            custom_tools: self.custom_tools.clone(),
            enable_memory: effective_policy.memory && self.enable_memory,
            enable_context_budget_tool: effective_policy.context_budget
                && self.enable_context_budget_tool,
            enable_session_history_tool: effective_policy.session_history,
            enable_defra_query: effective_policy.include_defra_query(),
            defra_query_scope: effective_policy.defra_query_collection_scope(),
            write_tools: effective_policy.write_decls_for_runtime(&self.write_tools),
            enable_skills: effective_policy.skills,
        }
    }

    /// Resolve the tool surface, dropping local-DID subagent targets whose
    /// behavior is not in the active local set. Remote-DID targets survive only
    /// when cross-deployment delegation is enabled (`allow_cross_deployment`);
    /// when it is false (the default, #377) remote-DID targets are filtered out
    /// so the model is never told about targets a runtime spawn would reject.
    pub(crate) async fn resolve_with_available_subagent_targets(
        &self,
        node: &EmbeddedNode,
        own_agent_did: &str,
        active_behavior_ids: &HashSet<String>,
    ) -> Result<ToolSurface> {
        let mut subagent_tools = self.subagent_tools.clone();
        let allow_cross_deployment = subagent_tools.allow_cross_deployment;
        subagent_tools.targets.retain(|target| {
            if target.agent_did == own_agent_did {
                active_behavior_ids.contains(&target.behavior_id)
            } else {
                // Remote-DID target: only surface when cross-deployment is enabled.
                allow_cross_deployment
            }
        });
        self.resolve_with_subagent_tools(node, subagent_tools).await
    }

    #[allow(dead_code)]
    pub(crate) fn resolve_with_available_subagent_targets_for_mcp_presence(
        &self,
        mcp_services_online: bool,
        own_agent_did: &str,
        active_behavior_ids: &HashSet<String>,
    ) -> ToolSurface {
        let mut subagent_tools = self.subagent_tools.clone();
        let allow_cross_deployment = subagent_tools.allow_cross_deployment;
        subagent_tools.targets.retain(|target| {
            if target.agent_did == own_agent_did {
                active_behavior_ids.contains(&target.behavior_id)
            } else {
                allow_cross_deployment
            }
        });
        self.resolve_with_subagent_tools_for_mcp_presence(mcp_services_online, subagent_tools)
    }

    pub(crate) fn behavior_policy(&self) -> &ToolPolicySurface {
        &self.behavior_policy
    }

    pub(crate) fn ceiling_policy(&self) -> &ToolPolicySurface {
        &self.ceiling_policy
    }

    pub(crate) fn static_policy(&self) -> &ToolPolicySurface {
        &self.static_policy
    }
}

impl Default for BehaviorToolConfig {
    fn default() -> Self {
        Self::meta_only()
    }
}

fn effective_string_allowlist(
    requested: Vec<String>,
    scope: &EndpointScope<String, ()>,
) -> Vec<String> {
    match scope {
        EndpointScope::None => Vec::new(),
        EndpointScope::All => dedupe_strings(requested),
        EndpointScope::Only(keys) => {
            let requested = dedupe_strings(requested);
            if requested.is_empty() {
                return keys.keys().cloned().collect();
            }
            requested
                .into_iter()
                .filter(|value| keys.contains_key(value))
                .collect()
        }
    }
}

impl std::fmt::Debug for BehaviorToolConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BehaviorToolConfig")
            .field("host_tools", &self.host_tools)
            .field("enable_meta_tools", &self.enable_meta_tools)
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
            .field("behavior_policy", &self.behavior_policy)
            .field("ceiling_policy", &self.ceiling_policy)
            .field("static_policy", &self.static_policy)
            .finish()
    }
}
