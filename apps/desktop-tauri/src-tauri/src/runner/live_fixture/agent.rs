use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{
    cli_tool, default_behavior_id_for_agent, default_inference_profile_id_for_behavior,
    default_tool_selection_id_for_behavior, ensure_agent_principal, load_agent_behavior,
    upsert_agent_behavior, AgentIdentity, DefraAgent, DocumentRuntimeOptions, KeyIdentity,
    ToolCeiling,
};
use defra_agent_desktop_core::client::ClientCore;
use defra_agent_protocol::row::{AgentBehaviorRow, InferenceProfileRow, ToolSelectionRow};
use serde_json::Value;
use tokio::sync::watch;
use tracing::Instrument;

use super::backend::AgentBackendConfig;
use super::workspace::seed_repo_workspace;
use super::DEFAULT_DEPLOYMENT_LABEL;

#[derive(Debug, Clone)]
pub(crate) struct LiveAgentDocs {
    pub(crate) behavior_id: String,
    pub(crate) backend_id: String,
    pub(crate) tool_selection_id: String,
    pub(crate) inference_profile_id: String,
}

pub(crate) struct RunningAgent {
    pub(crate) did: String,
    shutdown_tx: watch::Sender<bool>,
    run_task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl RunningAgent {
    pub(crate) async fn shutdown(self) -> Result<()> {
        let _ = self.shutdown_tx.send(true);
        self.run_task.await??;
        Ok(())
    }
}

pub(super) async fn spawn_live_agent(
    node_owner: Arc<ClientCore>,
    key_path: PathBuf,
    name: &str,
    backend: &AgentBackendConfig,
) -> Result<(RunningAgent, LiveAgentDocs, PathBuf)> {
    let tool_root = key_path
        .parent()
        .map(|parent| parent.join("tool-root"))
        .unwrap_or_else(|| std::env::temp_dir().join(format!("defra-agent-tools-{name}")));
    std::fs::create_dir_all(&tool_root)
        .with_context(|| format!("creating live tool root {}", tool_root.display()))?;
    seed_repo_workspace(&tool_root)?;

    let identity = Arc::new(KeyIdentity::load_or_create(key_path, None)?);
    let did = identity.did().to_string();
    let docs = seed_live_behavior_documents(node_owner.as_ref(), &did, name, backend).await?;
    let agent = DefraAgent::from_default_behavior_documents(
        node_owner.node_arc(),
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
        "live_bridge_agent",
        deployment_label = %DEFAULT_DEPLOYMENT_LABEL,
        agent_did = %did
    )));
    wait_for_runtime_process_state(node_owner.node(), &did, "ready").await?;

    Ok((
        RunningAgent {
            did,
            shutdown_tx,
            run_task,
        },
        docs,
        tool_root,
    ))
}

async fn seed_live_behavior_documents(
    core: &ClientCore,
    agent_did: &str,
    agent_name: &str,
    backend: &AgentBackendConfig,
) -> Result<LiveAgentDocs> {
    let behavior_id = default_behavior_id_for_agent(agent_did);
    let backend_id = format!("{agent_name}-backend");
    let tool_selection_id = default_tool_selection_id_for_behavior(&behavior_id);
    let inference_profile_id = default_inference_profile_id_for_behavior(&behavior_id);

    bind_default_behavior_backend(core.node(), agent_did, &backend_id, backend).await?;

    core.save_tool_selection(&ToolSelectionRow {
        selection_id: tool_selection_id.clone(),
        agent_did: Some(agent_did.to_string()),
        display_name: Some("Live Repo Audit Tools".to_string()),
        enable_file_tools: Some(true),
        file_tools_mode: Some("ReadOnly".to_string()),
        file_tool_root: None,
        enable_bash: Some(true),
        bash_mode: Some("ReadOnly".to_string()),
        command_execution_policy: None,
        command_allowed_argv_prefixes: Vec::new(),
        command_forbidden_argv_prefixes: Vec::new(),
        command_network_mode: None,
        cli_tool_names: vec!["rg".to_string()],
        enable_meta_tools: Some(false),
        allowed_mcp_service_ids: Vec::new(),
        delegate_to: vec![],
    })
    .await?;
    core.save_inference_profile(&InferenceProfileRow {
        profile_id: inference_profile_id.clone(),
        display_name: Some("Live Repo Audit Profile".to_string()),
        context_window: Some(131_072),
        max_output_tokens: Some(4_096),
        max_turns: Some(50),
        temperature: Some(0.0),
        stream_batch_ms: Some(250),
        deadline_duration_secs: Some(300),
    })
    .await?;
    core.save_behavior(&AgentBehaviorRow {
        behavior_id: behavior_id.clone(),
        agent_did: Some(agent_did.to_string()),
        display_name: Some("Live Repo Audit Default".to_string()),
        system_prompt: Some(
            "You are Amy, a repository analysis agent operating inside a live desktop integration test."
                .to_string(),
        ),
        backend_id: Some(backend_id.clone()),
        model_name: Some(backend.model_name.clone()),
        tool_selection_id: Some(tool_selection_id.clone()),
        inference_profile_id: Some(inference_profile_id.clone()),
        compaction_strategy: Some("StripThenSummarize".to_string()),
        compaction_threshold: Some(0.95),
        enabled: Some(true),
        created_at: Some(Utc::now().to_rfc3339()),
    })
    .await?;
    core.refresh_store().await?;

    Ok(LiveAgentDocs {
        behavior_id,
        backend_id,
        tool_selection_id,
        inference_profile_id,
    })
}

async fn bind_default_behavior_backend(
    node: &defra_agent::defra_node::EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
    backend: &AgentBackendConfig,
) -> Result<()> {
    let bootstrap = ensure_agent_principal(node, agent_did).await?;
    let escaped_backend_id = escape_graphql_string(backend_id);
    let escaped_endpoint = escape_graphql_string(&backend.endpoint);
    let escaped_provider_kind = escape_graphql_string(backend.provider_kind.as_str());
    let escaped_model_name = escape_graphql_string(&backend.model_name);
    let api_key_field = graphql_optional_string_field("api_key", backend.api_key.as_deref());
    let api_key_env_var_field =
        graphql_optional_string_field("api_key_env_var", backend.api_key_env_var.as_deref());
    let mutation = format!(
        r#"mutation {{
            upsert_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                add: {{
                    backend_id: "{escaped_backend_id}",
                    name: "{escaped_backend_id}",
                    provider_kind: "{escaped_provider_kind}",
                    endpoint: "{escaped_endpoint}",
                    {api_key_field}
                    {api_key_env_var_field}
                    max_concurrent: 2,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{escaped_model_name}"],
                    probe_status: "healthy"
                }},
                update: {{
                    name: "{escaped_backend_id}",
                    provider_kind: "{escaped_provider_kind}",
                    endpoint: "{escaped_endpoint}",
                    {api_key_field}
                    {api_key_env_var_field}
                    max_concurrent: 2,
                    max_queue_depth: 100,
                    enabled: true,
                    models: ["{escaped_model_name}"],
                    probe_status: "healthy"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!("upsert inference backend failed: {:?}", response.errors);
    }

    let mut default_behavior = load_agent_behavior(node, &bootstrap.default_behavior.behavior_id)
        .await?
        .expect("default behavior document");
    default_behavior.backend_id = Some(backend_id.to_string());
    default_behavior.model_name = Some(backend.model_name.clone());
    upsert_agent_behavior(node, &default_behavior).await?;
    Ok(())
}

fn graphql_optional_string_field(name: &str, value: Option<&str>) -> String {
    value
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!(r#"{name}: "{}","#, escape_graphql_string(value)))
        .unwrap_or_default()
}

async fn wait_for_runtime_process_state(
    node: &defra_agent::defra_node::EmbeddedNode,
    agent_did: &str,
    expected_process_state: &str,
) -> Result<()> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
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
