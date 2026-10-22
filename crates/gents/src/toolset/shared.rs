mod command;
mod context;
mod filesystem;

#[cfg(test)]
pub(crate) use command::apply_workspace_authority;
pub(crate) use command::is_secret_env_name;
pub(super) use command::run_command;
#[cfg(test)]
pub(crate) use command::run_command as run_command_for_test;
#[cfg(test)]
pub(crate) use command::select_sandbox_for_policy;
pub(crate) use command::shell_join;
pub(crate) use command::validate_command_policy;
pub(crate) use command::{
    admit_host_executable, default_lsp_network_mode, effective_command_policy,
    lsp_sandbox_for_effective, normalize_workspace_lifecycle_state, prepare_declared_cli_command,
    prepare_managed_command,
};
#[cfg(test)]
pub(super) use command::{build_shell_env_from_vars, validate_read_only_command};
pub(super) use command::{captured_channel, stopped_stream, truncate_stream};
pub use command::{
    workspace_write_sandbox_enforced, CommandConstraints, CommandExecutionMode,
    CommandExecutionPolicy, CommandNetworkMode, WorkspaceAuthority,
};
pub(crate) use context::ToolContext;
pub(super) use context::ToolError;
pub(super) use filesystem::{cap_output, render_file_contents};

pub(super) fn resolve_command_timeout(
    requested_secs: Option<u64>,
    foreground_default: std::time::Duration,
    foreground_max: std::time::Duration,
    background: bool,
) -> std::time::Duration {
    if background {
        return background_command_timeout(requested_secs);
    }
    let ceiling = foreground_max.max(foreground_default);
    match requested_secs {
        Some(secs) => std::time::Duration::from_secs(secs.max(1)).min(ceiling),
        None => foreground_default,
    }
}

pub(crate) fn background_command_timeout(requested_secs: Option<u64>) -> std::time::Duration {
    let budget = std::time::Duration::from_secs(super::BACKGROUND_COMMAND_TIMEOUT_SECS);
    match requested_secs {
        Some(secs) => std::time::Duration::from_secs(secs.max(1)).min(budget),
        None => budget,
    }
}

pub(super) fn resolve_command_timeout_in_scope(
    requested_secs: Option<u64>,
    foreground_default: std::time::Duration,
    foreground_max: std::time::Duration,
) -> std::time::Duration {
    let background = crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
        .is_some_and(|context| context.background);
    resolve_command_timeout(
        requested_secs,
        foreground_default,
        foreground_max,
        background,
    )
}
