use super::backend::{bind_default_behavior_backend, AgentBackendConfig};
use super::*;
use defra_agent::cli_tool;
use tracing::Instrument;

pub(crate) struct RunningAgent {
    pub(crate) did: String,
    pub(crate) tool_token: String,
    pub(crate) tool_root: std::path::PathBuf,
    shutdown_tx: watch::Sender<bool>,
    run_task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl RunningAgent {
    pub(crate) fn write_tool_file(&self, relative_path: &str, contents: &str) -> Result<()> {
        let path = self.tool_root.join(relative_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating live tool directory {}", parent.display()))?;
        }
        std::fs::write(&path, format!("{contents}\n"))
            .with_context(|| format!("writing live tool fixture {}", path.display()))?;
        Ok(())
    }

    pub(crate) async fn shutdown(self) -> Result<()> {
        let _ = self.shutdown_tx.send(true);
        self.run_task.await??;
        Ok(())
    }
}

pub(crate) async fn spawn_backed_agent(
    node: Arc<EmbeddedNode>,
    key_path: impl Into<std::path::PathBuf>,
    name: &str,
    backend: &AgentBackendConfig,
) -> Result<RunningAgent> {
    let key_path = key_path.into();
    let tool_root = key_path
        .parent()
        .map(|parent| parent.join("tool-root"))
        .unwrap_or_else(|| std::env::temp_dir().join(format!("defra-agent-tools-{name}")));
    std::fs::create_dir_all(&tool_root)
        .with_context(|| format!("creating live tool root {}", tool_root.display()))?;
    let tool_token = format!("DESKTOP_TOOL_TOKEN_{}", uuid::Uuid::new_v4().simple());
    std::fs::write(tool_root.join("notes.txt"), format!("{tool_token}\n")).with_context(|| {
        format!(
            "writing live tool fixture {}",
            tool_root.join("notes.txt").display()
        )
    })?;
    seed_repo_workspace(&tool_root)?;

    let identity = Arc::new(SimpleIdentity::new(name, key_path, None));
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        &format!("{name}-backend"),
        backend,
    )
    .await?;
    let did = identity.did().to_string();
    let agent = DefraAgent::from_default_behavior_documents(
        Arc::clone(&node),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readwrite(tool_root.clone()).with_cli_tool(cli_tool(
                "rg",
                "rg",
                "Search files with ripgrep",
            )),
            ..Default::default()
        },
    )
    .await?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run_task = tokio::spawn(agent.run(shutdown_rx).instrument(tracing::info_span!(
        "live_remote_agent",
        deployment_label = %name,
        agent_did = %did
    )));
    wait_for_runtime_process_state(node.as_ref(), &did, "ready").await?;
    Ok(RunningAgent {
        did,
        tool_token,
        tool_root,
        shutdown_tx,
        run_task,
    })
}

fn seed_repo_workspace(tool_root: &std::path::Path) -> Result<()> {
    let repo_root = workspace_repo_root()?;
    let workspace_root = tool_root.join("workspace");
    std::fs::create_dir_all(&workspace_root)
        .with_context(|| format!("creating seeded workspace {}", workspace_root.display()))?;

    for relative in [
        "Cargo.toml",
        "README.md",
        "docs",
        "crates/defra-agent",
        "crates/defra-agent-cli",
        "crates/defra-agent-desktop",
        "crates/defra-node",
        "crates/defra-agent-protocol",
    ] {
        let src = repo_root.join(relative);
        let dst = workspace_root.join(relative);
        if src.is_dir() {
            copy_dir_all(&src, &dst)?;
        } else if src.is_file() {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::copy(&src, &dst)
                .with_context(|| format!("copying {} -> {}", src.display(), dst.display()))?;
        }
    }

    let guide = "\
This workspace is a seeded copy of key defra-agent repository files for live desktop soak tests.
Start your exploration in ./workspace.
Useful directories:
- ./workspace/crates/defra-agent-desktop
- ./workspace/crates/defra-agent-cli
- ./workspace/crates/defra-agent-protocol
- ./workspace/crates/defra-agent
- ./workspace/crates/defra-node
";
    std::fs::write(tool_root.join("workspace-guide.txt"), guide).with_context(|| {
        format!(
            "writing {}",
            tool_root.join("workspace-guide.txt").display()
        )
    })?;
    Ok(())
}

fn workspace_repo_root() -> Result<std::path::PathBuf> {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .nth(2)
        .map(std::path::Path::to_path_buf)
        .ok_or_else(|| anyhow!("failed to locate repo root from {}", manifest_dir.display()))
}

fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &target).with_context(|| {
                format!("copying {} -> {}", entry.path().display(), target.display())
            })?;
        }
    }
    Ok(())
}

pub(crate) async fn wait_for_runtime_process_state(
    node: &EmbeddedNode,
    agent_did: &str,
    expected_process_state: &str,
) -> Result<()> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let query = format!(
            r#"{{
                AgentRuntime(
                    filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                    limit: 1
                ) {{
                    process_state
                }}
            }}"#
        );
        let response = node.execute(&query).await;
        if response.has_errors() {
            anyhow::bail!("AgentRuntime query failed: {:?}", response.errors);
        }
        let process_state = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRuntime"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("process_state"))
            .and_then(Value::as_str);
        if process_state == Some(expected_process_state) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for AgentRuntime {agent_did} to reach process_state={expected_process_state}; last={process_state:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
