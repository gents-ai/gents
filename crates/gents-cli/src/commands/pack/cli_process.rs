use std::path::Path;

use serde_json::Value;
use std::collections::BTreeMap;
use tokio::process::Command;

pub(super) fn path_arg(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[derive(Debug, thiserror::Error)]
pub(super) enum CliProcessError {
    #[error("running gents subcommand")]
    Spawn(#[source] std::io::Error),
    #[error("gents {subcommand} failed: {stderr}")]
    Failed { subcommand: String, stderr: String },
    #[error("parsing JSON from gents {subcommand}")]
    ParseJson {
        subcommand: String,
        #[source]
        source: serde_json::Error,
    },
}

fn subcommand(args: &[String]) -> String {
    args.first().cloned().unwrap_or_default()
}

fn check_status(output: &std::process::Output, args: &[String]) -> Result<(), CliProcessError> {
    if output.status.success() {
        return Ok(());
    }
    Err(CliProcessError::Failed {
        subcommand: subcommand(args),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

pub(super) async fn run_cli_text(bin: &Path, args: &[String]) -> Result<String, CliProcessError> {
    let output = Command::new(bin)
        .args(args)
        .output()
        .await
        .map_err(CliProcessError::Spawn)?;
    check_status(&output, args)?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub(super) async fn run_cli_json(bin: &Path, args: &[String]) -> Result<Value, CliProcessError> {
    let stdout = run_cli_text(bin, args).await?;
    serde_json::from_str(stdout.trim()).map_err(|source| CliProcessError::ParseJson {
        subcommand: subcommand(args),
        source,
    })
}

pub(super) async fn run_cli_json_with_env(
    bin: &Path,
    args: &[String],
    environment: BTreeMap<String, String>,
) -> Result<Value, CliProcessError> {
    let output = Command::new(bin)
        .args(args)
        .envs(environment)
        .output()
        .await
        .map_err(CliProcessError::Spawn)?;
    check_status(&output, args)?;
    serde_json::from_slice(&output.stdout).map_err(|source| CliProcessError::ParseJson {
        subcommand: subcommand(args),
        source,
    })
}
