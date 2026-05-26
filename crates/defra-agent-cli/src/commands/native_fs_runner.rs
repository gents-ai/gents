use std::io::{Read, Write};

use anyhow::Result;
use defra_native_fs_runner::protocol::{NativeFsRunnerRequest, NativeFsRunnerResponse};

use crate::NativeFsRunnerArgs;

pub(crate) fn native_fs_runner(args: NativeFsRunnerArgs) -> Result<()> {
    if let Err(error) = run(args) {
        let _ = serde_json::to_writer(
            std::io::stdout(),
            &NativeFsRunnerResponse {
                ok: false,
                output: None,
                error: Some(format!("{error:#}")),
            },
        );
        let _ = writeln!(std::io::stdout());
        std::process::exit(1);
    }
    Ok(())
}

fn run(args: NativeFsRunnerArgs) -> Result<()> {
    if args.self_test {
        defra_native_fs_runner::self_test()?;
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
    match defra_native_fs_runner::execute_request(root, request) {
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
                    error: Some(format!("{error:#}")),
                },
            )?;
            println!();
            std::process::exit(1);
        }
    }
}
