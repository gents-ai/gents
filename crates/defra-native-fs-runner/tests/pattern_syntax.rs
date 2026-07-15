//! Fences for #732 stage 1: glob understands brace alternation (globset) and
//! grep understands real regex (Rust regex syntax, linear-time) with a
//! literal fallback — both were silent zero-match traps for agents.

use defra_native_fs_runner::execute_request_with_base;
use defra_native_fs_runner::protocol::{GlobArgs, GrepArgs, NativeFsRunnerRequest};
use serde_json::Value;

fn unique_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "defra-fs-pattern-syntax-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn run_glob(root: &std::path::Path, pattern: &str) -> Value {
    let output = execute_request_with_base(
        root.to_path_buf(),
        None,
        NativeFsRunnerRequest::Glob(GlobArgs {
            pattern: pattern.to_string(),
            path: None,
            max_matches: 100,
            raw_json: true,
            max_entries_visited: None,
            max_wall_ms: None,
        }),
    )
    .unwrap();
    serde_json::from_str(&output).unwrap()
}

fn run_grep(root: &std::path::Path, pattern: &str, case_sensitive: bool) -> Value {
    let output = execute_request_with_base(
        root.to_path_buf(),
        None,
        NativeFsRunnerRequest::Grep(GrepArgs {
            pattern: pattern.to_string(),
            path: None,
            case_sensitive,
            max_matches: 100,
            raw_json: true,
            max_entries_visited: None,
            max_bytes_read: None,
            max_wall_ms: None,
        }),
    )
    .unwrap();
    serde_json::from_str(&output).unwrap()
}

#[test]
fn glob_brace_alternation_matches() {
    let root = unique_root("braces");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "x").unwrap();
    std::fs::write(root.join("src/Cargo.toml"), "x").unwrap();
    std::fs::write(root.join("src/notes.md"), "x").unwrap();

    let value = run_glob(&root, "src/*.{rs,toml}");

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 2, "{value}");
}

#[test]
fn grep_regex_pattern_matches_and_reports_syntax() {
    let root = unique_root("regex");
    std::fs::write(
        root.join("notes.txt"),
        "version 0.6.12 shipped\nversion 0x6y12 is not a version\n",
    )
    .unwrap();

    let value = run_grep(&root, r"0\.6\.12", true);

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 1, "{value}");
    assert_eq!(value["matches"][0]["line_number"], 1);
    assert_eq!(value["pattern_syntax"], "regex");
}

#[test]
fn grep_invalid_regex_falls_back_to_literal() {
    let root = unique_root("fallback");
    std::fs::write(root.join("code.rs"), "call foo(bar)\n").unwrap();

    let value = run_grep(&root, "foo(", true);

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 1, "{value}");
    assert_eq!(value["pattern_syntax"], "literal");
}

#[test]
fn grep_case_insensitive_folds_unicode_for_ascii_needles() {
    // Review finding: the ASCII-CI fast path missed lines whose match needs
    // Unicode simple case folding (Kelvin sign U+212A folds to 'k').
    let root = unique_root("kelvin");
    std::fs::write(root.join("units.txt"), "\u{212A}elvin scale\n").unwrap();

    let value = run_grep(&root, "kelvin", false);

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 1, "{value}");
}
