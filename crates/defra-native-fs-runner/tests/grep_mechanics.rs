//! Fences for #729: grep bounds per-file work — oversized files and binary
//! files are skipped (and counted), and case-insensitive matching keeps
//! working for both ASCII and non-ASCII needles.

use defra_native_fs_runner::execute_request_with_base;
use defra_native_fs_runner::protocol::{GrepArgs, NativeFsRunnerRequest};
use serde_json::Value;

mod support;
use support::{run_grep, unique_root};

#[test]
fn grep_skips_files_over_size_cap_and_counts_them() {
    let root = unique_root("size-cap");
    let mut big = "padding\n".repeat(400_000); // ~3.2 MiB, over the 2 MiB cap
    big.push_str("needle\n");
    std::fs::write(root.join("big.log"), big).unwrap();
    std::fs::write(root.join("small.txt"), "needle\n").unwrap();

    let value = run_grep(&root, "needle", true);

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 1, "{value}");
    assert_eq!(value["matches"][0]["path"], "small.txt");
    assert_eq!(value["skipped_large_files"], 1);
}

#[test]
fn grep_skips_binary_files_and_counts_them() {
    let root = unique_root("binary");
    let mut binary = vec![0u8, 159, 146, 150, 0, 1, 2];
    binary.extend_from_slice(b"needle");
    std::fs::write(root.join("blob.bin"), binary).unwrap();
    std::fs::write(root.join("text.txt"), "needle\n").unwrap();

    let value = run_grep(&root, "needle", true);

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 1, "{value}");
    assert_eq!(value["matches"][0]["path"], "text.txt");
    assert_eq!(value["skipped_binary_files"], 1);
}

#[test]
fn grep_case_insensitive_ascii_still_matches() {
    let root = unique_root("ascii-ci");
    std::fs::write(root.join("a.txt"), "The NeEdLe is here\n").unwrap();

    let value = run_grep(&root, "needle", false);

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 1, "{value}");
}

#[test]
fn grep_case_insensitive_non_ascii_needle_still_matches() {
    let root = unique_root("unicode-ci");
    std::fs::write(root.join("menu.txt"), "un café noir\n").unwrap();

    let value = run_grep(&root, "CAFÉ", false);

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 1, "{value}");
}

#[test]
fn grep_explicit_single_file_over_size_cap_is_still_searched() {
    // The 2 MiB per-file cap is a tree-walk guard. A file the caller names
    // explicitly must be searched (the byte budget still bounds it honestly),
    // not silently skipped with returned_count=0.
    let root = unique_root("explicit-large");
    let mut big = String::from("needle at the top\n");
    big.push_str(&"padding\n".repeat(400_000)); // ~3.2 MiB
    std::fs::write(root.join("big.log"), big).unwrap();

    let output = execute_request_with_base(
        root.clone(),
        None,
        NativeFsRunnerRequest::Grep(GrepArgs {
            pattern: "needle".to_string(),
            path: Some("big.log".to_string()),
            case_sensitive: true,
            max_matches: 10,
            raw_json: true,
            max_entries_visited: None,
            max_bytes_read: None,
            max_wall_ms: None,
        }),
    )
    .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 1, "{value}");
    assert_eq!(value["skipped_large_files"], 0, "{value}");
}

#[test]
fn grep_explicit_file_over_byte_budget_reports_exhaustion_not_silence() {
    let root = unique_root("explicit-budget");
    std::fs::write(root.join("big.log"), "needle\n".repeat(50)).unwrap();

    let output = execute_request_with_base(
        root.clone(),
        None,
        NativeFsRunnerRequest::Grep(GrepArgs {
            pattern: "needle".to_string(),
            path: Some("big.log".to_string()),
            case_sensitive: true,
            max_matches: 100,
            raw_json: true,
            max_entries_visited: None,
            max_bytes_read: Some(10),
            max_wall_ms: None,
        }),
    )
    .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 0, "{value}");
    assert_eq!(value["walk"]["budget_exhausted"], true, "{value}");
    assert_eq!(value["truncated"], true, "{value}");
}

#[test]
fn grep_searches_non_utf8_text_files_lossily() {
    // NUL-free non-UTF-8 text (e.g. Latin-1 logs) is deliberately decoded
    // lossily and searched rather than silently skipped.
    let root = unique_root("latin1");
    std::fs::write(root.join("legacy.log"), b"caf\xe9 needle here\n").unwrap();

    let value = run_grep(&root, "needle", true);

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 1, "{value}");
}
