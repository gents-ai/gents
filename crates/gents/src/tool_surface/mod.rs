mod behavior_config;
mod build;
mod explain;
mod modes;
mod output_budget;
mod policy;
pub mod presets;
mod root_admission;
mod runtime_context;
mod selection;
mod timeouts;

pub use behavior_config::AgentToolSurfaceConfig;
pub(crate) use build::measured_available_mcp_service_ids;
pub use build::measured_mcp_services_for_access;
#[doc(hidden)]
pub use build::resolve_effective_tool_root;
pub use explain::{ToolSurfaceExplanation, ToolSurfaceWarning};
pub use modes::{BashMode, FileToolMode, ToolCeiling};
pub(crate) use output_budget::configured_output_budget;
pub use policy::{
    EndpointScope, RuntimeToolAvailability, ToolPolicyBash, ToolPolicySurface, ToolPolicyVersion,
    TOOL_POLICY_V1,
};
#[doc(hidden)]
pub use root_admission::{
    admit_authored_root, canonicalize_tools_root, load_workspace_root_policy,
    project_workspace_root_policy, resolve_configured_tool_root, WorkspaceRootDocument,
    WorkspaceRootPolicy,
};
pub(crate) use root_admission::{
    load_workspace_root_policy_in_txn, resolve_admitted_tool_root, RootExecutionGuard,
};
pub use runtime_context::ToolRuntimeContext;
pub use selection::{resolve_goal_capabilities, CustomToolFactory, ResolvedToolSelection};
pub(crate) use selection::{AgentToolConfig, BackgroundToolConfig};
pub use timeouts::{BackgroundTimeoutPolicy, BackgroundTimeouts, BoundedTimeout, ToolTimeouts};

use gents_protocol::error_chain::ErrorChain;
use rapidhash::{HashMapExt, RapidHashMap, RapidHashSet};
use std::path::PathBuf;

use crate::llm::tool::ToolDyn;
use anyhow::Result;

#[derive(Debug, thiserror::Error)]
pub enum ToolSurfaceError {
    #[error("unknown file tool mode {mode}")]
    UnknownFileToolMode { mode: String },
    #[error("unknown bash mode {mode}")]
    UnknownBashMode { mode: String },
    #[error("tool_policy_version is required")]
    ToolPolicyVersionRequired,
    #[error("unknown tool policy version {version:?}")]
    UnknownToolPolicyVersion { version: String },
    #[error(transparent)]
    Serialize(#[from] serde_json::Error),
    #[error("invalid lsp_config: {error}")]
    InvalidLspConfig { error: String },
    #[error("agent {agent_name} requires MCP services but meta tools are disabled by policy")]
    McpServicesRequireMeta { agent_name: String },
    #[error("agent {agent_name} requires MCP service {service_id:?}, but the effective tool policy denies it")]
    McpServiceDenied {
        agent_name: String,
        service_id: String,
    },
    #[error("agent {agent_name} requires MCP service {service_id:?}, but its MCP allowlist does not permit it")]
    McpServiceNotAllowlisted {
        agent_name: String,
        service_id: String,
    },
    #[error("agent {agent_name} backgroundable_tool_names entry {name:?} is not a registered backgroundable tool")]
    BackgroundableToolUnregistered { agent_name: String, name: String },
    #[error("required MCP services are not measured available: {services}")]
    McpServicesUnmeasured { services: String },
    #[error("duplicate scoped AgentTarget {target_id}")]
    DuplicateScopedAgentTarget { target_id: String },
    #[error("missing same-owner AgentTarget {id}")]
    MissingSameOwnerAgentTarget { id: String },
    #[error("invalid AgentTarget {id}")]
    InvalidAgentTarget { id: String },
    #[error("duplicate agent target name {name}")]
    DuplicateAgentTargetName { name: String },
    #[error("agent {agent_id} has active host tools but no explicit root under current WorkspaceRoot policy")]
    ActiveHostToolsWithoutRoot { agent_id: String },
    #[error("agent {agent_id} persisted root {} re-resolved to {} at the execution boundary", selected_root.display(), resolved_root.display())]
    RootReresolved {
        agent_id: String,
        selected_root: std::path::PathBuf,
        resolved_root: std::path::PathBuf,
    },
    #[error("agent {agent_id} persisted root {} is not admitted by current WorkspaceRoot policy at the execution boundary: {reason}", selected_root.display())]
    RootNotAdmittedAtBoundary {
        agent_id: String,
        selected_root: std::path::PathBuf,
        reason: String,
    },
    #[error("configured tool root must be absolute, got {}", root.display())]
    ConfiguredRootNotAbsolute { root: std::path::PathBuf },
    #[error("configured WorkspaceRoot policy requires an explicit root")]
    WorkspaceRootRequiresExplicitRoot,
    #[error(
        "Tools.host.root {root:?} is not admitted by published WorkspaceRoot policy: {reason}"
    )]
    RootNotAdmitted { root: String, reason: String },
    #[error("active host tools require an explicit root in Tools.host.root while WorkspaceRoot policy is configured. Set host.root to an admitted absolute workspace path (a sibling of host.files and host.bash). In config, read tools get first, then tools update with set.host preserving its other fields; do not put root inside host.files")]
    ActiveHostToolsRequireRoot,
    #[error("readwrite file tools require a configured tool root")]
    ReadWriteFileToolsRequireRoot,
    #[error("unrestricted bash requires a configured tool root")]
    UnrestrictedBashRequiresRoot,
    #[error("agent {agent_name} file tool root {} escapes operator tool root {}: {reason}", selection_root.display(), ceiling_root.display())]
    FileToolRootEscapes {
        agent_name: String,
        selection_root: std::path::PathBuf,
        ceiling_root: std::path::PathBuf,
        reason: String,
    },
    #[error("MCP availability requires node scope")]
    McpAvailabilityRequiresNodeScope,
    #[error("MCP availability received foreign service")]
    ForeignMcpService,
    #[error("duplicate scoped MCP service")]
    DuplicateScopedMcpService,
    #[error("MCP health query returned no rows array")]
    McpHealthRowsMissing,
    #[error("datastore tools require the node identity")]
    DatastoreRequiresIdentity,
    #[error("datastore identity differs from node DID")]
    DatastoreIdentityMismatch,
    #[error("write tool `{tool_name}` reached registration with an invalid declaration")]
    WriteToolInvalid { tool_name: String },
    #[error("write tool `{tool_name}` reached registration with a duplicate tool name")]
    WriteToolDuplicate { tool_name: String },
    #[error("missing notification policy")]
    MissingNotificationPolicy,
    #[error("write tool `{tool_name}` on collection `{collection}`: {error}. Declared by datastore surface `{surface}`; inspect with config datastore get, then use datastore update to correct its fields or schema collection update to correct the existing collection")]
    WriteToolNotWellFormed {
        tool_name: String,
        collection: String,
        error: String,
        surface: String,
    },
    #[error("query tool `{tool_name}` reached registration with an invalid declaration")]
    QueryToolInvalid { tool_name: String },
    #[error("query tool `{tool_name}` reached registration with a duplicate tool name")]
    QueryToolDuplicate { tool_name: String },
    #[error("eth query tool `{name}` reached registration with a duplicate tool name")]
    EthQueryToolDuplicate { name: String },
    #[error("eth call tool `{name}` reached registration with a duplicate tool name")]
    EthCallToolDuplicate { name: String },
    #[error("plugin tool `{name}` has the same name as another tool; rename one of them")]
    PluginToolDuplicate { name: String },
}

use crate::defra_query::{
    build_defra_query_tool, BoundedQueryTool, CollectionScope, DEFRA_QUERY_TOOL_NAME,
};
use crate::defra_write::BoundedWriteTool;
use crate::document_config::{AgentTargetDocument, QueryToolDecl, WriteToolDecl};
use crate::meta_tools::build_meta_tools;
use crate::toolset::{
    agent_tool_names, background_tool_names, build_agent_tools, build_background_tools,
    build_context_budget_tool, build_goal_tools, build_session_history_tool, CliToolConfig,
    ToolSet, CONTEXT_BUDGET_TOOL_NAME, SESSION_HISTORY_TOOL_NAME,
};
#[cfg(feature = "agent-memory")]
use crate::toolset::{build_memory_tool, MEMORY_TOOL_NAME};

const DEFAULT_CLI_TIMEOUT_SECS: u64 = 10;

#[derive(Clone)]
pub struct ToolSurface {
    host_tools: ToolSet,
    root_execution_guard: Option<RootExecutionGuard>,
    include_meta_tools: bool,
    include_goal_tools: bool,
    include_goal_creation: bool,
    allowed_mcp_service_ids: Vec<String>,
    remote_tools: Option<crate::document_config::RemoteTools>,
    agent_tools: AgentToolConfig,
    background_tools: BackgroundToolConfig,
    custom_tools: Vec<CustomToolFactory>,
    pub(super) enable_memory: bool,
    pub(super) enable_context_budget_tool: bool,
    pub(super) enable_session_history_tool: bool,
    pub(super) enable_schema_tool: bool,
    pub(super) enable_p2p_tool: bool,
    pub(super) enable_p2p_mutations: bool,
    pub(super) p2p_collections: EndpointScope<String, ()>,
    pub(super) enable_defra_query: bool,
    pub(super) defra_query_scope: CollectionScope,
    pub(super) application_write_collections: Vec<String>,
    pub(super) write_tools: Vec<WriteToolDecl>,
    pub(super) query_tools: Vec<QueryToolDecl>,
    pub(super) surface_of_tool: std::collections::BTreeMap<String, String>,
    pub(super) eth_queries: Vec<crate::eth::ResolvedEthQuery>,
    pub(super) eth_calls: Vec<crate::eth::ResolvedEthCall>,
    pub(super) plugin_tools: Vec<crate::document_config::PluginToolRef>,
    /// The plugin tools above resolved against the host plugin store when the
    /// surface was resolved. It feeds the runtime configuration fingerprint and
    /// slot comparison through this struct's Debug, so installing, removing or
    /// replacing a plugin re-admits an Agent whose build failed on the missing
    /// tool. `None` records a not-installed or pin-mismatched plugin and never
    /// makes an Agent unavailable: building tools stays the fail-closed gate.
    pub(super) plugin_resolutions: Vec<(
        crate::document_config::PluginToolRef,
        Option<crate::plugin::store::InstalledPlugin>,
    )>,
    pub(super) enable_skills: bool,
    pub(super) self_config: SelfConfigToolConfig,
    pub(super) lsp: Option<crate::toolset::lsp::LspToolConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SelfConfigToolConfig {
    pub enabled: bool,
    pub agent_id: String,
    pub categories: std::collections::BTreeSet<String>,
    pub no_lockout: bool,
    pub preview: bool,
    pub enable_pack_install: bool,
    pub enable_graph_tools: bool,
    pub process_ceiling: SelfConfigProcessCeiling,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct SelfConfigProcessCeiling {
    pub file_mode: FileToolMode,
    pub bash_mode: BashMode,
    pub root: Option<PathBuf>,
}

impl ToolSurface {
    pub(crate) fn root_execution_guard(&self) -> Option<&RootExecutionGuard> {
        self.root_execution_guard.as_ref()
    }

    pub(crate) fn source_fill_fields(&self) -> std::collections::BTreeSet<String> {
        let mut fields = self
            .write_tools
            .iter()
            .flat_map(|decl| decl.fields.iter())
            .chain(
                self.query_tools
                    .iter()
                    .flat_map(|decl| decl.filter_fields.iter()),
            )
            .chain(
                self.plugin_tools
                    .iter()
                    .flat_map(|plugin| plugin.input_fields.iter()),
            )
            .filter_map(|field| match &field.fill {
                Some(crate::document_config::WriteToolFieldFill::SourceField(source)) => {
                    Some(source.clone())
                }
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        if self
            .write_tools
            .iter()
            .any(|decl| decl.collection == crate::mailbox::MAILBOX_COLLECTION)
        {
            // Mailbox-capable event-trigger stages require explicit human-owner
            // lineage from the source document.
            fields.insert("requester_did".to_string());
        }
        fields
    }

    pub(crate) fn output_obligations(
        &self,
    ) -> Vec<(String, crate::document_config::WriteToolOutputObligation)> {
        self.write_tools
            .iter()
            .filter_map(|decl| {
                decl.output_obligation
                    .clone()
                    .map(|obligation| (decl.tool_name.clone(), obligation))
            })
            .collect()
    }

    pub fn host_tools(&self) -> &ToolSet {
        &self.host_tools
    }

    pub fn includes_meta_tools(&self) -> bool {
        self.include_meta_tools
    }

    pub fn includes_goal_tools(&self) -> bool {
        self.include_goal_tools
    }

    pub fn includes_goal_creation(&self) -> bool {
        self.include_goal_tools && self.include_goal_creation
    }

    pub fn includes_skills(&self) -> bool {
        self.enable_skills
    }

    pub(crate) fn remote_tools(&self) -> Option<&crate::document_config::RemoteTools> {
        self.remote_tools.as_ref()
    }

    pub(crate) fn job_tools(
        &self,
    ) -> rapidhash::RapidHashMap<String, crate::plugin_job::JobBinding> {
        crate::plugin_job::JobBinding::bind_all(&self.plugin_resolutions)
    }

    pub fn allowed_mcp_service_ids(&self) -> &[String] {
        &self.allowed_mcp_service_ids
    }

    #[cfg(test)]
    pub(crate) fn plugin_resolutions(
        &self,
    ) -> &[(
        crate::document_config::PluginToolRef,
        Option<crate::plugin::store::InstalledPlugin>,
    )] {
        &self.plugin_resolutions
    }

    #[allow(dead_code)]
    pub(crate) fn agent_tools(&self) -> &AgentToolConfig {
        &self.agent_tools
    }

    pub(crate) fn agent_targets(&self) -> &[AgentTargetDocument] {
        if self.agent_tools.enabled {
            &self.agent_tools.targets
        } else {
            &[]
        }
    }

    pub(crate) fn background_tools(&self) -> &BackgroundToolConfig {
        &self.background_tools
    }

    pub(crate) fn retain_agent_targets(
        &mut self,
        own_node_did: &str,
        active_agent_ids: &RapidHashSet<String>,
    ) {
        self.agent_tools.targets.retain(|target| {
            if target.target_node_did == own_node_did {
                active_agent_ids.contains(&target.agent_id)
            } else {
                true
            }
        });
    }

    pub fn tool_names(&self) -> Vec<String> {
        let mut names = self.host_tools.tool_names();
        if self.include_meta_tools {
            names.extend(crate::meta_tools::presented_tool_names(
                self.remote_tools.as_ref().unwrap_or(&Default::default()),
                &self.allowed_mcp_service_ids,
            ));
        }
        names.extend(agent_tool_names(&self.agent_tools));
        names.extend(background_tool_names(&self.background_tools));
        names.extend(self.custom_tools.iter().map(|tool| tool.name().to_string()));
        #[cfg(feature = "agent-memory")]
        if self.enable_memory {
            names.push(MEMORY_TOOL_NAME.to_string());
        }
        if self.enable_context_budget_tool {
            names.push(CONTEXT_BUDGET_TOOL_NAME.to_string());
        }
        if self.enable_schema_tool {
            names.push(crate::schema_tool::SCHEMA_TOOL_NAME.to_string());
        }
        if self.enable_p2p_tool {
            names.push(crate::p2p_tool::P2P_TOOL_NAME.to_string());
        }
        if self.enable_session_history_tool {
            names.push(SESSION_HISTORY_TOOL_NAME.to_string());
        }
        if self.enable_defra_query {
            names.push(DEFRA_QUERY_TOOL_NAME.to_string());
        }
        if !self.application_write_collections.is_empty() {
            names.push(crate::application_write::WRITE_TOOL_NAME.into());
        }
        if self.lsp.is_some() {
            names.push(crate::toolset::lsp::LSP_TOOL_NAME.to_string());
        }
        if self.include_goal_tools {
            names.push(crate::goal::GET_GOAL_TOOL_NAME.to_string());
            names.push(crate::goal::UPDATE_GOAL_TOOL_NAME.to_string());
            if self.include_goal_creation {
                names.push(crate::goal::CREATE_GOAL_TOOL_NAME.to_string());
            }
        }
        names.extend(crate::self_config::self_config_tool_names(
            &self.self_config,
        ));
        for decl in &self.write_tools {
            if decl.is_well_formed() {
                names.push(decl.tool_name.clone());
            }
        }
        for decl in &self.query_tools {
            if decl.is_well_formed() {
                names.push(decl.tool_name.clone());
            }
        }
        names.extend(self.eth_queries.iter().map(|query| query.tool_name()));
        names.extend(self.eth_calls.iter().map(|call| call.tool_name.clone()));
        names.extend(
            self.plugin_tools
                .iter()
                .map(|plugin| plugin.tool_name().to_string()),
        );
        build::dedupe_strings(names)
    }

    #[cfg(test)]
    pub(crate) fn lsp_config(&self) -> Option<&crate::toolset::lsp::LspToolConfig> {
        self.lsp.as_ref()
    }

    pub async fn build_tools(&self, runtime: &ToolRuntimeContext) -> Result<Vec<Box<dyn ToolDyn>>> {
        if let Some(guard) = &self.root_execution_guard {
            guard.validate(&runtime.node).await?;
        }
        let writethrough = self.lsp.as_ref().map(|config| {
            crate::toolset::lsp::LspWritethrough::new(runtime.lsp_pool.clone(), config.clone())
        });
        let mut tools = self
            .host_tools
            .build_native_tools_with_writethrough(writethrough)?;
        if self.include_meta_tools {
            tools.extend(
                build_meta_tools(
                    runtime.node.clone(),
                    runtime.mcp_pool.clone(),
                    runtime.health_map.clone(),
                    runtime.local_hostname.clone(),
                    runtime.local_subnet.clone(),
                    runtime.node_did.clone(),
                    self.allowed_mcp_service_ids.clone(),
                    self.remote_tools.clone().unwrap_or_default(),
                )
                .await?,
            );
        }
        tools.extend(build_agent_tools(self.agent_tools.clone()));
        tools.extend(build_background_tools(self.background_tools.clone()));
        for tool in &self.custom_tools {
            tools.push(tool.build()?);
        }
        #[cfg(feature = "agent-memory")]
        if self.enable_memory {
            tools.push(build_memory_tool(
                runtime.node.clone(),
                runtime.node_did.clone(),
            ));
        }
        if self.enable_context_budget_tool {
            tools.push(build_context_budget_tool(
                runtime.node.clone(),
                runtime.node_did.clone(),
            ));
        }
        if self.enable_schema_tool {
            tools.push(Box::new(crate::schema_tool::SchemaTool::new(
                runtime.node.clone(),
            )));
        }
        if self.enable_p2p_tool {
            tools.push(Box::new(crate::p2p_tool::P2pTool::new(
                runtime.node.clone(),
                runtime.identity.clone(),
                self.enable_p2p_mutations,
                self.p2p_collections.clone(),
            )));
        }
        if self.enable_session_history_tool {
            tools.push(build_session_history_tool(
                runtime.node.clone(),
                runtime.node_did.clone(),
            ));
        }
        let datastore_actor = if self.enable_defra_query
            || !self.application_write_collections.is_empty()
            || !self.query_tools.is_empty()
        {
            let identity = runtime
                .identity
                .as_ref()
                .ok_or(ToolSurfaceError::DatastoreRequiresIdentity)?;
            if !(identity.did() == runtime.node_did) {
                return Err(ToolSurfaceError::DatastoreIdentityMismatch.into());
            }
            Some(identity::Did::new(identity.did().to_owned())?)
        } else {
            None
        };
        if self.enable_defra_query {
            tools.push(build_defra_query_tool(
                runtime.node.clone(),
                self.defra_query_scope.clone(),
                datastore_actor.clone().expect("query actor was validated"),
            ));
        }
        if !self.application_write_collections.is_empty() {
            tools.push(Box::new(crate::application_write::WriteTool::new(
                crate::config_client::ConfigAccess::local(runtime.node.clone()),
                self.application_write_collections.iter().cloned().collect(),
                datastore_actor.clone(),
            )));
        }
        if let Some(lsp) = &self.lsp {
            tools.push(Box::new(crate::toolset::lsp::LspTool::new(
                lsp.clone(),
                runtime.lsp_pool.clone(),
            )?));
        }
        if self.include_goal_tools {
            tools.extend(build_goal_tools(self.include_goal_creation));
        }
        tools.extend(crate::self_config::build_self_config_tools(
            runtime.node.clone(),
            runtime.node_did.clone(),
            runtime.identity.clone(),
            &self.self_config,
            runtime.plugins.clone(),
        ));
        let mut registered_names: RapidHashSet<String> =
            tools.iter().map(|tool| tool.name()).collect();
        for decl in &self.write_tools {
            crate::mailbox::validate_mailbox_write_decl(decl)?;
            if !decl.is_well_formed() || !decl.output_obligation_is_well_formed() {
                return Err(ToolSurfaceError::WriteToolInvalid {
                    tool_name: decl.tool_name.clone(),
                }
                .into());
            }
            if !registered_names.insert(decl.tool_name.clone()) {
                return Err(ToolSurfaceError::WriteToolDuplicate {
                    tool_name: decl.tool_name.clone(),
                }
                .into());
            }
            if decl.collection == crate::mailbox::MAILBOX_COLLECTION {
                tools.push(Box::new(crate::mailbox::MailboxCreateTool::new(
                    runtime.node.clone(),
                    decl.notification
                        .clone()
                        .ok_or(ToolSurfaceError::MissingNotificationPolicy)?,
                )) as Box<dyn ToolDyn>);
            } else {
                let tool = BoundedWriteTool::new(
                    crate::config_client::ConfigAccess::local(runtime.node.clone()),
                    decl.clone(),
                )
                .declared_by(self.surface_of_tool.get(&decl.tool_name).cloned());
                tool.ensure_well_formed().map_err(|error| {
                    ToolSurfaceError::WriteToolNotWellFormed {
                        tool_name: decl.tool_name.clone(),
                        collection: decl.collection.clone(),
                        error: ErrorChain(error.as_ref()).to_string(),
                        surface: self
                            .surface_of_tool
                            .get(&decl.tool_name)
                            .map(String::as_str)
                            .unwrap_or("unresolved")
                            .to_owned(),
                    }
                })?;
                tools.push(Box::new(tool) as Box<dyn ToolDyn>);
            }
        }
        for decl in &self.query_tools {
            let tool = BoundedQueryTool::new(
                crate::config_client::ConfigAccess::local(runtime.node.clone()),
                decl.clone(),
            )
            .with_actor(datastore_actor.clone().expect("query actor was validated"))
            .declared_by(self.surface_of_tool.get(&decl.tool_name).cloned());
            if !tool.is_well_formed() {
                return Err(ToolSurfaceError::QueryToolInvalid {
                    tool_name: decl.tool_name.clone(),
                }
                .into());
            }
            if !registered_names.insert(tool.name()) {
                return Err(ToolSurfaceError::QueryToolDuplicate {
                    tool_name: decl.tool_name.clone(),
                }
                .into());
            }
            tools.push(Box::new(tool) as Box<dyn ToolDyn>);
        }
        for query in &self.eth_queries {
            let tool = crate::eth::EthQueryTool::new(query.clone());
            if !registered_names.insert(tool.name()) {
                return Err(ToolSurfaceError::EthQueryToolDuplicate { name: tool.name() }.into());
            }
            tools.push(Box::new(tool) as Box<dyn ToolDyn>);
        }
        for call in &self.eth_calls {
            let tool = crate::eth::EthCallTool::new(call.clone(), runtime.node.clone());
            if !registered_names.insert(tool.name()) {
                return Err(ToolSurfaceError::EthCallToolDuplicate { name: tool.name() }.into());
            }
            tools.push(Box::new(tool) as Box<dyn ToolDyn>);
        }
        for plugin in &self.plugin_tools {
            let tool = crate::plugin::tool::PluginTool::resolve(
                runtime.plugins.clone(),
                plugin,
                self.host_tools
                    .read_root()
                    .map(std::path::Path::to_path_buf),
            )?
            .with_data_node(runtime.node.clone());
            if !registered_names.insert(tool.name()) {
                return Err(ToolSurfaceError::PluginToolDuplicate { name: tool.name() }.into());
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
            .field("include_goal_tools", &self.include_goal_tools)
            .field("include_goal_creation", &self.include_goal_creation)
            .field("allowed_mcp_service_ids", &self.allowed_mcp_service_ids)
            .field("remote_tools", &self.remote_tools)
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
            .field("enable_p2p_tool", &self.enable_p2p_tool)
            .field("enable_p2p_mutations", &self.enable_p2p_mutations)
            .field("p2p_collections", &self.p2p_collections)
            .field(
                "enable_session_history_tool",
                &self.enable_session_history_tool,
            )
            .field("enable_defra_query", &self.enable_defra_query)
            .field("defra_query_scope", &self.defra_query_scope)
            .field("write_tools", &self.write_tools)
            .field("query_tools", &self.query_tools)
            .field("eth_queries", &self.eth_queries)
            .field("plugin_tools", &self.plugin_tools)
            .field("plugin_resolutions", &self.plugin_resolutions)
            .field("enable_skills", &self.enable_skills)
            .field("self_config", &self.self_config)
            .field(
                "lsp_digest",
                &self.lsp.as_ref().map(|config| config.digest.as_str()),
            )
            .field(
                "lsp_options",
                &self.lsp.as_ref().map(|config| {
                    (
                        config.format_on_write,
                        config.diagnostics_on_write,
                        config.diagnostics_on_edit,
                        config.diagnostics_deduplicate,
                        config.idle_timeout,
                    )
                }),
            )
            .finish()
    }
}

pub(crate) fn resolve_agent_target_descriptions(
    tool_surface: &ToolSurface,
) -> Vec<(String, String)> {
    tool_surface
        .agent_targets()
        .iter()
        .map(|target| {
            (
                target.name.clone(),
                target
                    .description
                    .as_deref()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
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
        env_vars: RapidHashMap::new(),
        working_dir: None,
        timeout_secs: DEFAULT_CLI_TIMEOUT_SECS,
        max_output_chars: crate::toolset::DEFAULT_MAX_COMMAND_CHARS,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod document_explanation_tests;
