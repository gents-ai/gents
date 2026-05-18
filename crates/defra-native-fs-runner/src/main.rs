use std::io::{Read, Write};
use std::path::PathBuf;

use defra_native_fs_runner::protocol::{NativeFsRunnerRequest, NativeFsRunnerResponse};

fn main() {
    if let Err(error) = run() {
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
}

fn run() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut root = None;
    let mut self_test = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--self-test" => self_test = true,
            "--root" => root = args.next().map(PathBuf::from),
            other => anyhow::bail!("unknown argument {other:?}"),
        }
    }

    if self_test {
        defra_native_fs_runner::self_test()?;
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
