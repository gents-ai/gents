mod command;
mod context;
mod filesystem;

pub(crate) use command::parse_argv_prefixes;
#[cfg(test)]
pub(super) use command::{
    build_shell_env_from_vars, select_sandbox_for_policy, validate_read_only_command,
};
pub(super) use command::{run_command, validate_command_policy};
pub use command::{CommandExecutionMode, CommandExecutionPolicy, CommandNetworkMode};
pub(super) use context::{ToolContext, ToolError};
pub(super) use filesystem::{cap_output, render_file_contents};

pub(super) fn default_max_list_entries() -> usize {
    super::DEFAULT_MAX_LIST_ENTRIES
}

pub(super) fn default_max_file_chars() -> usize {
    super::DEFAULT_MAX_FILE_CHARS
}

pub(super) fn default_max_matches() -> usize {
    super::DEFAULT_MAX_MATCHES
}

/// Resolves the effective command timeout for a bash tool call (#985).
///
/// Foreground: an omitted `timeout_secs` applies the tool's configured
/// default — the same value the schema advertises — and explicit requests are
/// capped at that value (it doubles as the operator's foreground ceiling).
/// Background (spawn_process): the foreground ceiling does not apply; the
/// run gets the `BACKGROUND_COMMAND_TIMEOUT_SECS` lifetime budget instead.
pub(super) fn resolve_command_timeout(
    requested_secs: Option<u64>,
    foreground_default: std::time::Duration,
    background: bool,
) -> std::time::Duration {
    let ceiling = if background {
        std::time::Duration::from_secs(super::BACKGROUND_COMMAND_TIMEOUT_SECS)
    } else {
        foreground_default
    };
    match requested_secs {
        Some(secs) => std::time::Duration::from_secs(secs.max(1)).min(ceiling),
        None => ceiling,
    }
}

/// Scope-aware wrapper: reads whether the current execution was backgrounded
/// from the task-local tool runtime scope.
pub(super) fn resolve_command_timeout_in_scope(
    requested_secs: Option<u64>,
    foreground_default: std::time::Duration,
) -> std::time::Duration {
    let background = crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
        .is_some_and(|context| context.background);
    resolve_command_timeout(requested_secs, foreground_default, background)
}
