mod command;
mod context;
mod filesystem;

pub(super) use command::{run_command, validate_read_only_command};
pub(super) use context::{ToolContext, ToolError};
pub(super) use filesystem::{
    collect_entries, collect_glob_matches, collect_grep_matches, render_file_contents,
    truncate_text,
};

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
