#[path = "inference.rs"]
mod inference;

use anyhow::{Context, Result};
use gents::eval::runner::embedded::EmbeddedHome;
use gents::{
    cli_tool, default_inference_profile_id_for_agent, DocumentRuntimeOptions, Gents,
    InferenceBackend, ToolCeiling,
};
use gents_desktop_core::client::PrincipalIdentity;
use gents_protocol::row::{decode_node_readiness_snapshot, NodeReadinessRow};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

use super::backend::LiveBackendConfig;
use super::workspace::seed_repo_workspace;

#[derive(Debug, Clone)]
pub(crate) struct LiveAgentDocs {
    pub(crate) agent_did: String,
    pub(crate) target_agent_did: String,
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
    home: &EmbeddedHome,
    config_actor: &PrincipalIdentity,
    agent_root: PathBuf,
    name: &str,
    backend: &LiveBackendConfig,
    target_backend: Option<&LiveBackendConfig>,
) -> Result<(RunningNode, LiveAgentDocs, PathBuf)> {
    let tool_root = agent_root.join("tool-root");
    std::fs::create_dir_all(&tool_root)
        .with_context(|| format!("creating live tool root {}", tool_root.display()))?;
    seed_repo_workspace(&tool_root)?;

    let node_did = home.identity.did().to_string();
    let docs =
        seed_live_agent_documents(home, config_actor, &node_did, name, backend, target_backend)
            .await?;
    let agent = Gents::from_default_agent_documents(
        home.node.clone(),
        Arc::clone(&home.identity),
        DocumentRuntimeOptions {
            backend_admission: Some(home.backend_admission()),
            tool_ceiling: ToolCeiling::readwrite(tool_root.clone())
                .with_command_timeout_secs(30)
                .with_cli_tool(cli_tool("rg", "rg", "Search files with ripgrep")),
            ..Default::default()
        },
    )
    .await?;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run_task = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_process_state(home, &node_did).await?;

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
    home: &EmbeddedHome,
    config_actor: &PrincipalIdentity,
    node_did: &str,
    agent_name: &str,
    backend: &LiveBackendConfig,
    target_backend: Option<&LiveBackendConfig>,
) -> Result<LiveAgentDocs> {
    use gents::documents::PackConfig;
    use gents::store::config_client::{
        apply_desired_state_plan, load_inference_backend_in_txn, ConfigAccess,
        DesiredStateApplyDocument, DesiredStateApplyPlan, GraphqlEndpoint, NodePermission,
    };
    use serde_json::json;
    let inference = inference::LiveInferenceSettings::from_env()?;
    let config_actor_did = config_actor.did();
    anyhow::ensure!(
        home.node.node_identity_did() == Some(node_did),
        "runtime node identity mismatch"
    );
    anyhow::ensure!(
        node_did != config_actor_did,
        "runtime and User identities must differ"
    );
    let actor_did = config_actor_did.parse()?;
    let runtime_access = ConfigAccess::local_as(home.node.clone(), node_did.parse()?);
    for permission in [
        NodePermission::CollectionGet,
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
        NodePermission::DocumentDelete,
        NodePermission::DacRelationAdd,
        NodePermission::DacRelationDelete,
    ] {
        runtime_access
            .grant_node_permission(&actor_did, permission)
            .await?;
    }
    let capability = gents::store::config_client::ClientSigningCapability::from_ed25519_key(
        config_actor.private_key_bytes(),
    )?;
    let access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(
            home.graphql_endpoint()
                .context("runtime HTTP endpoint missing")?,
            config_actor_did,
        )
        .with_client_signing_capability(capability)?
        .with_client_signed_agent_creates(),
    );
    let node_input =
        json!({"node_did":node_did,"display_name":agent_name,"created_by":config_actor_did});
    let initial = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: gents::Collection::Node,
        add: node_input.clone(),
        update: node_input,
    }])?;
    access
        .transact_write("desktop.fixture.node", |txn| {
            let initial = &initial;
            Box::pin(async move { apply_desired_state_plan(txn, initial).await })
        })
        .await?;
    let binding = gents::store::identity::provision_agent_identity_authorized(
        &home.node,
        home.identity.as_ref(),
        &actor_did,
    )
    .await?;
    let target_binding = gents::store::identity::provision_agent_identity_authorized(
        &home.node,
        home.identity.as_ref(),
        &actor_did,
    )
    .await?;
    let agent_did = binding.agent_did;
    let target_agent_did = target_binding.agent_did;
    anyhow::ensure!(
        agent_did != node_did
            && agent_did != config_actor_did
            && target_agent_did != node_did
            && target_agent_did != config_actor_did
            && target_agent_did != agent_did,
        "fixture User, Node, and Agents must have distinct identities"
    );
    let backend_id = format!("{agent_name}-backend");
    let target_backend_id = if target_backend.is_some() {
        format!("{agent_name}-target-backend")
    } else {
        backend_id.clone()
    };
    let tools_id = format!("{agent_did}-tools");
    let target_tools_id = format!("{target_agent_did}-tools");
    let inference_profile_id = default_inference_profile_id_for_agent(&agent_did);
    let target_profile_id = default_inference_profile_id_for_agent(&target_agent_did);
    let context_id = format!("{agent_did}-context");
    let target_context_id = format!("{target_agent_did}-context");
    let execution_id = format!("{agent_did}-execution");
    let sampling_id = format!("{agent_did}-sampling");
    let compaction_id = format!("{agent_did}-compaction");
    let target_id = format!("{agent_did}-target");
    let target = target_backend.unwrap_or(backend);
    let mut config = json!({
        "node":{"node_did":node_did,"display_name":agent_name,"default_agent_did":agent_did,"created_by":config_actor_did},
        "agents":[
            {"agent_did":agent_did,"creation_nonce":binding.creation_nonce,"node_did":node_did,"display_name":"Live Repo Audit Default","context_id":context_id,"inference_profile_id":inference_profile_id},
            {"agent_did":target_agent_did,"creation_nonce":target_binding.creation_nonce,"node_did":node_did,"display_name":"Live Repo Audit","context_id":target_context_id,"inference_profile_id":target_profile_id}
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
        "agent_targets":[{"node_did":node_did,"target_id":target_id,"target_node_did":node_did,"agent_did":target_agent_did,"name":"repo-audit","description":"Local repository audit agent for the desktop live fixture"}],
        "inference_profiles":[
            {"node_did":node_did,"profile_id":inference_profile_id,"display_name":"Live Repo Audit Profile","backend_id":backend_id,"model_name":backend.model_name,"context_window":131072,"sampling_id":sampling_id,"execution_id":execution_id},
            {"node_did":node_did,"profile_id":target_profile_id,"display_name":"Live Repo Audit Target Profile","backend_id":target_backend_id,"model_name":target.model_name,"context_window":131072,"sampling_id":sampling_id,"execution_id":execution_id}
        ],
        "inference_sampling":[{"node_did":node_did,"sampling_id":sampling_id}],
        "inference_execution":[{"node_did":node_did,"execution_id":execution_id,"stream_batch_ms":250,"stream_liveness_timeout_secs":60,"deadline_duration_secs":300}],
        "compactions":[{"node_did":node_did,"compaction_id":compaction_id,"strategy":"StripThenSummarize","threshold":0.95}]
    });
    inference.apply(&mut config);
    access
        .transact_write("desktop.fixture.config", |txn| {
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
                Ok::<_, anyhow::Error>(())
            })
        })
        .await?;
    Ok(LiveAgentDocs {
        agent_did,
        target_agent_did,
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
    use gents::documents::BackendAuth;
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
        physical_doc_id: existing
            .as_ref()
            .and_then(|value| value.physical_doc_id.clone()),
        node_did: node_did.into(),
        backend_id: backend_id.into(),
        name: backend_id.into(),
        provider_kind: backend.provider_kind,
        openai_wire_api: existing.as_ref().and_then(|value| value.openai_wire_api),
        endpoint: backend.endpoint.clone(),
        auth,
        claude_platform: existing.and_then(|value| value.claude_platform),
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

async fn wait_for_runtime_process_state(home: &EmbeddedHome, node_did: &str) -> Result<()> {
    let access = gents::ConfigAccess::local_as(home.node.clone(), node_did.parse()?);
    let query = format!(
        r#"{{ NodeReadiness(filter: {{ node_did: {{ _eq: "{}" }} }}, limit: 1) {{ node_did snapshot_json updated_at }} }}"#,
        gents::store::graphql::escape_graphql_string(node_did)
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let response = access.execute(&query).await?;
        let process_state = response["data"]["NodeReadiness"]
            .as_array()
            .and_then(|rows| rows.first())
            .cloned()
            .and_then(|row| serde_json::from_value::<NodeReadinessRow>(row).ok())
            .and_then(|row| decode_node_readiness_snapshot(&row, node_did).ok())
            .map(|snapshot| snapshot.process_state);
        if process_state.map(|state| state.as_str()) == Some("ready") {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for NodeReadiness {node_did} to reach ready; last={process_state:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
