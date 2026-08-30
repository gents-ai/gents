#[path = "live_fixture/agent.rs"]
mod agent;
#[path = "live_fixture/backend.rs"]
mod backend;
#[path = "live_fixture/replication.rs"]
mod replication;
#[path = "live_fixture/workspace.rs"]
mod workspace;

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths, PeerRecord};
use gents_desktop_core::local_runtime::DesktopInitSummary;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;
use tracing_subscriber::prelude::*;

use gents_desktop_bridge::types::{DesktopBootstrapSummary, SavedPeerView};

use self::agent::{spawn_live_agent, RunningAgent};
use self::backend::AgentBackendConfig;
pub(crate) use self::backend::LiveBackendOverride;
pub(crate) use self::backend::LiveSubagentBackendOverride;
use self::replication::{
    configure_live_replicators, wait_for_connectable_iroh_addr, wait_for_connected_peer,
    wait_for_live_documents, write_peer_directory_records,
};
use self::workspace::seed_runner_agent_home;

const DEFAULT_DEPLOYMENT_LABEL: &str = "Fleet E2E Agent";
const DEFAULT_AGENT_NAME: &str = "fleet-e2e-agent";

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

    pub(crate) fn start(
        backend_override: Option<LiveBackendOverride>,
        subagent_backend_override: Option<LiveSubagentBackendOverride>,
    ) -> Result<Arc<Self>> {
        init_live_runner_tracing();

        let backend = AgentBackendConfig::resolve(backend_override.as_ref())?;
        let subagent_backend =
            AgentBackendConfig::resolve_subagent(subagent_backend_override.as_ref(), &backend)?;
        let runtime = live_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let remote_paths = DesktopPaths::from_root(tempdir.path().join("remote"));
        let desktop_paths = DesktopPaths::from_root(tempdir.path().join("desktop"));
        let agent_home = tempdir.path().join("agent-home");

        let remote_core = Arc::new(runtime.block_on(ClientCore::start_with_paths_and_options(
            remote_paths,
            live_core_options(),
        ))?);

        let agent_key = tempdir.path().join("agent").join("fleet-e2e-agent.key");
        let (running_agent, docs, tool_root) = runtime.block_on(spawn_live_agent(
            Arc::clone(&remote_core),
            agent_key,
            DEFAULT_AGENT_NAME,
            &backend,
            subagent_backend.as_ref(),
        ))?;

        let remote_addr = runtime.block_on(wait_for_connectable_iroh_addr(
            remote_core.as_ref(),
            DEFAULT_DEPLOYMENT_LABEL,
        ))?;
        let mut peer_record =
            PeerRecord::new(DEFAULT_DEPLOYMENT_LABEL, &remote_addr, &running_agent.did);
        peer_record.source = Some("local-standard".to_string());
        // This fixture owns both nodes and installs both directional
        // replicators below. Keep the durable route truthful while the normal
        // supervisor is intentionally disabled for this manually managed
        // topology.
        peer_record.pairing_ready = true;
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
                let mut sync_updates = desktop_core.sync_state_updates();
                loop {
                    tokio::select! {
                        changed = store_updates.changed() => {
                            if changed.is_err() {
                                break;
                            }
                            update_version.fetch_add(1, Ordering::SeqCst);
                        }
                        changed = sync_updates.changed() => {
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

    pub(crate) fn start_desktop_only() -> Result<Arc<Self>> {
        init_live_runner_tracing();

        let runtime = live_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let remote_paths = DesktopPaths::from_root(tempdir.path().join("remote-empty"));
        let desktop_paths = DesktopPaths::from_root(tempdir.path().join("desktop"));
        let agent_home = tempdir.path().join("agent-home");
        std::fs::create_dir_all(&agent_home)?;

        let remote_core = Arc::new(runtime.block_on(ClientCore::start_with_paths_and_options(
            remote_paths,
            live_core_options(),
        ))?);
        let desktop_core = Arc::new(runtime.block_on(ClientCore::start_with_paths_and_options(
            desktop_paths.clone(),
            live_core_options(),
        ))?);
        let p2p_listen_address = desktop_core
            .listen_addresses()
            .first()
            .cloned()
            .unwrap_or_default();

        let init_summary = DesktopInitSummary {
            status: "initialized",
            source: "bridge-runner-desktop-only",
            status_endpoint: None,
            agent_home: agent_home.display().to_string(),
            desktop_home: desktop_paths.root().display().to_string(),
            peer_directory: desktop_paths.peer_directory_path().display().to_string(),
            label: "Desktop Only".to_string(),
            agent_name: String::new(),
            agent_did: String::new(),
            graphql: String::new(),
            p2p_transport: "iroh".to_string(),
            p2p_peer_id: desktop_core.local_peer_id().to_string(),
            p2p_listen_address,
            peer_record_id: String::new(),
            next_steps: vec![],
        };

        tracing::info!("desktop-only bridge fixture ready");

        let update_version = Arc::new(AtomicU64::new(1));
        let update_task = {
            let desktop_core = Arc::clone(&desktop_core);
            let update_version = Arc::clone(&update_version);
            runtime.spawn(async move {
                let mut store_updates = desktop_core.store_updates();
                let mut sync_updates = desktop_core.sync_state_updates();
                loop {
                    tokio::select! {
                        changed = store_updates.changed() => {
                            if changed.is_err() {
                                break;
                            }
                            update_version.fetch_add(1, Ordering::SeqCst);
                        }
                        changed = sync_updates.changed() => {
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
            deployment_label: "Desktop Only".to_string(),
            agent_did: String::new(),
            tool_root: PathBuf::new(),
            init_summary,
            bootstrap_saved_peers: Vec::new(),
            update_version,
            update_task: Mutex::new(Some(update_task)),
            running_agent: Mutex::new(None),
            shutdown_started: AtomicBool::new(false),
        }))
    }

    pub(crate) async fn build_bootstrap_summary(&self) -> DesktopBootstrapSummary {
        DesktopBootstrapSummary {
            default_agent_home: self.agent_home.display().to_string(),
            init_agent_name: non_empty_clone(&self.init_summary.agent_name),
            init_agent_did: non_empty_clone(&self.init_summary.agent_did),
            init_tool_ceiling: Some("Readwrite".to_string()),
            init_tool_root: self
                .tool_root
                .to_str()
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
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

fn non_empty_clone(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.to_string())
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
        let filter = std::env::var("GENTS_DESKTOP_TEST_LOG")
            .map(tracing_subscriber::EnvFilter::new)
            .unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("warn,gents_desktop_tauri=info")
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use anyhow::{bail, Context, Result};
    use axum::extract::State;
    use axum::http::{header, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use axum::Router;
    use gents::default_behavior_id_for_agent;
    use gents::graphql::escape_graphql_string;
    use gents_desktop_core::client::ClientCore;
    use gents_protocol::row::{decode_behavior_readiness_snapshot, AgentBehaviorReadinessRow};
    use serde_json::Value;
    use tokio::sync::oneshot;

    use super::{LiveBackendOverride, LiveBridgeFixture};
    use gents_desktop_bridge::commands::{
        delete_skill_config, save_skill_config, send_chat_message,
    };
    use gents_desktop_bridge::snapshot::build_session_snapshot_for_agent_with_transcript;
    use gents_desktop_bridge::types::{ChatSendRequest, SkillDeleteRequest, SkillSaveRequest};

    const MODEL_NAME: &str = "desktop-live-skill-mock";

    #[test]
    fn live_fixture_replicates_skill_create_delete_to_agent_node() -> Result<()> {
        let _guard = live_fixture_test_lock();
        let mock = MockChatEndpoint::start(MODEL_NAME, "ok")?;
        let fixture = LiveBridgeFixture::start(Some(mock.backend_override(MODEL_NAME)), None)?;

        let result = fixture
            .runtime()
            .block_on(skill_create_delete_case(fixture.as_ref()));
        let shutdown_result = fixture.runtime().block_on(fixture.shutdown());
        shutdown_result?;
        result
    }

    #[test]
    fn live_fixture_desktop_chat_slash_skill_loads_on_agent_node() -> Result<()> {
        let _guard = live_fixture_test_lock();
        let mock = MockChatEndpoint::start(MODEL_NAME, "skill loaded")?;
        let fixture = LiveBridgeFixture::start(Some(mock.backend_override(MODEL_NAME)), None)?;

        let result = fixture
            .runtime()
            .block_on(slash_skill_chat_case(fixture.as_ref(), &mock));
        let shutdown_result = fixture.runtime().block_on(fixture.shutdown());
        shutdown_result?;
        result
    }

    async fn skill_create_delete_case(fixture: &LiveBridgeFixture) -> Result<()> {
        let agent_did = fixture.agent_did().to_string();
        let behavior_id = default_behavior_id_for_agent(&agent_did);
        let skill_id = "desktop-crud-skill";
        let skill_body = "CRUD skill body should replicate to the agent node.";

        save_skill_config(
            fixture.desktop_core().as_ref(),
            skill_save_request(&agent_did, skill_id, skill_body),
        )
        .await?;
        wait_for_remote_skill(fixture.remote_core().as_ref(), skill_id)
            .await
            .context("skill did not replicate to the agent node after create")?;

        bind_skill_to_behavior(fixture, &agent_did, &behavior_id, skill_id, true).await?;
        wait_for_remote_behavior_skill_refs(
            fixture.remote_core().as_ref(),
            &behavior_id,
            &[skill_id],
            &[skill_id],
        )
        .await
        .context("behavior skill refs did not replicate to the agent node")?;

        delete_skill_config(
            fixture.desktop_core().as_ref(),
            SkillDeleteRequest {
                skill_id: skill_id.to_string(),
                agent_did: agent_did.clone(),
            },
        )
        .await?;
        wait_for_remote_skill_absent(fixture.remote_core().as_ref(), skill_id)
            .await
            .context("skill remained queryable on the agent node after delete")?;
        wait_for_remote_behavior_skill_refs(fixture.remote_core().as_ref(), &behavior_id, &[], &[])
            .await
            .context("behavior skill refs were not pruned on the agent node")?;

        Ok(())
    }

    async fn slash_skill_chat_case(
        fixture: &LiveBridgeFixture,
        mock: &MockChatEndpoint,
    ) -> Result<()> {
        let agent_did = fixture.agent_did().to_string();
        let behavior_id = default_behavior_id_for_agent(&agent_did);
        let skill_id = "desktop-review";
        let skill_body = "UNIQUE_DESKTOP_SKILL_BODY_USE_THIS_REVIEW_PROTOCOL";
        let task = "summarize the current workspace state";

        save_skill_config(
            fixture.desktop_core().as_ref(),
            skill_save_request(&agent_did, skill_id, skill_body),
        )
        .await?;
        wait_for_remote_skill(fixture.remote_core().as_ref(), skill_id).await?;
        let generation_before_bind =
            wait_for_remote_runtime_generation(fixture.remote_core().as_ref(), &agent_did)
                .await
                .context("runtime status missing before skill binding")?;
        bind_skill_to_behavior(fixture, &agent_did, &behavior_id, skill_id, false).await?;
        wait_for_remote_behavior_skill_refs(
            fixture.remote_core().as_ref(),
            &behavior_id,
            &[skill_id],
            &[],
        )
        .await?;
        wait_for_remote_runtime_generation_after(
            fixture.remote_core().as_ref(),
            &agent_did,
            generation_before_bind,
        )
        .await
        .context("agent runtime did not reconcile the skill binding before chat submit")?;

        let submitted = send_chat_message(
            fixture.desktop_core().as_ref(),
            ChatSendRequest {
                agent_did: agent_did.clone(),
                behavior_id: Some(behavior_id.clone()),
                session_id: None,
                content: format!("/{skill_id}\n{task}"),
                caused_by_source_doc_id: None,
            },
        )
        .await?;

        let transcript_page = gents_desktop_core::client::load_session_transcript_page(
            fixture.desktop_core().node(),
            &submitted.session_id,
            Some(&agent_did),
            None,
            None,
            None,
        )
        .await?;
        let context_store = gents_desktop_core::client::load_session_context_store(
            fixture.desktop_core().node(),
            &submitted.session_id,
            Some(&agent_did),
            None,
        )
        .await?;
        let session = build_session_snapshot_for_agent_with_transcript(
            fixture.desktop_core().as_ref(),
            Some(&agent_did),
            &submitted.session_id,
            Some(&submitted.request_id),
            Some(&transcript_page.store),
            Some(&context_store),
            true,
            true,
        )
        .await
        .context("desktop session snapshot missing after skill chat submit")?;
        let pending_turn = session
            .pending_turn
            .context("desktop session snapshot did not expose the opening turn")?;
        assert_eq!(pending_turn.content, task);
        assert_eq!(pending_turn.selected_skill_ids, vec![skill_id.to_string()]);

        let request =
            wait_for_remote_request(fixture.remote_core().as_ref(), &submitted.request_id).await?;
        assert_eq!(request.get("content").and_then(Value::as_str), Some(task));
        assert_eq!(
            selected_skill_ids_from_metadata(request.get("metadata").and_then(Value::as_str)),
            vec![skill_id.to_string()]
        );

        let captured = wait_for_captured_chat_request(mock, skill_body).await?;
        assert!(
            captured.to_string().contains(skill_body),
            "mock model request did not include selected skill body: {captured}"
        );

        Ok(())
    }

    fn live_fixture_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("live fixture test lock poisoned")
    }

    async fn bind_skill_to_behavior(
        fixture: &LiveBridgeFixture,
        agent_did: &str,
        behavior_id: &str,
        skill_id: &str,
        include_exclude: bool,
    ) -> Result<()> {
        fixture.desktop_core().refresh_store().await?;
        let snapshot = fixture.desktop_core().store().snapshot();
        let mut behavior = snapshot
            .behavior_row(agent_did, behavior_id)
            .cloned()
            .with_context(|| format!("behavior {behavior_id} not present in desktop store"))?;
        behavior.skill_refs = vec![skill_id.to_string()];
        behavior.skill_excludes = if include_exclude {
            vec![skill_id.to_string()]
        } else {
            Vec::new()
        };
        fixture.desktop_core().save_behavior(&behavior).await
    }

    fn skill_save_request(agent_did: &str, skill_id: &str, instructions: &str) -> SkillSaveRequest {
        SkillSaveRequest {
            skill_id: skill_id.to_string(),
            agent_did: agent_did.to_string(),
            scope: "behavior".to_string(),
            name: skill_id.to_string(),
            description: Some(format!("Test skill {skill_id}")),
            instructions: instructions.to_string(),
            tool_refs: Vec::new(),
            display_name: Some(skill_id.to_string()),
            enabled: Some(true),
        }
    }

    async fn wait_for_remote_skill(core: &ClientCore, skill_id: &str) -> Result<Value> {
        wait_for_row(
            "remote Skill create",
            Duration::from_secs(60),
            || async move {
                let rows = query_skill_rows(core, skill_id).await?;
                Ok(rows.into_iter().next())
            },
        )
        .await
    }

    async fn wait_for_remote_skill_absent(core: &ClientCore, skill_id: &str) -> Result<()> {
        wait_for_condition(
            "remote Skill delete",
            Duration::from_secs(60),
            || async move { Ok(query_skill_rows(core, skill_id).await?.is_empty()) },
        )
        .await
    }

    async fn wait_for_remote_behavior_skill_refs(
        core: &ClientCore,
        behavior_id: &str,
        expected_refs: &[&str],
        expected_excludes: &[&str],
    ) -> Result<()> {
        wait_for_condition(
            "remote AgentBehavior skill refs",
            Duration::from_secs(60),
            || async move {
                let rows = query_agent_behavior_rows(core, behavior_id).await?;
                let Some(row) = rows.first() else {
                    return Ok(false);
                };
                Ok(string_list(row.get("skill_refs")) == expected_refs
                    && string_list(row.get("skill_excludes")) == expected_excludes)
            },
        )
        .await
    }

    async fn wait_for_remote_request(core: &ClientCore, request_id: &str) -> Result<Value> {
        wait_for_row(
            "remote AgentRequest",
            Duration::from_secs(60),
            || async move {
                let request_id = escape_graphql_string(request_id);
                let query = format!(
                    r#"{{
                    AgentRequest(
                        filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                        limit: 1
                    ) {{ request_id content metadata lifecycle_state status }}
                }}"#
                );
                let rows = query_rows(core, &query, "AgentRequest").await?;
                Ok(rows.into_iter().next())
            },
        )
        .await
    }

    async fn wait_for_remote_runtime_generation(core: &ClientCore, agent_did: &str) -> Result<u64> {
        wait_for_row(
            "remote authoritative runtime generation",
            Duration::from_secs(60),
            || async move { query_runtime_observation(core, agent_did).await },
        )
        .await
        .map(|observation| observation.active_generation)
    }

    async fn wait_for_remote_runtime_generation_after(
        core: &ClientCore,
        agent_did: &str,
        previous_generation: u64,
    ) -> Result<()> {
        wait_for_condition(
            "remote authoritative runtime generation advance",
            Duration::from_secs(90),
            || async move {
                let Some(observation) = query_runtime_observation(core, agent_did).await? else {
                    return Ok(false);
                };
                if observation.last_reconcile_result == "error" {
                    bail!(
                        "runtime reconcile failed while waiting for skill binding: {}",
                        observation.last_reconcile_error
                    );
                }
                Ok(observation.active_generation > previous_generation
                    && observation.reconcile_phase == "idle")
            },
        )
        .await
    }

    async fn wait_for_captured_chat_request(
        mock: &MockChatEndpoint,
        needle: &str,
    ) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let captured = mock.captured_chat_requests();
            if let Some(request) = captured
                .iter()
                .find(|request| request.to_string().contains(needle))
            {
                return Ok(request.clone());
            }
            if Instant::now() >= deadline {
                bail!(
                    "timed out waiting for mock chat request containing {needle:?}; captured={captured:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn query_skill_rows(core: &ClientCore, skill_id: &str) -> Result<Vec<Value>> {
        let skill_id = escape_graphql_string(skill_id);
        let query = format!(
            r#"{{
                Skill(
                    filter: {{ skill_id: {{ _eq: "{skill_id}" }} }}
                ) {{ skill_id agent_did scope name instructions enabled }}
            }}"#
        );
        query_rows(core, &query, "Skill").await
    }

    async fn query_agent_behavior_rows(core: &ClientCore, behavior_id: &str) -> Result<Vec<Value>> {
        let behavior_id = escape_graphql_string(behavior_id);
        let query = format!(
            r#"{{
                AgentBehavior(
                    filter: {{ behavior_id: {{ _eq: "{behavior_id}" }} }},
                    limit: 1
                ) {{ behavior_id skill_refs skill_excludes }}
            }}"#
        );
        query_rows(core, &query, "AgentBehavior").await
    }

    struct RemoteRuntimeObservation {
        active_generation: u64,
        reconcile_phase: String,
        last_reconcile_result: String,
        last_reconcile_error: String,
    }

    async fn query_runtime_observation(
        core: &ClientCore,
        agent_did: &str,
    ) -> Result<Option<RemoteRuntimeObservation>> {
        let expected_agent_did = agent_did.to_string();
        let agent_did = escape_graphql_string(agent_did);
        let query = format!(
            r#"{{
                AgentBehaviorReadiness(
                    filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                    limit: 1
                ) {{ agent_did snapshot_json updated_at }}
                AgentRuntime(
                    filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                    limit: 1
                ) {{
                    reconcile_phase
                    last_reconcile_result
                    last_reconcile_error
                }}
            }}"#
        );
        let response = core.node().execute(&query).await;
        if response.has_errors() {
            bail!("query runtime observation failed: {:?}", response.errors);
        }
        let data = response
            .data
            .as_ref()
            .context("runtime observation missing data")?;
        let Some(readiness_value) = data
            .get("AgentBehaviorReadiness")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .cloned()
        else {
            return Ok(None);
        };
        let readiness_row: AgentBehaviorReadinessRow = serde_json::from_value(readiness_value)?;
        let readiness = decode_behavior_readiness_snapshot(&readiness_row, &expected_agent_did)
            .map_err(|reason| anyhow::anyhow!("invalid behavior readiness: {reason:?}"))?;
        let Some(runtime) = data
            .get("AgentRuntime")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
        else {
            return Ok(None);
        };
        Ok(Some(RemoteRuntimeObservation {
            active_generation: readiness.active_generation,
            reconcile_phase: runtime
                .get("reconcile_phase")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            last_reconcile_result: runtime
                .get("last_reconcile_result")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            last_reconcile_error: runtime
                .get("last_reconcile_error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string(),
        }))
    }

    async fn query_rows(core: &ClientCore, query: &str, collection: &str) -> Result<Vec<Value>> {
        let response = core.node().execute(query).await;
        if response.has_errors() {
            bail!("query {collection} failed: {:?}", response.errors);
        }
        Ok(response
            .data
            .as_ref()
            .and_then(|data| data.get(collection))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    async fn wait_for_row<T, F, Fut>(
        label: &'static str,
        timeout: Duration,
        mut check: F,
    ) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<Option<T>>>,
    {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(row) = check().await? {
                return Ok(row);
            }
            if Instant::now() >= deadline {
                bail!("timed out waiting for {label}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn wait_for_condition<F, Fut>(
        label: &'static str,
        timeout: Duration,
        mut check: F,
    ) -> Result<()>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<bool>>,
    {
        let deadline = Instant::now() + timeout;
        loop {
            if check().await? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!("timed out waiting for {label}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn string_list(value: Option<&Value>) -> Vec<String> {
        match value {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            Some(Value::String(value)) if !value.trim().is_empty() => {
                vec![value.trim().to_string()]
            }
            _ => Vec::new(),
        }
    }

    fn selected_skill_ids_from_metadata(metadata: Option<&str>) -> Vec<String> {
        let Some(metadata) = metadata else {
            return Vec::new();
        };
        let Ok(value) = serde_json::from_str::<Value>(metadata) else {
            return Vec::new();
        };
        string_list(value.get("selected_skill_ids"))
    }

    #[derive(Clone)]
    struct MockState {
        model_name: String,
        final_text: String,
        captured: Arc<Mutex<Vec<Value>>>,
    }

    struct MockChatEndpoint {
        endpoint: String,
        captured: Arc<Mutex<Vec<Value>>>,
        shutdown: Option<oneshot::Sender<()>>,
        join: Option<std::thread::JoinHandle<()>>,
    }

    impl MockChatEndpoint {
        fn start(model_name: &str, final_text: &str) -> Result<Self> {
            let captured = Arc::new(Mutex::new(Vec::new()));
            let state = Arc::new(MockState {
                model_name: model_name.to_string(),
                final_text: final_text.to_string(),
                captured: Arc::clone(&captured),
            });
            let (port_tx, port_rx) = std::sync::mpsc::channel::<u16>();
            let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
            let join = std::thread::spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                runtime.block_on(async move {
                    let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await else {
                        return;
                    };
                    let port = listener.local_addr().map(|addr| addr.port()).unwrap_or(0);
                    let _ = port_tx.send(port);
                    let app = Router::new()
                        .route("/v1/models", get(handle_models))
                        .route("/models", get(handle_models))
                        .route("/v1/chat/completions", post(handle_chat))
                        .route("/chat/completions", post(handle_chat))
                        .fallback(handle_fallback)
                        .with_state(state);
                    let _ = axum::serve(listener, app)
                        .with_graceful_shutdown(async move {
                            let _ = shutdown_rx.await;
                        })
                        .await;
                });
            });

            let port = port_rx
                .recv()
                .context("mock chat endpoint failed to bind a port")?;
            Ok(Self {
                endpoint: format!("http://127.0.0.1:{port}/v1"),
                captured,
                shutdown: Some(shutdown_tx),
                join: Some(join),
            })
        }

        fn backend_override(&self, model_name: &str) -> LiveBackendOverride {
            LiveBackendOverride {
                inference_url: Some(self.endpoint.clone()),
                model_name: Some(model_name.to_string()),
                provider: Some("openai-compatible".to_string()),
                api_key: Some("desktop-live-test-key".to_string()),
                api_key_env_var: None,
            }
        }

        fn captured_chat_requests(&self) -> Vec<Value> {
            self.captured
                .lock()
                .expect("captured mock request mutex poisoned")
                .clone()
        }
    }

    impl Drop for MockChatEndpoint {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }

    async fn handle_models(State(state): State<Arc<MockState>>) -> Response {
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::json!({ "data": [{ "id": state.model_name }] }).to_string(),
        )
            .into_response()
    }

    async fn handle_chat(State(state): State<Arc<MockState>>, body: String) -> Response {
        let request_json = match serde_json::from_str::<Value>(&body) {
            Ok(value) => value,
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    [(header::CONTENT_TYPE, "application/json")],
                    r#"{"error":"invalid json"}"#,
                )
                    .into_response()
            }
        };
        state
            .captured
            .lock()
            .expect("captured mock request mutex poisoned")
            .push(request_json);
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            completion_text_sse(&state.final_text),
        )
            .into_response()
    }

    async fn handle_fallback() -> Response {
        (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"error":"not found"}"#,
        )
            .into_response()
    }

    fn completion_text_sse(text: &str) -> String {
        let chunk_1 = serde_json::json!({
            "choices": [{ "delta": { "content": text }, "finish_reason": null }],
            "usage": null
        });
        let chunk_2 = serde_json::json!({
            "choices": [{
                "delta": { "content": null, "tool_calls": [] },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 24, "completion_tokens": 6, "total_tokens": 30 }
        });
        format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            serde_json::to_string(&chunk_1).expect("serialize completion chunk 1"),
            serde_json::to_string(&chunk_2).expect("serialize completion chunk 2"),
        )
    }
}
