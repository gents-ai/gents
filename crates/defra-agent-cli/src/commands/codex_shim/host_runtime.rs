mod fuzzy;
mod git;
mod paths;

pub(super) use fuzzy::{
    fuzzy_file_search, fuzzy_file_search_session_start, fuzzy_file_search_session_stop,
    fuzzy_file_search_session_update,
};
pub(super) use git::git_diff_to_remote;

#[derive(Debug)]
pub(super) struct HostRuntimeError {
    pub(super) code: i64,
    pub(super) message: String,
}
