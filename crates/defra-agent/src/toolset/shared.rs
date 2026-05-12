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
pub(super) use filesystem::{
    collect_entries, collect_glob_matches, collect_grep_matches, default_ignored_names,
    render_file_contents, truncate_inline, truncate_text, FilesystemEntry,
};
#[cfg(test)]
pub(super) use filesystem::block_next_sorted_children_for_test;

pub(super) fn default_max_list_entries() -> usize {
    super::DEFAULT_MAX_LIST_ENTRIES
}

pub(super) fn default_max_file_chars() -> usize {
    super::DEFAULT_MAX_FILE_CHARS
}

pub(super) fn default_max_matches() -> usize {
    super::DEFAULT_MAX_MATCHES
}

pub(super) fn default_command_timeout_secs() -> u64 {
    super::DEFAULT_COMMAND_TIMEOUT_SECS
}
