use serde::Deserialize;

use super::shared::{
    default_command_timeout_secs, default_max_file_chars, default_max_list_entries,
    default_max_matches,
};

#[derive(Debug, Deserialize)]
pub(super) struct ListFilesArgs {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default = "default_max_list_entries")]
    pub max_entries: usize,
    #[serde(default)]
    pub raw_json: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct ReadFileArgs {
    pub path: String,
    #[serde(default)]
    pub start_line: Option<usize>,
    #[serde(default)]
    pub end_line: Option<usize>,
    #[serde(default = "default_max_file_chars")]
    pub max_chars: usize,
    #[serde(default)]
    pub raw_json: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct GlobArgs {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default = "default_max_matches")]
    pub max_matches: usize,
    #[serde(default)]
    pub raw_json: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct GrepArgs {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default = "default_max_matches")]
    pub max_matches: usize,
    #[serde(default)]
    pub raw_json: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct WriteFileArgs {
    pub path: String,
    pub content: String,
    #[serde(default)]
    pub raw_json: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct EditFileArgs {
    pub path: String,
    pub old_text: String,
    pub new_text: String,
    #[serde(default)]
    pub replace_all: bool,
    #[serde(default)]
    pub raw_json: bool,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub match_mode: Option<String>,
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub expected_content_hash: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct BashArgs {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default = "default_command_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub raw_json: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct CliToolArgs {
    #[serde(default)]
    pub argv: Vec<String>,
}
