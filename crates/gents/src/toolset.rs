use std::path::PathBuf;
use std::time::Duration;

use crate::llm::tool::ToolDyn;
use anyhow::Result;
use std::collections::HashMap;

mod args;
mod bash_tools;
mod cli_tool;
mod context_budget;
pub mod edit_match;
mod file_tools;
mod goal;
pub(crate) mod lsp;
#[cfg(feature = "agent-memory")]
mod memory;
mod native_runner;
pub use native_runner::enable_self_runner;
pub(crate) mod session_history;
mod title_audit_usage;
pub(crate) use title_audit_usage::load_parent_inclusive_budget_usage;
pub(crate) use title_audit_usage::load_session_goal_usage;
pub use title_audit_usage::{ParentAuditUsage, ParentAuditUsageObservation};
mod shared;
pub(crate) use shared::is_secret_env_name;
mod session_message;
#[cfg(test)]
mod tests;

use bash_tools::{ReadOnlyBashTool, UnrestrictedBashTool};
use cli_tool::CliTool;
use file_tools::{EditFileTool, GlobTool, GrepTool, ListFilesTool, ReadFileTool, WriteFileTool};
use session_message::{
    AgentInterruptTool, AgentListTool, AgentMessageTool, AgentNewTool, AgentSessionListTool,
    AgentSessionReadTool, CancelProcessTool, ListProcessesTool, ReadProcessTool, SpawnProcessTool,
    WaitProcessTool,
};

use crate::tool_surface::{AgentToolConfig, BackgroundToolConfig};

#[cfg(test)]
pub(crate) fn read_only_bash_for_test(
    root: &std::path::Path,
    allowlist: Vec<String>,
) -> Box<dyn crate::llm::tool::ToolDyn> {
    Box::new(ReadOnlyBashTool::new(
        ToolContext::new(root.to_path_buf(), false).unwrap(),
        Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS),
        allowlist,
    ))
}

#[cfg(all(test, unix))]
pub(crate) fn write_capable_bash_for_test(
    root: &std::path::Path,
) -> Box<dyn crate::llm::tool::ToolDyn> {
    Box::new(UnrestrictedBashTool::new(
        ToolContext::new(root.to_path_buf(), false).unwrap(),
        Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS),
    ))
}

#[cfg(test)]
pub(crate) fn cli_tool_for_test(config: CliToolConfig) -> Box<dyn crate::llm::tool::ToolDyn> {
    Box::new(CliTool::new(config))
}

pub use context_budget::{
    build_context_budget_tool, load_context_budget_snapshot, ContextBudgetScope,
    ContextBudgetSnapshot, LastRequestContextSnapshot, CONTEXT_BUDGET_TOOL_NAME,
};
pub use gents_loop::tool_policy::CommandPolicyDenial;
pub(crate) use gents_loop::tool_policy::DenialReason;
pub(crate) mod denial {
    pub(crate) use gents_loop::tool_policy::DenialReason;
}
pub(crate) use goal::build_goal_tools;
pub(crate) use goal::{CreateGoalArgs, GetGoalArgs, UpdateGoalArgs};
pub use lsp::{
    lsp_action_authorized, lsp_advertised, lsp_apply_authorized, result_looks_failed,
    result_path_matches, LspAction, LspMutationSource,
};
#[cfg(feature = "agent-memory")]
pub use memory::{build_memory_tool, MEMORY_TOOL_NAME};
pub use session_history::{
    build_session_history_tool, load_pinned_request_context_observation,
    load_request_context_observation, load_session_context_details, load_session_history_snapshot,
    load_session_history_snapshot_with_access, load_session_inference_observation,
    load_session_investigation, load_session_request_doc_ids, RequestContextObservation,
    SessionCompactionEvent, SessionContextDetails, SessionHistoryRow, SessionHistorySnapshot,
    SessionInferenceObservation, SessionInvestigationSnapshot, SessionRequestEvent,
    SessionTokenUsage, SessionToolCallStats, SESSION_HISTORY_TOOL_NAME,
};
#[cfg(test)]
pub(crate) use shared::apply_workspace_authority;
#[cfg(test)]
pub(crate) use shared::run_command_for_test;
pub(crate) use shared::shell_join;
#[cfg(test)]
pub(crate) use shared::validate_command_policy;
pub(crate) use shared::ToolContext;
pub(crate) use shared::background_command_timeout;
pub(crate) use shared::{
    admit_host_executable, default_lsp_network_mode, effective_command_policy,
    lsp_sandbox_for_effective, normalize_workspace_lifecycle_state, prepare_managed_command,
};
pub use shared::{
    workspace_write_sandbox_enforced, CommandConstraints, CommandExecutionMode,
    CommandExecutionPolicy, CommandNetworkMode, WorkspaceAuthority,
};

pub fn default_read_only_command_policy() -> CommandExecutionPolicy {
    CommandExecutionPolicy::read_only(default_read_only_commands())
}

pub(crate) const DEFAULT_MAX_FILE_CHARS: usize = 32_000;
pub(crate) const DEFAULT_MAX_COMMAND_CHARS: usize = 16_000;
pub(crate) const MAX_CONFIGURED_COMMAND_CHARS: usize = 1_000_000;
pub(crate) const DEFAULT_MAX_LIST_ENTRIES: usize = 200;
pub(crate) const DEFAULT_MAX_MATCHES: usize = 200;
pub(crate) const MAX_CONFIGURED_FILE_CHARS: usize = 1_000_000;
pub(crate) const MAX_CONFIGURED_FILE_RESULTS: usize = 5_000;
pub(crate) const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 120;
pub(crate) const BACKGROUND_COMMAND_TIMEOUT_SECS: u64 = 36_000;
pub const AGENT_NEW_TOOL_NAME: &str = "agent_new";
pub const AGENT_MESSAGE_TOOL_NAME: &str = "agent_message";
pub const AGENT_INTERRUPT_TOOL_NAME: &str = "agent_interrupt";
pub const AGENT_LIST_TOOL_NAME: &str = "agent_list";
pub const AGENT_SESSION_LIST_TOOL_NAME: &str = "agent_session_list";
pub const AGENT_SESSION_READ_TOOL_NAME: &str = "agent_session_read";
pub const AGENT_TOOL_NAMES: [&str; 6] = [
    AGENT_NEW_TOOL_NAME,
    AGENT_MESSAGE_TOOL_NAME,
    AGENT_INTERRUPT_TOOL_NAME,
    AGENT_LIST_TOOL_NAME,
    AGENT_SESSION_LIST_TOOL_NAME,
    AGENT_SESSION_READ_TOOL_NAME,
];
pub(crate) const SPAWN_PROCESS_TOOL_NAME: &str = "spawn_process";
pub(crate) const WAIT_PROCESS_TOOL_NAME: &str = "wait_process";
pub(crate) const LIST_PROCESSES_TOOL_NAME: &str = "list_processes";
pub(crate) const READ_PROCESS_TOOL_NAME: &str = "read_process";
pub(crate) const CANCEL_PROCESS_TOOL_NAME: &str = "cancel_process";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliToolConfig {
    pub name: String,
    pub binary_path: PathBuf,
    pub description: String,
    pub allowed_argv_prefixes: Vec<Vec<String>>,
    pub env_vars: HashMap<String, String>,
    pub working_dir: Option<PathBuf>,
    pub timeout_secs: u64,
    pub max_output_chars: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSet {
    tools: Vec<NativeTool>,
    read_root: Option<PathBuf>,
    background_lifetime: Duration,
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
                    timeout_max: Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS),
                    max_output_chars: DEFAULT_MAX_COMMAND_CHARS,
                    allowlist: default_read_only_commands(),
                    policy: CommandExecutionPolicy::read_only(default_read_only_commands()),
                },
            ],
            read_root: None,
            background_lifetime: Duration::from_secs(BACKGROUND_COMMAND_TIMEOUT_SECS),
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
                    timeout_max: Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS),
                    max_output_chars: DEFAULT_MAX_COMMAND_CHARS,
                    allowlist: default_read_only_commands(),
                    policy: CommandExecutionPolicy::read_only(default_read_only_commands()),
                },
                NativeTool::WriteFile { root: root.clone() },
                NativeTool::EditFile { root: root.clone() },
                NativeTool::BashUnrestricted {
                    timeout: Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS),
                    timeout_max: Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS),
                    max_output_chars: DEFAULT_MAX_COMMAND_CHARS,
                    root: root.clone(),
                    policy: CommandExecutionPolicy::write_capable(),
                },
            ],
            read_root: Some(root),
            background_lifetime: Duration::from_secs(BACKGROUND_COMMAND_TIMEOUT_SECS),
        }
    }

    pub fn meta_only() -> Self {
        Self {
            tools: Vec::new(),
            read_root: None,
            background_lifetime: Duration::from_secs(BACKGROUND_COMMAND_TIMEOUT_SECS),
        }
    }

    pub fn builder() -> ToolSetBuilder {
        ToolSetBuilder::default()
    }

    pub fn native_tools(&self) -> &[NativeTool] {
        &self.tools
    }

    pub fn read_root(&self) -> Option<&std::path::Path> {
        self.read_root.as_deref()
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.tools.iter().map(NativeTool::tool_name).collect()
    }

    pub fn backgroundable_tool_names(&self) -> Vec<String> {
        self.tools
            .iter()
            .filter(|tool| tool.backgroundable())
            .map(NativeTool::tool_name)
            .collect()
    }

    pub fn is_backgroundable_tool_name(&self, name: &str) -> bool {
        self.tools
            .iter()
            .any(|tool| tool.tool_name() == name && tool.backgroundable())
    }

    pub fn build_native_tools(&self) -> Result<Vec<Box<dyn ToolDyn>>> {
        self.build_native_tools_with_writethrough(None)
    }

    pub fn build_native_tools_with_writethrough(
        &self,
        writethrough: Option<lsp::LspWritethrough>,
    ) -> Result<Vec<Box<dyn ToolDyn>>> {
        let read_context = match &self.read_root {
            Some(root) => ToolContext::new(root.clone(), read_root_requires_create(&self.tools))?,
            None => ToolContext::from_default_read_root()?,
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
                NativeTool::WriteFile { root } => {
                    let mut writer = WriteFileTool::new(ToolContext::new(root.clone(), true)?);
                    if let Some(writethrough) = writethrough.clone() {
                        writer = writer.with_writethrough(writethrough);
                    }
                    built.push(Box::new(writer));
                }
                NativeTool::EditFile { root } => {
                    let mut editor = EditFileTool::new(ToolContext::new(root.clone(), true)?);
                    if let Some(writethrough) = writethrough.clone() {
                        editor = editor.with_writethrough(writethrough);
                    }
                    built.push(Box::new(editor));
                }
                NativeTool::BashReadOnly {
                    timeout,
                    timeout_max,
                    max_output_chars,
                    policy,
                    ..
                } => built.push(Box::new(
                    ReadOnlyBashTool::with_policy(
                        read_context.clone(),
                        *timeout,
                        *timeout_max,
                        *max_output_chars,
                        policy.clone(),
                    )
                    .with_background_lifetime(self.background_lifetime),
                )),
                NativeTool::BashUnrestricted {
                    timeout,
                    timeout_max,
                    max_output_chars,
                    root,
                    policy,
                } => built.push(Box::new(
                    UnrestrictedBashTool::with_policy(
                        ToolContext::new(root.clone(), true)?,
                        *timeout,
                        *timeout_max,
                        *max_output_chars,
                        policy.clone(),
                    )
                    .with_background_lifetime(self.background_lifetime),
                )),
                NativeTool::Cli(tool) => built.push(Box::new(CliTool::new(tool.clone()))),
            }
        }
        Ok(built)
    }
}

fn read_root_requires_create(tools: &[NativeTool]) -> bool {
    tools.iter().any(|tool| {
        matches!(
            tool,
            NativeTool::WriteFile { .. }
                | NativeTool::EditFile { .. }
                | NativeTool::BashUnrestricted { .. }
        )
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeTool {
    ListFiles {
        max_entries: usize,
    },
    ReadFile {
        max_chars: usize,
    },
    Glob {
        max_matches: usize,
    },
    Grep {
        max_matches: usize,
    },
    WriteFile {
        root: PathBuf,
    },
    EditFile {
        root: PathBuf,
    },
    BashReadOnly {
        timeout: Duration,
        timeout_max: Duration,
        max_output_chars: usize,
        allowlist: Vec<String>,
        policy: CommandExecutionPolicy,
    },
    BashUnrestricted {
        timeout: Duration,
        timeout_max: Duration,
        max_output_chars: usize,
        root: PathBuf,
        policy: CommandExecutionPolicy,
    },
    Cli(CliToolConfig),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileToolLimits {
    pub max_read_chars: usize,
    pub max_list_entries: usize,
    pub max_matches: usize,
}

impl Default for FileToolLimits {
    fn default() -> Self {
        Self {
            max_read_chars: DEFAULT_MAX_FILE_CHARS,
            max_list_entries: DEFAULT_MAX_LIST_ENTRIES,
            max_matches: DEFAULT_MAX_MATCHES,
        }
    }
}

pub(crate) fn is_bash_tool_name(name: &str) -> bool {
    name == NativeTool::BASH_NAME || name == NativeTool::BASH_UNRESTRICTED_NAME
}

impl NativeTool {
    const LIST_FILES_NAME: &'static str = <ListFilesTool as crate::llm::tool::Tool>::NAME;
    const READ_FILE_NAME: &'static str = <ReadFileTool as crate::llm::tool::Tool>::NAME;
    const GLOB_NAME: &'static str = <GlobTool as crate::llm::tool::Tool>::NAME;
    const GREP_NAME: &'static str = <GrepTool as crate::llm::tool::Tool>::NAME;
    const WRITE_FILE_NAME: &'static str = <WriteFileTool as crate::llm::tool::Tool>::NAME;
    const EDIT_FILE_NAME: &'static str = <EditFileTool as crate::llm::tool::Tool>::NAME;
    const BASH_NAME: &'static str = <ReadOnlyBashTool as crate::llm::tool::Tool>::NAME;
    const BASH_UNRESTRICTED_NAME: &'static str =
        <UnrestrictedBashTool as crate::llm::tool::Tool>::NAME;

    pub(crate) const ALL_NAMES: [&'static str; 8] = [
        Self::LIST_FILES_NAME,
        Self::READ_FILE_NAME,
        Self::GLOB_NAME,
        Self::GREP_NAME,
        Self::WRITE_FILE_NAME,
        Self::EDIT_FILE_NAME,
        Self::BASH_NAME,
        Self::BASH_UNRESTRICTED_NAME,
    ];

    pub fn tool_name(&self) -> String {
        match self {
            Self::ListFiles { .. } => Self::LIST_FILES_NAME.to_string(),
            Self::ReadFile { .. } => Self::READ_FILE_NAME.to_string(),
            Self::Glob { .. } => Self::GLOB_NAME.to_string(),
            Self::Grep { .. } => Self::GREP_NAME.to_string(),
            Self::WriteFile { .. } => Self::WRITE_FILE_NAME.to_string(),
            Self::EditFile { .. } => Self::EDIT_FILE_NAME.to_string(),
            Self::BashReadOnly { .. } => Self::BASH_NAME.to_string(),
            Self::BashUnrestricted { .. } => Self::BASH_UNRESTRICTED_NAME.to_string(),
            Self::Cli(tool) => tool.name.clone(),
        }
    }

    pub fn backgroundable(&self) -> bool {
        matches!(
            self,
            Self::BashReadOnly { .. } | Self::BashUnrestricted { .. }
        )
    }
}

#[derive(Debug, Default)]
pub struct ToolSetBuilder {
    tools: Vec<NativeTool>,
    read_root: Option<PathBuf>,
    background_lifetime: Option<Duration>,
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

    pub fn read_file_tools_with_limits(mut self, limits: FileToolLimits) -> Self {
        self.tools.extend([
            NativeTool::ListFiles {
                max_entries: limits.max_list_entries,
            },
            NativeTool::ReadFile {
                max_chars: limits.max_read_chars,
            },
            NativeTool::Glob {
                max_matches: limits.max_matches,
            },
            NativeTool::Grep {
                max_matches: limits.max_matches,
            },
        ]);
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

    pub fn bash_read_only(self) -> Self {
        let timeout = Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS);
        self.bash_read_only_with_limits(timeout, timeout, DEFAULT_MAX_COMMAND_CHARS)
    }

    pub fn bash_read_only_with_limits(
        self,
        timeout: Duration,
        timeout_max: Duration,
        max_output_chars: usize,
    ) -> Self {
        self.bash_read_only_with_policy_and_limits(
            CommandExecutionPolicy::read_only(default_read_only_commands()),
            timeout,
            timeout_max,
            max_output_chars,
        )
    }

    pub fn bash_read_only_with_policy(self, policy: CommandExecutionPolicy) -> Self {
        let timeout = Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS);
        self.bash_read_only_with_policy_and_limits(
            policy,
            timeout,
            timeout,
            DEFAULT_MAX_COMMAND_CHARS,
        )
    }

    pub fn bash_read_only_with_policy_and_limits(
        mut self,
        policy: CommandExecutionPolicy,
        timeout: Duration,
        timeout_max: Duration,
        max_output_chars: usize,
    ) -> Self {
        self.tools.push(NativeTool::BashReadOnly {
            timeout,
            timeout_max: timeout_max.max(timeout),
            max_output_chars,
            allowlist: default_read_only_commands(),
            policy,
        });
        self
    }

    pub fn bash_unrestricted(self, root: impl Into<PathBuf>) -> Self {
        let timeout = Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS);
        self.bash_unrestricted_with_limits(root, timeout, timeout, DEFAULT_MAX_COMMAND_CHARS)
    }

    pub fn bash_unrestricted_with_limits(
        self,
        root: impl Into<PathBuf>,
        timeout: Duration,
        timeout_max: Duration,
        max_output_chars: usize,
    ) -> Self {
        self.bash_unrestricted_with_policy_and_limits(
            root,
            CommandExecutionPolicy::write_capable(),
            timeout,
            timeout_max,
            max_output_chars,
        )
    }

    pub fn bash_unrestricted_with_policy(
        self,
        root: impl Into<PathBuf>,
        policy: CommandExecutionPolicy,
    ) -> Self {
        let timeout = Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS);
        self.bash_unrestricted_with_policy_and_limits(
            root,
            policy,
            timeout,
            timeout,
            DEFAULT_MAX_COMMAND_CHARS,
        )
    }

    pub fn bash_unrestricted_with_policy_and_limits(
        mut self,
        root: impl Into<PathBuf>,
        policy: CommandExecutionPolicy,
        timeout: Duration,
        timeout_max: Duration,
        max_output_chars: usize,
    ) -> Self {
        self.tools.push(NativeTool::BashUnrestricted {
            timeout,
            timeout_max: timeout_max.max(timeout),
            max_output_chars,
            root: root.into(),
            policy,
        });
        self
    }

    pub fn cli_tool(mut self, tool: CliToolConfig) -> Self {
        self.tools.push(NativeTool::Cli(tool));
        self
    }

    pub fn background_lifetime(mut self, lifetime: Duration) -> Self {
        self.background_lifetime = Some(lifetime);
        self
    }

    pub fn build(self) -> ToolSet {
        ToolSet {
            tools: self.tools,
            read_root: self.read_root,
            background_lifetime: self
                .background_lifetime
                .unwrap_or(Duration::from_secs(BACKGROUND_COMMAND_TIMEOUT_SECS)),
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

pub fn is_session_message_tool(tool_name: &str) -> bool {
    tool_name == AGENT_NEW_TOOL_NAME || tool_name == AGENT_MESSAGE_TOOL_NAME
}

pub(crate) fn agent_tool_names(config: &AgentToolConfig) -> Vec<String> {
    if !config.tools_enabled() {
        return Vec::new();
    }
    AGENT_TOOL_NAMES
        .into_iter()
        .filter(|name| *name != AGENT_NEW_TOOL_NAME || config.agent_new_enabled())
        .map(str::to_string)
        .collect()
}

pub(crate) fn build_agent_tools(config: AgentToolConfig) -> Vec<Box<dyn ToolDyn>> {
    if !config.tools_enabled() {
        return Vec::new();
    }
    let mut tools: Vec<Box<dyn ToolDyn>> = Vec::with_capacity(AGENT_TOOL_NAMES.len());
    if config.agent_new_enabled() {
        tools.push(Box::new(AgentNewTool::new(config)));
    }
    tools.push(Box::new(AgentMessageTool));
    tools.push(Box::new(AgentInterruptTool));
    tools.push(Box::new(AgentListTool));
    tools.push(Box::new(AgentSessionListTool));
    tools.push(Box::new(AgentSessionReadTool));
    tools
}

pub(crate) fn background_tool_names(config: &BackgroundToolConfig) -> Vec<String> {
    if !config.tools_enabled() {
        return Vec::new();
    }

    [
        SPAWN_PROCESS_TOOL_NAME,
        WAIT_PROCESS_TOOL_NAME,
        LIST_PROCESSES_TOOL_NAME,
        READ_PROCESS_TOOL_NAME,
        CANCEL_PROCESS_TOOL_NAME,
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

pub(crate) fn build_background_tools(config: BackgroundToolConfig) -> Vec<Box<dyn ToolDyn>> {
    if !config.tools_enabled() {
        return Vec::new();
    }

    vec![
        Box::new(SpawnProcessTool::new(config.clone())),
        Box::new(WaitProcessTool),
        Box::new(ListProcessesTool),
        Box::new(ReadProcessTool),
        Box::new(CancelProcessTool),
    ]
}

fn default_read_only_commands() -> Vec<String> {
    [
        "pwd",
        "ls",
        "find",
        "cat",
        "head",
        "tail",
        "sed",
        "grep",
        "rg",
        "wc",
        "stat",
        "cmp",
        "sha256sum",
        "file",
        "git",
        "date",
        "hostname",
        "uptime",
        "df",
        "vm_stat",
        "ps",
        "lsof",
        "curl",
        "launchctl",
        "tailscale",
        "sudo",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}
