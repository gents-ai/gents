//! Fences for #729: every traversal reports what it walked, and budgets bound
//! the walk so a zero-match search over a huge root returns partial results
//! with explicit exhaustion metadata instead of scanning everything.

use gents_fs_runner::protocol::{GlobArgs, GrepArgs, ListFilesArgs, NativeFsRunnerRequest};

mod support;
use support::{run_json, unique_root};

#[test]
fn glob_reports_walk_stats_on_success() {
    let root = unique_root("glob-stats");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "a").unwrap();
    std::fs::write(root.join("src/b.rs"), "b").unwrap();

    let value = run_json(
        &root,
        NativeFsRunnerRequest::Glob(GlobArgs {
            pattern: "**/*.rs".to_string(),
            path: None,
            max_matches: 10,
            raw_json: true,
            max_entries_visited: None,
            max_wall_ms: None,
        }),
    );

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 2);
    let walk = &value["walk"];
    // src dir + 2 files.
    assert_eq!(walk["entries_visited"], 3);
    assert_eq!(walk["budget_exhausted"], false);
    assert!(walk["stopped_at"].is_null());
    assert!(walk["elapsed_ms"].is_u64());
}

#[test]
fn glob_entry_budget_returns_partial_results_with_exhaustion_metadata() {
    let root = unique_root("glob-entry-budget");
    for index in 0..30 {
        std::fs::write(root.join(format!("file-{index:02}.txt")), "x").unwrap();
    }

    let value = run_json(
        &root,
        NativeFsRunnerRequest::Glob(GlobArgs {
            pattern: "**/*.txt".to_string(),
            path: None,
            max_matches: 100,
            raw_json: true,
            max_entries_visited: Some(10),
            max_wall_ms: None,
        }),
    );

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["ok"], true);
    let walk = &value["walk"];
    assert_eq!(walk["budget_exhausted"], true);
    assert_eq!(walk["entries_visited"], 10);
    assert!(walk["stopped_at"].is_string());
    // Exhaustion means the result set is incomplete.
    assert_eq!(value["truncated"], true);
    assert!(value["total_count"].is_null());
    let returned = value["returned_count"].as_u64().unwrap();
    assert!(returned > 0 && returned < 30, "returned {returned}");
}

#[test]
fn grep_bytes_budget_stops_reading_files() {
    let root = unique_root("grep-bytes-budget");
    for index in 0..3 {
        std::fs::write(
            root.join(format!("log-{index}.txt")),
            format!("needle {index}\n{}", "padding\n".repeat(20)),
        )
        .unwrap();
    }

    let value = run_json(
        &root,
        NativeFsRunnerRequest::Grep(GrepArgs {
            pattern: "needle".to_string(),
            path: None,
            case_sensitive: true,
            max_matches: 100,
            raw_json: true,
            max_entries_visited: None,
            max_bytes_read: Some(200),
            max_wall_ms: None,
        }),
    );

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["ok"], true);
    let walk = &value["walk"];
    assert_eq!(walk["budget_exhausted"], true);
    assert_eq!(value["truncated"], true);
    let returned = value["returned_count"].as_u64().unwrap();
    assert!(returned < 3, "returned {returned}");
    let bytes_read = value["bytes_read"].as_u64().unwrap();
    assert!(bytes_read >= 1, "bytes_read {bytes_read}");
}

#[test]
fn glob_wall_budget_stops_walk() {
    let root = unique_root("glob-wall-budget");
    let slow_dir = root.join("aaa-slow");
    std::fs::create_dir_all(&slow_dir).unwrap();
    std::fs::write(slow_dir.join("inner.txt"), "x").unwrap();
    std::fs::write(root.join("zzz-after.txt"), "x").unwrap();

    // Existing test hook: sorted_children sleeps when reading this directory.
    // The runner canonicalizes its root, so the hook path must be canonical
    // too (macOS temp dirs live behind a /var -> /private/var symlink).
    std::env::set_var(
        "DEFRA_NATIVE_FS_RUNNER_BLOCK_DIR",
        std::fs::canonicalize(&slow_dir)
            .unwrap()
            .to_string_lossy()
            .to_string(),
    );
    std::env::set_var("DEFRA_NATIVE_FS_RUNNER_BLOCK_MS", "300");

    let value = run_json(
        &root,
        NativeFsRunnerRequest::Glob(GlobArgs {
            pattern: "**/*.txt".to_string(),
            path: None,
            max_matches: 100,
            raw_json: true,
            max_entries_visited: None,
            max_wall_ms: Some(50),
        }),
    );

    std::env::remove_var("DEFRA_NATIVE_FS_RUNNER_BLOCK_DIR");
    std::env::remove_var("DEFRA_NATIVE_FS_RUNNER_BLOCK_MS");
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(value["ok"], true);
    let walk = &value["walk"];
    assert_eq!(walk["budget_exhausted"], true);
    assert!(walk["elapsed_ms"].as_u64().unwrap() >= 50);
    // The walk stopped inside/after the slow directory: the later sibling was
    // never reached.
    let matches = value["matches"].as_array().unwrap();
    assert!(
        !matches.iter().any(|entry| entry["path"]
            .as_str()
            .unwrap_or_default()
            .contains("zzz-after")),
        "walk should have stopped before zzz-after: {matches:?}"
    );
}

#[test]
fn recursive_list_files_honors_entry_budget() {
    let root = unique_root("list-entry-budget");
    for index in 0..30 {
        std::fs::write(root.join(format!("file-{index:02}.txt")), "x").unwrap();
    }

    let value = run_json(
        &root,
        NativeFsRunnerRequest::ListFiles(ListFilesArgs {
            path: None,
            recursive: true,
            max_entries: 100,
            raw_json: true,
            max_entries_visited: Some(10),
            max_wall_ms: None,
        }),
    );

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["ok"], true);
    let walk = &value["walk"];
    assert_eq!(walk["budget_exhausted"], true);
    assert_eq!(value["truncated"], true);
    let returned = value["returned_count"].as_u64().unwrap();
    assert!(returned < 30, "returned {returned}");
}
