//! Shared plumbing for the demo command: subprocess invocation, terminal
//! prompts, and small string/path helpers.
//!
//! Every stdin read in the demo goes through one owned [`StdinLines`] so the
//! first-run backend picker, the interactive shell, and `reconfigure` never
//! fight over the terminal — mixing a blocking `std::io::stdin` read with an
//! async tokio reader loses buffered input on piped stdin.

use std::io::Write as _;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio::process::Command;

/// The single owned stdin line reader threaded through the whole demo session.
pub(super) type StdinLines = tokio::io::Lines<tokio::io::BufReader<tokio::io::Stdin>>;

/// Print a prompt without a trailing newline and flush it.
pub(super) fn prompt(text: &str) {
    print!("{text}");
    let _ = std::io::stdout().flush();
}

/// Print `text`, then read one line from the shared reader.
pub(super) async fn prompt_line(reader: &mut StdinLines, text: &str) -> Result<String> {
    prompt(text);
    Ok(reader.next_line().await?.unwrap_or_default())
}

/// Read one line without echoing it (best-effort via `stty`; a non-terminal
/// stdin, e.g. piped input, just reads normally since there is nothing to echo).
pub(super) async fn prompt_secret(reader: &mut StdinLines, text: &str) -> Result<String> {
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

pub(super) fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

pub(super) fn short(did: &str) -> String {
    if did.len() > 16 {
        format!("{}…", &did[..16])
    } else {
        did.to_string()
    }
}

pub(super) fn path_arg(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

/// Build an owned arg vector from string slices (paths must already be strings).
pub(super) fn cli(args: &[&str]) -> Vec<String> {
    args.iter().map(|value| value.to_string()).collect()
}

pub(super) async fn run_cli_text(bin: &Path, args: &[String]) -> Result<String> {
    let output = Command::new(bin)
        .args(args)
        .output()
        .await
        .context("running gents subcommand")?;
    if !output.status.success() {
        bail!(
            "gents {} failed: {}",
            args.first().cloned().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub(super) async fn run_cli_json(bin: &Path, args: &[String]) -> Result<Value> {
    let stdout = run_cli_text(bin, args).await?;
    serde_json::from_str(stdout.trim()).with_context(|| {
        format!(
            "parsing JSON from gents {}",
            args.first().cloned().unwrap_or_default()
        )
    })
}
