//! Terminal prompt plumbing shared by the interactive `onboard` command and the
//! `demo` session. One owned [`StdinLines`] reader is threaded through a whole
//! interactive flow so successive prompts never fight over buffered stdin —
//! mixing a blocking `std::io::stdin` read with an async tokio reader loses
//! buffered input on piped stdin.

use std::io::Write as _;

use anyhow::Result;
use tokio::io::AsyncBufReadExt as _;

/// The single owned stdin line reader threaded through an interactive session.
pub(crate) type StdinLines = tokio::io::Lines<tokio::io::BufReader<tokio::io::Stdin>>;

/// Build the owned stdin line reader for an interactive session.
pub(crate) fn stdin_lines() -> StdinLines {
    tokio::io::BufReader::new(tokio::io::stdin()).lines()
}

/// Print `text` and flush, without a trailing newline (for inline prompts).
pub(crate) fn prompt(text: &str) {
    print!("{text}");
    let _ = std::io::stdout().flush();
}

/// Print `text`, then read one line from the shared reader.
pub(crate) async fn prompt_line(reader: &mut StdinLines, text: &str) -> Result<String> {
    prompt(text);
    Ok(reader.next_line().await?.unwrap_or_default())
}

/// Read one line without echoing it (best-effort via `stty`; a non-terminal
/// stdin, e.g. piped input, just reads normally since there is nothing to echo).
pub(crate) async fn prompt_secret(reader: &mut StdinLines, text: &str) -> Result<String> {
    prompt(text);
    let hidden = set_terminal_echo(false);
    let line = reader.next_line().await;
    if hidden {
        set_terminal_echo(true);
        println!();
    }
    Ok(line?.unwrap_or_default())
}

fn set_terminal_echo(on: bool) -> bool {
    std::process::Command::new("stty")
        .arg(if on { "echo" } else { "-echo" })
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Trimmed value, or `None` when empty/whitespace.
pub(crate) fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}
