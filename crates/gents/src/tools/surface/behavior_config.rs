use rapidhash::RapidHashSet;

use anyhow::Result;
use defra_node::EmbeddedNode;

use crate::tools::native::ToolSet;

use super::build::{
    build_host_tools, dedupe_agent_targets, dedupe_strings, downgrade_bash, downgrade_file_tools,
    enabled_mcp_service_ids, measured_available_mcp_service_ids,
};
use super::modes::{BashMode, FileToolMode, ToolCeiling};
use super::policy::{EndpointScope, RuntimeToolAvailability, ToolPolicySurface};
use super::selection::{
    AgentToolConfig, BackgroundToolConfig, CustomToolFactory, ResolvedToolSelection,
};
use super::ToolSurface;
use crate::documents::{QueryToolDecl, WriteToolDecl};

#[derive(Clone)]
pub struct AgentToolSurfaceConfig {
    host_tools: ToolSet,
    root_execution_guard: Option<super::RootExecutionGuard>,
    enable_meta_tools: bool,
    enable_goal_tools: bool,
    enable_goal_creation: bool,
    allowed_mcp_service_ids: Vec<String>,
    remote_tools: Option<crate::documents::RemoteTools>,
    required_mcp_service_ids: Vec<String>,
    agent_tools: AgentToolConfig,
    background_tools: BackgroundToolConfig,
    custom_tools: Vec<CustomToolFactory>,
    enable_memory: bool,
    enable_context_budget_tool: bool,
    enable_session_history_tool: bool,
    enable_schema_tool: bool,
    enable_defra_query: bool,
    defra_query_collections: Vec<String>,
    application_write_collections: Vec<String>,
    write_tools: Vec<WriteToolDecl>,
    query_tools: Vec<QueryToolDecl>,
    surface_of_tool: std::collections::BTreeMap<String, String>,
    plugin_tools: Vec<crate::documents::PluginToolRef>,
    self_config: super::SelfConfigToolConfig,
    agent_policy: ToolPolicySurface,
    ceiling_policy: ToolPolicySurface,
    static_policy: ToolPolicySurface,
    lsp_config: Option<String>,
    lsp_action_timeout: super::BoundedTimeout,
}

impl AgentToolSurfaceConfig {
    pub fn meta_only() -> Self {
        let mut agent_policy = ToolPolicySurface::ceiling_with_host_modes(
            super::FileToolMode::Off,
            super::BashMode::Off,
        );
        agent_policy.goal_create = false;
        agent_policy.defra_query = false;
        agent_policy.defra_collections = EndpointScope::none();
        agent_policy.self_config = false;
        agent_policy.schema_management = false;
        agent_policy.p2p_read = false;
        agent_policy.p2p_mutate = false;
        agent_policy.p2p_collections = EndpointScope::none();
        agent_policy.self_config_categories = EndpointScope::none();
        Self {
            host_tools: ToolSet::meta_only(),
            root_execution_guard: None,
            enable_meta_tools: true,
            enable_goal_tools: true,
            enable_goal_creation: false,
            allowed_mcp_service_ids: Vec::new(),
            remote_tools: None,
            required_mcp_service_ids: Vec::new(),
            agent_tools: AgentToolConfig::default(),
            background_tools: BackgroundToolConfig::default(),
            custom_tools: Vec::new(),
            enable_memory: false,
            enable_context_budget_tool: true,
            enable_session_history_tool: true,
            enable_schema_tool: false,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            application_write_collections: Vec::new(),
            write_tools: Vec::new(),
            query_tools: Vec::new(),
            surface_of_tool: Default::default(),
            plugin_tools: Vec::new(),
            self_config: super::SelfConfigToolConfig::default(),
            agent_policy: agent_policy.clone(),
            ceiling_policy: ToolPolicySurface::ceiling_with_host_modes(
                super::FileToolMode::Off,
                super::BashMode::Off,
            ),
            static_policy: agent_policy,
            lsp_config: None,
            lsp_action_timeout: super::BoundedTimeout::lsp_action(None, None),
        }
    }

    pub fn from_selection(
        agent_name: &str,
        selection: ResolvedToolSelection,
        ceiling: &ToolCeiling,
        custom_tools: Vec<CustomToolFactory>,
    ) -> Result<Self> {
        Self::from_selection_with_agent_tools(
            agent_name,
            selection,
            ceiling,
            AgentToolConfig::default(),
            custom_tools,
        )
    }

    pub fn from_tools_document(
        agent_name: &str,
        tools: &crate::documents::Tools,
        ceiling: &ToolCeiling,
        custom_tools: Vec<CustomToolFactory>,
    ) -> Result<Self> {
        Self::from_tools_document_with_surfaces(agent_name, tools, &[], ceiling, custom_tools)
    }

    pub fn from_tools_document_with_surfaces(
        agent_name: &str,
        tools: &crate::documents::Tools,
        surfaces: &[crate::documents::DatastoreToolSurfaceDocument],
        ceiling: &ToolCeiling,
        custom_tools: Vec<CustomToolFactory>,
    ) -> Result<Self> {
        Self::from_tools_documents(agent_name, tools, surfaces, &[], ceiling, custom_tools)
    }

    pub fn from_tools_documents(
        agent_name: &str,
        tools: &crate::documents::Tools,
        surfaces: &[crate::documents::DatastoreToolSurfaceDocument],
        targets: &[crate::documents::AgentTargetDocument],
        ceiling: &ToolCeiling,
        custom_tools: Vec<CustomToolFactory>,
    ) -> Result<Self> {
        let merged = crate::documents::merge_datastore_tool_surfaces(tools, surfaces)?;
        let mut resolved = ResolvedToolSelection::from_document(tools)?;
        resolved.write_tools = merged.write_tools;
        resolved.query_tools = merged.query_tools;
        resolved.surface_of_tool = merged.surface_of_tool;
        Self::from_selection_with_agent_tools(
            agent_name,
            resolved,
            ceiling,
            AgentToolConfig::from_document_with_targets(tools, targets)?,
            custom_tools,
        )
    }

    pub(crate) fn from_selection_with_agent_tools(
        agent_name: &str,
        selection: ResolvedToolSelection,
        ceiling: &ToolCeiling,
        agent_tools: AgentToolConfig,
        custom_tools: Vec<CustomToolFactory>,
    ) -> Result<Self> {
        let custom_tool_names = custom_tools
            .iter()
            .map(|tool| tool.name().to_string())
            .collect::<Vec<_>>();
        crate::documents::validate_surface_tool_names(
            &selection.write_tools,
            &selection.query_tools,
            &selection.cli_tool_names,
            &custom_tool_names,
        )?;
        if selection.enable_lsp {
            crate::tools::native::lsp::LspConfigDocument::parse_operator(
                selection.lsp_config.as_deref(),
            )
            .map_err(|err| anyhow::anyhow!("invalid lsp_config: {err}"))?;
        }
        let agent_policy = ToolPolicySurface::from_selection(&selection, &agent_tools);
        let ceiling_policy = ceiling.policy().clone();
        let static_policy = ToolPolicySurface::effective(
            &agent_policy,
            &ceiling_policy,
            &ToolPolicySurface::runtime_all(),
        );
        let ResolvedToolSelection {
            file_tools: requested_file_tools,
            file_tool_root,
            bash: requested_bash,
            command_policy,
            cli_tool_names,
            command_output_limits,
            timeouts,
            file_limits,
            enable_meta_tools: _,
            enable_goal_tools: _,
            enable_goal_creation: _,
            allowed_mcp_service_ids,
            remote_tools,
            required_mcp_service_ids,
            backgroundable_tool_names,
            enable_memory,
            enable_session_history_tool: _,
            p2p_collections: _,
            enable_schema_tool: _,
            enable_p2p_tool: _,
            enable_p2p_mutations: _,
            enable_context_budget,
            enable_defra_query: _,
            defra_query_collections: _,
            application_write_collections,
            write_tools,
            query_tools,
            surface_of_tool,
            enable_self_config: _,
            self_config_categories: _,
            self_config_no_lockout,
            self_config_preview,
            enable_pack_install,
            enable_lsp: _,
            lsp_config,
            plugin_tools,
        } = selection;
        let file_tools = downgrade_file_tools(agent_name, requested_file_tools, static_policy.file);
        let bash = downgrade_bash(agent_name, requested_bash, static_policy.bash.tool);
        let cli_tool_names = static_policy.filter_cli_names(cli_tool_names);
        let root_was_authored = file_tool_root.is_some();
        let host_tools = build_host_tools(
            agent_name,
            file_tools,
            bash,
            command_policy,
            &static_policy.bash,
            file_tool_root.as_deref(),
            &cli_tool_names,
            &command_output_limits,
            &timeouts,
            file_limits,
            static_policy.lsp,
            ceiling,
        )?;
        let root_execution_guard = (file_tools != FileToolMode::Off
            || bash != BashMode::Off
            || !cli_tool_names.is_empty()
            || static_policy.lsp)
            .then(|| super::RootExecutionGuard {
                agent_id: agent_name.to_string(),
                selected_root: root_was_authored
                    .then(|| host_tools.read_root().map(ToOwned::to_owned))
                    .flatten(),
                ceiling_root: ceiling.root().map(ToOwned::to_owned),
            });

        let effective_allowed_mcp_service_ids =
            effective_string_allowlist(allowed_mcp_service_ids, &static_policy.mcp_services);
        let required_mcp_service_ids = dedupe_strings(required_mcp_service_ids);
        if !required_mcp_service_ids.is_empty() && !static_policy.meta {
            anyhow::bail!(
                "agent {agent_name} requires MCP services but meta tools are disabled by policy"
            );
        }
        for service_id in &required_mcp_service_ids {
            if !static_policy.mcp_services.permits(service_id) {
                anyhow::bail!(
                    "agent {agent_name} requires MCP service {service_id:?}, but the effective tool policy denies it"
                );
            }
            if !effective_allowed_mcp_service_ids.contains(service_id) {
                anyhow::bail!(
                    "agent {agent_name} requires MCP service {service_id:?}, but its MCP allowlist does not permit it"
                );
            }
        }

        let background_allowlist = dedupe_strings(
            static_policy
                .filter_background_tools(backgroundable_tool_names)
                .into_iter()
                .filter_map(|name| match (name.as_str(), bash) {
                    ("bash_unrestricted", BashMode::ReadOnly) => Some("bash".to_owned()),
                    ("bash_unrestricted" | "bash", BashMode::Off) => None,
                    _ => Some(name),
                })
                .collect(),
        );
        for name in &background_allowlist {
            let allowed_mcp_wrapper = static_policy.meta
                && remote_tools.as_ref().is_some_and(|remote| {
                    remote.services.iter().any(|service| {
                        static_policy.mcp_services.permits(&service.mcp_service_id)
                            && effective_allowed_mcp_service_ids.contains(&service.mcp_service_id)
                            && match service.style {
                                crate::documents::RemoteToolStyle::Discovery => {
                                    name == "call_tool" && !service.background_tool_names.is_empty()
                                }
                                crate::documents::RemoteToolStyle::Flat => {
                                    service.background_tool_names.iter().any(|tool| {
                                        crate::meta_tools::flat_tool_name(
                                            &service.mcp_service_id,
                                            tool,
                                        ) == *name
                                    })
                                }
                            }
                    })
                });
            if !allowed_mcp_wrapper && !host_tools.is_backgroundable_tool_name(name) {
                anyhow::bail!(
                    "agent {agent_name} backgroundable_tool_names entry {name:?} is not a registered backgroundable tool"
                );
            }
        }

        let mut effective_agent_targets = dedupe_agent_targets(agent_tools.targets);
        effective_agent_targets.retain(|target| {
            static_policy
                .agent_targets
                .permits(&(target.target_node_did.clone(), target.agent_id.clone()))
        });

        Ok(Self {
            host_tools,
            root_execution_guard,
            enable_meta_tools: static_policy.meta,
            enable_goal_tools: static_policy.include_goal_tools(),
            enable_goal_creation: static_policy.include_goal_creation(),
            allowed_mcp_service_ids: effective_allowed_mcp_service_ids,
            remote_tools,
            required_mcp_service_ids,
            agent_tools: AgentToolConfig {
                targets: effective_agent_targets,
                enabled: static_policy.session_messages,
            },
            background_tools: BackgroundToolConfig {
                allowlist: background_allowlist,
                timeouts: timeouts.background.clone(),
            },
            custom_tools,
            enable_memory: static_policy.memory && enable_memory,
            enable_context_budget_tool: static_policy.context_budget && enable_context_budget,
            enable_session_history_tool: static_policy.session_history,
            enable_schema_tool: static_policy.schema_management,
            enable_defra_query: static_policy.include_defra_query(),
            defra_query_collections: static_policy.defra_query_collections_for_runtime(),
            application_write_collections: static_policy
                .application_write_collections_for_runtime(&application_write_collections),
            write_tools: static_policy.write_decls_for_runtime(&write_tools),
            query_tools: static_policy.query_decls_for_runtime(&query_tools),
            surface_of_tool,
            plugin_tools: plugin_tools
                .into_iter()
                .filter(|plugin| {
                    static_policy
                        .plugin_tools
                        .permits(&plugin.tool_name().to_string())
                })
                .collect(),
            self_config: super::SelfConfigToolConfig {
                enabled: static_policy.include_self_config(),
                agent_id: agent_name.to_string(),
                categories: static_policy.self_config_category_set(),
                no_lockout: self_config_no_lockout,
                preview: self_config_preview,
                enable_pack_install,
                process_ceiling: super::SelfConfigProcessCeiling {
                    file_mode: ceiling.file_tools(),
                    bash_mode: ceiling.bash(),
                    root: ceiling.root().map(ToOwned::to_owned),
                },
            },
            agent_policy,
            ceiling_policy,
            static_policy,
            lsp_config,
            lsp_action_timeout: timeouts.lsp_action,
        })
    }

    pub fn host_tools(&self) -> &ToolSet {
        &self.host_tools
    }

    pub fn meta_tools_requested(&self) -> bool {
        self.enable_meta_tools
    }

    pub fn goal_tools_requested(&self) -> bool {
        self.enable_goal_tools
    }

    pub fn goal_creation_requested(&self) -> bool {
        self.enable_goal_creation
    }

    pub(crate) fn memory_requested(&self) -> bool {
        self.enable_memory
    }

    pub(crate) fn defra_query_requested(&self) -> bool {
        self.enable_defra_query
    }

    pub(crate) fn self_config_requested(&self) -> bool {
        self.self_config.enabled
    }

    pub(crate) fn context_budget_requested(&self) -> bool {
        self.enable_context_budget_tool
    }

    pub(crate) fn remote_tools(&self) -> Option<&crate::documents::RemoteTools> {
        self.remote_tools.as_ref()
    }

    pub fn allowed_mcp_service_ids(&self) -> &[String] {
        &self.allowed_mcp_service_ids
    }

    pub fn required_mcp_service_ids(&self) -> &[String] {
        &self.required_mcp_service_ids
    }

    #[allow(dead_code)]
    pub(crate) fn agent_tools(&self) -> &AgentToolConfig {
        &self.agent_tools
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

    pub async fn resolve(
        &self,
        node: &std::sync::Arc<EmbeddedNode>,
        node_did: &str,
        plugins: &crate::plugin::executor::PluginExecutor,
    ) -> Result<ToolSurface> {
        self.resolve_with_agent_tools(node, node_did, plugins, AgentToolConfig::default())
            .await
    }

    async fn resolve_with_agent_tools(
        &self,
        node: &std::sync::Arc<EmbeddedNode>,
        node_did: &str,
        plugins: &crate::plugin::executor::PluginExecutor,
        agent_tools: AgentToolConfig,
    ) -> Result<ToolSurface> {
        let available_service_ids = enabled_mcp_service_ids(node, node_did).await?;
        if !self.required_mcp_service_ids.is_empty() {
            let measured_available = measured_available_mcp_service_ids(node, node_did).await?;
            let measured_available = measured_available.iter().collect::<RapidHashSet<_>>();
            let missing = self
                .required_mcp_service_ids
                .iter()
                .filter(|service_id| !measured_available.contains(service_id))
                .cloned()
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                anyhow::bail!(
                    "required MCP services are not measured available: {}",
                    missing.join(", ")
                );
            }
        }
        let availability = RuntimeToolAvailability::from_online_mcp_services(available_service_ids);
        let mut surface =
            self.resolve_with_agent_tools_for_runtime_availability(availability, agent_tools);
        surface.plugin_resolutions = resolve_plugin_identities(plugins, &surface.plugin_tools);
        Ok(surface)
    }

    #[allow(dead_code)]
    pub(crate) fn resolve_with_agent_tools_for_mcp_presence(
        &self,
        mcp_services_online: bool,
        agent_tools: AgentToolConfig,
    ) -> ToolSurface {
        self.resolve_with_agent_tools_for_runtime_availability(
            RuntimeToolAvailability::for_mcp_presence(mcp_services_online),
            agent_tools,
        )
    }

    pub(crate) fn resolve_with_agent_tools_for_runtime_availability(
        &self,
        availability: RuntimeToolAvailability,
        agent_tools: AgentToolConfig,
    ) -> ToolSurface {
        let effective_policy = self.static_policy.meet(&availability.policy);
        let include_meta_tools = effective_policy.include_meta_tools();
        let include_goal_tools = effective_policy.include_goal_tools();
        let include_goal_creation = effective_policy.include_goal_creation();
        let allowed_mcp_service_ids = effective_string_allowlist(
            self.allowed_mcp_service_ids.clone(),
            &effective_policy.mcp_services,
        );

        ToolSurface {
            host_tools: self.host_tools.clone(),
            root_execution_guard: self.root_execution_guard.clone(),
            include_meta_tools,
            include_goal_tools,
            include_goal_creation,
            allowed_mcp_service_ids,
            remote_tools: self.remote_tools.clone(),
            agent_tools,
            background_tools: self.background_tools.clone(),
            custom_tools: self.custom_tools.clone(),
            enable_memory: effective_policy.memory && self.enable_memory,
            enable_context_budget_tool: effective_policy.context_budget
                && self.enable_context_budget_tool,
            enable_session_history_tool: effective_policy.session_history,
            enable_schema_tool: effective_policy.schema_management,
            enable_p2p_tool: effective_policy.p2p_read,
            enable_p2p_mutations: effective_policy.permits_p2p_mutation(),
            p2p_collections: effective_policy.p2p_collections.clone(),
            enable_defra_query: effective_policy.include_defra_query(),
            defra_query_scope: effective_policy.defra_query_collection_scope(),
            application_write_collections: effective_policy
                .application_write_collections_for_runtime(&self.application_write_collections),
            write_tools: effective_policy.write_decls_for_runtime(&self.write_tools),
            query_tools: effective_policy.query_decls_for_runtime(&self.query_tools),
            surface_of_tool: self.surface_of_tool.clone(),
            plugin_tools: self
                .plugin_tools
                .iter()
                .filter(|plugin| {
                    effective_policy
                        .plugin_tools
                        .permits(&plugin.tool_name().to_string())
                })
                .cloned()
                .collect(),
            plugin_resolutions: Vec::new(),
            enable_skills: effective_policy.skills,
            self_config: super::SelfConfigToolConfig {
                enabled: effective_policy.include_self_config(),
                categories: effective_policy.self_config_category_set(),
                ..self.self_config.clone()
            },
            lsp: effective_policy.lsp.then(|| {
                let doc = crate::tools::native::lsp::LspConfigDocument::parse_operator(
                    self.lsp_config.as_deref(),
                )
                .unwrap_or_default();
                let servers = crate::tools::native::lsp::merge_catalog(self.lsp_config.as_deref());
                let workspace = self
                    .host_tools
                    .read_root()
                    .map(ToOwned::to_owned)
                    .or_else(|| {
                        self.host_tools
                            .native_tools()
                            .iter()
                            .find_map(|tool| match tool {
                                crate::tools::native::NativeTool::WriteFile { root }
                                | crate::tools::native::NativeTool::EditFile { root }
                                | crate::tools::native::NativeTool::BashUnrestricted {
                                    root, ..
                                } => Some(root.clone()),
                                _ => None,
                            })
                    })
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
                let overlay = doc
                    .network_mode
                    .as_deref()
                    .and_then(|mode| crate::tools::native::CommandNetworkMode::parse(mode).ok());
                let constraints = crate::tools::native::lsp::constraints_from_effective_policy(
                    &effective_policy,
                    overlay,
                );
                crate::tools::native::lsp::LspToolConfig {
                    lsp: true,
                    file: effective_policy.file,
                    digest: crate::tools::native::lsp::config_digest(
                        &workspace,
                        &servers,
                        &constraints,
                    ),
                    workspace,
                    session_id: String::new(),
                    agent_id: self.self_config.agent_id.clone(),
                    servers,
                    constraints,
                    format_on_write: doc.format_on_write.unwrap_or(false),
                    diagnostics_on_write: doc.diagnostics_on_write.unwrap_or(false),
                    diagnostics_on_edit: doc.diagnostics_on_edit.unwrap_or(false),
                    diagnostics_deduplicate: doc.diagnostics_deduplicate.unwrap_or(false),
                    idle_timeout: doc.idle_timeout(),
                    action_timeout: self.lsp_action_timeout,
                }
            }),
        }
    }

    pub(crate) async fn resolve_with_available_agent_targets(
        &self,
        node: &std::sync::Arc<EmbeddedNode>,
        own_node_did: &str,
        active_agent_ids: &RapidHashSet<String>,
        plugins: &crate::plugin::executor::PluginExecutor,
    ) -> Result<ToolSurface> {
        let mut agent_tools = self.agent_tools.clone();
        agent_tools.targets.retain(|target| {
            target.target_node_did != own_node_did || active_agent_ids.contains(&target.agent_id)
        });
        self.resolve_with_agent_tools(node, own_node_did, plugins, agent_tools)
            .await
    }

    #[allow(dead_code)]
    pub(crate) fn resolve_with_available_agent_targets_for_mcp_presence(
        &self,
        mcp_services_online: bool,
        own_node_did: &str,
        active_agent_ids: &RapidHashSet<String>,
    ) -> ToolSurface {
        let mut agent_tools = self.agent_tools.clone();
        agent_tools.targets.retain(|target| {
            target.target_node_did != own_node_did || active_agent_ids.contains(&target.agent_id)
        });
        self.resolve_with_agent_tools_for_mcp_presence(mcp_services_online, agent_tools)
    }

    pub(crate) fn agent_policy(&self) -> &ToolPolicySurface {
        &self.agent_policy
    }

    pub(crate) fn ceiling_policy(&self) -> &ToolPolicySurface {
        &self.ceiling_policy
    }

    pub(crate) fn static_policy(&self) -> &ToolPolicySurface {
        &self.static_policy
    }
}

impl Default for AgentToolSurfaceConfig {
    fn default() -> Self {
        Self::meta_only()
    }
}

fn resolve_plugin_identities(
    plugins: &crate::plugin::executor::PluginExecutor,
    plugin_tools: &[crate::documents::PluginToolRef],
) -> Vec<(
    crate::documents::PluginToolRef,
    Option<crate::plugin::store::InstalledPlugin>,
)> {
    plugin_tools
        .iter()
        .map(|plugin| {
            let record = plugins
                .resolve(&plugin.plugin, plugin.digest.as_deref())
                .ok();
            (plugin.clone(), record)
        })
        .collect()
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
            requested
                .into_iter()
                .filter(|value| keys.contains_key(value))
                .collect()
        }
    }
}

impl std::fmt::Debug for AgentToolSurfaceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentToolSurfaceConfig")
            .field("host_tools", &self.host_tools)
            .field("enable_meta_tools", &self.enable_meta_tools)
            .field("enable_goal_tools", &self.enable_goal_tools)
            .field("enable_goal_creation", &self.enable_goal_creation)
            .field("allowed_mcp_service_ids", &self.allowed_mcp_service_ids)
            .field("remote_tools", &self.remote_tools)
            .field("required_mcp_service_ids", &self.required_mcp_service_ids)
            .field("agent_tools", &self.agent_tools)
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
            .field("enable_schema_tool", &self.enable_schema_tool)
            .field(
                "enable_session_history_tool",
                &self.enable_session_history_tool,
            )
            .field("enable_defra_query", &self.enable_defra_query)
            .field("defra_query_collections", &self.defra_query_collections)
            .field("write_tools", &self.write_tools)
            .field("query_tools", &self.query_tools)
            .field("agent_policy", &self.agent_policy)
            .field("ceiling_policy", &self.ceiling_policy)
            .field("static_policy", &self.static_policy)
            .field("lsp_config", &self.lsp_config)
            .finish()
    }
}
