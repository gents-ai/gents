use super::backend::{bind_default_behavior_backend, AgentBackendConfig};
use super::*;

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
            tool_ceiling: ToolCeiling::readwrite(tool_root.clone()),
            ..Default::default()
        },
    )
    .await?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run_task = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_process_state(node.as_ref(), &did, "ready").await?;
    Ok(RunningAgent {
        did,
        tool_token,
        tool_root,
        shutdown_tx,
        run_task,
    })
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
