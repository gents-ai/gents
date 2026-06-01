use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rig::completion::ToolDefinition;
use rig::tool::{ToolDyn, ToolError};
use rig::wasm_compat::WasmBoxedFuture;
use tokio::process::Command;

use super::args::CliToolArgs;
use super::shared::{truncate_text, ToolError as LocalToolError};
use super::CliToolConfig;

#[derive(Clone)]
pub(super) struct CliTool {
    config: CliToolConfig,
}

impl CliTool {
    pub(super) fn new(config: CliToolConfig) -> Self {
        Self { config }
    }
}

impl ToolDyn for CliTool {
    fn name(&self) -> String {
        self.config.name.clone()
    }

    fn definition(&self, _prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
        let config = self.config.clone();
        Box::pin(async move {
            ToolDefinition {
                name: config.name,
                description: config.description,
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "argv": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    }
                }),
            }
        })
    }

    fn call(&self, args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
        let config = self.config.clone();
        Box::pin(async move {
            let args: CliToolArgs = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            validate_argv_policy(&config, &args.argv)
                .map_err(|error| ToolError::ToolCallError(Box::new(LocalToolError::from(error))))?;
            let output = run_cli_command(&config, &args.argv)
                .await
                .map_err(|error| ToolError::ToolCallError(Box::new(LocalToolError::from(error))))?;
            serde_json::to_string(&output).map_err(ToolError::JsonError)
        })
    }
}

fn validate_argv_policy(config: &CliToolConfig, argv: &[String]) -> Result<()> {
    if config.allowed_argv_prefixes.is_empty() {
        return Ok(());
    }

    let matches = config.allowed_argv_prefixes.iter().any(|prefix| {
        argv.len() >= prefix.len()
            && argv
                .iter()
                .zip(prefix.iter())
                .all(|(left, right)| left == right)
    });
    if matches {
        return Ok(());
    }

    bail!(
        "argv for tool '{}' does not match any approved prefix policy",
        config.name
    )
}

async fn run_cli_command(config: &CliToolConfig, argv: &[String]) -> Result<String> {
    let mut command = Command::new(&config.binary_path);
    command
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("NO_COLOR", "1")
        .env("CLICOLOR", "0")
        .env("TERM", "dumb")
        .kill_on_drop(true);

    for (key, value) in &config.env_vars {
        command.env(key, value);
    }

    let cwd = match config.working_dir.as_ref() {
        Some(path) => {
            if !path.is_dir() {
                bail!(
                    "working directory for tool '{}' is not a directory: {}",
                    config.name,
                    path.display()
                );
            }
            path.clone()
        }
        None => std::env::current_dir().context("determining current working directory")?,
    };
    command.current_dir(&cwd);

    let output = tokio::time::timeout(
        Duration::from_secs(config.timeout_secs.max(1)),
        command.output(),
    )
    .await
    .with_context(|| format!("timed out after {}s", config.timeout_secs.max(1)))??;

    let stdout = truncate_text(
        &String::from_utf8_lossy(&output.stdout),
        super::DEFAULT_MAX_COMMAND_CHARS,
    );
    let stderr = truncate_text(
        &String::from_utf8_lossy(&output.stderr),
        super::DEFAULT_MAX_COMMAND_CHARS,
    );
    let exit_code = output.status.code().unwrap_or(-1);
    let command_line = std::iter::once(config.binary_path.display().to_string())
        .chain(argv.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");

    Ok(format!(
        "cwd: {}\ncommand: {}\nexit_code: {}\nstdout:\n{}\nstderr:\n{}",
        cwd.display(),
        command_line,
        exit_code,
        if stdout.is_empty() {
            "(empty)"
        } else {
            &stdout
        },
        if stderr.is_empty() {
            "(empty)"
        } else {
            &stderr
        },
    ))
}
