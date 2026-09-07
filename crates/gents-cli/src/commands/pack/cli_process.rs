use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio::process::Command;

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
