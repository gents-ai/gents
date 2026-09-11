use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use gents::graphql::escape_graphql_string;
use gents::{
    cli_tool, default_behavior_id_for_agent, default_inference_profile_id_for_behavior,
    AgentIdentity, DocumentRuntimeOptions, Gents, InferenceBackend, KeyIdentity, ToolCeiling,
};
use gents_desktop_core::client::ClientCore;
use gents_protocol::row::{decode_behavior_readiness_snapshot, AgentBehaviorReadinessRow};
use serde_json::Value;
use tokio::sync::watch;
use tracing::Instrument;

use super::backend::AgentBackendConfig;
use super::workspace::seed_repo_workspace;
use super::DEFAULT_DEPLOYMENT_LABEL;

#[derive(Debug, Clone)]
pub(crate) struct LiveAgentDocs {
    pub(crate) behavior_id: String,
    pub(crate) subagent_behavior_id: String,
    pub(crate) backend_id: String,
    pub(crate) subagent_backend_id: String,
    pub(crate) tools_id: String,
    pub(crate) subagent_tools_id: String,
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
    subagent_backend: Option<&AgentBackendConfig>,
) -> Result<(RunningAgent, LiveAgentDocs, PathBuf)> {
    let tool_root = key_path
        .parent()
        .map(|parent| parent.join("tool-root"))
        .unwrap_or_else(|| std::env::temp_dir().join(format!("gents-tools-{name}")));
    std::fs::create_dir_all(&tool_root)
        .with_context(|| format!("creating live tool root {}", tool_root.display()))?;
    seed_repo_workspace(&tool_root)?;

    let identity = Arc::new(KeyIdentity::load_or_create(key_path, None)?);
    let did = identity.did().to_string();
    let docs =
        seed_live_behavior_documents(node_owner.as_ref(), &did, name, backend, subagent_backend)
            .await?;
    let agent = Gents::from_default_behavior_documents(
        node_owner.node_arc(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readwrite(tool_root.clone())
                .with_command_timeout_secs(30)
                .with_cli_tool(cli_tool("rg", "rg", "Search files with ripgrep")),
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
    subagent_backend: Option<&AgentBackendConfig>,
) -> Result<LiveAgentDocs> {
    use gents::config_client::{
        apply_desired_state_plan, load_inference_backend_in_txn, ConfigAccess,
        DesiredStateApplyPlan,
    };
    use gents::document_config::PackConfig;
    use serde_json::json;
    let behavior_id = default_behavior_id_for_agent(agent_did);
    let subagent_behavior_id = format!("{agent_did}:live-repo-audit-subagent");
    let backend_id = format!("{agent_name}-backend");
    let subagent_backend_id = if subagent_backend.is_some() {
        format!("{agent_name}-subagent-backend")
    } else {
        backend_id.clone()
    };
    let tools_id = format!("{behavior_id}-tools");
    let subagent_tools_id = format!("{subagent_behavior_id}-tools");
    let inference_profile_id = default_inference_profile_id_for_behavior(&behavior_id);
    let subagent_profile_id = default_inference_profile_id_for_behavior(&subagent_behavior_id);
    let context_id = format!("{behavior_id}-context");
    let subagent_context_id = format!("{subagent_behavior_id}-context");
    let execution_id = format!("{behavior_id}-execution");
    let sampling_id = format!("{behavior_id}-sampling");
    let compaction_id = format!("{behavior_id}-compaction");
    let target_id = format!("{behavior_id}-subagent-target");
    let subagent = subagent_backend.unwrap_or(backend);
    let config = json!({
        "agent_principal":{"agent_did":agent_did,"display_name":agent_name,"default_behavior_id":behavior_id},
        "agent_behaviors":[
            {"agent_did":agent_did,"behavior_id":behavior_id,"display_name":"Live Repo Audit Default","context_id":context_id,"inference_profile_id":inference_profile_id},
            {"agent_did":agent_did,"behavior_id":subagent_behavior_id,"display_name":"Live Repo Audit Subagent","context_id":subagent_context_id,"inference_profile_id":subagent_profile_id}
        ],
        "contexts":[
            {"agent_did":agent_did,"context_id":context_id,"system_prompt":"You are Amy, a repository analysis agent operating inside a live desktop integration test. Keep answers concise. Use only the exact files requested by the user, and do not explore the wider repository unless explicitly asked. When the user explicitly asks you to use the local subagent, call spawn_subagent with name \"repo-audit-subagent\" and await_mode \"background\", then call wait_subagent with the returned child_request_id to retrieve the child's result before you reply to the user. When the user explicitly asks you to launch a native background process, call spawn_process with tool_name \"bash_unrestricted\" and the exact requested arguments. Do not call wait_process, read_process, list_processes, or cancel_process unless the user explicitly asks.","tools_id":tools_id,"compaction_id":compaction_id},
            {"agent_did":agent_did,"context_id":subagent_context_id,"system_prompt":"You are Amy's local repo audit subagent inside a live desktop integration test. Read only the exact files requested by the parent and return concise findings.","tools_id":subagent_tools_id,"compaction_id":compaction_id}
        ],
        "tools":[
            {"agent_did":agent_did,"tools_id":tools_id,"display_name":"Live Repo Audit Tools",
             "host":{"files":{"mode":"ReadOnly"},"bash":{"mode":"Unrestricted","background_enabled":true}},
             "built_ins":{"enable_context_budget":true},
             "subagents":{"target_ids":[target_id],"spawn_enabled":true,"steering_enabled":true,"background_enabled":true,"allow_cross_principal":false,"cross_principal_spawn_timeout_secs":60}},
            {"agent_did":agent_did,"tools_id":subagent_tools_id,"display_name":"Live Repo Audit Subagent Tools",
             "host":{"files":{"mode":"ReadOnly"}},"built_ins":{"enable_context_budget":true}}
        ],
        "subagent_targets":[{"agent_did":agent_did,"target_id":target_id,"target_agent_did":agent_did,"behavior_id":subagent_behavior_id,"name":"repo-audit-subagent","description":"Local repository audit subagent for the desktop live fixture"}],
        "inference_profiles":[
            {"agent_did":agent_did,"profile_id":inference_profile_id,"display_name":"Live Repo Audit Profile","backend_id":backend_id,"model_name":backend.model_name,"context_window":131072,"max_output_tokens":1024,"sampling_id":sampling_id,"execution_id":execution_id},
            {"agent_did":agent_did,"profile_id":subagent_profile_id,"display_name":"Live Repo Audit Subagent Profile","backend_id":subagent_backend_id,"model_name":subagent.model_name,"context_window":131072,"max_output_tokens":1024,"sampling_id":sampling_id,"execution_id":execution_id}
        ],
        "inference_sampling":[{"agent_did":agent_did,"sampling_id":sampling_id,"temperature":0.0}],
        "inference_execution":[{"agent_did":agent_did,"execution_id":execution_id,"max_turns":20,"stream_batch_ms":250,"stream_liveness_timeout_secs":60,"deadline_duration_secs":300}],
        "compactions":[{"agent_did":agent_did,"compaction_id":compaction_id,"strategy":"StripThenSummarize","threshold":0.95}]
    });
    ConfigAccess::transact_local(core.node(), None, "desktop.fixture.config", |txn| {
        let mut config = config.clone();
        let backend_id = &backend_id;
        let subagent_backend_id = &subagent_backend_id;
        Box::pin(async move {
            let existing = load_inference_backend_in_txn(txn, agent_did, backend_id).await?;
            let mut backends = vec![live_backend_candidate(
                existing, agent_did, backend_id, backend,
            )?];
            if subagent_backend.is_some() {
                let existing =
                    load_inference_backend_in_txn(txn, agent_did, subagent_backend_id).await?;
                backends.push(live_backend_candidate(
                    existing,
                    agent_did,
                    subagent_backend_id,
                    subagent,
                )?);
            }
            config["inference_backends"] = serde_json::to_value(backends)?;
            let config: PackConfig = serde_json::from_value(config)?;
            let plan = DesiredStateApplyPlan::from_pack_config(&config)?;
            apply_desired_state_plan(txn, &plan).await?;
            Ok(())
        })
    })
    .await?;
    core.refresh_store().await?;
    Ok(LiveAgentDocs {
        behavior_id,
        subagent_behavior_id,
        backend_id,
        subagent_backend_id,
        tools_id,
        subagent_tools_id,
        inference_profile_id,
    })
}

fn live_backend_candidate(
    existing: Option<InferenceBackend>,
    agent_did: &str,
    backend_id: &str,
    backend: &AgentBackendConfig,
) -> Result<InferenceBackend> {
    use gents::document_config::BackendAuth;
    let auth = match (&backend.api_key, &backend.api_key_env_var) {
        (Some(_), Some(_)) => anyhow::bail!("live backend must select one credential source"),
        (Some(key), None) => BackendAuth::ApiKey { key: key.clone() },
        (None, Some(variable)) => BackendAuth::Environment {
            variable: variable.clone(),
        },
        (None, None) => existing
            .as_ref()
            .map(|value| value.auth.clone())
            .unwrap_or_else(|| {
                if backend.provider_kind.is_agent_scoped_oauth() {
                    BackendAuth::PrincipalOAuth
                } else {
                    BackendAuth::Unauthenticated
                }
            }),
    };
    let candidate = InferenceBackend {
        agent_did: agent_did.into(),
        backend_id: backend_id.into(),
        name: backend_id.into(),
        provider_kind: backend.provider_kind,
        openai_wire_api: existing.and_then(|value| value.openai_wire_api),
        endpoint: backend.endpoint.clone(),
        auth,
        connect_timeout_secs: None,
        discovery_timeout_secs: None,
        max_concurrent: Some(2),
        max_queue_depth: Some(100),
        enabled: true,
        tags: Vec::new(),
    };
    candidate.validate()?;
    Ok(candidate)
}

async fn wait_for_runtime_process_state(
    node: &gents::defra_node::EmbeddedNode,
    agent_did: &str,
    expected_process_state: &str,
) -> Result<()> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let query = format!(
            r#"{{
                AgentBehaviorReadiness(
                    filter: {{ agent_did: {{ _eq: "{escaped_agent_did}" }} }},
                    limit: 1
                ) {{
                    agent_did
                    snapshot_json
                    updated_at
                }}
            }}"#
        );
        let response = node.execute(&query).await;
        if response.has_errors() {
            anyhow::bail!("AgentBehaviorReadiness query failed: {:?}", response.errors);
        }
        let process_state = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentBehaviorReadiness"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .cloned()
            .and_then(|row| serde_json::from_value::<AgentBehaviorReadinessRow>(row).ok())
            .and_then(|row| decode_behavior_readiness_snapshot(&row, agent_did).ok())
            .map(|snapshot| snapshot.process_state);
        if process_state.map(|state| state.as_str()) == Some(expected_process_state) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for AgentBehaviorReadiness {agent_did} to reach process_state={expected_process_state}; last={process_state:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::BackendProviderKind;

    #[test]
    fn live_backend_candidate_preserves_or_explicitly_replaces_one_credential_source() {
        use gents::document_config::BackendAuth;
        let existing: InferenceBackend = serde_json::from_value(serde_json::json!({
            "agent_did":"owner", "backend_id":"backend", "name":"backend",
            "provider_kind":"OpenAiCompatible", "endpoint":"http://old.example/v1",
            "auth":{"kind":"api_key","key":"stored-secret"}
        }))
        .unwrap();
        let mut requested = AgentBackendConfig {
            endpoint: "http://new.example/v1".to_string(),
            model_name: "new-model".to_string(),
            provider_kind: BackendProviderKind::OpenAiCompatible,
            api_key: None,
            api_key_env_var: None,
        };
        let preserved =
            live_backend_candidate(Some(existing.clone()), "owner", "backend", &requested).unwrap();
        assert!(matches!(preserved.auth, BackendAuth::ApiKey { key } if key == "stored-secret"));
        requested.api_key_env_var = Some("BACKEND_API_KEY".into());
        let replaced =
            live_backend_candidate(Some(existing), "owner", "backend", &requested).unwrap();
        assert!(
            matches!(replaced.auth, BackendAuth::Environment { variable } if variable == "BACKEND_API_KEY")
        );
        requested.api_key = Some("second-secret".into());
        assert!(live_backend_candidate(None, "owner", "backend", &requested)
            .unwrap_err()
            .to_string()
            .contains("one credential source"));
    }
}
