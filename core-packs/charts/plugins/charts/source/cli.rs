//! The plugin's standard input and output: one JSON object in, one JSON
//! result out. A failure is one sentence on standard error and a non-zero
//! exit code.

use std::io::Write as _;

/// Largest request read from standard input.
const MAX_REQUEST: usize = 64 * 1024 * 1024;

/// Reads the request, runs it and writes the result, then exits.
pub fn main() -> ! {
    let code = match run() {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("{message}");
            1
        }
    };
    std::process::exit(code);
}

fn run() -> Result<(), String> {
    let raw = crate::load::read_limited(std::io::stdin().lock(), MAX_REQUEST).map_err(|e| e.0)?;
    let out = crate::run(&raw).map_err(|e| e.0)?;
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(out.as_bytes())
        .and_then(|()| stdout.write_all(b"\n"))
        .map_err(|e| format!("the result could not be written: {e}"))
}
