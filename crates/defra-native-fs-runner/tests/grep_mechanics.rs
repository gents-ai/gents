//! Fences for #729: grep bounds per-file work — oversized files and binary
//! files are skipped (and counted), and case-insensitive matching keeps
//! working for both ASCII and non-ASCII needles.

use defra_native_fs_runner::execute_request_with_base;
use defra_native_fs_runner::protocol::{GrepArgs, NativeFsRunnerRequest};
use serde_json::Value;

fn unique_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "defra-fs-grep-mechanics-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
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
