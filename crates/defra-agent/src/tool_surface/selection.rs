use std::sync::Arc;

use anyhow::Result;
use rig::tool::ToolDyn;

use super::modes::{BashMode, FileToolMode};

use std::path::PathBuf;

use crate::document_config::SubagentTarget;
use crate::toolset::CommandExecutionPolicy;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SubagentToolConfig {
    pub targets: Vec<SubagentTarget>,
    pub spawn_enabled: bool,
    pub steering_enabled: bool,
    pub background_enabled: bool,
    /// When false (default), cross-deployment (remote-DID) subagent delegation is
    /// disabled: remote-DID targets are not surfaced to the model and remote spawns
    /// are rejected at runtime. Cross-deployment is deferred pending ACP; only
    /// trusted-fleet deployments should opt in.
    pub allow_cross_deployment: bool,
}

impl SubagentToolConfig {
    pub(crate) fn tools_enabled(&self) -> bool {
        self.spawn_enabled && !self.targets.is_empty()
    }

    pub(crate) fn steering_tools_enabled(&self) -> bool {
        self.tools_enabled() && self.steering_enabled
    }

    pub(crate) fn steer_subagent_enabled(&self) -> bool {
        self.steering_tools_enabled() && self.background_enabled
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
    pub allowed_mcp_service_ids: Vec<String>,
    pub backgroundable_tool_names: Vec<String>,
    /// Enable the feature-gated, per-agent persistent key-value memory tool.
    pub enable_memory: bool,
    /// Enable the narrower `sessions` convenience tool for recent session history.
    pub enable_session_history_tool: bool,
    /// Enable the read-only `defra_query` structured query tool.
    pub enable_defra_query: bool,
    /// Optional allowlist of collections `defra_query` may read. Empty = all.
    pub defra_query_collections: Vec<String>,
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
            allowed_mcp_service_ids: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_memory: false,
            enable_session_history_tool: false,
            enable_defra_query: true,
            defra_query_collections: Vec::new(),
        }
    }
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
