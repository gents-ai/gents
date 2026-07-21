use anyhow::{anyhow, Result};

pub mod protocol;

mod context;
mod model;
mod output;
mod tools;
mod traversal;

pub use tools::{execute_request, execute_request_with_base};

use protocol::{GlobArgs, NativeFsRunnerRequest};

pub fn self_test() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "defra-native-fs-runner-self-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src"))?;
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n")?;
    let output = execute_request(
        root.clone(),
        NativeFsRunnerRequest::Glob(GlobArgs {
            pattern: "**/*.rs".to_string(),
            path: Some(".".to_string()),
            max_matches: 10,
            raw_json: false,
            max_entries_visited: None,
            max_wall_ms: None,
        }),
    )?;
    let _ = std::fs::remove_dir_all(&root);
    if output.contains("src/main.rs") {
        Ok(())
    } else {
        Err(anyhow!(
            "self-test output did not include src/main.rs: {output}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{GrepArgs, ListFilesArgs, NativeFsRunnerRequest};

    #[test]
    fn request_with_base_resolves_relative_paths_from_base() {
        let root = std::env::temp_dir().join(format!(
            "defra-native-fs-runner-root-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let base = root.join("repo");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("README.md"), "ok").unwrap();
        std::fs::write(root.join("HOME.md"), "wrong").unwrap();

        let output = execute_request_with_base(
            root.clone(),
            Some(base),
            NativeFsRunnerRequest::ListFiles(ListFilesArgs {
                path: None,
                recursive: false,
                max_entries: 10,
                raw_json: false,
                max_entries_visited: None,
                max_wall_ms: None,
            }),
        )
        .unwrap();

        let _ = std::fs::remove_dir_all(&root);
        assert!(output.contains("README.md"));
        assert!(!output.contains("HOME.md"));
    }

    #[test]
    fn grep_accepts_single_file_path() {
        let root = std::env::temp_dir().join(format!(
            "defra-native-fs-runner-grep-file-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("repo/src")).unwrap();
        std::fs::write(root.join("repo/src/lib.rs"), "pub fn useful() {}\n").unwrap();

        let output = execute_request_with_base(
            root.clone(),
            Some(root.join("repo")),
            NativeFsRunnerRequest::Grep(GrepArgs {
                pattern: "useful".to_string(),
                path: Some("src/lib.rs".to_string()),
                case_sensitive: true,
                max_matches: 10,
                raw_json: false,
                max_entries_visited: None,
                max_bytes_read: None,
                max_wall_ms: None,
            }),
        )
        .unwrap();

        let _ = std::fs::remove_dir_all(&root);
        assert!(output.contains("src/lib.rs:L1: pub fn useful() {}"));
    }
}
