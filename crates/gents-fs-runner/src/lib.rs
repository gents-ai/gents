use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{anyhow, Result};

pub mod protocol;

mod context;
mod model;
mod output;
mod tools;
mod traversal;

pub use tools::{execute_request, execute_request_with_base};

use protocol::{GlobArgs, NativeFsRunnerRequest, NativeFsRunnerResponse};

/// Run the native filesystem runner's hidden command-line protocol.
///
/// Hosts that embed Gents can expose this from their own executable, allowing
/// filesystem work to retain its killable process boundary without shipping a
/// second sidecar binary.
pub fn run_stdio_from_args(args: impl IntoIterator<Item = String>) -> Result<()> {
    let mut args = args.into_iter();
    let mut root = None;
    let mut base = None;
    let mut self_test_requested = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--self-test" => self_test_requested = true,
            "--root" => root = args.next().map(PathBuf::from),
            "--base" => base = args.next().map(PathBuf::from),
            other => anyhow::bail!("unknown argument {other:?}"),
        }
    }

    if self_test_requested {
        self_test()?;
        println!("self-test ok");
        return Ok(());
    }

    let root = match root {
        Some(root) => root,
        None => std::env::current_dir()?,
    };
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let request: NativeFsRunnerRequest = serde_json::from_str(&input)?;
    let output = execute_request_with_base(root, base, request)?;
    serde_json::to_writer(
        std::io::stdout(),
        &NativeFsRunnerResponse {
            ok: true,
            output: Some(output),
            error: None,
        },
    )?;
    println!();
    Ok(())
}

/// Write a protocol-shaped failure for a runner invocation.
pub fn write_stdio_error(error: &anyhow::Error) {
    let _ = serde_json::to_writer(
        std::io::stdout(),
        &NativeFsRunnerResponse {
            ok: false,
            output: None,
            error: Some(format!("{error:#}")),
        },
    );
    let _ = writeln!(std::io::stdout());
}

pub fn self_test() -> Result<()> {
    let root =
        std::env::temp_dir().join(format!("gents-fs-runner-self-test-{}", std::process::id()));
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
        let root =
            std::env::temp_dir().join(format!("gents-fs-runner-root-{}", std::process::id()));
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
        let root =
            std::env::temp_dir().join(format!("gents-fs-runner-grep-file-{}", std::process::id()));
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
