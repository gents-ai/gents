use std::io::{stdin, stdout, IsTerminal};
use std::process::ExitCode;

use gents_shell::{cli, terminal};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let set = match cli::parse(&args) {
        Ok(set) => set,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    if !(stdin().is_terminal() && stdout().is_terminal()) {
        eprintln!("gents-shell needs a terminal: stdin and stdout must both be terminals");
        return ExitCode::from(2);
    }
    if let Err(error) = terminal::run(set) {
        eprintln!("gents-shell: {error}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
