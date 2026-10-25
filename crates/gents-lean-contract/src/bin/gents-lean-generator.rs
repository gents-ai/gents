use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{ensure, Context, Result};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 3,
        "usage: gents-lean-generator <proofs-dir> <target> <file>"
    );
    let target = args[1].to_str().context("Lean target is not UTF-8")?;
    let file = args[2]
        .to_str()
        .context("Lean generator path is not UTF-8")?;
    let output =
        gents_lean_contract::run_prepared_generator(&PathBuf::from(&args[0]), target, file)?;
    io::stdout().lock().write_all(output.as_bytes())?;
    Ok(())
}
