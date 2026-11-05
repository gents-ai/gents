use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use tokio::process::Command;

pub(super) fn path_arg(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

/// The words before the first flag, which name the subcommand.
fn command_name(args: &[String]) -> String {
    args.iter()
        .take_while(|arg| !arg.starts_with('-'))
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ")
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
            command_name(args),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub(super) async fn run_cli_json(bin: &Path, args: &[String]) -> Result<Value> {
    let stdout = run_cli_text(bin, args).await?;
    serde_json::from_str(stdout.trim())
        .with_context(|| format!("parsing JSON from gents {}", command_name(args)))
}

pub(super) async fn run_cli_json_with_env(
    bin: &Path,
    args: &[String],
    environment: BTreeMap<String, String>,
) -> Result<Value> {
    let output = Command::new(bin)
        .args(args)
        .envs(environment)
        .output()
        .await
        .context("running gents subcommand")?;
    if !output.status.success() {
        bail!(
            "gents {} failed: {}",
            command_name(args),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout)
        .with_context(|| format!("parsing JSON from gents {}", command_name(args)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_name_is_every_word_before_the_first_flag() {
        let args = ["utils", "debug", "request", "show", "--id", "x", "extra"].map(String::from);
        assert_eq!(command_name(&args), "utils debug request show");
        assert_eq!(command_name(&[]), "");
    }
}
