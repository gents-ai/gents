//! Fences for #729: glob prunes its walk to the pattern's literal path
//! prefix, so `infra/services/**/*.yml` never walks unrelated siblings, and a
//! nonexistent prefix returns immediately with a diagnostic instead of a
//! full-tree walk that looks identical to a genuine miss.

use defra_native_fs_runner::execute_request_with_base;
use defra_native_fs_runner::protocol::{GlobArgs, NativeFsRunnerRequest};
use serde_json::Value;

mod support;
use support::{run_glob, unique_root};

#[test]
fn glob_prunes_walk_to_literal_pattern_prefix() {
    let root = unique_root("prune");
    std::fs::create_dir_all(root.join("infra/services/grafana")).unwrap();
    std::fs::write(root.join("infra/services/grafana/a.yml"), "a").unwrap();
    std::fs::write(root.join("infra/services/grafana/b.yml"), "b").unwrap();
    // A large unrelated sibling tree the pruned walk must never enter.
    std::fs::create_dir_all(root.join("bulk")).unwrap();
    for index in 0..50 {
        std::fs::write(root.join(format!("bulk/noise-{index:02}.txt")), "x").unwrap();
    }

    let value = run_glob(&root, "infra/services/grafana/**/*.yml");

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 2, "{value}");
    assert_eq!(value["pattern_prefix"], "infra/services/grafana");
    assert_eq!(value["pattern_prefix_exists"], true);
    let visited = value["walk"]["entries_visited"].as_u64().unwrap();
    assert!(
        visited <= 3,
        "pruned walk should only visit the grafana subtree, visited {visited}"
    );
}

#[test]
fn glob_missing_prefix_returns_immediately_with_diagnostic() {
    let root = unique_root("missing-prefix");
    std::fs::create_dir_all(root.join("real")).unwrap();
    for index in 0..20 {
        std::fs::write(root.join(format!("real/file-{index:02}.txt")), "x").unwrap();
    }

    let value = run_glob(&root, "no/such/dir/**/*.yml");

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["ok"], true);
    assert_eq!(value["returned_count"], 0);
    assert_eq!(value["pattern_prefix"], "no/such/dir");
    assert_eq!(value["pattern_prefix_exists"], false);
    assert_eq!(value["walk"]["entries_visited"], 0, "{value}");
}

#[test]
fn glob_fully_literal_pattern_matches_without_wide_walk() {
    let root = unique_root("literal");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(root.join("src/lib.rs"), "").unwrap();
    std::fs::create_dir_all(root.join("bulk")).unwrap();
    for index in 0..50 {
        std::fs::write(root.join(format!("bulk/noise-{index:02}.txt")), "x").unwrap();
    }

    let value = run_glob(&root, "src/main.rs");

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 1, "{value}");
    assert_eq!(value["matches"][0]["path"], "src/main.rs");
    let visited = value["walk"]["entries_visited"].as_u64().unwrap();
    assert!(visited <= 2, "visited {visited}");
}

#[test]
fn glob_explicit_prefix_into_default_ignored_dir_is_honored() {
    // Naming an ignored directory in the pattern is an explicit request, the
    // same as passing it as `path` — the prefix walk enters it.
    let root = unique_root("ignored-prefix");
    std::fs::create_dir_all(root.join("target/debug")).unwrap();
    std::fs::write(root.join("target/debug/build.log"), "x").unwrap();

    let value = run_glob(&root, "target/debug/*.log");

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 1, "{value}");
    assert_eq!(value["matches"][0]["path"], "target/debug/build.log");
}

#[test]
fn glob_prefix_cannot_escape_root() {
    let root = unique_root("escape");
    let outside = root.join("outside");
    let inside = root.join("inside");
    std::fs::create_dir_all(outside.join("secrets")).unwrap();
    std::fs::write(outside.join("secrets/key.pem"), "x").unwrap();
    std::fs::create_dir_all(&inside).unwrap();

    // Base (and root) is `inside`; a `..` prefix must not walk `outside`.
    let output = execute_request_with_base(
        inside.clone(),
        None,
        NativeFsRunnerRequest::Glob(GlobArgs {
            pattern: "../outside/secrets/*.pem".to_string(),
            path: None,
            max_matches: 100,
            raw_json: true,
            max_entries_visited: None,
            max_wall_ms: None,
        }),
    )
    .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 0, "{value}");
    assert!(!output.contains("key.pem"), "escaped the root: {output}");
}

#[test]
fn glob_prefix_pruning_composes_with_path_argument() {
    // The pattern is matched against base-relative display paths, so the
    // literal prefix must resolve against the BASE, not the path argument —
    // path="crates" + pattern="crates/..." must not double-join into
    // crates/crates/... and report false zero matches.
    let root = unique_root("prefix-with-path");
    std::fs::create_dir_all(root.join("crates/defra-agent/src")).unwrap();
    std::fs::write(root.join("crates/defra-agent/src/lib.rs"), "x").unwrap();

    let output = execute_request_with_base(
        root.clone(),
        None,
        NativeFsRunnerRequest::Glob(GlobArgs {
            pattern: "crates/defra-agent/**/*.rs".to_string(),
            path: Some("crates".to_string()),
            max_matches: 100,
            raw_json: true,
            max_entries_visited: None,
            max_wall_ms: None,
        }),
    )
    .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 1, "{value}");
    assert_eq!(value["matches"][0]["path"], "crates/defra-agent/src/lib.rs");
    assert_eq!(value["pattern_prefix_exists"], true);
}

#[test]
fn glob_prefix_disjoint_from_path_returns_empty_without_walk() {
    let root = unique_root("prefix-disjoint");
    std::fs::create_dir_all(root.join("crates")).unwrap();
    for index in 0..30 {
        std::fs::write(root.join(format!("crates/noise-{index:02}.rs")), "x").unwrap();
    }
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(root.join("docs/readme.md"), "x").unwrap();

    let output = execute_request_with_base(
        root.clone(),
        None,
        NativeFsRunnerRequest::Glob(GlobArgs {
            pattern: "docs/*.md".to_string(),
            path: Some("crates".to_string()),
            max_matches: 100,
            raw_json: true,
            max_entries_visited: None,
            max_wall_ms: None,
        }),
    )
    .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();

    let _ = std::fs::remove_dir_all(&root);
    // The prefix exists under base, but nothing under path can ever match:
    // return immediately instead of walking the path subtree.
    assert_eq!(value["returned_count"], 0, "{value}");
    assert_eq!(value["pattern_prefix_exists"], true);
    assert_eq!(value["walk"]["entries_visited"], 0, "{value}");
}

#[cfg(unix)]
#[test]
fn glob_symlinked_prefix_is_reported_missing_without_walk() {
    // The walk never follows directory symlinks, so a symlink-spelled
    // pattern cannot match any walked display path. Pruning must not
    // silently traverse the resolved target either — reject the prefix and
    // report it missing.
    let root = unique_root("prefix-symlink");
    std::fs::create_dir_all(root.join("real/src")).unwrap();
    std::fs::write(root.join("real/src/a.rs"), "x").unwrap();
    std::os::unix::fs::symlink(root.join("real/src"), root.join("src")).unwrap();

    let value = run_glob(&root, "src/*.rs");

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 0, "{value}");
    assert_eq!(value["pattern_prefix_exists"], false, "{value}");
    assert_eq!(value["walk"]["entries_visited"], 0, "{value}");
}
