//! Fences for #732 stage 2: walks respect .gitignore files encountered in
//! the walked tree (nested repos under a home-directory tool root get their
//! generated junk filtered before it consumes walk budget), while dotfiles
//! stay visible — agents rely on `.github/...` patterns and dotfile listings.

use defra_native_fs_runner::execute_request_with_base;
use defra_native_fs_runner::protocol::{
    GlobArgs, GrepArgs, ListFilesArgs, NativeFsRunnerRequest,
};
use serde_json::Value;

fn unique_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "defra-fs-gitignore-{tag}-{}-{:?}",
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

#[test]
fn walk_respects_gitignore_in_walked_tree() {
    let root = unique_root("basic");
    std::fs::write(root.join(".gitignore"), "junk/\n").unwrap();
    std::fs::create_dir_all(root.join("junk")).unwrap();
    std::fs::write(root.join("junk/hit.txt"), "needle\n").unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/hit.txt"), "needle\n").unwrap();

    let glob_value = run_glob(&root, "**/*.txt");
    let grep_output = execute_request_with_base(
        root.clone(),
        None,
        NativeFsRunnerRequest::Grep(GrepArgs {
            pattern: "needle".to_string(),
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
    let grep_value: Value = serde_json::from_str(&grep_output).unwrap();

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(glob_value["returned_count"], 1, "{glob_value}");
    assert_eq!(glob_value["matches"][0]["path"], "src/hit.txt");
    assert_eq!(grep_value["returned_count"], 1, "{grep_value}");
    assert_eq!(grep_value["matches"][0]["path"], "src/hit.txt");
}

#[test]
fn gitignore_negation_is_honored() {
    let root = unique_root("negation");
    std::fs::write(root.join(".gitignore"), "*.log\n!keep.log\n").unwrap();
    std::fs::write(root.join("drop.log"), "x").unwrap();
    std::fs::write(root.join("keep.log"), "x").unwrap();

    let value = run_glob(&root, "**/*.log");

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 1, "{value}");
    assert_eq!(value["matches"][0]["path"], "keep.log");
}

#[test]
fn nested_gitignore_scopes_to_its_directory() {
    let root = unique_root("nested");
    std::fs::create_dir_all(root.join("sub/local")).unwrap();
    std::fs::write(root.join("sub/.gitignore"), "local/\n").unwrap();
    std::fs::write(root.join("sub/local/a.txt"), "x").unwrap();
    std::fs::create_dir_all(root.join("local")).unwrap();
    std::fs::write(root.join("local/b.txt"), "x").unwrap();

    let value = run_glob(&root, "**/*.txt");

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(value["returned_count"], 1, "{value}");
    assert_eq!(value["matches"][0]["path"], "local/b.txt");
}

#[test]
fn dotfiles_remain_visible_to_walks_and_listings() {
    let root = unique_root("dotfiles");
    std::fs::create_dir_all(root.join(".github/workflows")).unwrap();
    std::fs::write(root.join(".github/workflows/ci.yml"), "x").unwrap();

    let glob_value = run_glob(&root, ".github/workflows/*.yml");
    let list_output = execute_request_with_base(
        root.clone(),
        None,
        NativeFsRunnerRequest::ListFiles(ListFilesArgs {
            path: None,
            recursive: false,
            max_entries: 100,
            raw_json: true,
            max_entries_visited: None,
            max_wall_ms: None,
        }),
    )
    .unwrap();
    let list_value: Value = serde_json::from_str(&list_output).unwrap();

    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(glob_value["returned_count"], 1, "{glob_value}");
    let entries = list_value["entries"].as_array().unwrap();
    assert!(
        entries.iter().any(|entry| entry["path"] == ".github"),
        "{entries:?}"
    );
}
