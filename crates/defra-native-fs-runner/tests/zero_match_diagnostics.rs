//! Fences for #729: a zero-match search tells the model where it was
//! anchored — the top-level entries of the searched directory — so a wrong
//! path anchor is corrected on the first attempt instead of retried through
//! repeated full walks.

use defra_native_fs_runner::execute_request_with_base;
use defra_native_fs_runner::protocol::{GlobArgs, GrepArgs, NativeFsRunnerRequest};
use serde_json::Value;

mod support;
use support::{glob_request, unique_root};

#[test]
fn zero_match_glob_reports_search_dir_entries() {
    let root = unique_root("glob");
    std::fs::create_dir_all(root.join("amygdala/infra")).unwrap();
    std::fs::write(root.join("amygdala/infra/deploy.yml"), "x").unwrap();

    let output =
        execute_request_with_base(root.clone(), None, glob_request("infra/**/*.yml")).unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 0, "{value}");
    assert_eq!(value["pattern_prefix_exists"], false);
    let entries = value["search_dir_entries"].as_array().unwrap();
    assert!(
        entries.iter().any(|entry| entry == "amygdala"),
        "search_dir_entries should reveal the actual top level: {entries:?}"
    );
}

#[test]
fn matching_glob_omits_search_dir_entries() {
    let root = unique_root("glob-hit");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "x").unwrap();

    let output = execute_request_with_base(root.clone(), None, glob_request("src/*.rs")).unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 1, "{value}");
    assert!(value["search_dir_entries"].is_null());
}

#[test]
fn zero_match_grep_reports_search_dir_entries() {
    let root = unique_root("grep");
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(root.join("docs/notes.md"), "nothing here\n").unwrap();

    let output = execute_request_with_base(
        root.clone(),
        None,
        NativeFsRunnerRequest::Grep(GrepArgs {
            pattern: "absent-needle".to_string(),
            path: None,
            case_sensitive: true,
            max_matches: 100,
            raw_json: true,
            max_entries_visited: None,
            max_bytes_read: None,
            max_wall_ms: None,
        }),
    )
    .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 0, "{value}");
    let entries = value["search_dir_entries"].as_array().unwrap();
    assert!(entries.iter().any(|entry| entry == "docs"), "{entries:?}");
}

#[test]
fn budget_exhausted_zero_match_omits_search_dir_entries() {
    // Review finding: a budget-stopped zero-match is NOT an anchoring
    // problem; emitting the "your path anchor is wrong" hint sends the model
    // off to re-anchor a correct pattern.
    let root = unique_root("budget-no-hint");
    for index in 0..30 {
        std::fs::write(root.join(format!("file-{index:02}.txt")), "x").unwrap();
    }

    let output = execute_request_with_base(
        root.clone(),
        None,
        NativeFsRunnerRequest::Glob(GlobArgs {
            pattern: "**/*.zzz".to_string(),
            path: None,
            max_matches: 100,
            raw_json: true,
            max_entries_visited: Some(5),
            max_wall_ms: None,
        }),
    )
    .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 0, "{value}");
    assert_eq!(value["walk"]["budget_exhausted"], true, "{value}");
    assert!(value["search_dir_entries"].is_null(), "{value}");
}
