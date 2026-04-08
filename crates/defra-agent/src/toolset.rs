use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use defra_node::EmbeddedNode;
use rig::tool::ToolDyn;

mod args;
mod bash_tools;
mod delegate;
mod file_tools;
mod shared;
#[cfg(test)]
mod tests;

use bash_tools::{ReadOnlyBashTool, UnrestrictedBashTool};
use delegate::DelegateToAgentTool;
use file_tools::{EditFileTool, GlobTool, GrepTool, ListFilesTool, ReadFileTool, WriteFileTool};
use shared::ToolContext;

const DEFAULT_MAX_FILE_CHARS: usize = 32_000;
const DEFAULT_MAX_COMMAND_CHARS: usize = 16_000;
const DEFAULT_MAX_LIST_ENTRIES: usize = 200;
const DEFAULT_MAX_MATCHES: usize = 200;
const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 10;
const DELEGATE_POLL_INTERVAL_MS: u64 = 200;
const DELEGATE_WAIT_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSet {
    tools: Vec<NativeTool>,
    read_root: Option<PathBuf>,
}

impl ToolSet {
    pub fn readonly() -> Self {
        Self {
            tools: vec![
                NativeTool::ListFiles {
                    max_entries: DEFAULT_MAX_LIST_ENTRIES,
                },
                NativeTool::ReadFile {
                    max_chars: DEFAULT_MAX_FILE_CHARS,
                },
                NativeTool::Glob {
                    max_matches: DEFAULT_MAX_MATCHES,
                },
                NativeTool::Grep {
                    max_matches: DEFAULT_MAX_MATCHES,
                },
                NativeTool::BashReadOnly {
                    timeout: Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS),
                    allowlist: default_read_only_commands(),
                },
            ],
            read_root: None,
        }
    }

    pub fn readwrite(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            tools: vec![
                NativeTool::ListFiles {
                    max_entries: DEFAULT_MAX_LIST_ENTRIES,
                },
                NativeTool::ReadFile {
                    max_chars: DEFAULT_MAX_FILE_CHARS,
                },
                NativeTool::Glob {
                    max_matches: DEFAULT_MAX_MATCHES,
                },
                NativeTool::Grep {
                    max_matches: DEFAULT_MAX_MATCHES,
                },
                NativeTool::BashReadOnly {
                    timeout: Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS),
                    allowlist: default_read_only_commands(),
                },
                NativeTool::WriteFile { root: root.clone() },
                NativeTool::EditFile { root: root.clone() },
                NativeTool::BashUnrestricted {
                    timeout: Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS),
                    root: root.clone(),
                },
            ],
            read_root: Some(root),
        }
    }

    pub fn meta_only() -> Self {
        Self {
            tools: Vec::new(),
            read_root: None,
        }
    }

    pub fn builder() -> ToolSetBuilder {
        ToolSetBuilder::default()
    }

    pub fn native_tools(&self) -> &[NativeTool] {
        &self.tools
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.tools.iter().map(NativeTool::tool_name).collect()
    }

    pub fn build_native_tools(&self) -> Result<Vec<Box<dyn ToolDyn>>> {
        let read_context = match &self.read_root {
            Some(root) => ToolContext::new(root.clone(), false)?,
            None => ToolContext::from_home()?,
        };

        let mut built: Vec<Box<dyn ToolDyn>> = Vec::new();
        for tool in &self.tools {
            match tool {
                NativeTool::ListFiles { max_entries } => built.push(Box::new(ListFilesTool::new(
                    read_context.clone(),
                    *max_entries,
                ))),
                NativeTool::ReadFile { max_chars } => built.push(Box::new(ReadFileTool::new(
                    read_context.clone(),
                    *max_chars,
                ))),
                NativeTool::Glob { max_matches } => {
                    built.push(Box::new(GlobTool::new(read_context.clone(), *max_matches)))
                }
                NativeTool::Grep { max_matches } => {
                    built.push(Box::new(GrepTool::new(read_context.clone(), *max_matches)))
                }
                NativeTool::WriteFile { root } => built.push(Box::new(WriteFileTool::new(
                    ToolContext::new(root.clone(), true)?,
                ))),
                NativeTool::EditFile { root } => built.push(Box::new(EditFileTool::new(
                    ToolContext::new(root.clone(), true)?,
                ))),
                NativeTool::BashReadOnly { timeout, allowlist } => built.push(Box::new(
                    ReadOnlyBashTool::new(read_context.clone(), *timeout, allowlist.clone()),
                )),
                NativeTool::BashUnrestricted { timeout, root } => built.push(Box::new(
                    UnrestrictedBashTool::new(ToolContext::new(root.clone(), true)?, *timeout),
                )),
            }
        }
        Ok(built)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeTool {
    ListFiles { max_entries: usize },
    ReadFile { max_chars: usize },
    Glob { max_matches: usize },
    Grep { max_matches: usize },
    WriteFile { root: PathBuf },
    EditFile { root: PathBuf },
    BashReadOnly {
        timeout: Duration,
        allowlist: Vec<String>,
    },
    BashUnrestricted { timeout: Duration, root: PathBuf },
}

impl NativeTool {
    pub fn tool_name(&self) -> String {
        match self {
            Self::ListFiles { .. } => "list_files".to_string(),
            Self::ReadFile { .. } => "read_file".to_string(),
            Self::Glob { .. } => "glob".to_string(),
            Self::Grep { .. } => "grep".to_string(),
            Self::WriteFile { .. } => "write_file".to_string(),
            Self::EditFile { .. } => "edit_file".to_string(),
            Self::BashReadOnly { .. } => "bash".to_string(),
            Self::BashUnrestricted { .. } => "bash_unrestricted".to_string(),
        }
    }
}

#[derive(Debug, Default)]
pub struct ToolSetBuilder {
    tools: Vec<NativeTool>,
    read_root: Option<PathBuf>,
}

impl ToolSetBuilder {
    pub fn read_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.read_root = Some(root.into());
        self
    }

    pub fn list_files(mut self) -> Self {
        self.tools.push(NativeTool::ListFiles {
            max_entries: DEFAULT_MAX_LIST_ENTRIES,
        });
        self
    }

    pub fn read_file(mut self) -> Self {
        self.tools.push(NativeTool::ReadFile {
            max_chars: DEFAULT_MAX_FILE_CHARS,
        });
        self
    }

    pub fn glob(mut self) -> Self {
        self.tools.push(NativeTool::Glob {
            max_matches: DEFAULT_MAX_MATCHES,
        });
        self
    }

    pub fn grep(mut self) -> Self {
        self.tools.push(NativeTool::Grep {
            max_matches: DEFAULT_MAX_MATCHES,
        });
        self
    }

    pub fn write_file(mut self, root: impl Into<PathBuf>) -> Self {
        self.tools.push(NativeTool::WriteFile { root: root.into() });
        self
    }

    pub fn edit_file(mut self, root: impl Into<PathBuf>) -> Self {
        self.tools.push(NativeTool::EditFile { root: root.into() });
        self
    }

    pub fn bash_read_only(mut self) -> Self {
        self.tools.push(NativeTool::BashReadOnly {
            timeout: Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS),
            allowlist: default_read_only_commands(),
        });
        self
    }

    pub fn bash_unrestricted(mut self, root: impl Into<PathBuf>) -> Self {
        self.tools.push(NativeTool::BashUnrestricted {
            timeout: Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS),
            root: root.into(),
        });
        self
    }

    pub fn build(self) -> ToolSet {
        ToolSet {
            tools: self.tools,
            read_root: self.read_root,
        }
    }
}

pub fn build_native_tools() -> Result<Vec<Box<dyn ToolDyn>>> {
    ToolSet::builder()
        .list_files()
        .read_file()
        .bash_read_only()
        .build()
        .build_native_tools()
}

pub fn build_delegate_tool(node: Arc<EmbeddedNode>) -> Box<dyn ToolDyn> {
    Box::new(DelegateToAgentTool::new(node))
}

fn default_read_only_commands() -> Vec<String> {
    [
        "pwd", "ls", "find", "cat", "head", "tail", "sed", "grep", "rg", "wc", "stat", "file",
        "git",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}
