use std::collections::HashMap;

use crate::llm::tool::BoxFuture;
use crate::llm::tool::ToolDefinition;
use crate::llm::tool::{ToolDyn, ToolError};
use crate::managed_exec::{run_managed_exec, ManagedExecOutcome, ManagedExecRequest};
use crate::tool_call_lifecycle::runtime::current_tool_runtime_context;
use anyhow::{bail, Context, Result};

use super::args::CliToolArgs;
use super::shared::{cap_output, ToolError as LocalToolError};
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

    fn definition(&self, _prompt: String) -> BoxFuture<'_, ToolDefinition> {
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

    fn call(&self, args: String) -> BoxFuture<'_, Result<String, ToolError>> {
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

fn cli_tool_environment(config: &CliToolConfig) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    env.insert("PAGER".to_string(), "cat".to_string());
    env.insert("GIT_PAGER".to_string(), "cat".to_string());
    env.insert("NO_COLOR".to_string(), "1".to_string());
    env.insert("CLICOLOR".to_string(), "0".to_string());
    env.insert("TERM".to_string(), "dumb".to_string());
    for (key, value) in &config.env_vars {
        env.insert(key.clone(), value.clone());
    }
    env
}

async fn run_cli_command(config: &CliToolConfig, argv: &[String]) -> Result<String> {
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

    let timeout_secs = config.timeout_secs.max(1);
    let runtime = current_tool_runtime_context();
    let request_deadline = runtime.as_ref().and_then(|runtime| runtime.deadline_at);
    let command_deadline = chrono::Utc::now()
        + chrono::Duration::from_std(std::time::Duration::from_secs(timeout_secs))
            .unwrap_or_else(|_| chrono::Duration::days(36_500));
    let deadline_at =
        Some(request_deadline.map_or(command_deadline, |deadline| deadline.min(command_deadline)));
    let cancellation_token = runtime
        .as_ref()
        .map(|runtime| runtime.cancellation_token.clone())
        .unwrap_or_default();
    let live_output = runtime.and_then(|runtime| runtime.live_output);

    let outcome = run_managed_exec(ManagedExecRequest {
        argv: std::iter::once(config.binary_path.display().to_string())
            .chain(argv.iter().cloned())
            .collect::<Vec<_>>(),
        cwd: cwd.clone(),
        deadline_at,
        cancellation_token,
        max_output_bytes: usize::MAX,
        stdin: Vec::new(),
        environment: Some(cli_tool_environment(config)),
        tool_name: Some(config.name.clone()),
        live_output,
    })
    .await;

    let (exit_code, stdout_bytes, stderr_bytes) = match outcome {
        ManagedExecOutcome::Exited {
            code,
            stdout,
            stderr,
            ..
        } => (code.unwrap_or(-1), stdout, stderr),
        ManagedExecOutcome::TimedOut { .. } => {
            bail!("timed out after {timeout_secs}s")
        }
        ManagedExecOutcome::Cancelled { .. } => {
            bail!("command cancelled by the owning request")
        }
        ManagedExecOutcome::SpawnFailed { error } => bail!(error),
    };

    let (stdout, _) = cap_output(
        &String::from_utf8_lossy(&stdout_bytes),
        super::DEFAULT_MAX_COMMAND_CHARS,
    );
    let (stderr, _) = cap_output(
        &String::from_utf8_lossy(&stderr_bytes),
        super::DEFAULT_MAX_COMMAND_CHARS,
    );
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(timeout_secs: u64) -> CliToolConfig {
        CliToolConfig {
            name: "sh".into(),
            binary_path: "/bin/sh".into(),
            description: String::new(),
            allowed_argv_prefixes: vec![],
            env_vars: HashMap::from([("GENTS_T".to_string(), "1".to_string())]),
            working_dir: None,
            timeout_secs,
        }
    }

    #[tokio::test]
    async fn reports_exit_code_and_output_and_env() {
        let out = run_cli_command(
            &config(5),
            &["-c".into(), "echo $GENTS_T; echo err 1>&2; exit 3".into()],
        )
        .await
        .unwrap();
        assert!(out.contains("exit_code: 3"), "{out}");
        assert!(out.contains("stdout:\n1"), "{out}");
        assert!(out.contains("stderr:\nerr"), "{out}");
    }

    #[tokio::test]
    async fn tool_timeout_kills_the_process_group() {
        let err = run_cli_command(&config(1), &["-c".into(), "sleep 30".into()])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out after 1s"), "{err}");
    }

    #[tokio::test]
    async fn request_cancellation_stops_the_command() {
        use crate::tool_call_lifecycle::runtime::scope_request_tool_execution;

        let token = tokio_util::sync::CancellationToken::new();
        let cancel = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            cancel.cancel();
        });
        let err = scope_request_tool_execution(
            None,
            token,
            run_cli_command(&config(30), &["-c".into(), "sleep 30".into()]),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("cancelled"), "{err}");
    }
}
