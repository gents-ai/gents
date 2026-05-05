use std::sync::Arc;

use anyhow::Result;
use rig::tool::ToolDyn;

use super::modes::{BashMode, FileToolMode};

use std::path::PathBuf;

use crate::toolset::CommandExecutionPolicy;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSelection {
    pub file_tools: FileToolMode,
    pub file_tool_root: Option<PathBuf>,
    pub bash: BashMode,
    pub command_policy: Option<CommandExecutionPolicy>,
    pub cli_tool_names: Vec<String>,
    pub enable_meta_tools: bool,
    pub delegate_to: Vec<String>,
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
            delegate_to: Vec::new(),
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
