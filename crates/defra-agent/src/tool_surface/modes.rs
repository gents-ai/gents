use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::toolset::CliToolConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileToolMode {
    #[default]
    Off,
    ReadOnly,
    ReadWrite,
}

impl FileToolMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "" | "Off" => Ok(Self::Off),
            "ReadOnly" => Ok(Self::ReadOnly),
            "ReadWrite" => Ok(Self::ReadWrite),
            other => bail!("unknown file tool mode {other}"),
        }
    }

    pub(super) fn rank(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::ReadOnly => 1,
            Self::ReadWrite => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BashMode {
    #[default]
    Off,
    ReadOnly,
    Unrestricted,
}

impl BashMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "" | "Off" => Ok(Self::Off),
            "ReadOnly" => Ok(Self::ReadOnly),
            "Unrestricted" => Ok(Self::Unrestricted),
            other => bail!("unknown bash mode {other}"),
        }
    }

    pub(super) fn rank(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::ReadOnly => 1,
            Self::Unrestricted => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCeiling {
    file_tools: FileToolMode,
    bash: BashMode,
    cli_tools: Vec<CliToolConfig>,
    root: Option<PathBuf>,
}

impl ToolCeiling {
    pub fn meta_only() -> Self {
        Self {
            file_tools: FileToolMode::Off,
            bash: BashMode::Off,
            cli_tools: Vec::new(),
            root: None,
        }
    }

    pub fn readonly() -> Self {
        Self {
            file_tools: FileToolMode::ReadOnly,
            bash: BashMode::ReadOnly,
            cli_tools: Vec::new(),
            root: None,
        }
    }

    pub fn readonly_at(root: impl Into<PathBuf>) -> Self {
        Self {
            file_tools: FileToolMode::ReadOnly,
            bash: BashMode::ReadOnly,
            cli_tools: Vec::new(),
            root: Some(root.into()),
        }
    }

    pub fn readwrite(root: impl Into<PathBuf>) -> Self {
        Self {
            file_tools: FileToolMode::ReadWrite,
            bash: BashMode::Unrestricted,
            cli_tools: Vec::new(),
            root: Some(root.into()),
        }
    }

    pub fn with_cli_tool(mut self, tool: CliToolConfig) -> Self {
        self.cli_tools.push(tool);
        self
    }

    pub fn with_cli_tools<I>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = CliToolConfig>,
    {
        self.cli_tools.extend(tools);
        self
    }

    pub fn file_tools(&self) -> FileToolMode {
        self.file_tools
    }

    pub fn bash(&self) -> BashMode {
        self.bash
    }

    pub fn cli_tools(&self) -> &[CliToolConfig] {
        &self.cli_tools
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }
}

impl Default for ToolCeiling {
    fn default() -> Self {
        Self::meta_only()
    }
}
