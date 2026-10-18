#![allow(dead_code)]

use std::path::{Path, PathBuf};

use gents_fs_runner::execute_request_with_base;
use gents_fs_runner::protocol::{GlobArgs, GrepArgs, NativeFsRunnerRequest};
use serde_json::Value;

pub fn unique_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "gents-fs-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

pub fn run_json(root: &Path, request: NativeFsRunnerRequest) -> Value {
    let output = execute_request_with_base(root.to_path_buf(), None, request).unwrap();
    serde_json::from_str(&output).unwrap()
}

pub fn glob_request(pattern: &str) -> NativeFsRunnerRequest {
    NativeFsRunnerRequest::Glob(GlobArgs {
        pattern: pattern.to_string(),
        path: None,
        max_matches: 100,
        raw_json: true,
        max_entries_visited: None,
        max_wall_ms: None,
    })
}

pub fn grep_request(pattern: &str, case_sensitive: bool) -> NativeFsRunnerRequest {
    NativeFsRunnerRequest::Grep(GrepArgs {
        pattern: pattern.to_string(),
        path: None,
        case_sensitive,
        max_matches: 100,
        raw_json: true,
        max_entries_visited: None,
        max_bytes_read: None,
        max_wall_ms: None,
    })
}

pub fn run_glob(root: &Path, pattern: &str) -> Value {
    run_json(root, glob_request(pattern))
}

pub fn run_grep(root: &Path, pattern: &str, case_sensitive: bool) -> Value {
    run_json(root, grep_request(pattern, case_sensitive))
}
