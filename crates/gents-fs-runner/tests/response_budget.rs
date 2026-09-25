//! Fences for #1764: a result whose encoded response would exceed the host's
//! read limit is cut to fit and reported truncated, never an error.

use gents_fs_runner::execute_request_with_base;
use gents_fs_runner::protocol::{
    GlobArgs, GrepArgs, ListFilesArgs, NativeFsRunnerRequest, NativeFsRunnerResponse,
};
use gents_fs_runner::MAX_RESPONSE_BYTES;
use serde_json::Value;

mod support;
use support::unique_root;

/// Long nested paths and long non-ASCII lines full of characters that JSON
/// escapes, so both encodings inflate the output.
fn noisy_tree(label: &str) -> std::path::PathBuf {
    let root = unique_root(label);
    let dir = root.join(format!(
        "{}/{}",
        "deeply_nested_directory_name_é".repeat(3),
        "second_level_directory_ü".repeat(3)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let line = format!("needle \"é\\ü\" {}\n", "\"ö\\\t".repeat(120));
    for file in 0..60 {
        std::fs::write(
            dir.join(format!("file_{file:03}_with_a_long_name_ñ.txt")),
            line.repeat(100),
        )
        .unwrap();
    }
    root
}

fn encoded_len(output: &str) -> usize {
    serde_json::to_string(&NativeFsRunnerResponse {
        ok: true,
        output: Some(output.to_owned()),
        error: None,
    })
    .unwrap()
    .len()
        + 1
}

#[test]
fn grep_past_the_response_budget_truncates_instead_of_failing() {
    let root = noisy_tree("grep-budget");
    for raw_json in [true, false] {
        let output = execute_request_with_base(
            root.clone(),
            None,
            NativeFsRunnerRequest::Grep(GrepArgs {
                pattern: "needle".to_string(),
                path: None,
                case_sensitive: true,
                max_matches: 5_000,
                raw_json,
                max_entries_visited: None,
                max_bytes_read: None,
                max_wall_ms: None,
            }),
        )
        .unwrap();
        assert!(
            encoded_len(&output) <= MAX_RESPONSE_BYTES,
            "raw_json={raw_json}: {} bytes",
            encoded_len(&output)
        );
        if raw_json {
            let value: Value = serde_json::from_str(&output).unwrap();
            assert_eq!(value["truncated"], true);
            assert_eq!(value["total_count"], Value::Null);
            let returned = value["returned_count"].as_u64().unwrap();
            assert!(returned > 0 && returned < 5_000, "{returned}");
            assert_eq!(value["matches"].as_array().unwrap().len() as u64, returned);
        } else {
            assert!(output.contains("\"truncated\":true"), "text output");
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn list_and_glob_past_the_response_budget_truncate() {
    let root = unique_root("list-budget");
    let dir = root.join("ñ".repeat(60));
    std::fs::create_dir_all(&dir).unwrap();
    for file in 0..5_000 {
        std::fs::write(
            dir.join(format!("{file:05}_{}.txt", "\"é\\".repeat(40))),
            "",
        )
        .unwrap();
    }
    let list = execute_request_with_base(
        root.clone(),
        None,
        NativeFsRunnerRequest::ListFiles(ListFilesArgs {
            path: None,
            recursive: true,
            max_entries: 5_000,
            raw_json: true,
            max_entries_visited: None,
            max_wall_ms: None,
        }),
    )
    .unwrap();
    let glob = execute_request_with_base(
        root.clone(),
        None,
        NativeFsRunnerRequest::Glob(GlobArgs {
            pattern: "**/*.txt".to_string(),
            path: None,
            max_matches: 5_000,
            raw_json: true,
            max_entries_visited: None,
            max_wall_ms: None,
        }),
    )
    .unwrap();
    for (tool, output) in [("list_files", list), ("glob", glob)] {
        assert!(encoded_len(&output) <= MAX_RESPONSE_BYTES, "{tool}");
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["truncated"], true, "{tool}");
        let returned = value["returned_count"].as_u64().unwrap();
        assert!(returned > 0 && returned < 4_000, "{tool}: {returned}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn results_within_the_budget_are_unchanged() {
    let root = unique_root("small-budget");
    std::fs::write(root.join("a.txt"), "needle\n").unwrap();
    let output = execute_request_with_base(
        root.clone(),
        None,
        NativeFsRunnerRequest::Grep(GrepArgs {
            pattern: "needle".to_string(),
            path: None,
            case_sensitive: true,
            max_matches: 5_000,
            raw_json: true,
            max_entries_visited: None,
            max_bytes_read: None,
            max_wall_ms: None,
        }),
    )
    .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["truncated"], false);
    assert_eq!(value["total_count"], 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn empty_result_with_long_escape_rich_pattern_stays_within_budget() {
    let root = unique_root("long-pattern");
    std::fs::write(root.join("a.txt"), "haystack\n").unwrap();
    // Just under the pattern limit, and escape-heavy in both JSON encodings.
    let pattern = format!("{}needle/*.txt", "\"é\\".repeat(900));
    assert!(pattern.len() <= 4096);
    for raw_json in [true, false] {
        let output = execute_request_with_base(
            root.clone(),
            None,
            NativeFsRunnerRequest::Glob(GlobArgs {
                pattern: pattern.clone(),
                path: None,
                max_matches: 5_000,
                raw_json,
                max_entries_visited: None,
                max_wall_ms: None,
            }),
        )
        .unwrap();
        assert!(
            encoded_len(&output) <= MAX_RESPONSE_BYTES,
            "raw_json={raw_json}"
        );
        if raw_json {
            let value: Value = serde_json::from_str(&output).unwrap();
            assert_eq!(value["returned_count"], 0);
            let echoed = value["pattern"].as_str().unwrap();
            assert!(echoed.ends_with("... [truncated]"), "{echoed}");
            assert!(echoed.chars().count() < 1100);
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn oversized_pattern_is_refused_with_a_bounded_error() {
    let root = unique_root("oversized-pattern");
    let pattern = format!("[{}", "\"\\\u{1}".repeat(300_000));
    for request in [
        NativeFsRunnerRequest::Glob(GlobArgs {
            pattern: pattern.clone(),
            path: None,
            max_matches: 10,
            raw_json: true,
            max_entries_visited: None,
            max_wall_ms: None,
        }),
        NativeFsRunnerRequest::Grep(GrepArgs {
            pattern: pattern.clone(),
            path: None,
            case_sensitive: true,
            max_matches: 10,
            raw_json: true,
            max_entries_visited: None,
            max_bytes_read: None,
            max_wall_ms: None,
        }),
    ] {
        let error = execute_request_with_base(root.clone(), None, request).unwrap_err();
        let line = gents_fs_runner::error_response_line(&error);
        assert!(line.len() < 4096, "{} bytes", line.len());
        let response: NativeFsRunnerResponse = serde_json::from_str(line.trim_end()).unwrap();
        assert!(!response.ok);
        assert!(response.error.unwrap().contains("at most 4096"));
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn escape_rich_invalid_pattern_error_is_bounded() {
    let root = unique_root("invalid-pattern");
    // A pattern within the size limit that fails to compile; its message
    // echoes the pattern, and the error envelope stays bounded.
    let pattern = format!("[{}", "\"\\\u{1}".repeat(1_000));
    let error = execute_request_with_base(
        root.clone(),
        None,
        NativeFsRunnerRequest::Glob(GlobArgs {
            pattern,
            path: None,
            max_matches: 10,
            raw_json: true,
            max_entries_visited: None,
            max_wall_ms: None,
        }),
    )
    .unwrap_err();
    let line = gents_fs_runner::error_response_line(&error);
    assert!(line.len() <= MAX_RESPONSE_BYTES, "{} bytes", line.len());
    let response: NativeFsRunnerResponse = serde_json::from_str(line.trim_end()).unwrap();
    assert!(!response.ok);
    assert!(response.error.unwrap().contains("invalid glob pattern"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn error_envelope_is_bounded_for_any_message() {
    let error = anyhow::anyhow!("{}", "\"\\\u{1}".repeat(1_000_000));
    let line = gents_fs_runner::error_response_line(&error);
    assert!(line.len() <= MAX_RESPONSE_BYTES, "{} bytes", line.len());
    let response: NativeFsRunnerResponse = serde_json::from_str(line.trim_end()).unwrap();
    assert!(response.error.unwrap().ends_with("... [truncated]"));
}
