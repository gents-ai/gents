//! workflow plugin entry point: one JSON value on stdin, one on stdout; an
//! engine failure exits 1 with the host's message on stderr so the host keeps
//! its verdict and wording.
use std::io::{Read, Write};

fn main() {
    let mut raw = String::new();
    std::io::stdin()
        .read_to_string(&mut raw)
        .expect("read stdin");
    let out = match serde_json::from_str(&raw) {
        Ok(input) => match workflow::step(input) {
            workflow::Stepped::Output(out) => out,
            workflow::Stepped::Trap(message) => {
                eprintln!("{message}");
                std::process::exit(1);
            }
        },
        Err(e) => workflow::fail(
            "workflow_invalid_input",
            &format!("input is not JSON ({e})"),
        ),
    };
    let mut stdout = std::io::stdout();
    serde_json::to_writer(&mut stdout, &out).expect("write stdout");
    stdout.flush().expect("flush stdout");
}
