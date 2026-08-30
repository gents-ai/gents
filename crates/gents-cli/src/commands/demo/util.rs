use std::io::Write as _;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio::process::Command;

pub(super) type StdinLines = tokio::io::Lines<tokio::io::BufReader<tokio::io::Stdin>>;

pub(super) fn prompt(text: &str) {
    print!("{text}");
    let _ = std::io::stdout().flush();
}

pub(super) async fn prompt_line(reader: &mut StdinLines, text: &str) -> Result<String> {
    prompt(text);
    Ok(reader.next_line().await?.unwrap_or_default())
}

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
