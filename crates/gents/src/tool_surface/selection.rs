use std::sync::Arc;

use crate::llm::tool::ToolDyn;
use anyhow::Result;

use super::modes::{BashMode, FileToolMode};

use std::path::PathBuf;

use crate::document_config::SubagentTargetDocument;
use crate::tool_call_lifecycle::AwaitMode;
use crate::toolset::{
    default_read_only_command_policy, CommandExecutionMode, CommandExecutionPolicy,
    CommandNetworkMode,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SubagentToolConfig {
    pub targets: Vec<SubagentTargetDocument>,
    pub spawn_enabled: bool,
    pub steering_enabled: bool,
    pub background_enabled: bool,
    pub default_await_mode: AwaitMode,
    /// When false (default), cross-deployment (remote-DID) subagent delegation is
    /// disabled: remote-DID targets are not surfaced to the model and remote spawns
    /// are rejected at runtime. Cross-deployment is deferred pending ACP; only
    /// trusted-fleet deployments should opt in.
    pub allow_cross_deployment: bool,
}

impl SubagentToolConfig {
    /// Project the canonical `Tools.subagents` group. Targets are NOT populated
    /// here: `SubagentTools.target_ids` are references to SubagentTarget
    /// documents, resolved and pushed by the runtime snapshot owner. Every
    /// control defaults to disabled when the group or flag is absent; selecting
    /// targets never implicitly enables spawn.
    pub(crate) fn from_document(tools: &crate::document_config::Tools) -> Result<Self> {
        tools.validate()?;
        let group = tools.subagents.as_ref();
        let background_enabled = group
            .and_then(|group| group.background_enabled)
            .unwrap_or(false);
        let default_await_mode = match group.and_then(|group| group.default_await_mode.as_deref()) {
            None => AwaitMode::default(),
            Some(value) => AwaitMode::from_persisted(value)
                .ok_or_else(|| anyhow::anyhow!("invalid subagent default await mode {value:?}"))?,
        };
        Ok(Self {
            targets: Vec::new(),
            spawn_enabled: group.and_then(|group| group.spawn_enabled).unwrap_or(false),
            steering_enabled: group
                .and_then(|group| group.steering_enabled)
                .unwrap_or(false),
            background_enabled,
            default_await_mode,
            allow_cross_deployment: group
                .and_then(|group| group.allow_cross_principal)
                .unwrap_or(false),
        })
    }

    pub(crate) fn from_document_with_targets<'a>(
        tools: &crate::document_config::Tools,
        targets: impl IntoIterator<Item = &'a crate::document_config::SubagentTargetDocument>,
    ) -> Result<Self> {
        let mut resolved = Self::from_document(tools)?;
        let mut by_id = std::collections::HashMap::new();
        for target in targets {
            anyhow::ensure!(
                by_id
                    .insert(
                        (target.agent_did.as_str(), target.target_id.as_str()),
                        target
                    )
                    .is_none(),
                "duplicate scoped SubagentTarget {}",
                target.target_id
            );
        }
        let mut names = std::collections::HashSet::new();
        for id in tools
            .subagents
            .as_ref()
            .into_iter()
            .flat_map(|group| &group.target_ids)
        {
            let target = by_id
                .get(&(tools.agent_did.as_str(), id.as_str()))
                .ok_or_else(|| anyhow::anyhow!("missing same-owner SubagentTarget {id}"))?;
            anyhow::ensure!(
                !target.name.trim().is_empty()
                    && !target.target_agent_did.trim().is_empty()
                    && !target.behavior_id.trim().is_empty(),
                "invalid SubagentTarget {id}"
            );
            anyhow::ensure!(
                names.insert(target.name.as_str()),
                "duplicate subagent target name {}",
                target.name
            );
            resolved.targets.push((*target).clone());
        }
        Ok(resolved)
    }

    pub(crate) fn tools_enabled(&self) -> bool {
        self.spawn_enabled && !self.targets.is_empty()
    }

    /// Inspection is part of the background-subagent capability. A behavior
    /// that can launch a child asynchronously must also be able to read that
    /// child's transcript without requiring the stronger steering permission.
    pub(crate) fn background_inspection_tools_enabled(&self) -> bool {
        self.tools_enabled() && self.background_enabled
    }

    pub(crate) fn steer_subagent_enabled(&self) -> bool {
        self.background_inspection_tools_enabled() && self.steering_enabled
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BackgroundToolConfig {
    pub allowlist: Vec<String>,
}

impl BackgroundToolConfig {
    pub(crate) fn tools_enabled(&self) -> bool {
        !self.allowlist.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSelection {
    pub file_tools: FileToolMode,
    pub file_tool_root: Option<PathBuf>,
    pub bash: BashMode,
    pub command_policy: Option<CommandExecutionPolicy>,
    pub cli_tool_names: Vec<String>,
    pub enable_meta_tools: bool,
    /// Enables the session-owned durable goal read/update tools independently
    /// of generic MCP discovery and dispatch.
    pub enable_goal_tools: bool,
    /// Enables model-originated durable goal creation. Creation is an
    /// additional capability: it is effective only when `enable_goal_tools`
    /// is also enabled.
    pub enable_goal_creation: bool,
    pub allowed_mcp_service_ids: Vec<String>,
    pub remote_tools: Option<crate::document_config::RemoteTools>,
    pub required_mcp_service_ids: Vec<String>,
    pub backgroundable_tool_names: Vec<String>,
    pub enable_memory: bool,
    pub enable_session_history_tool: bool,
    pub enable_context_budget: bool,
    pub enable_defra_query: bool,
    pub defra_query_collections: Vec<String>,
    pub write_tools: Vec<crate::document_config::WriteToolDecl>,
    pub query_tools: Vec<crate::document_config::QueryToolDecl>,
    pub enable_self_config: bool,
    pub self_config_categories: Option<Vec<String>>,
    pub self_config_no_lockout: bool,
    pub self_config_dry_run: bool,
    pub enable_lsp: bool,
    pub lsp_config: Option<String>,
    pub eth_queries: Vec<crate::eth::ResolvedEthQuery>,
    pub eth_calls: Vec<crate::eth::ResolvedEthCall>,
    /// Derived from canonical `Tools.remote.services[].background_tool_names`:
    /// the generic MCP dispatch wrapper is backgroundable when any selected
    /// service permits background execution. Not document-writable input.
    pub remote_background_names: Vec<String>,
}

impl Default for ToolSelection {
    fn default() -> Self {
        Self {
            file_tools: FileToolMode::Off,
            file_tool_root: None,
            bash: BashMode::Off,
            command_policy: None,
            cli_tool_names: Vec::new(),
            enable_meta_tools: true,
            enable_goal_tools: true,
            enable_goal_creation: false,
            allowed_mcp_service_ids: Vec::new(),
            remote_tools: None,
            required_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_memory: false,
            enable_session_history_tool: false,
            enable_context_budget: true,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
            write_tools: Vec::new(),
            query_tools: Vec::new(),
            enable_self_config: false,
            self_config_categories: None,
            self_config_no_lockout: false,
            self_config_dry_run: false,
            enable_lsp: false,
            lsp_config: None,
            eth_queries: Vec::new(),
            eth_calls: Vec::new(),
            remote_background_names: Vec::new(),
        }
    }
}

impl ToolSelection {
    /// Project the canonical `document_config::Tools` nested groups.
    ///
    /// Disabled-by-absence: an omitted group or unset flag grants nothing. There
    /// is no policy version: the canonical document has no historical
    /// re-interpretation contract. Timeout
    /// and background defaults belong to the individual capability owners, not
    /// this projection. Datastore write/query declarations and eth tools are
    /// expanded from their referenced documents by the caller.
    pub(crate) fn from_document(tools: &crate::document_config::Tools) -> anyhow::Result<Self> {
        tools.validate()?;
        let host = tools.host.as_ref();
        let files = host.and_then(|host| host.files.as_ref());
        let bash_group = host.and_then(|host| host.bash.as_ref());
        let built_ins = tools.built_ins.as_ref();
        let datastore = tools.datastore.as_ref();
        let integrations = tools.integrations.as_ref();
        let self_config_group = tools.self_config.as_ref();

        let file_tools = files.map(|files| files.mode).unwrap_or_default();
        let bash = bash_group.map(|bash| bash.mode).unwrap_or_default();
        let command_policy = command_policy_from_document(bash_group, bash)?;
        let cli_tool_names = host
            .map(|host| host.cli.iter().map(|tool| tool.name.clone()).collect())
            .unwrap_or_default();

        // Remote selections use explicit service ids. Any selected service needs
        // the meta dispatch wrapper to be callable, so selecting services enables
        // meta tools; empty selections grant none.
        let remote_services = tools
            .remote
            .as_ref()
            .map(|remote| remote.services.as_slice())
            .unwrap_or(&[]);
        let enable_meta_tools = !remote_services.is_empty();
        let mut allowed_mcp_service_ids = Vec::with_capacity(remote_services.len());
        let mut required_mcp_service_ids = Vec::new();
        let mut remote_background_names = Vec::new();
        for service in remote_services {
            let service_id = &service.mcp_service_id;
            if !allowed_mcp_service_ids.contains(&service_id.to_string()) {
                allowed_mcp_service_ids.push(service_id.to_string());
            }
            if service.required {
                required_mcp_service_ids.push(service_id.to_string());
            }
            // Background capability is per-service; the generic dispatch wrapper
            // is the surface entry for background MCP calls.
            if !service.background_tool_names.is_empty() {
                match service.style {
                    crate::document_config::RemoteToolStyle::Discovery => {
                        remote_background_names.push("call_tool".to_string())
                    }
                    crate::document_config::RemoteToolStyle::Flat => remote_background_names
                        .extend(
                            service
                                .background_tool_names
                                .iter()
                                .map(|name| crate::meta_tools::flat_tool_name(service_id, name)),
                        ),
                }
            }
        }
        allowed_mcp_service_ids.sort();
        allowed_mcp_service_ids.dedup();
        required_mcp_service_ids.sort();
        required_mcp_service_ids.dedup();

        let (enable_goal_tools, enable_goal_creation) = resolve_goal_capabilities(
            built_ins.and_then(|built| built.enable_goal_tools),
            built_ins.and_then(|built| built.enable_goal_creation),
        );

        Ok(Self {
            file_tools,
            // Tools.host.root is the shared default cwd for files, bash, CLI, LSP,
            // and task commands. Relative paths resolve against runtime cwd in the
            // existing root owner (tool_surface/build.rs); absent uses runtime cwd.
            file_tool_root: host
                .and_then(|host| host.root.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            bash,
            command_policy,
            cli_tool_names,
            enable_meta_tools,
            enable_goal_tools,
            enable_goal_creation,
            allowed_mcp_service_ids,
            remote_tools: tools.remote.clone(),
            required_mcp_service_ids,
            // Bash backgrounding stays the only native host background capability;
            // the derived per-mode allowlist is materialized by the adapter.
            backgroundable_tool_names: Vec::new(),
            enable_memory: built_ins
                .and_then(|built| built.enable_memory)
                .unwrap_or(false),
            enable_session_history_tool: built_ins
                .and_then(|built| built.enable_session_history_tool)
                .unwrap_or(false),
            // Canonical default is disabled-by-absence, matching
            // ToolPolicy.resolveGoalTools-style fail-closed decoding.
            enable_context_budget: built_ins
                .and_then(|built| built.enable_context_budget)
                .unwrap_or(false),
            enable_defra_query: datastore
                .and_then(|datastore| datastore.enable_defra_query)
                .unwrap_or(false),
            defra_query_collections: datastore
                .and_then(|datastore| datastore.defra_query_collections.as_deref())
                .unwrap_or(&[])
                .iter()
                .cloned()
                .collect(),
            // Canonical surfaces own datastore write declarations; canonical
            // DatastoreToolSurface/EthTool documents are expanded by the caller.
            write_tools: Vec::new(),
            query_tools: Vec::new(),
            enable_self_config: self_config_group
                .and_then(|group| group.enable_self_config)
                .unwrap_or(false),
            self_config_categories: self_config_group.and_then(|group| {
                group
                    .self_config_categories
                    .as_ref()
                    .map(|categories| categories.clone())
            }),
            self_config_no_lockout: self_config_group
                .and_then(|group| group.self_config_no_lockout)
                .unwrap_or(false),
            self_config_dry_run: self_config_group
                .and_then(|group| group.self_config_dry_run)
                .unwrap_or(false),
            enable_lsp: integrations
                .map(|group| group.lsp.is_some())
                .unwrap_or(false),
            lsp_config: integrations
                .and_then(|group| group.lsp.as_ref())
                .and_then(|lsp| lsp.config.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            eth_queries: Vec::new(),
            eth_calls: Vec::new(),
            remote_background_names,
        })
    }
}

pub fn resolve_goal_capabilities(
    explicit_goal_tools: Option<bool>,
    explicit_goal_creation: Option<bool>,
) -> (bool, bool) {
    (
        explicit_goal_tools.unwrap_or(false),
        explicit_goal_creation.unwrap_or(false),
    )
}

fn command_policy_from_document(
    bash_group: Option<&crate::document_config::BashTools>,
    bash: BashMode,
) -> anyhow::Result<Option<CommandExecutionPolicy>> {
    let execution_mode = bash_group.and_then(|bash| bash.execution_mode);
    let network_mode = bash_group.and_then(|bash| bash.network_mode);
    let allowed = bash_group
        .and_then(|bash| bash.allowed_argv_prefixes.as_ref())
        .filter(|prefixes| !prefixes.is_empty())
        .map(|prefixes| prefixes.as_slice());
    let forbidden = bash_group
        .and_then(|bash| bash.forbidden_argv_prefixes.as_ref())
        .filter(|prefixes| !prefixes.is_empty())
        .map(|prefixes| prefixes.as_slice());
    let read_only_allowlist = bash_group
        .and_then(|bash| bash.read_only_commands.as_ref())
        .filter(|list| !list.is_empty())
        .map(|list| list.as_slice());
    let has_policy = execution_mode.is_some()
        || network_mode.is_some()
        || allowed.is_some()
        || forbidden.is_some()
        || read_only_allowlist.is_some();

    if !has_policy {
        return if matches!(bash, BashMode::Unrestricted) {
            Ok(Some(
                CommandExecutionPolicy::write_capable()
                    .with_mode(CommandExecutionMode::Unrestricted),
            ))
        } else {
            Ok(None)
        };
    }

    // Canonical bash.mode owns the exposed bash tool; execution_mode can only
    // narrow within it. Off/ReadOnly force read-only execution.
    let mode = match bash {
        BashMode::Off | BashMode::ReadOnly => CommandExecutionMode::ReadOnly,
        BashMode::Unrestricted => execution_mode.unwrap_or(CommandExecutionMode::Unrestricted),
    };

    let base = if matches!(mode, CommandExecutionMode::ReadOnly) {
        default_read_only_command_policy()
    } else {
        CommandExecutionPolicy::write_capable()
    };
    let base = match (mode, read_only_allowlist) {
        (CommandExecutionMode::ReadOnly, Some(list)) if !list.is_empty() => {
            base.with_read_only_allowlist(list.to_vec())
        }
        _ => base,
    };
    let allowed = allowed.map(<[Vec<String>]>::to_vec).unwrap_or_default();
    let forbidden = forbidden.map(<[Vec<String>]>::to_vec).unwrap_or_default();
    Ok(Some(
        base.with_mode(mode)
            .with_allowed_argv_prefixes(allowed)
            .with_forbidden_argv_prefixes(forbidden)
            .with_network_mode(network_mode.unwrap_or(CommandNetworkMode::Inherit)),
    ))
}

type CustomToolFactoryFn = Arc<dyn Fn() -> Result<Box<dyn ToolDyn>> + Send + Sync>;

#[derive(Clone)]
pub struct CustomToolFactory {
    name: String,
    factory: CustomToolFactoryFn,
}

impl CustomToolFactory {
    pub fn new(
        name: impl Into<String>,
        factory: impl Fn() -> Result<Box<dyn ToolDyn>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            factory: Arc::new(factory),
        }
    }

    pub fn from_tool<T>(tool: T) -> Self
    where
        T: ToolDyn + Clone + Send + Sync + 'static,
    {
        let name = tool.name();
        Self::new(name, move || Ok(Box::new(tool.clone()) as Box<dyn ToolDyn>))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn build(&self) -> Result<Box<dyn ToolDyn>> {
        (self.factory)()
    }
}

impl std::fmt::Debug for CustomToolFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomToolFactory")
            .field("name", &self.name)
            .finish()
    }
}
