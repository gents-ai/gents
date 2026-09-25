use std::collections::HashMap;

use crate::llm::tool::BoxFuture;
use crate::llm::tool::ToolDefinition;
use crate::llm::tool::{ToolDyn, ToolError};
use crate::managed_exec::{run_managed_exec, ManagedExecOutcome, ManagedExecRequest};
use crate::tool_call_lifecycle::runtime::tool_execution_bounds;
use anyhow::{bail, Context, Result};

use super::args::CliToolArgs;
use super::shared::{
    captured_channel, stopped_stream, truncate_stream, ToolError as LocalToolError,
};
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
            deny_artifact_scope().map_err(LocalToolError::into_dispatch_error)?;
            let args: CliToolArgs = serde_json::from_str(&args).map_err(ToolError::JsonError)?;
            validate_argv_policy(&config, &args.argv)
                .map_err(|error| ToolError::ToolCallError(Box::new(LocalToolError::from(error))))?;
            let output = run_cli_command(&config, &args.argv)
                .await
                .map_err(LocalToolError::into_dispatch_error)?;
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

fn deny_artifact_scope() -> Result<(), LocalToolError> {
    if crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
        .is_some_and(|scope| scope.workspace_artifact.is_some())
    {
        return Err(LocalToolError::reported_failure(
            crate::tool_call_lifecycle::FailureClass::PolicyDenied,
            "direct CLI subprocesses are unavailable in artifact-scoped requests; use the sandboxed Bash path".into(),
        ));
    }
    Ok(())
}

async fn run_cli_command(
    config: &CliToolConfig,
    argv: &[String],
) -> Result<String, LocalToolError> {
    deny_artifact_scope()?;
    let cwd = match config.working_dir.as_ref() {
        Some(path) => {
            if !path.is_dir() {
                return Err(anyhow::anyhow!(
                    "working directory for tool '{}' is not a directory: {}",
                    config.name,
                    path.display()
                )
                .into());
            }
            path.clone()
        }
        None => std::env::current_dir().context("determining current working directory")?,
    };

    let timeout_secs = config.timeout_secs.max(1);
    let bounds = tool_execution_bounds(std::time::Duration::from_secs(timeout_secs));

    let outcome = run_managed_exec(ManagedExecRequest {
        argv: std::iter::once(config.binary_path.display().to_string())
            .chain(argv.iter().cloned())
            .collect::<Vec<_>>(),
        cwd: cwd.clone(),
        deadline_at: bounds.deadline_at,
        cancellation_token: bounds.cancellation_token,
        max_output_bytes: usize::MAX,
        stdin: Vec::new(),
        environment: Some(cli_tool_environment(config)),
        tool_name: Some(config.name.clone()),
        // Committed as captured, so a request deadline that stops this call
        // presents the output so far instead of only its cause.
        live_output: bounds.live_output.clone(),
    })
    .await;

    render_cli_outcome(config, &cwd, argv, outcome, bounds.live_output.as_ref()).await
}

async fn render_cli_outcome(
    config: &CliToolConfig,
    cwd: &std::path::Path,
    argv: &[String],
    outcome: ManagedExecOutcome,
    live_output: Option<&gents_loop::live_output::LiveToolOutputWriter>,
) -> Result<String, LocalToolError> {
    let timeout_secs = config.timeout_secs.max(1);
    let (exit_code, stdout_bytes, stderr_bytes, terminal_cause) = match outcome {
        ManagedExecOutcome::Exited {
            code,
            stdout,
            stderr,
            ..
        } => (Some(code.unwrap_or(-1)), stdout, stderr, None),
        ManagedExecOutcome::TimedOut { stdout, stderr, .. } => (
            None,
            stdout,
            stderr,
            Some(format!("timed out after {timeout_secs}s")),
        ),
        ManagedExecOutcome::Cancelled { stdout, stderr, .. } => (
            None,
            stdout,
            stderr,
            Some("command cancelled by the owning request".to_owned()),
        ),
        ManagedExecOutcome::SpawnFailed { error } => return Err(anyhow::anyhow!(error).into()),
    };

    let stdout_raw = String::from_utf8_lossy(&stdout_bytes);
    let stderr_raw = String::from_utf8_lossy(&stderr_bytes);
    let channel = if terminal_cause.is_some() {
        stopped_stream
    } else {
        truncate_stream
    };
    let stdout = channel(&stdout_raw, config.max_output_chars);
    let stderr = channel(&stderr_raw, config.max_output_chars);
    let command_line = std::iter::once(config.binary_path.display().to_string())
        .chain(argv.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");

    let mut head = match &terminal_cause {
        Some(cause) => format!("{cause}\n"),
        None => String::new(),
    };
    head.push_str(&format!(
        "cwd: {}\ncommand: {}\nexit_code: {}\n",
        cwd.display(),
        command_line,
        exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "unavailable".to_owned()),
    ));
    let rendered = format!(
        "{head}stdout:\n{}\nstderr:\n{}",
        if stdout.content.is_empty() {
            "(empty)"
        } else {
            &stdout.content
        },
        if stderr.content.is_empty() {
            "(empty)"
        } else {
            &stderr.content
        },
    );
    // A completed call's text is the JSON string `call` returns; a stopped
    // call's is the reported failure text itself.
    let expected = match terminal_cause {
        Some(_) => rendered.clone(),
        None => serde_json::to_string(&rendered).context("encoding CLI tool output")?,
    };
    if let Some(writer) = live_output {
        writer
            .prepare_command_presentation(
                &expected,
                gents_loop::live_output::CommandPresentationLayout::Labeled {
                    head: &head,
                    json_string: terminal_cause.is_none(),
                },
                captured_channel(&stdout_raw, &stdout),
                captured_channel(&stderr_raw, &stderr),
            )
            .await?;
    }
    if let Some(cause) = terminal_cause {
        // Captured command text must not reclassify the terminal cause.
        return Err(LocalToolError::reported_failure(
            gents_loop::tool_call_lifecycle::runtime::classify_error_text(&cause),
            rendered,
        ));
    }
    Ok(rendered)
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
            max_output_chars: super::super::DEFAULT_MAX_COMMAND_CHARS,
        }
    }

    #[tokio::test]
    async fn artifact_scope_denies_cli_tool_before_direct_launch() {
        let fixture = crate::workspace::artifact_test_fixture(&[]).await;
        let source = fixture.grant.source_root().to_path_buf();
        let sentinel = source.join("cli-must-not-launch");
        let mut config = config(5);
        config.working_dir = Some(source.clone());
        let tool = CliTool::new(config.clone());
        crate::tool_call_lifecycle::runtime::scope_request_tool_execution_with_workspace_overlay(
            None,
            tokio_util::sync::CancellationToken::new(),
            crate::tool_call_lifecycle::runtime::ToolWorkspaceScope {
                workspace_cwd: Some(source.clone()),
                workspace_root: Some(source),
                workspace_authority: Some(crate::toolset::WorkspaceAuthority::ReadOnly),
                workspace_artifact: Some(fixture.grant.clone()),
            },
            None,
            None,
            None,
            Default::default(),
            false,
            async {
                let argv = vec!["-c".to_owned(), "touch cli-must-not-launch".to_owned()];
                let error = run_cli_command(&config, &argv).await.unwrap_err();
                assert!(error.to_string().contains("artifact-scoped"));
                let args = serde_json::json!({"argv": argv}).to_string();
                let outcome = crate::tool_call_lifecycle::ToolOutcome::from_dispatch(
                    "sh",
                    tool.call(args).await,
                );
                assert!(matches!(
                    outcome,
                    crate::tool_call_lifecycle::ToolOutcome::Failed {
                        class: crate::tool_call_lifecycle::FailureClass::PolicyDenied,
                        ..
                    }
                ));
            },
        )
        .await;
        assert!(!sentinel.exists());
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
    async fn configured_output_cap_bounds_each_channel() {
        let mut config = config(5);
        config.max_output_chars = 7;
        let out = run_cli_command(
            &config,
            &[
                "-c".into(),
                "printf '%20s' out; printf '%20s' err >&2".into(),
            ],
        )
        .await
        .unwrap();
        assert!(out.contains(&format!("stdout:\n{:7}\n", "")), "{out}");
        assert!(out.contains(&format!("stderr:\n{:7}\n", "")), "{out}");
        assert!(out.contains("first 7 of 20 bytes"), "{out}");
    }

    #[tokio::test]
    async fn tool_timeout_kills_the_process_group() {
        let err = run_cli_command(
            &config(1),
            &[
                "-c".into(),
                "printf '%17000s' ' '; printf 'timeout unavailable transport'; printf 'last stderr' >&2; sleep 30".into(),
            ],
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("timed out after 1s"), "{err}");
        let diagnostic = err.to_string();
        let stdout = diagnostic
            .split_once("stdout:\n")
            .unwrap()
            .1
            .split_once("\nstderr:\n")
            .unwrap()
            .0;
        let tail = stdout
            .strip_prefix("[Showing last 16000 of 17029 bytes]\n\n")
            .unwrap_or_else(|| panic!("stopped output keeps a marked tail: {err}"));
        assert!(tail.ends_with("timeout unavailable transport"), "{err}");
        assert_eq!(tail.len(), super::super::DEFAULT_MAX_COMMAND_CHARS);
        assert!(err.to_string().contains("stderr:\nlast stderr"), "{err}");
        assert!(err.to_string().contains("exit_code: unavailable"), "{err}");
        assert!(matches!(
            crate::tool_call_lifecycle::ToolOutcome::from_dispatch(
                "cli",
                Err(err.into_dispatch_error())
            ),
            crate::tool_call_lifecycle::ToolOutcome::Failed {
                class,
                ..
            } if class == gents_loop::tool_call_lifecycle::runtime::classify_error_text("timed out after 1s")
        ));
    }

    #[tokio::test]
    async fn request_cancellation_stops_the_command() {
        use crate::tool_call_lifecycle::runtime::scope_request_tool_execution;

        let token = tokio_util::sync::CancellationToken::new();
        let cancel = token.clone();
        let temp = tempfile::tempdir().unwrap();
        let ready = temp.path().join("output-written");
        let mut config = config(30);
        config
            .env_vars
            .insert("GENTS_READY".into(), ready.display().to_string());
        let cancellation = tokio::spawn(async move {
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                while !ready.exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("command must write output before cancellation");
            cancel.cancel();
        });
        let err = scope_request_tool_execution(
            None,
            token,
            run_cli_command(&config, &["-c".into(),
                "printf 'partial stdout'; printf 'partial stderr' >&2; : > \"$GENTS_READY\"; sleep 30".into()]),
        )
        .await
        .unwrap_err();
        cancellation.await.unwrap();
        assert!(err.to_string().contains("cancelled"), "{err}");
        assert!(err.to_string().contains("stdout:\npartial stdout"), "{err}");
        assert!(err.to_string().contains("stderr:\npartial stderr"), "{err}");
    }

    async fn committed_run(
        name: &str,
    ) -> (
        std::sync::Arc<defra_node::EmbeddedNode>,
        crate::tool_call_lifecycle::ToolCallLifecycle,
        gents_loop::live_output::LiveToolOutputRegistry,
        gents_loop::live_output::LiveToolOutputWriter,
    ) {
        let (node, _path, tool) =
            crate::tool_call_lifecycle::admission_fixture::published_spawn_parent(name).await;
        let binding = tool
            .tool_output_binding()
            .expect("running tool output binding");
        let registry = gents_loop::live_output::LiveToolOutputRegistry::default();
        let writer = registry
            .canonical_writer_for(
                tool.doc_id().unwrap().to_owned(),
                std::sync::Arc::new(binding),
            )
            .await;
        (node, tool, registry, writer)
    }

    async fn presented(
        node: &std::sync::Arc<defra_node::EmbeddedNode>,
        tool: &crate::tool_call_lifecycle::ToolCallLifecycle,
    ) -> String {
        crate::tool_call_lifecycle::load_tool_call_presentation(
            &crate::config_client::ConfigAccess::Local(node.clone()),
            tool.doc_id().unwrap(),
            tool.agent_did(),
            tool.session_id(),
            tool.requester_did(),
        )
        .await
        .unwrap()
        .result
        .expect("terminal delivery presentation")
    }

    async fn committed_output(
        node: &std::sync::Arc<defra_node::EmbeddedNode>,
        tool: &crate::tool_call_lifecycle::ToolCallLifecycle,
    ) -> String {
        crate::background_tools::canonical_tool_output(
            node.as_ref(),
            tool.doc_id().unwrap(),
            tool.request_doc_id().unwrap(),
            tool.session_id(),
            tool.agent_did(),
            tool.requester_did(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn request_deadline_presents_output_committed_before_the_cut() {
        let (node, mut tool, _registry, writer) = committed_run("cli-request-deadline").await;
        let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(CliTool::new(config(30)))];
        let args = serde_json::json!({"argv": ["-c",
            "printf 'partial stdout'; printf 'partial stderr' >&2; sleep 30"]})
        .to_string();
        let started = std::time::Instant::now();
        let outcome =
            gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_session(
                Some(chrono::Utc::now() + chrono::Duration::seconds(2)),
                tokio_util::sync::CancellationToken::new(),
                None,
                None,
                None,
                gents_loop::loop_stream::dispatch_tool(&tools, "sh", args, Some(writer), None),
            )
            .await;
        assert!(
            matches!(
                outcome,
                crate::tool_call_lifecycle::ToolOutcome::TimedOut { .. }
            ),
            "{outcome:?}"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(20));

        let raw = committed_output(&node, &tool).await;
        assert!(
            raw.contains("partial stdout") && raw.contains("partial stderr"),
            "captured output must be committed before the deadline: {raw:?}"
        );
        assert!(tool.timeout().await.unwrap());
        let presented = presented(&node, &tool).await;
        assert!(presented.starts_with(&raw), "{presented:?}");
        assert!(
            presented.contains("\ntool call deadline exceeded at "),
            "{presented:?}"
        );
    }

    #[tokio::test]
    async fn committed_output_reconstructs_completed_and_stopped_results() {
        use crate::tool_call_lifecycle::ToolOutcome;

        for (name, timeout_secs, script) in [
            (
                "cli-completed",
                30,
                "printf 'out \"q\"\\n'; printf 'err\\t' >&2; exit 4",
            ),
            (
                // Output ending mid-sequence, and an invalid byte beside a
                // complete scalar.
                "cli-invalid-utf8",
                30,
                "printf 'ok\\303'; printf '\\377\\303\\251' >&2; exit 4",
            ),
            (
                // CRLF lines past the head budget: the head cut must stay a
                // byte prefix of the committed output.
                "cli-crlf-head",
                30,
                "i=0; while [ $i -lt 3000 ]; do printf 'line %d\\r\\n' $i; i=$((i+1)); done; exit 4",
            ),
            (
                "cli-stopped",
                1,
                "printf '%17000s' ' '; printf 'last out'; printf 'last err' >&2; sleep 30",
            ),
        ] {
            let (node, mut tool, registry, writer) = committed_run(name).await;
            let cli = CliTool::new(config(timeout_secs));
            let args = serde_json::json!({"argv": ["-c", script]}).to_string();
            let outcome = ToolOutcome::from_dispatch(
                "sh",
                gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_session(
                    None,
                    tokio_util::sync::CancellationToken::new(),
                    None,
                    Some(writer),
                    None,
                    cli.call(args),
                )
                .await,
            );
            let text = outcome.model_facing_text().to_owned();
            let presentation = registry
                .take_prepared_presentation(tool.doc_id().unwrap(), &text)
                .await
                .unwrap()
                .expect("CLI tool prepares a presentation over committed output");
            match outcome {
                ToolOutcome::Completed(_) => {
                    assert!(text.contains("exit_code: 4"), "{text}");
                    if name == "cli-crlf-head" {
                        // JSON-encoded: the shown prefix keeps its final `\r`.
                        assert!(text.contains(r"\r\n\n[Showing lines 1-"), "CRLF head kept");
                    }
                    tool.complete_with_presentation(&text, Some(presentation))
                        .await
                        .unwrap();
                }
                ToolOutcome::Failed { class, .. } => {
                    assert!(text.starts_with("timed out after 1s\n"), "{text}");
                    assert!(
                        text.contains("[Showing last 16000 of 17008 bytes]"),
                        "{text}"
                    );
                    tool.fail_with_presentation(&text, class, Some(presentation))
                        .await
                        .unwrap();
                }
                other => panic!("{name}: unexpected outcome {other:?}"),
            }
            assert_eq!(presented(&node, &tool).await, text, "{name}");
        }
    }
}
