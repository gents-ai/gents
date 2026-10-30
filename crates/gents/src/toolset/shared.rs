mod command;
mod context;
mod filesystem;

#[cfg(test)]
pub(crate) use command::apply_workspace_authority;
#[cfg(test)]
pub(super) use command::command_env::build_shell_env_from_vars;
pub(crate) use command::command_env::is_secret_env_name;
pub(crate) use command::command_host_executable::admit_host_executable;
pub(crate) use command::command_launch::prepare_declared_cli_command;
#[cfg(test)]
pub(crate) use command::command_launch::select_sandbox_for_policy;
pub use command::command_launch::workspace_write_sandbox_enforced;
pub(crate) use command::command_output::shell_join;
pub(super) use command::command_output::{captured_channel, stopped_stream, truncate_stream};
pub(super) use command::command_run::run_command;
#[cfg(test)]
pub(crate) use command::command_run::run_command as run_command_for_test;
pub(crate) use command::command_validation::validate_command_policy;
#[cfg(test)]
pub(super) use command::command_validation::validate_read_only_command;
pub(crate) use command::{
    current_managed_command_credential_snapshot, default_lsp_network_mode,
    effective_command_policy, lsp_sandbox_for_effective, normalize_workspace_lifecycle_state,
    prepare_managed_command,
};
pub use command::{
    CommandConstraints, CommandExecutionMode, CommandExecutionPolicy, CommandNetworkMode,
    WorkspaceAuthority,
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
    let background = crate::tools::call::lifecycle::runtime::current_tool_runtime_context()
        .is_some_and(|context| context.background);
    resolve_command_timeout(
        requested_secs,
        foreground_default,
        foreground_max,
        background,
    )
}
