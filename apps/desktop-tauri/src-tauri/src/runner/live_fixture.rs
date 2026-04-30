use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::{
    cli_tool, default_behavior_id_for_agent, default_inference_profile_id_for_behavior,
    default_tool_selection_id_for_behavior, ensure_agent_principal, load_agent_behavior,
    upsert_agent_behavior, AgentIdentity, BackendProviderKind, DefraAgent, DocumentRuntimeOptions,
    KeyIdentity, ToolCeiling,
};
use defra_agent_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths, PeerRecord};
use defra_agent_desktop_core::local_runtime::DesktopInitSummary;
use defra_agent_protocol::client_protocol::ClientTurnState;
use defra_agent_protocol::row::{AgentBehaviorRow, InferenceProfileRow, ToolSelectionRow};
use serde_json::Value;
use tokio::runtime::Runtime;
use tokio::sync::{watch, Mutex};
use tracing::Instrument;
use tracing_subscriber::prelude::*;

use crate::bridge::types::{DesktopBootstrapSummary, SavedPeerView};

const DEFAULT_DEPLOYMENT_LABEL: &str = "Amy Server";
const DEFAULT_AGENT_NAME: &str = "amy";
const LIVE_BACKEND_PREFIX: &str = "DEFRA_AGENT_DESKTOP_LIVE_BACKEND";

#[derive(Debug, Clone, Default)]
pub(crate) struct LiveBackendOverride {
    pub(crate) inference_url: Option<String>,
    pub(crate) model_name: Option<String>,
    pub(crate) provider: Option<String>,
    pub(crate) api_key: Option<String>,
    pub(crate) api_key_env_var: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct AgentBackendConfig {
    pub(crate) endpoint: String,
    pub(crate) model_name: String,
    pub(crate) provider_kind: BackendProviderKind,
    pub(crate) api_key: Option<String>,
    pub(crate) api_key_env_var: Option<String>,
}

impl AgentBackendConfig {
    pub(crate) fn resolve(override_config: Option<&LiveBackendOverride>) -> Result<Self> {
        let endpoint = override_config
            .and_then(|config| normalize_optional_owned(config.inference_url.as_ref()))
            .or_else(|| optional_env(&format!("{LIVE_BACKEND_PREFIX}_ENDPOINT")));
        let model_name = override_config
            .and_then(|config| normalize_optional_owned(config.model_name.as_ref()))
            .or_else(|| optional_env(&format!("{LIVE_BACKEND_PREFIX}_MODEL")))
            .or_else(|| optional_env("DEFRA_AGENT_TEST_OPENROUTER_MODEL"))
            .unwrap_or_else(|| "openai/gpt-4o-mini".to_string());
        let provider_kind = override_config
            .and_then(|config| normalize_optional_owned(config.provider.as_ref()))
            .or_else(|| optional_env(&format!("{LIVE_BACKEND_PREFIX}_PROVIDER")));
        let api_key = override_config
            .and_then(|config| normalize_optional_owned(config.api_key.as_ref()))
            .or_else(|| optional_env(&format!("{LIVE_BACKEND_PREFIX}_API_KEY")));
        let api_key_env_var = override_config
            .and_then(|config| normalize_optional_owned(config.api_key_env_var.as_ref()))
            .or_else(|| optional_env(&format!("{LIVE_BACKEND_PREFIX}_API_KEY_ENV_VAR")));

        if endpoint.is_some()
            || provider_kind.is_some()
            || api_key.is_some()
            || api_key_env_var.is_some()
        {
            if let Some(env_var_name) = api_key_env_var.as_deref() {
                std::env::var(env_var_name).with_context(|| {
                    format!(
                        "set {env_var_name} because {LIVE_BACKEND_PREFIX}_API_KEY_ENV_VAR points at it"
                    )
                })?;
            }

            return Ok(Self {
                endpoint: endpoint.context(format!(
                    "set {LIVE_BACKEND_PREFIX}_ENDPOINT or OPENROUTER_API_KEY to run the live Tauri bridge runner"
                ))?,
                model_name,
                provider_kind: BackendProviderKind::parse_optional(provider_kind.as_deref())?,
                api_key,
                api_key_env_var,
            });
        }

        if std::env::var("OPENROUTER_API_KEY").is_ok() {
            return Ok(Self {
                endpoint: "https://openrouter.ai/api/v1".to_string(),
                model_name,
                provider_kind: BackendProviderKind::OpenRouter,
                api_key: None,
                api_key_env_var: Some("OPENROUTER_API_KEY".to_string()),
            });
        }

        anyhow::bail!(
            "set {LIVE_BACKEND_PREFIX}_ENDPOINT or OPENROUTER_API_KEY to run the live Tauri bridge runner"
        );
    }
}

#[derive(Debug, Clone)]
struct LiveAgentDocs {
    behavior_id: String,
    backend_id: String,
    tool_selection_id: String,
    inference_profile_id: String,
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

pub(crate) struct LiveBridgeFixture {
    runtime: Arc<Runtime>,
    _tempdir: tempfile::TempDir,
    desktop_paths: DesktopPaths,
    agent_home: PathBuf,
    desktop_core: Arc<ClientCore>,
    remote_core: Arc<ClientCore>,
    deployment_label: String,
    agent_did: String,
    tool_root: PathBuf,
    init_summary: DesktopInitSummary,
    bootstrap_saved_peers: Vec<SavedPeerView>,
    update_version: Arc<AtomicU64>,
    update_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    running_agent: Mutex<Option<RunningAgent>>,
    shutdown_started: AtomicBool,
}

impl LiveBridgeFixture {
    pub(crate) fn runtime(&self) -> &Arc<Runtime> {
        &self.runtime
    }

    pub(crate) fn init_summary(&self) -> DesktopInitSummary {
        self.init_summary.clone()
    }

    pub(crate) fn desktop_core(&self) -> &Arc<ClientCore> {
        &self.desktop_core
    }

    pub(crate) fn remote_core(&self) -> &Arc<ClientCore> {
        &self.remote_core
    }

    pub(crate) fn agent_did(&self) -> &str {
        &self.agent_did
    }

    pub(crate) fn deployment_label(&self) -> &str {
        &self.deployment_label
    }

    pub(crate) fn tool_root(&self) -> &Path {
        &self.tool_root
    }

    pub(crate) fn update_version(&self) -> u64 {
        self.update_version.load(Ordering::SeqCst)
    }

    pub(crate) async fn shutdown(&self) -> Result<()> {
        if self.shutdown_started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        if let Some(task) = self.update_task.lock().await.take() {
            task.abort();
            let _ = task.await;
        }

        if let Some(agent) = self.running_agent.lock().await.take() {
            agent.shutdown().await?;
        }

        self.remote_core.shutdown().await?;
        self.desktop_core.shutdown().await?;
        Ok(())
    }

    pub(crate) fn start(backend_override: Option<LiveBackendOverride>) -> Result<Arc<Self>> {
        init_live_runner_tracing();

        let backend = AgentBackendConfig::resolve(backend_override.as_ref())?;
        let runtime = live_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let remote_paths = DesktopPaths::from_root(tempdir.path().join("remote"));
        let desktop_paths = DesktopPaths::from_root(tempdir.path().join("desktop"));
        let agent_home = tempdir.path().join("agent-home");

        let remote_core = Arc::new(runtime.block_on(ClientCore::start_with_paths_and_options(
            remote_paths,
            live_core_options(),
        ))?);

        let agent_key = tempdir.path().join("agent").join("amy.key");
        let (running_agent, docs, tool_root) = runtime.block_on(spawn_live_agent(
            Arc::clone(&remote_core),
            agent_key,
            DEFAULT_AGENT_NAME,
            &backend,
        ))?;

        let remote_addr = runtime.block_on(wait_for_connectable_iroh_addr(
            remote_core.as_ref(),
            DEFAULT_DEPLOYMENT_LABEL,
        ))?;
        let mut peer_record =
            PeerRecord::new(DEFAULT_DEPLOYMENT_LABEL, &remote_addr, &running_agent.did);
        peer_record.source = Some("bridge-runner".to_string());
        write_peer_directory_records(&desktop_paths, &[peer_record.clone()])?;

        let desktop_core = Arc::new(runtime.block_on(ClientCore::start_with_paths_and_options(
            desktop_paths.clone(),
            live_core_options(),
        ))?);

        runtime.block_on(configure_live_replicators(
            desktop_core.as_ref(),
            remote_core.as_ref(),
            DEFAULT_DEPLOYMENT_LABEL,
        ))?;
        runtime.block_on(wait_for_connected_peer(
            desktop_core.as_ref(),
            remote_core.local_peer_id(),
            "desktop -> amy",
        ))?;
        runtime.block_on(wait_for_connected_peer(
            remote_core.as_ref(),
            desktop_core.local_peer_id(),
            "amy -> desktop",
        ))?;
        runtime.block_on(wait_for_live_documents(
            desktop_core.as_ref(),
            &running_agent.did,
            &docs,
        ))?;

        seed_runner_agent_home(
            &agent_home,
            DEFAULT_AGENT_NAME,
            &running_agent.did,
            remote_core.local_peer_id(),
            &remote_addr,
        )?;

        let remote_peer_id = remote_core.local_peer_id().to_string();
        let init_summary = DesktopInitSummary {
            status: "initialized",
            source: "bridge-runner",
            status_endpoint: None,
            agent_home: agent_home.display().to_string(),
            desktop_home: desktop_paths.root().display().to_string(),
            peer_directory: desktop_paths.peer_directory_path().display().to_string(),
            label: DEFAULT_DEPLOYMENT_LABEL.to_string(),
            agent_name: DEFAULT_AGENT_NAME.to_string(),
            agent_did: running_agent.did.clone(),
            graphql: String::new(),
            p2p_transport: "iroh".to_string(),
            p2p_peer_id: remote_peer_id.clone(),
            p2p_listen_address: remote_addr.clone(),
            peer_record_id: peer_record.peer_id.clone(),
            next_steps: vec![],
        };

        let bootstrap_saved_peers = vec![SavedPeerView {
            peer_id: peer_record.peer_id.clone(),
            label: peer_record.label.clone(),
            agent_did: peer_record.agent_did.clone(),
            addr: peer_record.addr.clone(),
            source: peer_record.source.clone(),
            graphql: peer_record.graphql.clone(),
        }];

        tracing::info!(
            agent_did = %running_agent.did,
            tool_root = %tool_root.display(),
            "live bridge fixture ready"
        );

        let update_version = Arc::new(AtomicU64::new(1));
        let update_task = {
            let desktop_core = Arc::clone(&desktop_core);
            let update_version = Arc::clone(&update_version);
            runtime.spawn(async move {
                let mut store_updates = desktop_core.store_updates();
                let mut health_updates = desktop_core.p2p_health_updates();
                loop {
                    tokio::select! {
                        changed = store_updates.changed() => {
                            if changed.is_err() {
                                break;
                            }
                            update_version.fetch_add(1, Ordering::SeqCst);
                        }
                        changed = health_updates.changed() => {
                            if changed.is_err() {
                                break;
                            }
                            update_version.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
            })
        };

        Ok(Arc::new(Self {
            runtime,
            _tempdir: tempdir,
            desktop_paths,
            agent_home,
            desktop_core,
            remote_core,
            deployment_label: DEFAULT_DEPLOYMENT_LABEL.to_string(),
            agent_did: running_agent.did.clone(),
            tool_root,
            init_summary,
            bootstrap_saved_peers,
            update_version,
            update_task: Mutex::new(Some(update_task)),
            running_agent: Mutex::new(Some(running_agent)),
            shutdown_started: AtomicBool::new(false),
        }))
    }

    pub(crate) async fn build_bootstrap_summary(&self) -> DesktopBootstrapSummary {
        DesktopBootstrapSummary {
            default_agent_home: self.agent_home.display().to_string(),
            init_agent_name: Some(DEFAULT_AGENT_NAME.to_string()),
            init_agent_did: Some(self.init_summary.agent_did.clone()),
            init_tool_ceiling: Some("Readwrite".to_string()),
            init_tool_root: Some(self.tool_root.display().to_string()),
            desktop_home: self.desktop_paths.root().display().to_string(),
            peer_directory_path: self
                .desktop_paths
                .peer_directory_path()
                .display()
                .to_string(),
            node_data_dir: self.desktop_paths.node_data_dir().display().to_string(),
            log_file_path: self.desktop_paths.log_file_path().display().to_string(),
            agent_home_exists: self.agent_home.exists(),
            desktop_home_exists: self.desktop_paths.root().exists(),
            peer_directory_exists: self.desktop_paths.peer_directory_path().exists(),
            saved_peers: self.bootstrap_saved_peers.clone(),
        }
    }
}

fn live_runtime() -> Result<Arc<Runtime>> {
    const STACK_BYTES: usize = 16 * 1024 * 1024;
    Ok(Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(4)
            .thread_stack_size(STACK_BYTES)
            .build()?,
    ))
}

fn live_core_options() -> ClientCoreOptions {
    let mut options = ClientCoreOptions::local_only();
    options.bind_addr = Some(IpAddr::V4(Ipv4Addr::LOCALHOST));
    options.max_concurrent_push_tasks = 32;
    options.rate_limit_burst = 5_000;
    options.rate_limit_rate = 500.0;
    options.install_replicators_on_bootstrap = false;
    options
}

fn init_live_runner_tracing() {
    static INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INIT.get_or_init(|| {
        let filter = std::env::var("DEFRA_AGENT_DESKTOP_TEST_LOG")
            .map(tracing_subscriber::EnvFilter::new)
            .unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("warn,defra_agent_desktop_tauri=info")
            });
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .compact()
                    .without_time(),
            )
            .try_init();
    });
}

fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_optional_owned(value: Option<&String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn graphql_optional_string_field(name: &str, value: Option<&str>) -> String {
    value
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!(r#"{name}: "{}","#, escape_graphql_string(value)))
        .unwrap_or_default()
}

async fn spawn_live_agent(
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
        cli_tool_names: vec!["rg".to_string()],
        enable_meta_tools: Some(false),
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

fn seed_runner_agent_home(
    agent_home: &Path,
    agent_name: &str,
    agent_did: &str,
    peer_id: &str,
    listen_address: &str,
) -> Result<()> {
    std::fs::create_dir_all(agent_home)?;
    std::fs::write(
        agent_home.join("init.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "agent_name": agent_name,
            "agent_did": agent_did,
        }))?,
    )?;
    std::fs::write(
        agent_home.join("runtime.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "graphql": "",
            "agent_name": agent_name,
            "agent_did": agent_did,
            "p2p_transport": "iroh",
            "p2p_peer_id": peer_id,
            "p2p_listen_addresses": [listen_address],
        }))?,
    )?;
    Ok(())
}

fn seed_repo_workspace(tool_root: &Path) -> Result<()> {
    let repo_root = workspace_repo_root()?;
    let workspace_root = tool_root.join("workspace");
    std::fs::create_dir_all(&workspace_root)
        .with_context(|| format!("creating seeded workspace {}", workspace_root.display()))?;
    copy_repo_tree(&repo_root, &workspace_root)?;
    Ok(())
}

fn workspace_repo_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("failed to locate repo root from {}", manifest_dir.display()))
}

fn copy_repo_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if should_skip_workspace_entry(&file_name) {
            continue;
        }
        let source_path = entry.path();
        let target_path = dst.join(file_name.as_ref());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_repo_tree(&source_path, &target_path)?;
        } else if file_type.is_file() {
            if let Some(parent) = target_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::copy(&source_path, &target_path).with_context(|| {
                format!(
                    "copying {} -> {}",
                    source_path.display(),
                    target_path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn should_skip_workspace_entry(name: &str) -> bool {
    matches!(
        name,
        ".git" | "target" | "node_modules" | ".next" | ".turbo" | "dist" | "build" | ".direnv"
    )
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

async fn wait_for_connectable_iroh_addr(core: &ClientCore, label: &str) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let addrs = core.p2p().listen_addresses().await?;
        if let Some(addr) = addrs
            .iter()
            .find(|addr| addr.contains("/p2p/") || addr.starts_with("endpoint"))
        {
            return Ok(addr.clone());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for {label} listen address");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn configure_live_replicators(
    desktop_core: &ClientCore,
    remote_core: &ClientCore,
    label: &str,
) -> Result<()> {
    let desktop_addr = wait_for_connectable_iroh_addr(desktop_core, "desktop").await?;
    let remote_addr = wait_for_connectable_iroh_addr(remote_core, label).await?;
    let desktop_peer_id = desktop_core.local_peer_id().to_string();
    let remote_peer_id = remote_core.local_peer_id().to_string();

    connect_peer_with_retry(
        desktop_core,
        &remote_addr,
        &remote_peer_id,
        &format!("desktop -> {label}"),
    )
    .await?;
    connect_peer_with_retry(
        remote_core,
        &desktop_addr,
        &desktop_peer_id,
        &format!("{label} -> desktop"),
    )
    .await?;
    set_replicator_with_retry(
        remote_core,
        &desktop_addr,
        &format!("{label} -> desktop replicator"),
        subscribed_collection_names_for_runner(),
    )
    .await?;
    set_replicator_with_retry(
        desktop_core,
        &remote_addr,
        &format!("desktop -> {label} replicator"),
        subscribed_collection_names_for_runner(),
    )
    .await?;
    Ok(())
}

async fn connect_peer_with_retry(
    core: &ClientCore,
    addr: &str,
    peer_id: &str,
    label: &str,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if is_connected_peer(core, peer_id).await? {
            return Ok(());
        }

        match core.p2p().connect_peer(addr).await {
            Ok(()) => {
                wait_for_connected_peer(core, peer_id, label).await?;
                return Ok(());
            }
            Err(error) => {
                if is_connected_peer(core, peer_id).await? {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    anyhow::bail!("timed out connecting {label} to {peer_id}: {error}");
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
}

async fn is_connected_peer(core: &ClientCore, peer_id: &str) -> Result<bool> {
    let peers = core.p2p().connected_peers().await?;
    Ok(peers.iter().any(|peer| peer.contains(peer_id)))
}

async fn wait_for_connected_peer(core: &ClientCore, peer_id: &str, label: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if is_connected_peer(core, peer_id).await? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for connected peer {peer_id} on {label}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn set_replicator_with_retry(
    core: &ClientCore,
    addr: &str,
    label: &str,
    collections: Vec<String>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match core
            .p2p()
            .add_replicator(collections.clone(), Some(addr), Vec::new(), None)
            .await
        {
            Ok(()) => return Ok(()),
            Err(error) => {
                if Instant::now() >= deadline {
                    anyhow::bail!("timed out configuring {label}: {error}");
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
}

fn subscribed_collection_names_for_runner() -> Vec<String> {
    defra_agent_protocol::schemas::RUNTIME_COLLECTION_NAMES
        .iter()
        .chain(defra_agent_protocol::schemas::ALL_COLLECTION_NAMES.iter())
        .map(|name| (*name).to_string())
        .collect()
}

fn write_peer_directory_records(paths: &DesktopPaths, records: &[PeerRecord]) -> Result<()> {
    std::fs::create_dir_all(paths.root())?;
    let payload = serde_json::json!({ "peers": records });
    std::fs::write(
        paths.peer_directory_path(),
        serde_json::to_vec_pretty(&payload)?,
    )?;
    Ok(())
}

async fn wait_for_live_documents(
    desktop_core: &ClientCore,
    agent_did: &str,
    docs: &LiveAgentDocs,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        desktop_core.refresh_store().await?;
        let snapshot = desktop_core.store().snapshot();
        let has_principal = snapshot
            .agent_principals
            .iter()
            .any(|row| row.agent_did == agent_did);
        let has_behavior = snapshot
            .behaviors
            .iter()
            .any(|row| row.behavior_id == docs.behavior_id);
        let has_backend = snapshot
            .inference_backends
            .iter()
            .any(|row| row.backend_id == docs.backend_id);
        let has_tools = snapshot
            .tool_selections
            .iter()
            .any(|row| row.selection_id == docs.tool_selection_id);
        let has_profile = snapshot
            .inference_profiles
            .iter()
            .any(|row| row.profile_id == docs.inference_profile_id);

        if has_principal && has_behavior && has_backend && has_tools && has_profile {
            return Ok(());
        }

        if Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for live documents to replicate to desktop");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub(crate) fn can_send_in_turn(state: ClientTurnState) -> bool {
    matches!(
        state,
        ClientTurnState::Completed
            | ClientTurnState::Failed
            | ClientTurnState::Superseded
            | ClientTurnState::Interrupted
    )
}

pub(crate) fn turn_state_label(state: ClientTurnState) -> &'static str {
    match state {
        ClientTurnState::WaitingForClaim => "waitingForClaim",
        ClientTurnState::Streaming => "streaming",
        ClientTurnState::Completed => "completed",
        ClientTurnState::Failed => "failed",
        ClientTurnState::Superseded => "superseded",
        ClientTurnState::Interrupted => "interrupted",
    }
}
