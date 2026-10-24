use std::io::{Read, Write};

use gents_fs_runner::protocol::{NativeFsRunnerRequest, NativeFsRunnerResponse};
use gents_protocol::error_chain::ErrorChain;

use crate::NativeFsRunnerArgs;

#[derive(Debug, thiserror::Error)]
pub(crate) enum NativeFsRunnerError {
    #[error(transparent)]
    Runner(#[from] gents_fs_runner::RunnerError),
    #[error(transparent)]
    Tool(#[from] gents_fs_runner::ToolError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub(crate) fn native_fs_runner(args: NativeFsRunnerArgs) -> Result<(), NativeFsRunnerError> {
    if let Err(error) = run(args) {
        let _ = serde_json::to_writer(
            std::io::stdout(),
            &NativeFsRunnerResponse {
                ok: false,
                output: None,
                error: Some(ErrorChain(&error).to_string()),
            },
        );
        let _ = writeln!(std::io::stdout());
        std::process::exit(1);
    }
    Ok(())
}

fn run(args: NativeFsRunnerArgs) -> Result<(), NativeFsRunnerError> {
    if args.self_test {
        gents_fs_runner::self_test()?;
        println!("self-test ok");
        return Ok(());
    }

    let root = match args.root {
        Some(root) => root,
        None => std::env::current_dir()?,
    };
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let request: NativeFsRunnerRequest = serde_json::from_str(&input)?;
    match gents_fs_runner::execute_request_with_base(root, args.base, request) {
        Ok(output) => {
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
        Err(error) => {
            serde_json::to_writer(
                std::io::stdout(),
                &NativeFsRunnerResponse {
                    ok: false,
                    output: None,
                    error: Some(ErrorChain(&error).to_string()),
                },
            )?;
            println!();
            std::process::exit(1);
        }
    }
}
