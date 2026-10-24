use std::io::{Read, Write};
use std::path::PathBuf;

pub mod protocol;

mod context;
mod model;
mod output;
mod tools;
mod traversal;

pub use context::PathError;
pub use output::OutputError;
pub use output::{error_response_line, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES};
pub use tools::{execute_request, execute_request_with_base, ToolError};

use protocol::{GlobArgs, NativeFsRunnerRequest, NativeFsRunnerResponse};

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("unknown argument {0:?}")]
    UnknownArgument(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("runner request exceeds {MAX_REQUEST_BYTES} bytes")]
    RequestTooLarge,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error("self-test output did not include src/main.rs: {output}")]
    SelfTestMissingFile { output: String },
}

type Result<T, E = RunnerError> = std::result::Result<T, E>;

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
            other => return Err(RunnerError::UnknownArgument(other.to_owned())),
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
    let request = read_request(std::io::stdin())?;
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

fn read_request(reader: impl Read) -> Result<NativeFsRunnerRequest> {
    let mut input = String::new();
    reader
        .take(MAX_REQUEST_BYTES as u64 + 1)
        .read_to_string(&mut input)?;
    if input.len() > MAX_REQUEST_BYTES {
        return Err(RunnerError::RequestTooLarge);
    }
    Ok(serde_json::from_str(&input)?)
}

pub fn write_stdio_error(error: &dyn std::error::Error) {
    let _ = std::io::stdout().write_all(error_response_line(error).as_bytes());
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
        Err(RunnerError::SelfTestMissingFile { output })
    }
}
