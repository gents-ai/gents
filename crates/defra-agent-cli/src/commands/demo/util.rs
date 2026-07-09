//! Shared plumbing for the demo command: subprocess invocation and small
//! string/path helpers. Terminal prompts live in [`crate::prompt`] and are
//! re-exported here so demo call sites keep using `super::util::…`.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio::process::Command;

// One stdin reader and prompt implementation, shared with `onboard`, so stty
// secret handling and buffered-input behavior never drift between the two.
pub(super) use crate::prompt::{non_empty, prompt, prompt_line, prompt_secret, StdinLines};

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
        .context("running defra-agent subcommand")?;
    if !output.status.success() {
        bail!(
            "defra-agent {} failed: {}",
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
            "parsing JSON from defra-agent {}",
            args.first().cloned().unwrap_or_default()
        )
    })
}
