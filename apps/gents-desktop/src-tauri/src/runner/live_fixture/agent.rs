#[path = "inference.rs"]
mod inference;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use gents::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use gents::{
    cli_tool, default_agent_id_for_node, default_inference_profile_id_for_agent,
    DocumentRuntimeOptions, Gents, InferenceBackend, KeyIdentity, NodeIdentity, ToolCeiling,
};
use gents_desktop_core::client::ClientCore;
use gents_protocol::row::{decode_node_readiness_snapshot, NodeReadinessRow};
use serde_json::Value;
use tokio::sync::watch;
use tracing::Instrument;

use super::backend::LiveBackendConfig;
use super::workspace::seed_repo_workspace;
use super::DEFAULT_DEPLOYMENT_LABEL;

#[derive(Debug, Clone)]
pub(crate) struct LiveAgentDocs {
    pub(crate) agent_id: String,
    pub(crate) target_agent_id: String,
    pub(crate) target_id: String,
    pub(crate) tools_id: String,
    pub(crate) target_tools_id: String,
    pub(crate) inference_profile_id: String,
}

pub(crate) struct RunningNode {
    pub(crate) node_did: String,
    shutdown_tx: watch::Sender<bool>,
    run_task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl RunningNode {
    pub(crate) async fn shutdown(self) -> Result<()> {
        let _ = self.shutdown_tx.send(true);
        self.run_task.await??;
        Ok(())
    }
}

pub(super) async fn spawn_live_node(
    node_owner: Arc<ClientCore>,
    key_path: PathBuf,
    name: &str,
    backend: &LiveBackendConfig,
    target_backend: Option<&LiveBackendConfig>,
) -> Result<(RunningNode, LiveAgentDocs, PathBuf)> {
    let tool_root = key_path
        .parent()
        .map(|parent| parent.join("tool-root"))
        .unwrap_or_else(|| std::env::temp_dir().join(format!("gents-tools-{name}")));
    std::fs::create_dir_all(&tool_root)
        .with_context(|| format!("creating live tool root {}", tool_root.display()))?;
    seed_repo_workspace(&tool_root)?;

    let identity = Arc::new(KeyIdentity::load_or_create(key_path, None)?);
    let node_did = identity.did().to_string();
    let docs = seed_live_agent_documents(
        node_owner.as_ref(),
        &node_did,
        name,
        backend,
        target_backend,
    )
    .await?;
    let agent = Gents::from_default_agent_documents(
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
        "live_bridge_node",
        deployment_label = %DEFAULT_DEPLOYMENT_LABEL,
        node_did = %node_did
    )));
    wait_for_runtime_process_state(node_owner.node(), &node_did, "ready").await?;

    Ok((
        RunningNode {
            node_did,
            shutdown_tx,
            run_task,
        },
        docs,
        tool_root,
    ))
}

async fn seed_live_agent_documents(
    core: &ClientCore,
    node_did: &str,
    agent_name: &str,
    backend: &LiveBackendConfig,
    target_backend: Option<&LiveBackendConfig>,
) -> Result<LiveAgentDocs> {
    use gents::config_client::{
        apply_desired_state_plan, load_inference_backend_in_txn, ConfigAccess,
        DesiredStateApplyPlan,
    };
    use gents::document_config::PackConfig;
    use serde_json::json;
    let inference = inference::LiveInferenceSettings::from_env()?;
    let agent_id = default_agent_id_for_node(node_did);
    let target_agent_id = format!("{node_did}:live-repo-audit");
    let backend_id = format!("{agent_name}-backend");
    let target_backend_id = if target_backend.is_some() {
        format!("{agent_name}-target-backend")
    } else {
        backend_id.clone()
    };
    let tools_id = format!("{agent_id}-tools");
    let target_tools_id = format!("{target_agent_id}-tools");
    let inference_profile_id = default_inference_profile_id_for_agent(&agent_id);
    let target_profile_id = default_inference_profile_id_for_agent(&target_agent_id);
    let context_id = format!("{agent_id}-context");
    let target_context_id = format!("{target_agent_id}-context");
    let execution_id = format!("{agent_id}-execution");
    let sampling_id = format!("{agent_id}-sampling");
    let compaction_id = format!("{agent_id}-compaction");
    let target_id = format!("{agent_id}-target");
    let target = target_backend.unwrap_or(backend);
    let mut config = json!({
        "node":{"node_did":node_did,"display_name":agent_name,"default_agent_id":agent_id},
        "agents":[
            {"agent_id":agent_id,"node_did":node_did,"display_name":"Live Repo Audit Default","context_id":context_id,"inference_profile_id":inference_profile_id},
            {"agent_id":target_agent_id,"node_did":node_did,"display_name":"Live Repo Audit","context_id":target_context_id,"inference_profile_id":target_profile_id}
        ],
        "contexts":[
            {"node_did":node_did,"context_id":context_id,"system_prompt":"You are Amy, a repository analysis agent operating inside a live desktop integration test. Keep answers concise. Use only the exact files requested by the user, and do not explore the wider repository unless explicitly asked. When the user explicitly asks you to use the local repo audit agent, call agent_new with agent \"repo-audit\" and the requested prompt; its result arrives later as a message, so say that it has started and reply again when that message arrives. When the user explicitly asks you to launch a native background process, call spawn_process with tool_name \"bash_unrestricted\" and the exact requested arguments. Do not call wait_process, read_process, list_processes, or cancel_process unless the user explicitly asks.","tools_id":tools_id,"compaction_id":compaction_id},
            {"node_did":node_did,"context_id":target_context_id,"system_prompt":"You are Amy's local repo audit agent inside a live desktop integration test. Read only the exact files requested by the requesting agent and return concise findings.","tools_id":target_tools_id,"compaction_id":compaction_id}
        ],
        "tools":[
            {"node_did":node_did,"tools_id":tools_id,"display_name":"Live Repo Audit Tools",
             "host":{"files":{"mode":"ReadOnly"},"bash":{"mode":"Unrestricted","background_enabled":true}},
             "built_ins":{"enable_context_budget":true},
             "agents":{"target_ids":[target_id],"enabled":true}},
            {"node_did":node_did,"tools_id":target_tools_id,"display_name":"Live Repo Audit Target Tools",
             "host":{"files":{"mode":"ReadOnly"}},"built_ins":{"enable_context_budget":true}}
        ],
        "agent_targets":[{"node_did":node_did,"target_id":target_id,"target_node_did":node_did,"agent_id":target_agent_id,"name":"repo-audit","description":"Local repository audit agent for the desktop live fixture"}],
        "inference_profiles":[
            {"node_did":node_did,"profile_id":inference_profile_id,"display_name":"Live Repo Audit Profile","backend_id":backend_id,"model_name":backend.model_name,"context_window":131072,"sampling_id":sampling_id,"execution_id":execution_id},
            {"node_did":node_did,"profile_id":target_profile_id,"display_name":"Live Repo Audit Target Profile","backend_id":target_backend_id,"model_name":target.model_name,"context_window":131072,"sampling_id":sampling_id,"execution_id":execution_id}
        ],
        "inference_sampling":[{"node_did":node_did,"sampling_id":sampling_id}],
        "inference_execution":[{"node_did":node_did,"execution_id":execution_id,"stream_batch_ms":250,"stream_liveness_timeout_secs":60,"deadline_duration_secs":300}],
        "compactions":[{"node_did":node_did,"compaction_id":compaction_id,"strategy":"StripThenSummarize","threshold":0.95}]
    });
    inference.apply(&mut config);
    ConfigAccess::transact_local(core.node(), None, "desktop.fixture.config", |txn| {
        let mut config = config.clone();
        let backend_id = &backend_id;
        let target_backend_id = &target_backend_id;
        Box::pin(async move {
            let existing = load_inference_backend_in_txn(txn, node_did, backend_id).await?;
            let mut backends = vec![live_backend_candidate(
                existing, node_did, backend_id, backend,
            )?];
            if target_backend.is_some() {
                let existing =
                    load_inference_backend_in_txn(txn, node_did, target_backend_id).await?;
                backends.push(live_backend_candidate(
                    existing,
                    node_did,
                    target_backend_id,
                    target,
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
        agent_id,
        target_agent_id,
        target_id,
        tools_id,
        target_tools_id,
        inference_profile_id,
    })
}

fn live_backend_candidate(
    existing: Option<InferenceBackend>,
    node_did: &str,
    backend_id: &str,
    backend: &LiveBackendConfig,
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
                if backend.provider_kind.is_node_scoped_oauth() {
                    BackendAuth::NodeOAuth { account_ref: None }
                } else {
                    BackendAuth::Unauthenticated
                }
            }),
    };
    let candidate = InferenceBackend {
        node_did: node_did.into(),
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
    node_did: &str,
    expected_process_state: &str,
) -> Result<()> {
    let escaped_node_did = escape_graphql_string(node_did);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let query = format!(
            r#"{{
                NodeReadiness(
                    filter: {{ node_did: {{ _eq: "{escaped_node_did}" }} }},
                    limit: 1
                ) {{
                    node_did
                    snapshot_json
                    updated_at
                }}
            }}"#
        );
        let response = graphql_with_transaction_retry(&node, &query, "NodeReadiness query").await?;
        let process_state = response
            .data
            .as_ref()
            .and_then(|data| data.get("NodeReadiness"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .cloned()
            .and_then(|row| serde_json::from_value::<NodeReadinessRow>(row).ok())
            .and_then(|row| decode_node_readiness_snapshot(&row, node_did).ok())
            .map(|snapshot| snapshot.process_state);
        if process_state.map(|state| state.as_str()) == Some(expected_process_state) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for NodeReadiness {node_did} to reach process_state={expected_process_state}; last={process_state:?}"
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
            "node_did":"owner", "backend_id":"backend", "name":"backend",
            "provider_kind":"OpenAiCompatible", "endpoint":"http://old.example/v1",
            "auth":{"kind":"api_key","key":"stored-secret"}
        }))
        .unwrap();
        let mut requested = LiveBackendConfig {
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
